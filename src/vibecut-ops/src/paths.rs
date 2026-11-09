//! Mount-point conventions shared by every transport. Mirrors
//! `docker/compose.yml` — `/footage` is the read-only bind-mount for
//! source material, `/projects` is the read-write mount for TOMLs,
//! rendered output, and session logs.
//!
//! Every path a caller can influence goes through one of the checked
//! resolvers here: [`resolve_projects_checked`] and
//! [`resolve_footage_checked`] for paths passed as arguments, and
//! [`check_config_paths`] for the paths written inside a config.

use std::path::{Component, Path, PathBuf};

use camino::Utf8Path;
use vibecut::project_config::{CardLayer, Config};

use crate::error::OpsError;

pub const FOOTAGE_ROOT: &str = "/footage";
pub const PROJECTS_ROOT: &str = "/projects";

/// Where a config may name a font file from besides the mounts: the
/// image's own fonts (fontconfig's DejaVu lives here) are public and
/// read-only, and a font is the one input a user is likely to point at
/// the image itself for.
const SYSTEM_FONT_DIRS: &[&str] = &["/usr/share/fonts", "/usr/local/share/fonts"];

/// `[project].models_dir`'s built-in default in
/// `vibecut::project_config`. It names no location the caller chose, so
/// it is left alone; any other value is confined like the rest of a
/// config's inputs.
const DEFAULT_MODELS_DIR: &str = "models";

/// Resolve a user-supplied footage folder (`discover_sources`,
/// `propose_vibe`) with the same checks as
/// [`resolve_projects_checked`]:
///
/// - relative paths are anchored under `/footage`; `""` and `"."` mean
///   the whole mount;
/// - an absolute path must sit under `/footage` or `/projects`, the two
///   mounts a config's inputs may come from (see
///   [`check_config_paths`]), so a folder discovery accepts is one a
///   render accepts too;
/// - no `..`, segment-wise prefixes, no symlinks out of the mount.
///
/// Rejections are [`OpsError::InvalidInput`] — a 400 on REST.
pub fn resolve_footage_checked(p: &str) -> Result<PathBuf, OpsError> {
    resolve_footage_under(Path::new(FOOTAGE_ROOT), Path::new(PROJECTS_ROOT), p)
}

/// [`resolve_footage_checked`] against arbitrary roots, so tests can
/// use temp dirs instead of the mounts.
pub(crate) fn resolve_footage_under(
    footage: &Path,
    projects: &Path,
    p: &str,
) -> Result<PathBuf, OpsError> {
    let resolved = match resolve_under(footage, p) {
        Ok(path) => Ok(path),
        Err(_) if is_rooted(&p.replace('\\', "/")) => resolve_under(projects, p),
        Err(e) => Err(e),
    };
    resolved.map_err(|_| {
        OpsError::invalid_with_hint(
            format!(
                "footage folder is outside {} and {}: {p:?}",
                footage.display(),
                projects.display()
            ),
            format!(
                "pass a folder relative to {0}, or an absolute path under {0} or {1}",
                footage.display(),
                projects.display()
            ),
        )
    })
}

/// Resolve a user-supplied path against `/projects`, refusing anything
/// that would land outside it. Every transport hands this paths from
/// unauthenticated callers (REST bodies and query strings, MCP tool
/// arguments), so it is the one place that keeps them inside the
/// mount:
///
/// - a `..` component anywhere is rejected outright, even one that
///   would stay inside (`out/../cfg.toml`) — there is no legitimate
///   reason to send one;
/// - relative paths are anchored under `/projects`; `""` and `"."`
///   mean the mount itself;
/// - rooted paths (POSIX `/…`, a Windows drive `C:…`, UNC `//…`) are
///   accepted only when they already sit under `/projects`, compared
///   segment by segment so `/projects-evil` doesn't pass as a prefix;
/// - the deepest part of the result that exists is canonicalised and
///   must still be under the canonical root, so a symlink inside the
///   mount can't lead back out of it.
///
/// Rejections are [`OpsError::InvalidInput`] — a 400 on REST.
pub fn resolve_projects_checked(p: &str) -> Result<PathBuf, OpsError> {
    resolve_under(Path::new(PROJECTS_ROOT), p)
}

/// [`resolve_projects_checked`] against an arbitrary root, so tests
/// can exercise the real checks (symlinks included) against a temp
/// dir instead of a `/projects` mount.
///
/// Both `/` and `\` count as separators, on every platform, and
/// "rooted" is decided on the string rather than with
/// `Path::is_absolute()`: that method is platform-dependent — on
/// Windows `Path::new("/etc/passwd").is_absolute()` is **false** (no
/// drive prefix), so a POSIX-rooted path would fall through to the
/// "relative, anchor it under the root" branch on a dev host while
/// being exactly the traversal this guard exists to catch on Linux,
/// where the server actually runs.
pub(crate) fn resolve_under(root: &Path, p: &str) -> Result<PathBuf, OpsError> {
    let escapes = || {
        OpsError::invalid_with_hint(
            format!("path escapes the {} mount: {p:?}", root.display()),
            format!(
                "pass a path relative to {0}, or an absolute path under {0}",
                root.display()
            ),
        )
    };

    if p.contains('\0') {
        return Err(OpsError::invalid(format!("path contains a NUL byte: {p:?}")));
    }

    let normalized = p.replace('\\', "/");
    let segments = path_segments(&normalized);
    if segments.contains(&"..") {
        return Err(escapes());
    }

    let rest: &[&str] = if is_rooted(&normalized) {
        let root_normalized = root.to_string_lossy().replace('\\', "/");
        let root_segments = path_segments(&root_normalized);
        if !segments.starts_with(&root_segments) {
            return Err(escapes());
        }
        &segments[root_segments.len()..]
    } else {
        &segments
    };

    let mut candidate = root.to_path_buf();
    for seg in rest {
        // Each segment must be one plain file name on *this* platform:
        // on Windows a mid-path `C:` parses as a drive prefix, and
        // `PathBuf::push` would replace everything before it.
        let mut comps = Path::new(seg).components();
        match (comps.next(), comps.next()) {
            (Some(Component::Normal(_)), None) => candidate.push(seg),
            _ => return Err(escapes()),
        }
    }

    if stays_under_root(root, &candidate) {
        Ok(candidate)
    } else {
        Err(escapes())
    }
}

/// Rooted on any platform: POSIX `/…` (UNC `//…` included) or a
/// Windows drive `C:…`. Callers normalise `\` to `/` first.
fn is_rooted(normalized: &str) -> bool {
    normalized.starts_with('/') || normalized.as_bytes().get(1) == Some(&b':')
}

/// Split on `/` (callers normalise `\` first), dropping empty and `.`
/// segments so `a//b` and `./a` compare the same as `a/b` and `a`.
fn path_segments(normalized: &str) -> Vec<&str> {
    normalized
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect()
}

/// The lexical checks above can't see symlinks: `link/cfg.toml` is
/// lexically fine when `link -> /etc`. Canonicalise the deepest part
/// of `candidate` that exists — the file itself on a read, its nearest
/// existing parent on a write — and require it to still sit under the
/// canonical root.
fn stays_under_root(root: &Path, candidate: &Path) -> bool {
    let Ok(canon_root) = root.canonicalize() else {
        // No root on disk (a dev host or test container without the
        // mount): nothing under it exists either, so there is no
        // symlink to follow out of it.
        return true;
    };
    let mut probe = candidate;
    loop {
        if probe.symlink_metadata().is_ok() {
            // It exists, possibly as a dangling link. If it can't be
            // canonicalised (a dangling or unreadable link), refuse it
            // rather than let a write follow it out of the mount.
            return probe
                .canonicalize()
                .is_ok_and(|c| c.starts_with(&canon_root));
        }
        match probe.parent() {
            Some(parent) if parent.starts_with(root) => probe = parent,
            // Walked past the root without finding it on disk — it
            // existed a moment ago, so fail closed.
            _ => return false,
        }
    }
}

/// Require a `.toml` extension — config writes may only create or
/// overwrite config files, never the rendered mp4s, manifests, or
/// session logs that share the `/projects` mount.
pub fn ensure_toml_extension(path: &Path) -> Result<(), OpsError> {
    let is_toml = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("toml"));
    if is_toml {
        Ok(())
    } else {
        Err(OpsError::invalid(format!(
            "config path must end in .toml: {}",
            path.display()
        )))
    }
}

/// The mounts a config's own paths may point into, and the directory
/// its relative paths resolve against.
#[derive(Debug, Clone)]
pub(crate) struct ConfigRoots {
    pub(crate) footage: PathBuf,
    pub(crate) projects: PathBuf,
    /// The pipeline opens a config's paths exactly as written, so a
    /// relative one resolves against the server process's working
    /// directory: not the config's folder, and not `/projects`. In the
    /// image that is `/` (the runtime stage sets no `WORKDIR`), so
    /// `out/x.mp4` means `/out/x.mp4`.
    pub(crate) cwd: PathBuf,
}

impl ConfigRoots {
    /// The real mounts and this process's working directory.
    pub(crate) fn mounts() -> Result<Self, OpsError> {
        let cwd = std::env::current_dir()
            .map_err(|e| OpsError::internal(format!("cannot read the working directory: {e}")))?;
        Ok(Self {
            footage: PathBuf::from(FOOTAGE_ROOT),
            projects: PathBuf::from(PROJECTS_ROOT),
            cwd,
        })
    }
}

/// Resolve `p`, a path written inside a config, the way the pipeline
/// opens it (as given if rooted, otherwise against `cwd`) and require
/// the result to sit under one of `roots`, with [`resolve_under`]'s
/// lexical and symlink checks. `field` names the setting in the error.
pub(crate) fn confine_config_path(
    field: &str,
    p: &str,
    roots: &[&Path],
    cwd: &Path,
) -> Result<PathBuf, OpsError> {
    let relative = !is_rooted(&p.replace('\\', "/"));
    let absolute = if relative {
        cwd.join(p).to_string_lossy().into_owned()
    } else {
        p.to_owned()
    };
    if let Some(found) = roots.iter().find_map(|root| resolve_under(root, &absolute).ok()) {
        return Ok(found);
    }
    let mounts = roots
        .iter()
        .map(|r| r.display().to_string())
        .collect::<Vec<_>>()
        .join(" or ");
    let mut message = format!("{field} = {p:?} is outside {mounts}");
    if relative {
        message.push_str(&format!(
            " (a relative path in a config resolves against the server's working \
             directory, {}, not against the config's folder)",
            cwd.display()
        ));
    }
    Err(OpsError::invalid_with_hint(
        message,
        format!("use an absolute path under {mounts}"),
    ))
}

/// Confine every path inside `cfg` before it is rendered. A config
/// saved under `/projects` can name any path at all, and the pipeline
/// opens them as written, so this is what keeps a render inside the
/// mounts:
///
/// - what a render **writes** must sit under `/projects`: every
///   `[[outputs]].path`, the manifest beside the first one (render
///   results and drop-segment feedback read it back), and an explicit
///   `[project].temp_dir`. The rest of what it writes (the default
///   workspace, the analysis cache) goes in the first output's folder;
/// - what it **reads** must sit under `/footage` or `/projects`:
///   `[input].folder`, `[[input.videos]]`, `[[input.images]]`,
///   `[soundtrack].files`, intro/outro image layers, `[grading].lut`,
///   and a non-default `[project].models_dir`. A font file may also
///   come from the system font directories; a bare font name is an OS
///   font lookup rather than a path, and only has to stay free of `..`.
///
/// `[[input.ranges]]`, `[[selection.exclude]]` and multi-angle entries
/// name sources by file name or id, matched against what was
/// discovered and never opened, so they aren't checked. Rejections are
/// [`OpsError::InvalidInput`] — a 400 on REST.
pub fn check_config_paths(cfg: &Config) -> Result<(), OpsError> {
    check_config_paths_in(cfg, &ConfigRoots::mounts()?)
}

pub(crate) fn check_config_paths_in(cfg: &Config, roots: &ConfigRoots) -> Result<(), OpsError> {
    let cwd = roots.cwd.as_path();
    let writable = [roots.projects.as_path()];
    let readable = [roots.footage.as_path(), roots.projects.as_path()];
    let font_roots: Vec<&Path> = readable
        .iter()
        .copied()
        .chain(SYSTEM_FONT_DIRS.iter().map(Path::new))
        .collect();

    let write = |field: &str, p: &Utf8Path| {
        confine_config_path(field, p.as_str(), &writable, cwd).map(drop)
    };
    let read = |field: &str, p: &Utf8Path| {
        confine_config_path(field, p.as_str(), &readable, cwd).map(drop)
    };
    let font = |field: &str, name: &str| {
        let normalized = name.replace('\\', "/");
        if is_rooted(&normalized) {
            confine_config_path(field, name, &font_roots, cwd).map(drop)
        } else if path_segments(&normalized).contains(&"..") {
            Err(OpsError::invalid(format!("{field} = {name:?} must not contain `..`")))
        } else {
            Ok(())
        }
    };

    for (i, o) in cfg.outputs.iter().enumerate() {
        write(&format!("outputs[{i}].path"), &o.path)?;
    }
    if let Some(first) = cfg.outputs.first() {
        let manifest = vibecut::run_pipeline::manifest_path_for(&first.path);
        write("the manifest for outputs[0].path", &manifest)?;
    }
    if let Some(dir) = &cfg.project.temp_dir {
        write("project.temp_dir", dir)?;
    }

    if cfg.project.models_dir.as_str() != DEFAULT_MODELS_DIR {
        read("project.models_dir", &cfg.project.models_dir)?;
    }
    if let Some(folder) = &cfg.input.folder {
        read("input.folder", folder)?;
    }
    for (i, v) in cfg.input.videos.iter().enumerate() {
        read(&format!("input.videos[{i}].path"), &v.path)?;
    }
    for (i, img) in cfg.input.images.iter().enumerate() {
        read(&format!("input.images[{i}].path"), &img.path)?;
    }
    if let Some(soundtrack) = &cfg.soundtrack {
        for (i, f) in soundtrack.files.iter().enumerate() {
            read(&format!("soundtrack.files[{i}]"), f)?;
        }
    }
    if let Some(lut) = &cfg.grading.lut {
        read("grading.lut", lut)?;
    }
    for (card_name, card) in [("intro", &cfg.intro), ("outro", &cfg.outro)] {
        let Some(card) = card else { continue };
        for (i, layer) in card.layers.iter().enumerate() {
            match layer {
                CardLayer::Image(im) => read(&format!("{card_name}.layers[{i}].path"), &im.path)?,
                CardLayer::Text(t) => {
                    if let Some(f) = &t.font {
                        font(&format!("{card_name}.layers[{i}].font"), f)?;
                    }
                }
            }
        }
    }
    for (i, t) in cfg.texts.iter().enumerate() {
        if let Some(f) = &t.font {
            font(&format!("text[{i}].font"), f)?;
        }
    }
    Ok(())
}

/// Turn an arbitrary title into a filesystem-safe basename.
pub fn slugify(s: &str) -> String {
    let re = regex::Regex::new(r"[^A-Za-z0-9_-]+").unwrap();
    let result = re.replace_all(s, "_").trim_matches('_').to_lowercase();
    if result.is_empty() {
        "edit".to_string()
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Zi însorită la mare"), "zi_nsorit_la_mare");
        assert_eq!(slugify("Excursie 2026"), "excursie_2026");
        assert_eq!(slugify(""), "edit");
        assert_eq!(slugify("simple"), "simple");
        assert_eq!(slugify("ALL_CAPS"), "all_caps");
    }

    #[test]
    fn footage_paths_resolve_under_either_mount() {
        assert_eq!(resolve_footage_checked("").unwrap(), PathBuf::from("/footage"));
        assert_eq!(resolve_footage_checked(".").unwrap(), PathBuf::from("/footage"));
        assert_eq!(
            resolve_footage_checked("vacation").unwrap(),
            PathBuf::from("/footage/vacation")
        );
        assert_eq!(
            resolve_footage_checked("/footage/vacation").unwrap(),
            PathBuf::from("/footage/vacation")
        );
        assert_eq!(
            resolve_footage_checked("/projects/clips").unwrap(),
            PathBuf::from("/projects/clips")
        );
        // Relative paths anchor under /footage only.
        assert_eq!(
            resolve_footage_checked("projects/clips").unwrap(),
            PathBuf::from("/footage/projects/clips")
        );
    }

    #[test]
    fn footage_paths_outside_the_mounts_are_rejected() {
        for p in [
            "/",
            "/etc",
            "/abs",
            "/footage-evil",
            "../etc",
            "vacation/../../etc",
            "..\\etc",
            "/footage/../etc",
            "C:/Users",
            "cfg\0.mp4",
        ] {
            match resolve_footage_checked(p) {
                Err(OpsError::InvalidInput { .. }) => {}
                other => panic!("{p:?} should be rejected as invalid input, got {other:?}"),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn footage_symlinks_out_of_the_mount_are_rejected() {
        use std::os::unix::fs::symlink;

        let footage = temp_root("footage_root");
        let projects = temp_root("footage_projects");
        let outside = temp_root("footage_outside");
        std::fs::create_dir_all(footage.join("trip")).unwrap();
        symlink(&outside, footage.join("escape")).unwrap();

        assert_eq!(
            resolve_footage_under(&footage, &projects, "trip").unwrap(),
            footage.join("trip")
        );
        let absolute = footage.join("escape").to_string_lossy().into_owned();
        for p in ["escape", absolute.as_str()] {
            assert!(
                matches!(
                    resolve_footage_under(&footage, &projects, p),
                    Err(OpsError::InvalidInput { .. })
                ),
                "{p:?} leads out of the mount"
            );
        }

        for d in [&footage, &projects, &outside] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    fn assert_rejected(p: &str) {
        match resolve_projects_checked(p) {
            Err(OpsError::InvalidInput { .. }) => {}
            other => panic!("{p:?} should be rejected as invalid input, got {other:?}"),
        }
    }

    /// A fresh, empty, canonical temp dir standing in for `/projects`.
    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sv_paths_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn rejects_absolute_paths_outside_projects() {
        assert_rejected("/etc/passwd");
        assert_rejected("/proc/self/environ");
        assert_rejected("C:/Windows/System32");
        assert_rejected("C:\\Windows\\System32");
        assert_rejected("D:/anything");
        assert_rejected("c:relative-to-drive.toml");
        assert_rejected("//server/share/cfg.toml");
        assert_rejected("\\\\server\\share\\cfg.toml");
    }

    #[test]
    fn rejects_sibling_that_shares_the_root_prefix() {
        assert_rejected("/projects-evil/cfg.toml");
        assert_rejected("/projectsX");
    }

    #[test]
    fn rejects_parent_dir_traversal_with_either_separator() {
        assert_rejected("..");
        assert_rejected("../../etc/passwd");
        assert_rejected("out/../../etc/passwd");
        assert_rejected("..\\..\\Windows\\System32");
        assert_rejected("out\\..\\..\\etc\\passwd");
        assert_rejected("/projects/../etc/passwd");
        assert_rejected("\\projects\\..\\etc\\passwd");
        assert_rejected("/projects/out/..\\../etc/passwd");
        // Even a `..` that would stay inside is refused.
        assert_rejected("out/../cfg.toml");
    }

    #[test]
    fn rejects_nul_bytes() {
        assert_rejected("cfg.toml\0.mp4");
    }

    #[test]
    fn accepts_relative_paths() {
        assert_eq!(
            resolve_projects_checked("foo.toml").unwrap(),
            PathBuf::from("/projects/foo.toml")
        );
        assert_eq!(
            resolve_projects_checked("out/render.mp4").unwrap(),
            PathBuf::from("/projects/out/render.mp4")
        );
        assert_eq!(
            resolve_projects_checked("out\\render.mp4").unwrap(),
            PathBuf::from("/projects/out/render.mp4")
        );
        assert_eq!(
            resolve_projects_checked("./out//render.mp4").unwrap(),
            PathBuf::from("/projects/out/render.mp4")
        );
    }

    #[test]
    fn empty_and_dot_mean_the_mount_itself() {
        assert_eq!(resolve_projects_checked("").unwrap(), PathBuf::from("/projects"));
        assert_eq!(resolve_projects_checked(".").unwrap(), PathBuf::from("/projects"));
        assert_eq!(resolve_projects_checked("/projects").unwrap(), PathBuf::from("/projects"));
    }

    #[test]
    fn accepts_absolute_paths_under_projects() {
        assert_eq!(
            resolve_projects_checked("/projects/out/render.mp4").unwrap(),
            PathBuf::from("/projects/out/render.mp4")
        );
        assert_eq!(
            resolve_projects_checked("\\projects\\cfg.toml").unwrap(),
            PathBuf::from("/projects/cfg.toml")
        );
    }

    #[test]
    fn real_root_accepts_relative_absolute_and_not_yet_existing_paths() {
        let root = temp_root("real");
        std::fs::write(root.join("cfg.toml"), "x").unwrap();

        assert_eq!(resolve_under(&root, "cfg.toml").unwrap(), root.join("cfg.toml"));
        let abs = root.join("cfg.toml");
        assert_eq!(resolve_under(&root, &abs.to_string_lossy()).unwrap(), abs);
        // A write target whose parent directories don't exist yet.
        assert_eq!(
            resolve_under(&root, "new/dir/cfg.toml").unwrap(),
            root.join("new").join("dir").join("cfg.toml")
        );
        assert!(matches!(
            resolve_under(&root, "../outside.toml"),
            Err(OpsError::InvalidInput { .. })
        ));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn rejects_mid_path_drive_prefix_on_windows() {
        // `PathBuf::push("C:")` would discard the root on Windows.
        assert_rejected("out/C:/Windows/System32");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_that_escape_the_root() {
        use std::os::unix::fs::symlink;

        let root = temp_root("symlink_root");
        let outside = temp_root("symlink_outside");
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();

        // A directory link out of the mount: both a read of an existing
        // file and a write of a new one through it must be refused.
        symlink(&outside, root.join("link")).unwrap();
        for p in ["link/secret.txt", "link/new.toml", "link/deeper/new.toml"] {
            assert!(
                matches!(resolve_under(&root, p), Err(OpsError::InvalidInput { .. })),
                "{p:?} goes through a symlink out of the root"
            );
        }

        // A file link out of the mount.
        symlink(outside.join("secret.txt"), root.join("evil.toml")).unwrap();
        assert!(matches!(
            resolve_under(&root, "evil.toml"),
            Err(OpsError::InvalidInput { .. })
        ));

        // A dangling link: a write would follow it and create the target
        // outside the mount.
        symlink(outside.join("not-yet.toml"), root.join("dangling.toml")).unwrap();
        assert!(matches!(
            resolve_under(&root, "dangling.toml"),
            Err(OpsError::InvalidInput { .. })
        ));

        // A link that stays inside the mount is fine.
        std::fs::create_dir_all(root.join("real")).unwrap();
        symlink(root.join("real"), root.join("alias")).unwrap();
        assert_eq!(
            resolve_under(&root, "alias/cfg.toml").unwrap(),
            root.join("alias").join("cfg.toml")
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn toml_extension_required() {
        assert!(ensure_toml_extension(Path::new("/projects/cfg.toml")).is_ok());
        assert!(ensure_toml_extension(Path::new("/projects/CFG.TOML")).is_ok());
        for p in ["/projects/out/render.mp4", "/projects/cfg", "/projects/cfg.toml.bak"] {
            assert!(matches!(
                ensure_toml_extension(Path::new(p)),
                Err(OpsError::InvalidInput { .. })
            ));
        }
    }

    // ---------- paths inside a config ----------

    /// Temp `footage/`, `projects/` and `elsewhere/` dirs; relative
    /// config paths resolve against `elsewhere`, like `/` in the image.
    fn config_roots(tag: &str) -> ConfigRoots {
        let base = temp_root(&format!("cfg_{tag}"));
        for d in ["footage", "projects", "elsewhere"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        ConfigRoots {
            footage: base.join("footage"),
            projects: base.join("projects"),
            cwd: base.join("elsewhere"),
        }
    }

    fn remove_roots(r: &ConfigRoots) {
        let _ = std::fs::remove_dir_all(r.footage.parent().unwrap());
    }

    /// A config whose every path is inside the mounts, plus `extra`
    /// TOML (tables only) appended.
    fn config_with(r: &ConfigRoots, extra: &str) -> Config {
        let footage = r.footage.display();
        let projects = r.projects.display();
        let toml_text = format!(
            r#"
schema_version = 1
[project]
name = "x"
target_duration = "10s"
fps = 30
[input]
folder = '{footage}/trip'
[[outputs]]
path = '{projects}/out/x.mp4'
resolution = "1920x1080"
{extra}
"#
        );
        toml::from_str(&toml_text).unwrap_or_else(|e| panic!("{e}\n{toml_text}"))
    }

    fn assert_rejected_field(cfg: &Config, roots: &ConfigRoots, field: &str) {
        match check_config_paths_in(cfg, roots) {
            Err(OpsError::InvalidInput { message, .. }) => {
                assert!(message.contains(field), "expected {field} in {message:?}")
            }
            other => panic!("{field} should be rejected as invalid input, got {other:?}"),
        }
    }

    #[test]
    fn config_inside_the_mounts_passes() {
        let r = config_roots("ok");
        let footage = r.footage.display();
        let projects = r.projects.display();
        let cfg = config_with(
            &r,
            &format!(
                r#"
[[input.videos]]
path = '{footage}/extra.mp4'
[[input.images]]
path = '{projects}/photos/a.jpg'
[soundtrack]
files = ['{footage}/a.mp3', '{projects}/music/b.mp3']
[grading]
lut = '{projects}/luts/warm.cube'
[intro]
duration = "2s"
[[intro.layers]]
kind = "image"
path = '{projects}/logos/brand.png'
[[intro.layers]]
kind = "text"
content = "hi"
font = "arialbd.ttf"
[outro]
duration = "2s"
[[outro.layers]]
kind = "text"
content = "bye"
font = "/usr/share/fonts/vibecut-test/Missing-Bold.ttf"
[[outputs]]
path = '{projects}/out/x_vertical.mp4'
resolution = "1080x1920"
"#
            ),
        );
        // The default `models_dir = "models"` is left alone.
        assert_eq!(cfg.project.models_dir.as_str(), DEFAULT_MODELS_DIR);
        check_config_paths_in(&cfg, &r).expect("every path is inside the mounts");

        let mut cfg = cfg;
        cfg.project.temp_dir = Some(format!("{projects}/work").into());
        cfg.project.models_dir = format!("{projects}/models").into();
        check_config_paths_in(&cfg, &r).expect("temp_dir and models_dir under /projects");
        remove_roots(&r);
    }

    #[test]
    fn config_writes_must_stay_under_projects() {
        let r = config_roots("writes");
        let footage = r.footage.display();

        let mut cfg = config_with(&r, "");
        cfg.outputs[0].path = "/etc/whatever.mp4".into();
        assert_rejected_field(&cfg, &r, "outputs[0].path");

        // A later output, and one in the read-only footage mount.
        for bad in ["/etc/cron.d/x.mp4".to_string(), format!("{footage}/x.mp4")] {
            let mut cfg = config_with(&r, "");
            let mut extra = cfg.outputs[0].clone();
            extra.path = bad.into();
            cfg.outputs.push(extra);
            assert_rejected_field(&cfg, &r, "outputs[1].path");
        }

        let mut cfg = config_with(&r, "");
        cfg.project.temp_dir = Some("/tmp/workspace".into());
        assert_rejected_field(&cfg, &r, "project.temp_dir");

        // `..` is refused even when it would land back inside.
        let mut cfg = config_with(&r, "");
        cfg.outputs[0].path = format!("{}/out/../x.mp4", r.projects.display()).into();
        assert_rejected_field(&cfg, &r, "outputs[0].path");
        remove_roots(&r);
    }

    #[test]
    fn config_reads_must_stay_under_footage_or_projects() {
        let r = config_roots("reads");
        let footage = r.footage.display();
        let cases = [
            ("input.videos[0].path", "[[input.videos]]\npath = '/etc/passwd'".to_string()),
            ("input.images[0].path", "[[input.images]]\npath = '/root/a.jpg'".to_string()),
            (
                "soundtrack.files[1]",
                format!("[soundtrack]\nfiles = ['{footage}/a.mp3', '/etc/shadow']"),
            ),
            ("grading.lut", "[grading]\nlut = '/etc/passwd'".to_string()),
            (
                "intro.layers[0].path",
                "[intro]\nduration = \"2s\"\n[[intro.layers]]\nkind = \"image\"\npath = '/etc/x.png'"
                    .to_string(),
            ),
            (
                "outro.layers[0].path",
                "[outro]\nduration = \"2s\"\n[[outro.layers]]\nkind = \"image\"\npath = '/x.png'"
                    .to_string(),
            ),
            (
                "intro.layers[0].font",
                "[intro]\nduration = \"2s\"\n[[intro.layers]]\nkind = \"text\"\ncontent = \"x\"\nfont = '/etc/passwd'"
                    .to_string(),
            ),
            (
                "outro.layers[0].font",
                "[outro]\nduration = \"2s\"\n[[outro.layers]]\nkind = \"text\"\ncontent = \"x\"\nfont = '../../etc/passwd'"
                    .to_string(),
            ),
        ];
        for (field, extra) in cases {
            assert_rejected_field(&config_with(&r, &extra), &r, field);
        }

        let mut cfg = config_with(&r, "");
        cfg.input.folder = Some("/".into());
        assert_rejected_field(&cfg, &r, "input.folder");

        let mut cfg = config_with(&r, "");
        cfg.project.models_dir = "/etc".into();
        assert_rejected_field(&cfg, &r, "project.models_dir");
        remove_roots(&r);
    }

    #[test]
    fn relative_config_paths_resolve_against_the_working_directory() {
        let r = config_roots("relative");

        // `out/x.mp4` is what the pipeline opens relative to its cwd, so
        // with a cwd outside /projects it lands outside — and the error
        // says why.
        let mut cfg = config_with(&r, "");
        cfg.outputs[0].path = "out/x.mp4".into();
        match check_config_paths_in(&cfg, &r) {
            Err(OpsError::InvalidInput { message, hint }) => {
                assert!(message.contains("working directory"), "{message}");
                assert!(hint.unwrap().contains(&r.projects.display().to_string()));
            }
            other => panic!("relative output outside /projects accepted: {other:?}"),
        }

        // The same config with the server running from /projects is fine.
        let from_projects = ConfigRoots {
            cwd: r.projects.clone(),
            ..r.clone()
        };
        check_config_paths_in(&cfg, &from_projects).expect("cwd-relative output under /projects");
        assert_eq!(
            confine_config_path("f", "out/x.mp4", &[r.projects.as_path()], &r.projects).unwrap(),
            r.projects.join("out").join("x.mp4")
        );

        // And `..` can't climb out of the cwd either.
        cfg.input.folder = Some("../footage/trip".into());
        assert_rejected_field(&cfg, &from_projects, "input.folder");
        remove_roots(&r);
    }

    #[cfg(unix)]
    #[test]
    fn config_paths_through_symlinks_out_of_the_mounts_are_rejected() {
        use std::os::unix::fs::symlink;

        let r = config_roots("symlink");
        let outside = temp_root("cfg_symlink_outside");
        std::fs::write(outside.join("secret.json"), "{}").unwrap();
        symlink(&outside, r.footage.join("escape")).unwrap();

        let mut cfg = config_with(&r, "");
        cfg.input.folder = Some(format!("{}/escape", r.footage.display()).into());
        assert_rejected_field(&cfg, &r, "input.folder");

        // A manifest planted as a link out of the mount: render results
        // and drop-segment feedback would read it back.
        std::fs::create_dir_all(r.projects.join("out")).unwrap();
        symlink(outside.join("secret.json"), r.projects.join("out").join("x.manifest.json")).unwrap();
        assert_rejected_field(&config_with(&r, ""), &r, "manifest");

        remove_roots(&r);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
