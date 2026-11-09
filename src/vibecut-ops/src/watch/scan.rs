//! What a watched folder holds, and whether it has stopped changing.
//!
//! A watched root (`/footage`, plus `VIBECUT_WATCH_DIRS`) holds *units*:
//! each folder directly inside it is one trip, and a memory card — any
//! folder with a camera's `DCIM/100GOPRO`-style layout, the root itself
//! included — is one card whose clips live under `DCIM`. Finding units is
//! a `read_dir` of each root; looking inside one ([`snapshot`]) walks it.
//!
//! A unit is only rendered once it has looked exactly the same (every
//! file's size and modification time) for a while: files still being
//! copied keep growing, so they keep resetting the clock ([`Settling`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

/// How deep [`snapshot`] looks below a unit's footage folder (a card's
/// `DCIM/100GOPRO/…` is two levels down).
const MAX_DEPTH: usize = 6;

/// A trip folder or a memory card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitKind {
    Folder,
    Card,
}

/// One thing the watcher can turn into a reel.
#[derive(Debug, Clone, PartialEq)]
pub struct Unit {
    /// The trip folder, or the card's root (the folder holding `DCIM`).
    pub path: PathBuf,
    /// Where the clips are: the trip folder, or the card's `DCIM`.
    pub footage: PathBuf,
    pub kind: UnitKind,
    /// Display name: the folder's name, or a card's volume name
    /// ("SD card" when that is a serial number or a generic label).
    pub name: String,
}

/// Every unit under `roots`, in root order then name order. Cheap: one
/// `read_dir` per root plus a `DCIM` check per folder. Roots that don't
/// exist (a card reader with no card) are skipped.
pub fn find_units(roots: &[PathBuf]) -> Vec<Unit> {
    let mut out = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        if let Some(dcim) = dcim_dir(root) {
            out.push(Unit { path: root.clone(), footage: dcim, kind: UnitKind::Card, name: card_name(root) });
        }
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && !hidden(p) && !named(p, "DCIM"))
            .collect();
        dirs.sort();
        for dir in dirs {
            let unit = match dcim_dir(&dir) {
                Some(dcim) => Unit { name: card_name(&dir), path: dir, footage: dcim, kind: UnitKind::Card },
                None => Unit { name: file_name(&dir), footage: dir.clone(), path: dir, kind: UnitKind::Folder },
            };
            out.push(unit);
        }
    }
    out
}

/// `dir/DCIM` when `dir` has a camera card's layout: a `DCIM` folder
/// (any letter case) holding at least one DCF folder — three digits and
/// five characters, like `100GOPRO`, `101GOPRO` or DJI's `100MEDIA`.
pub fn dcim_dir(dir: &Path) -> Option<PathBuf> {
    let dcim = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| named(p, "DCIM") && p.is_dir())?;
    let has_dcf = std::fs::read_dir(&dcim).ok()?.flatten().any(|e| {
        let n = e.file_name();
        let n = n.to_string_lossy();
        n.len() == 8 && n.as_bytes()[..3].iter().all(u8::is_ascii_digit) && e.path().is_dir()
    });
    has_dcf.then_some(dcim)
}

/// A card's display name: its folder (volume) name, unless that is a
/// serial number (`3436-6331`, a UUID) or a generic label.
pub fn card_name(dir: &Path) -> String {
    let name = file_name(dir);
    let lower = name.to_ascii_lowercase();
    let hex_groups = |s: &str, lens: &[usize]| {
        let parts: Vec<&str> = s.split('-').collect();
        parts.len() == lens.len()
            && parts.iter().zip(lens).all(|(p, n)| p.len() == *n && p.bytes().all(|b| b.is_ascii_hexdigit()))
    };
    let generic = ["", "no name", "untitled", "sdcard", "sd card", "card", "footage", "media", "disk", "removable disk"];
    if generic.contains(&lower.as_str()) || hex_groups(&name, &[4, 4]) || hex_groups(&name, &[8, 4, 4, 4, 12]) {
        "SD card".to_string()
    } else {
        name
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn named(p: &Path, name: &str) -> bool {
    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.eq_ignore_ascii_case(name))
}

fn hidden(p: &Path) -> bool {
    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'))
}

/// A clip the pipeline would pick up: `.mp4` in any case, not a GoPro
/// proxy or thumbnail (the same rule as the footage list).
pub fn is_video(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, e)| e.eq_ignore_ascii_case("mp4"))
        && !vibecut::media_probe::is_gopro_sidecar(name)
}

/// A file a copy tool writes while it is still copying: a browser or
/// downloader's partial file, or rsync's hidden `.GX010001.MP4.a1B2c3`.
pub fn is_partial(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    ["part", "partial", "crdownload", "download", "tmp", "!ut", "opdownload"].contains(&ext)
        || (lower.starts_with('.') && lower.contains(".mp4."))
}

/// What a unit's footage folder held at one look.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// Every file (relative path, size, modification time in ns), hidden
    /// files included — an in-progress copy's temp file counts as change.
    /// Hidden folders (`.Trashes`, `.Spotlight-V100`) are not walked.
    pub files: Vec<(String, u64, u128)>,
    pub n_videos: u32,
    pub video_bytes: u64,
    /// Newest clip's modification time.
    pub newest_video: Option<SystemTime>,
    /// A partial-download file is present: a copy is still running.
    pub partial: bool,
}

impl Snapshot {
    /// Changes whenever any file appears, disappears, grows or is touched.
    pub fn fingerprint(&self) -> u64 {
        let mut h = FNV_OFFSET;
        for (rel, size, mtime) in &self.files {
            fnv(&mut h, rel.as_bytes());
            fnv(&mut h, &size.to_le_bytes());
            fnv(&mut h, &mtime.to_le_bytes());
        }
        h
    }

    /// Which clips these are — names and sizes only (no times, which a
    /// card can report differently from one mount to the next). Stable
    /// across restarts: a card that comes back with the same clips has
    /// the same id.
    pub fn content_id(&self) -> String {
        let mut h = FNV_OFFSET;
        for (rel, size, _) in self.files.iter().filter(|(rel, ..)| is_video(base_name(rel))) {
            fnv(&mut h, rel.to_ascii_lowercase().as_bytes());
            fnv(&mut h, &size.to_le_bytes());
        }
        format!("{h:016x}")
    }
}

fn base_name(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// Walk `footage` (up to [`MAX_DEPTH`] levels) and record every file.
pub fn snapshot(footage: &Path) -> Snapshot {
    let mut s = Snapshot::default();
    walk(footage, "", 0, &mut s);
    s.files.sort();
    s
}

fn walk(dir: &Path, prefix: &str, depth: usize, s: &mut Snapshot) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        let Ok(meta) = std::fs::metadata(&path) else {
            continue; // vanished mid-walk: the next look sees the change
        };
        if meta.is_dir() {
            if !name.starts_with('.') {
                walk(&path, &rel, depth + 1, s);
            }
            continue;
        }
        let mtime = meta.modified().ok();
        let nanos = mtime
            .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos());
        s.files.push((rel, meta.len(), nanos));
        if is_partial(&name) {
            s.partial = true;
        } else if is_video(&name) && !name.starts_with('.') {
            s.n_videos += 1;
            s.video_bytes += meta.len();
            if let Some(m) = mtime {
                s.newest_video = Some(s.newest_video.map_or(m, |n| n.max(m)));
            }
        }
    }
}

/// Remembers how each unit looked and since when, to tell a finished
/// copy from one still running.
#[derive(Debug, Default)]
pub struct Settling {
    seen: HashMap<PathBuf, (u64, Instant)>,
}

impl Settling {
    /// True once `path` has looked exactly the same (`fingerprint`) for at
    /// least `settle` — and always on a second look at the earliest, so a
    /// zero `settle` still needs two polls. Any change restarts the clock.
    pub fn settled(&mut self, path: &Path, fingerprint: u64, now: Instant, settle: Duration) -> bool {
        match self.seen.get(path) {
            Some((fp, since)) if *fp == fingerprint => now.saturating_duration_since(*since) >= settle,
            _ => {
                self.seen.insert(path.to_path_buf(), (fingerprint, now));
                false
            }
        }
    }

    /// Forget units that are gone (so one that comes back starts over).
    pub fn keep_only(&mut self, present: &HashSet<PathBuf>) {
        self.seen.retain(|p, _| present.contains(p));
    }
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a: a hash that stays the same across builds and restarts (std's
/// `DefaultHasher` promises neither), for ids written to disk.
fn fnv(h: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *h ^= u64::from(*b);
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // Field separator, so ("ab", "c") and ("a", "bc") differ.
    *h ^= 0xff;
    *h = h.wrapping_mul(0x0000_0100_0000_01b3);
}

/// A stable id for a string (entry ids).
pub fn stable_id(s: &str) -> String {
    let mut h = FNV_OFFSET;
    fnv(&mut h, s.as_bytes());
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vibecut_watch_scan_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(root: &Path, rel: &str, bytes: &[u8]) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    #[test]
    fn trip_folders_and_cards_are_told_apart() {
        let root = tmp("units");
        put(&root, "Coastline Bike Ride/GX010001.MP4", b"12");
        put(&root, "3436-6331/DCIM/100GOPRO/GX010042.MP4", b"1234");
        put(&root, "3436-6331/DCIM/100GOPRO/GL010042.LRV", b"1");
        put(&root, "3436-6331/MISC/version.txt", b"x");
        // A DCIM folder without a DCF folder in it is not a card.
        put(&root, "Holiday/DCIM/photos/a.jpg", b"x");
        put(&root, "Holiday/clip.mp4", b"x");
        put(&root, ".hidden/GX010009.MP4", b"x");
        let units = find_units(std::slice::from_ref(&root));
        let names: Vec<(&str, UnitKind)> = units.iter().map(|u| (u.name.as_str(), u.kind)).collect();
        assert_eq!(
            names,
            [("SD card", UnitKind::Card), ("Coastline Bike Ride", UnitKind::Folder), ("Holiday", UnitKind::Folder)]
        );
        let card = &units[0];
        assert_eq!(card.path, root.join("3436-6331"));
        assert_eq!(card.footage, root.join("3436-6331").join("DCIM"));
        assert_eq!(units[1].footage, units[1].path);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_root_that_is_itself_a_card_is_one_card() {
        let root = tmp("rootcard");
        put(&root, "DCIM/100GOPRO/GX010001.MP4", b"12");
        put(&root, "DCIM/101GOPRO/GX010002.MP4", b"12");
        put(&root, "MISC/x.txt", b"x");
        let units = find_units(std::slice::from_ref(&root));
        // The card, then MISC as a (video-less) folder — never DCIM itself.
        assert_eq!(units[0].kind, UnitKind::Card);
        assert_eq!(units[0].path, root);
        assert_eq!(units[0].footage, root.join("DCIM"));
        assert!(units.iter().all(|u| !named(&u.path, "DCIM")));
        assert_eq!(snapshot(&units[0].footage).n_videos, 2);
        // DJI's DCF folder name and a lower-case dcim work the same.
        let dji = tmp("dji");
        put(&dji, "Mini 4/dcim/100MEDIA/DJI_0001.MP4", b"1");
        let units = find_units(std::slice::from_ref(&dji));
        assert_eq!((units[0].kind, units[0].name.as_str()), (UnitKind::Card, "Mini 4"));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&dji);
    }

    #[test]
    fn card_names_hide_serials_and_generic_labels() {
        assert_eq!(card_name(Path::new("/cards/3436-6331")), "SD card");
        assert_eq!(card_name(Path::new("/cards/0b6a1f2e-9c1d-4e8b-a1b2-c3d4e5f60718")), "SD card");
        assert_eq!(card_name(Path::new("/media/NO NAME")), "SD card");
        assert_eq!(card_name(Path::new("/media/Untitled")), "SD card");
        assert_eq!(card_name(Path::new("/media/HERO12 Ski")), "HERO12 Ski");
    }

    #[test]
    fn snapshot_counts_clips_and_spots_copies_in_progress() {
        let root = tmp("snap");
        put(&root, "100GOPRO/GX010001.MP4", b"1234");
        put(&root, "100GOPRO/GX010001.THM", b"1");
        put(&root, "100GOPRO/GL010001.LRV", b"12");
        put(&root, ".Trashes/old.MP4", b"1");
        let s = snapshot(&root);
        assert_eq!((s.n_videos, s.video_bytes, s.partial), (1, 4, false));
        assert_eq!(s.files.len(), 3, "{:?}", s.files);
        assert!(s.newest_video.is_some());
        put(&root, "100GOPRO/.GX010002.MP4.Ab12Cd", b"1");
        assert!(snapshot(&root).partial);
        assert!(is_partial("GX010003.MP4.part") && is_partial("clip.crdownload"));
        assert!(!is_partial("GX010003.MP4"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn content_id_follows_the_clips_not_the_clock() {
        let root = tmp("cid");
        put(&root, "100GOPRO/GX010001.MP4", b"1234");
        put(&root, "100GOPRO/GX010001.THM", b"1");
        let a = snapshot(&root);
        // Touching a file changes the fingerprint (it's still being
        // written) but not which clips these are.
        let mut touched = a.clone();
        touched.files[0].2 += 1;
        assert_ne!(a.fingerprint(), touched.fingerprint());
        assert_eq!(a.content_id(), touched.content_id());
        // A new clip is new content.
        put(&root, "100GOPRO/GX010002.MP4", b"5678");
        let b = snapshot(&root);
        assert_ne!(a.content_id(), b.content_id());
        assert_eq!(a.content_id().len(), 16);
        // Stable across runs: a fixed value, not a per-process hash.
        assert_eq!(stable_id("/footage/trip"), stable_id("/footage/trip"));
        assert_ne!(stable_id("/footage/trip"), stable_id("/footage/trip2"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A file that is still growing is never settled; once it stops, the
    /// unit settles after the settle time.
    #[test]
    fn a_growing_file_is_ignored_until_it_stops_changing() {
        let root = tmp("grow");
        let settle = Duration::from_secs(20);
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut st = Settling::default();
        put(&root, "GX010001.MP4", b"1");
        assert!(!st.settled(&root, snapshot(&root).fingerprint(), at(0), settle));
        // Still copying: bigger on every look, so never settled.
        for (i, t) in [25, 50, 75].into_iter().enumerate() {
            put(&root, "GX010001.MP4", &vec![0u8; 10 * (i + 2)]);
            assert!(!st.settled(&root, snapshot(&root).fingerprint(), at(t), settle), "settled at {t}s while growing");
        }
        // Done copying: the same on the next look, but not for long enough yet…
        let done = snapshot(&root).fingerprint();
        assert!(!st.settled(&root, done, at(85), settle));
        // …then settled.
        assert!(st.settled(&root, done, at(95), settle));
        // A second file arriving starts the clock over.
        put(&root, "GX010002.MP4", b"2");
        assert!(!st.settled(&root, snapshot(&root).fingerprint(), at(100), settle));
        // Even with no settle time, it takes a second look.
        let mut quick = Settling::default();
        let fp = snapshot(&root).fingerprint();
        assert!(!quick.settled(&root, fp, at(0), Duration::ZERO));
        assert!(quick.settled(&root, fp, at(0), Duration::ZERO));
        // A unit that disappears starts over when it comes back.
        quick.keep_only(&HashSet::new());
        assert!(!quick.settled(&root, fp, at(1), Duration::ZERO));
        let _ = std::fs::remove_dir_all(&root);
    }
}
