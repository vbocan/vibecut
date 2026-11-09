//! Footage folders: what's under the `/footage` mount, so a UI can
//! offer "pick your trip" cards instead of asking for a folder name.
//!
//! A folder is listed when it (or anything below it) holds `.mp4`
//! files — the same files `discover_sources` picks up. Counts and
//! sizes come from the filesystem only (no ffprobe), so listing stays
//! instant even on large card dumps.

use std::path::Path;

use serde::Serialize;

use crate::error::OpsError;
use crate::paths::FOOTAGE_ROOT;

/// How deep to look for footage below each listed folder.
const MAX_DEPTH: usize = 6;

/// One folder of footage.
#[derive(Debug, Clone, Serialize)]
pub struct FootageFolder {
    /// Folder relative to `/footage` (`""` for the mount itself) — the
    /// value `discover_sources` and `propose` take.
    pub folder: String,
    /// Display name: the folder's own name, "All footage" for the mount.
    pub name: String,
    pub n_videos: u32,
    pub total_bytes: u64,
    /// Newest file modification time (RFC 3339), for sorting by recency.
    pub newest: Option<String>,
    /// One video inside, for a thumbnail.
    pub sample_video: Option<String>,
}

/// The mount itself plus each top-level subfolder that holds footage.
pub async fn list_folders() -> Result<Vec<FootageFolder>, OpsError> {
    tokio::task::spawn_blocking(|| list_folders_in(Path::new(FOOTAGE_ROOT)))
        .await
        .map_err(|e| OpsError::internal(format!("footage scan panicked: {e}")))
}

fn list_folders_in(root: &Path) -> Vec<FootageFolder> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    let mut subdirs: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !hidden(p))
        .collect();
    subdirs.sort();
    for dir in subdirs {
        if let Some(f) = summarize(root, &dir) {
            out.push(f);
        }
    }
    if let Some(mut all) = summarize(root, root) {
        all.name = "All footage".into();
        // Only worth offering when it adds something over the subfolders.
        let loose = out.iter().map(|f| f.n_videos).sum::<u32>() < all.n_videos;
        if out.is_empty() || loose {
            out.insert(0, all);
        }
    }
    out
}

fn hidden(p: &Path) -> bool {
    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'))
}

#[derive(Default)]
struct Tally {
    n: u32,
    bytes: u64,
    newest: Option<std::time::SystemTime>,
    sample: Option<std::path::PathBuf>,
}

fn summarize(root: &Path, dir: &Path) -> Option<FootageFolder> {
    let mut t = Tally::default();
    walk(dir, 0, &mut t);
    if t.n == 0 {
        return None;
    }
    let folder = dir
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("footage")
        .to_string();
    Some(FootageFolder {
        folder,
        name,
        n_videos: t.n,
        total_bytes: t.bytes,
        newest: t.newest.map(|m| chrono::DateTime::<chrono::Utc>::from(m).to_rfc3339()),
        sample_video: t.sample.map(|p| p.to_string_lossy().into_owned()),
    })
}

fn walk(dir: &Path, depth: usize, t: &mut Tally) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if hidden(&p) {
            continue;
        }
        if p.is_dir() {
            walk(&p, depth + 1, t);
        } else if p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("mp4"))
            // A GoPro `GL…` proxy renamed to .MP4 is not a clip.
            && !p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(vibecut::media_probe::is_gopro_sidecar)
        {
            let meta = std::fs::metadata(&p).ok();
            t.n += 1;
            t.bytes += meta.as_ref().map(|m| m.len()).unwrap_or(0);
            if let Some(m) = meta.and_then(|m| m.modified().ok()) {
                t.newest = Some(t.newest.map_or(m, |n| n.max(m)));
            }
            if t.sample.is_none() {
                t.sample = Some(p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_subfolders_with_footage_and_skips_empty_ones() {
        let root = std::env::temp_dir().join(format!("vibecut_footage_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("trip/day1")).unwrap();
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::write(root.join("trip/day1/GX010001.MP4"), b"1234").unwrap();
        std::fs::write(root.join("trip/GX010002.mp4"), b"12").unwrap();
        std::fs::write(root.join("trip/notes.txt"), b"x").unwrap();
        let folders = list_folders_in(&root);
        assert_eq!(folders.len(), 1, "{folders:?}");
        assert_eq!(folders[0].folder, "trip");
        assert_eq!(folders[0].n_videos, 2);
        assert_eq!(folders[0].total_bytes, 6);
        assert!(folders[0].sample_video.is_some());
        // Loose files at the root add an "All footage" entry.
        std::fs::write(root.join("GX010003.MP4"), b"1").unwrap();
        let folders = list_folders_in(&root);
        assert_eq!(folders[0].name, "All footage");
        assert_eq!(folders[0].n_videos, 3);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn proxies_and_thumbnails_are_not_counted_as_videos() {
        let root = std::env::temp_dir().join(format!("vibecut_footage_proxies_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("hero7")).unwrap();
        std::fs::create_dir_all(root.join("hero10")).unwrap();
        for (f, bytes) in [
            ("hero7/GOPR0042.MP4", &b"12345"[..]),
            ("hero7/GP010042.MP4", b"12345"),
            ("hero7/GOPR0042.LRV", b"1"),
            ("hero7/GP010042.LRV", b"1"),
            ("hero7/GOPR0042.THM", b"1"),
            ("hero10/GL010007.MP4", b"1"), // proxy renamed to .MP4
            ("hero10/GL010007.LRV", b"1"),
            ("hero10/GX010007.THM", b"1"),
            ("hero10/GX010007.MP4", b"1234"),
        ] {
            std::fs::write(root.join(f), bytes).unwrap();
        }
        let folders = list_folders_in(&root);
        let by_name = |n: &str| folders.iter().find(|f| f.name == n).unwrap();
        assert_eq!(by_name("hero7").n_videos, 2);
        assert_eq!(by_name("hero7").total_bytes, 10);
        assert_eq!(by_name("hero10").n_videos, 1, "{folders:?}");
        assert_eq!(by_name("hero10").total_bytes, 4);
        assert!(by_name("hero10").sample_video.as_deref().unwrap().ends_with("GX010007.MP4"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
