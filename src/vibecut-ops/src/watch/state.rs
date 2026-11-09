//! What the watcher has already seen, kept in `/projects/.vibecut-watch.json`
//! so a restart neither renders a folder twice nor forgets one it queued.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::scan::{stable_id, Unit, UnitKind};

/// Renders cut short by a restart are tried this many times in all.
pub const MAX_ATTEMPTS: u32 = 2;

/// Where a watched folder is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    /// Already there when the watcher first ran: left alone, never shown.
    Baseline,
    /// Still changing (being copied); rendered once it settles.
    Waiting,
    /// Settled; rendered when the watcher gets to it.
    Queued,
    /// Being proposed and rendered.
    Rendering,
    /// The reel is ready.
    Done,
    Failed,
    /// Its render was stopped (the Renders list's Stop).
    Stopped,
    /// Not rendered: the folder already has a project.
    Skipped,
}

impl EntryStatus {
    /// The watcher has dealt with it: it won't be rendered again by itself.
    pub fn is_settled(self) -> bool {
        !matches!(self, EntryStatus::Waiting)
    }
}

/// One folder or card the watcher found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchEntry {
    /// Stable id: from the path (a card's also from its clips).
    pub id: String,
    /// The trip folder or the card's root.
    pub path: String,
    /// What `discover` / `propose` take: relative to `/footage` when it is
    /// inside it, else absolute (a card's `DCIM`).
    pub folder: String,
    pub kind: UnitKind,
    pub name: String,
    /// Which clips (see `Snapshot::content_id`).
    #[serde(default)]
    pub content_id: String,
    pub status: EntryStatus,
    /// Why it is waiting, skipped or failed, in words.
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub n_videos: u32,
    #[serde(default)]
    pub total_bytes: u64,
    pub found_at: String,
    pub updated_at: String,
    /// Renders started for it (a restart mid-render counts one).
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub job_id: Option<String>,
    /// 0..1 while rendering.
    #[serde(default)]
    pub progress: Option<f32>,
    /// What the render is doing, in words.
    #[serde(default)]
    pub stage: Option<String>,
    /// The proposal (vibe, config_path, output_path, soundtrack, …),
    /// without its TOML.
    #[serde(default)]
    pub proposal: Option<serde_json::Value>,
    /// The finished render's result: the same body `GET /api/render/{id}`
    /// returns when done (output and version paths, manifest summary).
    #[serde(default)]
    pub render: Option<serde_json::Value>,
    /// A frame of the reel, for toasts and notifications.
    #[serde(default)]
    pub poster_path: Option<String>,
    /// Hidden from the list (still remembered as handled).
    #[serde(default)]
    pub dismissed: bool,
    /// Dismissed while still being copied: it stands for that copy until
    /// the copy settles, however its clips change meanwhile (a card's
    /// clips are part of what it is, so each new clip would otherwise look
    /// like new footage).
    #[serde(default)]
    pub follow_copy: bool,
    /// A handled trip folder that has since disappeared. One made again at
    /// its path with other clips is new footage; with the same clips (moved
    /// away and back, a share reconnecting) it is this one again.
    #[serde(default)]
    pub gone: bool,
}

impl WatchEntry {
    /// A new entry for `unit`.
    pub fn new(unit: &Unit, content_id: &str, footage_root: &Path, status: EntryStatus) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        let path = unit.path.to_string_lossy().into_owned();
        WatchEntry {
            id: entry_id(unit, content_id),
            folder: folder_arg(&unit.footage, footage_root),
            path,
            kind: unit.kind,
            name: unit.name.clone(),
            content_id: content_id.to_string(),
            status,
            note: None,
            n_videos: 0,
            total_bytes: 0,
            found_at: now.clone(),
            updated_at: now,
            attempts: 0,
            job_id: None,
            progress: None,
            stage: None,
            proposal: None,
            render: None,
            poster_path: None,
            dismissed: false,
            follow_copy: false,
            gone: false,
        }
    }

    /// Change the status (and note), stamping the time.
    pub fn set(&mut self, status: EntryStatus, note: Option<String>) {
        self.status = status;
        self.note = note;
        self.updated_at = chrono::Utc::now().to_rfc3339();
        if status != EntryStatus::Rendering {
            self.progress = None;
            self.stage = None;
        }
    }

    /// Shown in the "New footage found" list.
    pub fn visible(&self) -> bool {
        self.status != EntryStatus::Baseline && !self.dismissed
    }
}

/// The id a new entry for `unit` gets: from a trip folder's path, from a
/// card's path and clips (the clips it had when first seen, if it was
/// still being copied). Made unique by [`WatchFile::unique_id`]; entries
/// are matched to what is on disk by [`WatchFile::find_for`], not by id.
pub fn entry_id(unit: &Unit, content_id: &str) -> String {
    let key = match unit.kind {
        UnitKind::Folder => unit.path.to_string_lossy().into_owned(),
        UnitKind::Card => format!("{}\n{content_id}", unit.path.to_string_lossy()),
    };
    format!("w{}", stable_id(&key))
}

/// `footage` as `discover` / `propose` take it.
fn folder_arg(footage: &Path, footage_root: &Path) -> String {
    match footage.strip_prefix(footage_root) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.to_string_lossy().replace('\\', "/"),
        _ => footage.to_string_lossy().into_owned(),
    }
}

/// The state file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchFile {
    #[serde(default = "one")]
    pub version: u32,
    /// The UI toggle; `None` until someone uses it (the env default applies).
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Folders present on the first run have been recorded as `Baseline`.
    #[serde(default)]
    pub baseline_taken: bool,
    #[serde(default)]
    pub entries: Vec<WatchEntry>,
}

fn one() -> u32 {
    1
}

impl Default for WatchFile {
    fn default() -> Self {
        WatchFile { version: one(), enabled: None, baseline_taken: false, entries: Vec::new() }
    }
}

impl WatchFile {
    /// Read the state file. Missing: a fresh start. Unreadable or corrupt:
    /// set aside as `<file>.bad` and start fresh — which re-takes the
    /// baseline, so the worst case is a missed reel, never a storm of them.
    pub fn load(path: &Path) -> WatchFile {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return WatchFile::default(),
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "watch state unreadable; starting fresh");
                return WatchFile::default();
            }
        };
        match serde_json::from_str::<WatchFile>(&text) {
            Ok(f) => f,
            Err(e) => {
                let bad = path.with_extension("json.bad");
                let _ = std::fs::rename(path, &bad);
                tracing::warn!(error = %e, kept = %bad.display(), "watch state corrupt; starting fresh");
                WatchFile::default()
            }
        }
    }

    /// Write atomically (temp file + rename), so a crash mid-write leaves
    /// the previous state, not half a file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    /// Bring a loaded state up to date after a restart: folders that were
    /// still copying are looked at afresh, and a render the restart cut
    /// short is queued again — up to [`MAX_ATTEMPTS`] tries in all, so one
    /// that brings the server down can't loop. Queued ones stay queued.
    pub fn after_restart(&mut self) {
        self.entries.retain(|e| e.status != EntryStatus::Waiting);
        for e in &mut self.entries {
            if e.status == EntryStatus::Rendering {
                e.job_id = None;
                if e.attempts >= MAX_ATTEMPTS {
                    e.set(EntryStatus::Failed, Some("Interrupted by a restart twice — not trying again.".into()));
                } else {
                    e.set(EntryStatus::Queued, Some("Interrupted by a restart — making it again.".into()));
                }
            }
        }
    }

    /// The entry recording `unit` as dealt with, if any: any settled entry
    /// with its id (a folder: its path; a card: its path and clips).
    pub fn handled(&self, id: &str) -> Option<&WatchEntry> {
        self.entries.iter().find(|e| e.id == id && e.status.is_settled())
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut WatchEntry> {
        self.entries.iter_mut().find(|e| e.id == id)
    }

    /// The entry `unit`, holding the clips `content`, belongs to (its index):
    ///
    /// - a copy still in progress at its path — waiting, or dismissed while
    ///   waiting — whatever its clips are now;
    /// - a trip folder: the one handled at its path (a trip is handled
    ///   once), or, when that folder had gone, one with the same clips;
    /// - a card: the one handled at its path with these very clips (so the
    ///   same card back with new footage is new).
    ///
    /// `None`: new footage.
    pub fn find_for(&self, unit: &Unit, content: &str) -> Option<usize> {
        let path = unit.path.to_string_lossy();
        let here = |e: &WatchEntry| e.path == path && e.kind == unit.kind;
        let copying = |e: &WatchEntry| here(e) && (e.status == EntryStatus::Waiting || e.follow_copy);
        if let Some(i) = self.entries.iter().position(copying) {
            return Some(i);
        }
        let handled = |e: &WatchEntry| here(e) && e.status.is_settled();
        match unit.kind {
            UnitKind::Folder => self.entries.iter().position(|e| handled(e) && !e.gone).or_else(|| {
                // Entries from before clips were recorded for folders: same.
                let same = |e: &WatchEntry| e.content_id.is_empty() || e.content_id == content;
                self.entries.iter().position(|e| handled(e) && e.gone && same(e))
            }),
            UnitKind::Card => self.entries.iter().position(|e| handled(e) && e.content_id == content),
        }
    }

    /// `base`, or `base-2`, `base-3`, … when an entry already has that id
    /// (a folder made again, a card first seen with the clips an earlier
    /// copy was first seen with).
    pub fn unique_id(&self, base: String) -> String {
        let taken = |id: &str| self.entries.iter().any(|e| e.id == id);
        if !taken(&base) {
            return base;
        }
        (2..).map(|n| format!("{base}-{n}")).find(|id| !taken(id)).expect("an unused id")
    }

    /// The projects the watcher wrote itself (`config_path` of every
    /// entry's proposal): never "already in your projects".
    pub fn own_projects(&self) -> HashSet<PathBuf> {
        self.entries
            .iter()
            .filter_map(|e| e.proposal.as_ref()?.get("config_path")?.as_str())
            .map(PathBuf::from)
            .collect()
    }

    /// Entries for the list, newest first.
    pub fn visible(&self, limit: usize) -> Vec<&WatchEntry> {
        let mut v: Vec<&WatchEntry> = self.entries.iter().filter(|e| e.visible()).collect();
        v.sort_by(|a, b| b.found_at.cmp(&a.found_at));
        v.truncate(limit);
        v
    }

    /// The oldest queued entry.
    pub fn next_queued(&self) -> Option<&WatchEntry> {
        self.entries.iter().filter(|e| e.status == EntryStatus::Queued).min_by(|a, b| a.found_at.cmp(&b.found_at))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unit(path: &str, kind: UnitKind) -> Unit {
        let path = PathBuf::from(path);
        let footage = if kind == UnitKind::Card { path.join("DCIM") } else { path.clone() };
        Unit { name: "x".into(), path, footage, kind }
    }

    #[test]
    fn folders_are_one_entry_cards_one_per_set_of_clips() {
        let trip = unit("/footage/trip", UnitKind::Folder);
        assert_eq!(entry_id(&trip, "aa"), entry_id(&trip, "bb"));
        let card = unit("/cards/3436-6331", UnitKind::Card);
        assert_ne!(entry_id(&card, "aa"), entry_id(&card, "bb"));
        assert!(entry_id(&card, "aa").starts_with('w'));
        let root = Path::new("/footage");
        assert_eq!(WatchEntry::new(&trip, "", root, EntryStatus::Queued).folder, "trip");
        assert_eq!(WatchEntry::new(&card, "aa", root, EntryStatus::Queued).folder, "/cards/3436-6331/DCIM");
        let nested = unit("/footage/2026/Alps", UnitKind::Folder);
        assert_eq!(WatchEntry::new(&nested, "", root, EntryStatus::Queued).folder, "2026/Alps");
    }

    #[test]
    fn handled_state_survives_a_save_and_load() {
        let dir = std::env::temp_dir().join(format!("vibecut_watch_state_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(".vibecut-watch.json");
        let mut f = WatchFile { enabled: Some(true), baseline_taken: true, ..Default::default() };
        let mut done = WatchEntry::new(&unit("/footage/trip", UnitKind::Folder), "c1", Path::new("/footage"), EntryStatus::Done);
        done.render = Some(serde_json::json!({ "output_path": "/projects/out/trip.mp4" }));
        done.poster_path = Some("/projects/out/trip.poster.jpg".into());
        f.entries.push(done.clone());
        f.entries.push(WatchEntry::new(&unit("/footage/old", UnitKind::Folder), "", Path::new("/footage"), EntryStatus::Baseline));
        f.save(&path).unwrap();
        assert!(!path.with_extension("json.tmp").exists(), "temp file left behind");
        let back = WatchFile::load(&path);
        assert_eq!(back, f);
        assert_eq!(back.handled(&done.id).unwrap().render, done.render);
        // Baseline entries count as handled but aren't listed.
        assert_eq!(back.visible(10).len(), 1);
        assert!(back.handled(&back.entries[1].id).is_some());
        // Missing file: fresh start, no baseline yet.
        let fresh = WatchFile::load(&dir.join("nope.json"));
        assert!(!fresh.baseline_taken && fresh.entries.is_empty() && fresh.enabled.is_none());
        assert_eq!(fresh.version, 1);
        let on_disk: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk["version"], 1);
        // A corrupt file is set aside and the watcher starts over.
        std::fs::write(&path, "{ not json").unwrap();
        let fresh = WatchFile::load(&path);
        assert!(!fresh.baseline_taken && fresh.entries.is_empty());
        assert!(path.with_extension("json.bad").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restart_requeues_cut_short_renders_once_and_drops_waiting_ones() {
        let root = Path::new("/footage");
        let mut f = WatchFile::default();
        let mut cut_short = WatchEntry::new(&unit("/footage/a", UnitKind::Folder), "", root, EntryStatus::Rendering);
        cut_short.attempts = 1;
        cut_short.job_id = Some("job_1".into());
        let mut twice = WatchEntry::new(&unit("/footage/b", UnitKind::Folder), "", root, EntryStatus::Rendering);
        twice.attempts = MAX_ATTEMPTS;
        let waiting = WatchEntry::new(&unit("/footage/c", UnitKind::Folder), "", root, EntryStatus::Waiting);
        let mut queued = WatchEntry::new(&unit("/footage/d", UnitKind::Folder), "", root, EntryStatus::Queued);
        cut_short.found_at = "2026-11-09T10:00:00+00:00".into();
        queued.found_at = "2026-11-09T10:05:00+00:00".into();
        f.entries = vec![cut_short.clone(), twice.clone(), waiting.clone(), queued.clone()];
        f.after_restart();
        assert_eq!(f.entries.len(), 3);
        let a = f.get_mut(&cut_short.id).unwrap().clone();
        assert_eq!((a.status, a.job_id), (EntryStatus::Queued, None));
        assert_eq!(f.get_mut(&twice.id).unwrap().status, EntryStatus::Failed);
        assert_eq!(f.get_mut(&queued.id).unwrap().status, EntryStatus::Queued);
        assert!(f.handled(&waiting.id).is_none());
        assert_eq!(f.next_queued().unwrap().id, cut_short.id);
    }
}
