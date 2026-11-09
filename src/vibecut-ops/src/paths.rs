//! Mount-point conventions shared by every transport. Mirrors
//! `docker/compose.yml` — `/footage` is the read-only bind-mount for
//! source material, `/projects` is the read-write mount for TOMLs,
//! rendered output, and session logs.

use std::path::{Component, Path, PathBuf};

use crate::error::OpsError;

pub const FOOTAGE_ROOT: &str = "/footage";
pub const PROJECTS_ROOT: &str = "/projects";

/// Resolve a user-supplied footage path against `/footage`. Empty or
/// `"."` means "the whole mount".
pub fn resolve_footage(p: &str) -> PathBuf {
    if p.is_empty() || p == "." {
        return PathBuf::from(FOOTAGE_ROOT);
    }
    let path = PathBuf::from(p);
    if path.is_absolute() {
        path
    } else {
        Path::new(FOOTAGE_ROOT).join(p)
    }
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

    let rooted = normalized.starts_with('/') || normalized.as_bytes().get(1) == Some(&b':');
    let rest: &[&str] = if rooted {
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
    fn footage_paths_resolve() {
        assert_eq!(resolve_footage(""), PathBuf::from("/footage"));
        assert_eq!(
            resolve_footage("vacation"),
            PathBuf::from("/footage/vacation")
        );
        assert_eq!(resolve_footage("/abs"), PathBuf::from("/abs"));
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
}
