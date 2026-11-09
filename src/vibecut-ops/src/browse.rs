//! Browse roots: the folders a UI may walk to pick footage from, one
//! level at a time — `/footage` plus the host's drives when the host
//! file system is mounted read-only at `/host` (Docker Desktop:
//! `/run/desktop/mnt/host` → `/host/c`, `/host/d`, …).
//!
//! Every root pairs a container path with the path the user knows it
//! by (`/host/d` ↔ `D:\`), so listings show `D:\GoPro\Alps` and the
//! same display path is accepted back (by [`browse`] and by
//! [`crate::paths::resolve_footage`], so discover / propose take it).
//!
//! Path safety: a browse path must name a folder under one root — no
//! `..`, no NUL, no hidden or system folder on the way — and its
//! canonical form (symlinks resolved) must stay under the root's own
//! canonical path. Listings never follow symlinks. Nothing here writes
//! anything.
//!
//! Configuration: `VIBECUT_BROWSE_ROOTS`, a `;`-separated list of
//! `label=container_path[=display_path]`, a bare container path, `auto`
//! (the `/host` drives, the default when unset) or `off`. `/footage`
//! ("Footage") is always the first root.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::OpsError;
use crate::paths::FOOTAGE_ROOT;

/// Where the host's file system is mounted (read-only) for browsing.
pub const HOST_ROOT: &str = "/host";
/// The roots setting (see the module docs).
pub const ENV_ROOTS: &str = "VIBECUT_BROWSE_ROOTS";

/// Subfolders listed for one folder; the rest are cut (`truncated`).
const MAX_FOLDERS: usize = 500;
/// Entries read from the listed folder itself.
const MAX_ENTRIES: usize = 5_000;
/// Entries read from each subfolder to count its videos.
const MAX_CHILD_ENTRIES: usize = 1_000;
/// Time for counting the subfolders' videos; the rest come back unscanned.
const TIME_BUDGET: Duration = Duration::from_secs(4);
/// Longest path accepted.
const MAX_INPUT_LEN: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export, rename_all = "snake_case"))]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    /// The `/footage` mount.
    Footage,
    /// A host drive (`/host/c` → `C:\`), or the whole host (`/host` →
    /// `/`) when it has no drive letters. Browsable, but too big to use
    /// whole as a footage folder.
    Drive,
    /// Any other configured folder.
    Folder,
}

/// One place browsing starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseRoot {
    pub label: String,
    /// Absolute path inside the container.
    pub container: PathBuf,
    /// The same place as the user knows it (`C:\`, `/footage`).
    pub display: String,
    pub kind: RootKind,
}

impl BrowseRoot {
    /// A Windows-style display path (`C:\…`): `\` separators, matched
    /// case-insensitively.
    fn windows(&self) -> bool {
        is_drive_path(&self.display)
    }

    fn sep(&self) -> char {
        if self.windows() {
            '\\'
        } else {
            '/'
        }
    }

    /// Display path of `rel` (components below the root).
    pub fn display_of(&self, rel: &[String]) -> String {
        if rel.is_empty() {
            return self.display.clone();
        }
        let sep = self.sep();
        let base = self.display.trim_end_matches(['/', '\\']);
        format!("{base}{sep}{}", rel.join(&sep.to_string()))
    }

    fn container_of(&self, rel: &[String]) -> PathBuf {
        rel.iter().fold(self.container.clone(), |p, c| p.join(c))
    }
}

/// `X:` at the start, then nothing or a separator.
fn is_drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || matches!(b[2], b'/' | b'\\'))
}

/// Folders a listing never shows and a path may not pass through:
/// hidden (`.git`, `.Trash`), Windows system (`$RECYCLE.BIN`, `System
/// Volume Information`), Docker Desktop's own (`wsl`, `wslg`) and other
/// places footage never lives.
pub fn is_hidden_name(name: &str) -> bool {
    const SKIP: [&str; 6] = ["System Volume Information", "wsl", "wslg", "node_modules", "lost+found", "@eaDir"];
    name.starts_with('.') || name.starts_with('$') || SKIP.iter().any(|s| s.eq_ignore_ascii_case(name))
}

/// The roots from the environment and what is mounted.
pub fn roots() -> Vec<BrowseRoot> {
    roots_from(std::env::var(ENV_ROOTS).ok().as_deref(), Path::new(FOOTAGE_ROOT), Path::new(HOST_ROOT))
}

/// [`roots`] given the setting and the two mount points.
pub fn roots_from(spec: Option<&str>, footage: &Path, host: &Path) -> Vec<BrowseRoot> {
    let mut out = vec![BrowseRoot {
        label: "Footage".into(),
        container: footage.to_path_buf(),
        display: footage.to_string_lossy().into_owned(),
        kind: RootKind::Footage,
    }];
    let spec = spec.map(str::trim).unwrap_or("");
    let tokens: Vec<&str> = if spec.is_empty() {
        vec!["auto"]
    } else {
        spec.split([';', '\n']).map(str::trim).filter(|t| !t.is_empty()).collect()
    };
    for t in tokens {
        match t.to_ascii_lowercase().as_str() {
            "auto" => out.extend(detect_host(host)),
            "off" | "none" => {}
            _ => match parse_root(t) {
                Some(r) => out.push(r),
                None => tracing::warn!(entry = t, "{ENV_ROOTS}: ignoring an entry that is not label=/absolute/path"),
            },
        }
    }
    // One root per container folder (the first wins).
    let mut seen = Vec::new();
    out.retain(|r| {
        let fresh = !seen.contains(&r.container);
        seen.push(r.container.clone());
        fresh
    });
    out
}

/// `label=/container[=display]` or `/container`.
fn parse_root(t: &str) -> Option<BrowseRoot> {
    let mut parts = t.splitn(3, '=');
    let first = parts.next()?.trim();
    let (label, container, display) = match parts.next() {
        Some(c) => (first.to_string(), c.trim(), parts.next().map(str::trim)),
        None => (String::new(), first, None),
    };
    let comps = split_posix(container);
    if !container.starts_with('/') || container.contains('\0') || comps.is_empty() || comps.iter().any(|c| *c == ".." || *c == ".") {
        return None;
    }
    let container = PathBuf::from(format!("/{}", comps.join("/")));
    let display = match display {
        Some(d) if !d.is_empty() && !d.contains('\0') => d.to_string(),
        Some(_) => return None,
        None => container.to_string_lossy().into_owned(),
    };
    let label = if label.is_empty() { comps.last().map(|c| c.to_string()).unwrap_or_default() } else { label };
    Some(BrowseRoot { label, container, display, kind: RootKind::Folder })
}

/// Docker Desktop's host mount: every one-letter folder is a drive. A
/// host mount without drive letters (Linux `/:/host`, macOS `/Users` +
/// `/Volumes` under `/host`) is one root, shown as `/`.
fn detect_host(host: &Path) -> Vec<BrowseRoot> {
    let Ok(entries) = std::fs::read_dir(host) else {
        return Vec::new();
    };
    let mut any = false;
    let mut drives: Vec<BrowseRoot> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            any = true;
            let b = name.as_bytes();
            (b.len() == 1 && b[0].is_ascii_alphabetic() && e.path().is_dir()).then(|| {
                let letter = name.to_ascii_uppercase();
                BrowseRoot {
                    label: format!("{letter}:"),
                    container: e.path(),
                    display: format!("{letter}:\\"),
                    kind: RootKind::Drive,
                }
            })
        })
        .collect();
    drives.sort_by(|a, b| a.label.cmp(&b.label));
    if drives.is_empty() && any {
        drives.push(BrowseRoot {
            label: "Computer".into(),
            container: host.to_path_buf(),
            display: "/".into(),
            kind: RootKind::Drive,
        });
    }
    drives
}

fn split_posix(s: &str) -> Vec<&str> {
    s.split('/').filter(|c| !c.is_empty()).collect()
}

fn split_windows(s: &str) -> Vec<&str> {
    s.split(['/', '\\']).filter(|c| !c.is_empty()).collect()
}

/// Which root `input` (a container or display path) is under, and the
/// raw components below it. Container paths win over display paths, the
/// deepest root over a shallower one.
fn locate(roots: &[BrowseRoot], input: &str) -> Option<(usize, Vec<String>)> {
    let tail = |comps: &[&str], n: usize| comps[n..].iter().map(|c| c.to_string()).collect::<Vec<_>>();
    if input.starts_with('/') {
        let comps = split_posix(input);
        let best = roots
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                let rc = split_posix(r.container.to_str()?);
                comps.starts_with(&rc).then_some((i, rc.len()))
            })
            .max_by_key(|&(_, n)| n);
        if let Some((i, n)) = best {
            return Some((i, tail(&comps, n)));
        }
    }
    roots
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            if r.windows() {
                if !is_drive_path(input) {
                    return None;
                }
                let comps = split_windows(input);
                let rc = split_windows(&r.display);
                let same = comps.len() >= rc.len() && comps.iter().zip(&rc).all(|(a, b)| a.eq_ignore_ascii_case(b));
                same.then(|| (i, rc.len(), tail(&comps, rc.len())))
            } else {
                if !input.starts_with('/') || !r.display.starts_with('/') {
                    return None;
                }
                let comps = split_posix(input);
                let rc = split_posix(&r.display);
                comps.starts_with(&rc).then(|| (i, rc.len(), tail(&comps, rc.len())))
            }
        })
        .max_by_key(|(_, n, _)| *n)
        .map(|(i, _, rest)| (i, rest))
}

/// A path resolved under a root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub root: usize,
    /// Components below the root (canonical: symlinks resolved).
    pub rel: Vec<String>,
}

/// Resolve `input` under `roots` with every safety check (see the module
/// docs). Missing → not found; anything else not allowed → invalid.
pub fn resolve_in(roots: &[BrowseRoot], input: &str) -> Result<Resolved, OpsError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(OpsError::invalid("no folder given"));
    }
    if input.len() > MAX_INPUT_LEN || input.contains('\0') {
        return Err(OpsError::invalid("not a folder path"));
    }
    let outside = || {
        OpsError::invalid_with_hint(
            "that folder is outside the places Vibecut can browse",
            "pick a folder under one of the drives or /footage (GET /api/browse/roots lists them)",
        )
    };
    let (root, raw) = locate(roots, input).ok_or_else(outside)?;
    let mut rel = Vec::with_capacity(raw.len());
    for c in raw {
        match c.as_str() {
            "." => {}
            ".." => return Err(OpsError::invalid("a folder path may not contain `..`")),
            _ if is_hidden_name(&c) => return Err(OpsError::invalid("hidden and system folders can't be browsed")),
            _ => rel.push(c),
        }
    }
    let r = &roots[root];
    let canon_root = r
        .container
        .canonicalize()
        .map_err(|_| OpsError::not_found_with("drive or folder", format!("{} is not available", r.display)))?;
    let target = r.container_of(&rel);
    let canon = target
        .canonicalize()
        .map_err(|_| OpsError::not_found_with("folder", r.display_of(&rel)))?;
    let Ok(below) = canon.strip_prefix(&canon_root) else {
        // A link out of the root.
        return Err(outside());
    };
    let mut rel = Vec::new();
    for c in below.components() {
        let std::path::Component::Normal(c) = c else {
            return Err(outside());
        };
        let c = c.to_str().ok_or_else(|| OpsError::invalid("non-UTF-8 folder name"))?;
        if is_hidden_name(c) {
            return Err(OpsError::invalid("hidden and system folders can't be browsed"));
        }
        rel.push(c.to_string());
    }
    if !canon.is_dir() {
        return Err(OpsError::invalid(format!("not a folder: {}", r.display_of(&rel))));
    }
    Ok(Resolved { root, rel })
}

/// The container path a display path (`D:\GoPro\Alps`) names, for
/// discover / propose. `None` when it is under no root or isn't clean
/// (`..`); container paths under a root come back unchanged. No file
/// system checks: the caller checks the folder exists.
pub fn display_to_container(roots: &[BrowseRoot], input: &str) -> Option<PathBuf> {
    if input.contains('\0') {
        return None;
    }
    let (root, raw) = locate(roots, input.trim())?;
    let mut rel = Vec::new();
    for c in raw {
        match c.as_str() {
            "." => {}
            ".." => return None,
            _ => rel.push(c),
        }
    }
    Some(roots[root].container_of(&rel))
}

/// The display path of a container path under a root (unchanged when it
/// is under none).
pub fn container_to_display(roots: &[BrowseRoot], container: &str) -> String {
    match locate(roots, container) {
        Some((root, rel)) if container.starts_with('/') => roots[root].display_of(&rel),
        _ => container.to_string(),
    }
}

/// Footage folders may be anywhere under a root, but not a whole drive
/// nor a folder holding drives (`/host`, `/`): discover walks and probes
/// everything below the folder. Compared canonically, so `/host/c/..`
/// is `/host`.
pub fn check_footage_folder(path: &Path) -> Result<(), OpsError> {
    check_footage_folder_in(&roots(), path)
}

fn check_footage_folder_in(roots: &[BrowseRoot], path: &Path) -> Result<(), OpsError> {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let path = canon(path);
    let whole = roots.iter().find(|r| r.kind == RootKind::Drive && canon(&r.container).starts_with(&path));
    match whole {
        Some(r) if canon(&r.container) == path => Err(OpsError::invalid_with_hint(
            format!("{} is a whole drive", r.display),
            "pick the folder on it that holds your trip's videos",
        )),
        Some(_) => Err(OpsError::invalid_with_hint(
            format!("{} holds whole drives", path.display()),
            "pick the folder on a drive that holds your trip's videos",
        )),
        None => Ok(()),
    }
}

/// `(container, display)` for each root shown differently from its
/// container path — so a timeline export points an editor on the host at
/// `D:/GoPro/…` instead of `/host/d/GoPro/…`.
pub fn host_path_map() -> Vec<(String, String)> {
    roots()
        .into_iter()
        .filter(|r| r.kind != RootKind::Footage)
        .filter_map(|r| {
            let c = r.container.to_str()?.to_string();
            (c != r.display).then(|| (c, r.display.trim_end_matches(['/', '\\']).to_string()))
        })
        .filter(|(_, d)| !d.is_empty())
        .collect()
}

// ---------- listings ----------

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct BrowseRootInfo {
    pub label: String,
    /// Display path (`C:\`), what `browse` takes.
    pub path: String,
    pub container_path: String,
    pub kind: RootKind,
    /// The folder is there (a mount can be configured but missing).
    pub available: bool,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct BrowseCrumb {
    pub name: String,
    /// Display path.
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct BrowseEntry {
    pub name: String,
    /// Display path.
    pub path: String,
    /// What discover / propose take.
    pub container_path: String,
    /// Videos directly inside (not below).
    pub n_videos: u32,
    /// Songs (audio files) directly inside, for picking a music folder.
    pub n_songs: u32,
    pub has_subfolders: bool,
    /// One of those videos, for a thumbnail (container path).
    pub sample_video: Option<String>,
    /// False when the counts were cut short (a huge folder, or out of
    /// time): `n_videos` is a lower bound then.
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct BrowseListing {
    /// Display path of this folder.
    pub path: String,
    pub container_path: String,
    /// The folder's name (the root's label at a root).
    pub name: String,
    pub root: String,
    pub root_kind: RootKind,
    /// Display path one level up; `None` at a root.
    pub parent: Option<String>,
    /// Root first, this folder last.
    pub crumbs: Vec<BrowseCrumb>,
    /// It can be used as a footage folder (a whole drive can't).
    pub can_use: bool,
    /// Videos directly inside.
    pub n_videos: u32,
    /// Songs directly inside.
    pub n_songs: u32,
    pub sample_video: Option<String>,
    pub folders: Vec<BrowseEntry>,
    /// Some subfolders were left out (huge folder).
    pub truncated: bool,
}

/// The container folder a display or container path names, with every
/// check [`resolve_in`] makes, and its display path. For picking a music
/// folder; a whole drive (or a folder holding drives) is refused, since
/// the library reads every song below the folder.
pub fn resolve_folder(input: &str) -> Result<(PathBuf, String), OpsError> {
    let roots = roots();
    let Resolved { root, rel } = resolve_in(&roots, input)?;
    let r = &roots[root];
    let dir = r.container_of(&rel);
    if let Err(e) = check_footage_folder_in(&roots, &dir) {
        let what = match e {
            OpsError::InvalidInput { message, .. } => message,
            other => return Err(other),
        };
        return Err(OpsError::invalid_with_hint(what, "pick the folder that holds your songs"));
    }
    Ok((dir, r.display_of(&rel)))
}

/// The configured roots.
pub async fn list_roots() -> Result<Vec<BrowseRootInfo>, OpsError> {
    tokio::task::spawn_blocking(|| roots_info(&roots()))
        .await
        .map_err(|e| OpsError::internal(format!("browse task panicked: {e}")))
}

fn roots_info(roots: &[BrowseRoot]) -> Vec<BrowseRootInfo> {
    roots
        .iter()
        .map(|r| BrowseRootInfo {
            label: r.label.clone(),
            path: r.display.clone(),
            container_path: r.container.to_string_lossy().into_owned(),
            kind: r.kind,
            available: r.container.is_dir(),
        })
        .collect()
}

/// One folder's subfolders and videos. `path` is a display or container
/// path under a root.
pub async fn browse(path: &str) -> Result<BrowseListing, OpsError> {
    let path = path.to_string();
    tokio::task::spawn_blocking(move || browse_in(&roots(), &path, TIME_BUDGET))
        .await
        .map_err(|e| OpsError::internal(format!("browse task panicked: {e}")))?
}

#[derive(Default)]
struct Scan {
    n_videos: u32,
    n_songs: u32,
    sample: Option<String>,
    dirs: Vec<String>,
    capped: bool,
}

/// Read up to `max` entries of `dir`: videos (by the engine's rules) and
/// subfolders that aren't hidden. Symlinks are skipped, never followed.
fn scan(dir: &Path, max: usize) -> Scan {
    let mut s = Scan::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return s;
    };
    for (k, e) in entries.flatten().enumerate() {
        if k >= max {
            s.capped = true;
            break;
        }
        let Ok(ft) = e.file_type() else { continue };
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            if !is_hidden_name(name) {
                s.dirs.push(name.to_string());
            }
        } else if ft.is_file() && vibecut::media_probe::is_default_video_name(name) {
            s.n_videos += 1;
            if s.sample.as_deref().map_or(true, |cur| name < cur) {
                s.sample = Some(name.to_string());
            }
        } else if ft.is_file() && crate::music::is_audio_name(name) {
            s.n_songs += 1;
        }
    }
    s
}

fn browse_in(roots: &[BrowseRoot], input: &str, budget: Duration) -> Result<BrowseListing, OpsError> {
    let started = Instant::now();
    let Resolved { root, rel } = resolve_in(roots, input)?;
    let r = &roots[root];
    let dir = r.container_of(&rel);
    let here = scan(&dir, MAX_ENTRIES);
    let mut names = here.dirs;
    names.sort_by_key(|n| (n.to_lowercase(), n.clone()));
    let mut truncated = here.capped;
    if names.len() > MAX_FOLDERS {
        names.truncate(MAX_FOLDERS);
        truncated = true;
    }
    let folders = names
        .into_iter()
        .map(|name| {
            let mut child = rel.clone();
            child.push(name.clone());
            let container = r.container_of(&child);
            let (n_videos, n_songs, has_subfolders, sample, complete) = if started.elapsed() < budget {
                let s = scan(&container, MAX_CHILD_ENTRIES);
                (s.n_videos, s.n_songs, !s.dirs.is_empty(), s.sample, !s.capped)
            } else {
                (0, 0, false, None, false)
            };
            BrowseEntry {
                path: r.display_of(&child),
                sample_video: sample.map(|f| container.join(f).to_string_lossy().into_owned()),
                container_path: container.to_string_lossy().into_owned(),
                name,
                n_videos,
                n_songs,
                has_subfolders,
                complete,
            }
        })
        .collect();
    let mut crumbs = vec![BrowseCrumb { name: r.label.clone(), path: r.display.clone() }];
    for i in 0..rel.len() {
        crumbs.push(BrowseCrumb { name: rel[i].clone(), path: r.display_of(&rel[..=i]) });
    }
    Ok(BrowseListing {
        path: r.display_of(&rel),
        container_path: dir.to_string_lossy().into_owned(),
        name: rel.last().cloned().unwrap_or_else(|| r.label.clone()),
        root: r.label.clone(),
        root_kind: r.kind,
        parent: (!rel.is_empty()).then(|| r.display_of(&rel[..rel.len() - 1])),
        crumbs,
        can_use: !(rel.is_empty() && r.kind == RootKind::Drive),
        n_videos: here.n_videos,
        n_songs: here.n_songs,
        sample_video: here.sample.map(|f| dir.join(f).to_string_lossy().into_owned()),
        folders,
        truncated,
    })
}

#[cfg(test)]
mod tests;
