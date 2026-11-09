//! Watched footage folders: card in, reel out.
//!
//! When on, the watcher looks at `/footage` (and `VIBECUT_WATCH_DIRS`)
//! every few seconds. A new trip folder, or a camera card's
//! `DCIM/100GOPRO/…` layout, is rendered into a reel once it has stopped
//! changing (files still being copied keep it waiting): a project with the
//! defaults the Create screen would pick, rendered as an ordinary job —
//! one at a time, after any render already running. Listeners hear about
//! every step ([`WatchEvent`]): the web UI shows a "New footage found"
//! list and a toast, and a configured webhook gets the finished reel.
//!
//! What has been handled is kept in `/projects/.vibecut-watch.json`, so
//! a restart neither renders a folder again nor forgets one it queued.
//! The first time the watcher runs it only takes note of what is already
//! there (`baseline`); folders that already have a project are skipped.
//!
//! Off by default: `VIBECUT_WATCH=1` turns it on at start, and the UI's
//! toggle (`PUT /api/watch`) switches it at runtime and is remembered.

pub mod maker;
pub mod notify;
pub mod scan;
pub mod state;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex, Notify};

use crate::jobs::{JobRegistry, JobState};
use crate::paths::{FOOTAGE_ROOT, PROJECTS_ROOT};

pub use maker::{OpsReelMaker, Proposal, ReelMaker};
pub use notify::{reel_payload, WatchEvent};
pub use scan::UnitKind;
pub use state::{EntryStatus, WatchEntry, WatchFile};

use scan::{find_units, snapshot, Settling, Unit};
use state::entry_id;

/// The state file's name, in the projects folder.
pub const STATE_FILE: &str = ".vibecut-watch.json";
/// Entries the list shows at most.
const LIST_LIMIT: usize = 30;

/// How the watcher is set up (from the environment, see [`WatchSettings::from_vars`]).
#[derive(Debug, Clone)]
pub struct WatchSettings {
    /// Folders to watch: `/footage` first, then `VIBECUT_WATCH_DIRS`.
    pub roots: Vec<PathBuf>,
    /// `/footage`: entries' `folder` is relative to it when inside it.
    pub footage_root: PathBuf,
    /// `/projects`: where existing projects are looked for.
    pub projects_root: PathBuf,
    /// The state file; `None` keeps state in memory only.
    pub state_path: Option<PathBuf>,
    /// Time between looks.
    pub poll: Duration,
    /// How long a folder must stay unchanged before it is rendered.
    pub settle: Duration,
    /// How often a running render's progress is read.
    pub job_poll: Duration,
    /// On at start until someone uses the toggle (`VIBECUT_WATCH`).
    pub enabled_by_default: bool,
    /// Vibe for watched reels; `None`: picked from the footage.
    pub vibe: Option<String>,
    /// Length for watched reels; `None`: picked from the footage.
    pub target_duration: Option<String>,
}

impl WatchSettings {
    /// Nothing to watch, nothing written: the default for tests and for
    /// servers that never start the watcher.
    pub fn inert() -> Self {
        WatchSettings {
            roots: Vec::new(),
            footage_root: PathBuf::from(FOOTAGE_ROOT),
            projects_root: PathBuf::from(PROJECTS_ROOT),
            state_path: None,
            poll: Duration::from_secs(7),
            settle: Duration::from_secs(20),
            job_poll: Duration::from_secs(1),
            enabled_by_default: false,
            vibe: None,
            target_duration: None,
        }
    }

    pub fn from_env() -> Self {
        Self::from_vars(|k| std::env::var(k).ok())
    }

    /// Settings from environment variables (read through `get`):
    ///
    /// - `VIBECUT_WATCH` — `1` / `true` / `on` / `yes`: on at start.
    /// - `VIBECUT_WATCH_DIRS` — more folders to watch (a path list like
    ///   `PATH`; absolute paths only), e.g. where SD cards are mounted.
    /// - `VIBECUT_WATCH_INTERVAL` — seconds between looks (default 7).
    /// - `VIBECUT_WATCH_SETTLE` — seconds a folder must stay unchanged
    ///   before it is rendered (default 20).
    /// - `VIBECUT_WATCH_VIBE`, `VIBECUT_WATCH_LENGTH` — fixed vibe /
    ///   length for watched reels (default: picked from the footage).
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Self {
        let mut s = WatchSettings::inert();
        let footage = PathBuf::from(FOOTAGE_ROOT);
        s.roots.push(footage);
        if let Some(extra) = get("VIBECUT_WATCH_DIRS") {
            for d in std::env::split_paths(&extra) {
                if (d.is_absolute() || d.to_string_lossy().starts_with('/')) && !s.roots.contains(&d) {
                    s.roots.push(d);
                }
            }
        }
        s.state_path = Some(PathBuf::from(PROJECTS_ROOT).join(STATE_FILE));
        s.enabled_by_default = get("VIBECUT_WATCH").is_some_and(|v| truthy(&v));
        if let Some(n) = get("VIBECUT_WATCH_INTERVAL").and_then(|v| parse_secs(&v)) {
            s.poll = Duration::from_secs(n.clamp(1, 600));
        }
        if let Some(n) = get("VIBECUT_WATCH_SETTLE").and_then(|v| parse_secs(&v)) {
            s.settle = Duration::from_secs(n.min(3600));
        }
        s.vibe = maker::checked_vibe(get("VIBECUT_WATCH_VIBE"));
        s.target_duration = get("VIBECUT_WATCH_LENGTH").map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        s
    }
}

fn truthy(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes")
}

/// `"7"` or `"7s"`.
fn parse_secs(v: &str) -> Option<u64> {
    v.trim().trim_end_matches('s').trim().parse().ok()
}

/// The watcher: shared state, the event feed, and the two loops
/// ([`WatchService::spawn`]). Cheap to clone.
#[derive(Clone)]
pub struct WatchService {
    shared: Arc<Shared>,
}

struct Shared {
    settings: WatchSettings,
    jobs: JobRegistry,
    inner: Mutex<Inner>,
    events: broadcast::Sender<WatchEvent>,
    wake_scan: Notify,
    wake_worker: Notify,
}

struct Inner {
    file: WatchFile,
    enabled: bool,
    settling: Settling,
}

impl Default for WatchService {
    fn default() -> Self {
        WatchService::new(WatchSettings::inert(), JobRegistry::new())
    }
}

impl WatchService {
    /// Load the state file (if any) and bring it up to date after a
    /// restart. Renders go into `jobs`, next to everyone else's.
    pub fn new(settings: WatchSettings, jobs: JobRegistry) -> Self {
        let mut file = settings.state_path.as_deref().map(WatchFile::load).unwrap_or_default();
        file.after_restart();
        let enabled = file.enabled.unwrap_or(settings.enabled_by_default);
        let (events, _) = broadcast::channel(64);
        WatchService {
            shared: Arc::new(Shared {
                settings,
                jobs,
                inner: Mutex::new(Inner { file, enabled, settling: Settling::default() }),
                events,
                wake_scan: Notify::new(),
                wake_worker: Notify::new(),
            }),
        }
    }

    pub fn settings(&self) -> &WatchSettings {
        &self.shared.settings
    }

    /// Every event from now on. A listener that falls behind gets
    /// `Lagged` and should re-read [`WatchService::status_json`].
    pub fn subscribe(&self) -> broadcast::Receiver<WatchEvent> {
        self.shared.events.subscribe()
    }

    fn emit(&self, ev: WatchEvent) {
        let _ = self.shared.events.send(ev);
    }

    fn save(&self, inner: &Inner) {
        if let Some(path) = &self.shared.settings.state_path {
            if let Err(e) = inner.file.save(path) {
                tracing::warn!(error = %e, "could not save the watch state");
            }
        }
    }

    pub async fn enabled(&self) -> bool {
        self.shared.inner.lock().await.enabled
    }

    /// Whether any watched folder exists (no footage mount: nothing to watch).
    pub fn available(&self) -> bool {
        self.shared.settings.roots.iter().any(|r| r.is_dir())
    }

    /// `GET /api/watch`: on/off, what is watched and how, and the list.
    pub async fn status_json(&self) -> Value {
        let g = self.shared.inner.lock().await;
        let s = &self.shared.settings;
        json!({
            "enabled": g.enabled,
            "available": self.available(),
            "roots": s.roots.iter().map(|r| json!({ "path": r.to_string_lossy(), "exists": r.is_dir() })).collect::<Vec<_>>(),
            "interval_secs": s.poll.as_secs(),
            "settle_secs": s.settle.as_secs(),
            "vibe": s.vibe,
            "target_duration": s.target_duration,
            "entries": g.file.visible(LIST_LIMIT),
        })
    }

    /// The toggle. Remembered in the state file, so it outlives restarts
    /// (and wins over `VIBECUT_WATCH` from then on).
    pub async fn set_enabled(&self, on: bool) -> Value {
        {
            let mut g = self.shared.inner.lock().await;
            g.enabled = on;
            g.file.enabled = Some(on);
            self.save(&g);
        }
        self.emit(WatchEvent::Status { enabled: on });
        if on {
            self.shared.wake_scan.notify_one();
            self.shared.wake_worker.notify_one();
        }
        self.status_json().await
    }

    /// Take an entry off the list. One still waiting or queued is not
    /// rendered at all; one rendering can't be dismissed (stop its job).
    /// `Err` names why not; `Ok(None)`: no such entry.
    pub async fn dismiss(&self, id: &str) -> Result<Option<WatchEntry>, &'static str> {
        let entry = {
            let mut g = self.shared.inner.lock().await;
            let Some(e) = g.file.get_mut(id) else {
                return Ok(None);
            };
            match e.status {
                EntryStatus::Rendering => return Err("it is rendering — stop its render first"),
                EntryStatus::Waiting | EntryStatus::Queued => {
                    e.set(EntryStatus::Skipped, Some("Dismissed — no reel made.".into()))
                }
                _ => {}
            }
            e.dismissed = true;
            let e = e.clone();
            self.save(&g);
            e
        };
        self.emit(WatchEvent::Removed { id: entry.id.clone() });
        Ok(Some(entry))
    }

    /// Queue a failed, stopped or skipped entry again.
    pub async fn retry(&self, id: &str) -> Result<Option<WatchEntry>, &'static str> {
        let entry = {
            let mut g = self.shared.inner.lock().await;
            let Some(e) = g.file.get_mut(id) else {
                return Ok(None);
            };
            if !matches!(e.status, EntryStatus::Failed | EntryStatus::Stopped | EntryStatus::Skipped) {
                return Err("only a failed, stopped or skipped folder can be tried again");
            }
            e.set(EntryStatus::Queued, None);
            e.attempts = 0;
            e.dismissed = false;
            e.job_id = None;
            let e = e.clone();
            self.save(&g);
            e
        };
        self.emit(WatchEvent::Entry { entry: entry.clone() });
        self.shared.wake_worker.notify_one();
        Ok(Some(entry))
    }

    /// One look at the watched folders (nothing while off): new folders
    /// start waiting, settled ones are queued (or skipped when they
    /// already have a project), vanished ones that were still being
    /// copied leave the list. The first look ever only takes the baseline.
    pub async fn tick(&self, now: Instant) {
        let settings = &self.shared.settings;
        let (baseline, handled_folders) = {
            let g = self.shared.inner.lock().await;
            if !g.enabled {
                return;
            }
            let handled: HashSet<String> =
                g.file.entries.iter().filter(|e| e.status.is_settled()).map(|e| e.id.clone()).collect();
            (!g.file.baseline_taken, handled)
        };
        // Walk outside the lock: a card or a big copy can take a moment.
        let roots = settings.roots.clone();
        let looked = tokio::task::spawn_blocking(move || {
            find_units(&roots)
                .into_iter()
                .map(|u| {
                    // A trip folder dealt with before (or at the baseline)
                    // needs no walk: its id is its path.
                    let skip = u.kind == UnitKind::Folder && (baseline || handled_folders.contains(&entry_id(&u, "")));
                    let snap = (!skip).then(|| snapshot(&u.footage));
                    (u, snap)
                })
                .collect::<Vec<(Unit, Option<scan::Snapshot>)>>()
        })
        .await
        .unwrap_or_default();

        let mut g = self.shared.inner.lock().await;
        if !g.enabled {
            return;
        }
        let mut changed: Vec<String> = Vec::new();
        let mut present = HashSet::new();
        let mut current = HashSet::new();
        let mut queued = false;
        for (unit, snap) in looked {
            present.insert(unit.path.clone());
            let content = snap.as_ref().map(scan::Snapshot::content_id).unwrap_or_default();
            let id = entry_id(&unit, &content);
            if baseline {
                if g.file.handled(&id).is_none() {
                    let mut e = WatchEntry::new(&unit, &content, &settings.footage_root, EntryStatus::Baseline);
                    e.n_videos = snap.as_ref().map_or(0, |s| s.n_videos);
                    g.file.entries.push(e);
                }
                continue;
            }
            let Some(snap) = snap else { continue };
            if g.file.handled(&id).is_some() || snap.n_videos == 0 {
                continue;
            }
            current.insert(id.clone());
            let settled = g.settling.settled(&unit.path, snap.fingerprint(), now, settings.settle) && !snap.partial;
            if g.file.get_mut(&id).is_none() {
                g.file.entries.push(WatchEntry::new(&unit, &content, &settings.footage_root, EntryStatus::Waiting));
                changed.push(id.clone());
            }
            let skip_for = if settled {
                let paths = [unit.path.as_path(), unit.footage.as_path()];
                maker::existing_project(&settings.projects_root, &settings.footage_root, &paths)
            } else {
                None
            };
            let e = g.file.get_mut(&id).expect("just inserted");
            if (e.n_videos, e.total_bytes) != (snap.n_videos, snap.video_bytes) {
                e.n_videos = snap.n_videos;
                e.total_bytes = snap.video_bytes;
                e.updated_at = chrono::Utc::now().to_rfc3339();
                changed.push(id.clone());
            }
            if !settled {
                continue;
            }
            match skip_for {
                Some(cfg) => e.set(EntryStatus::Skipped, Some(format!("Already in your projects ({cfg}) — no new reel made."))),
                None => {
                    e.set(EntryStatus::Queued, None);
                    queued = true;
                }
            }
            changed.push(id);
        }
        // Copies that vanished (a card pulled mid-read), or whose clips
        // changed under a new id, leave the list.
        let mut removed = Vec::new();
        g.file.entries.retain(|e| {
            let gone = e.status == EntryStatus::Waiting && !current.contains(&e.id);
            if gone {
                removed.push(e.id.clone());
            }
            !gone
        });
        g.settling.keep_only(&present);
        if baseline {
            g.file.baseline_taken = true;
            self.save(&g);
            return;
        }
        if changed.is_empty() && removed.is_empty() {
            return;
        }
        self.save(&g);
        changed.dedup();
        for id in changed {
            if let Some(e) = g.file.get_mut(&id) {
                let entry = e.clone();
                self.emit(WatchEvent::Entry { entry });
            }
        }
        for id in removed {
            self.emit(WatchEvent::Removed { id });
        }
        drop(g);
        if queued {
            self.shared.wake_worker.notify_one();
        }
    }

    /// Update one entry, save, and tell listeners.
    async fn update(&self, id: &str, persist: bool, f: impl FnOnce(&mut WatchEntry)) -> Option<WatchEntry> {
        let mut g = self.shared.inner.lock().await;
        let e = g.file.get_mut(id)?;
        f(e);
        let entry = e.clone();
        if persist {
            self.save(&g);
        }
        drop(g);
        self.emit(WatchEvent::Entry { entry: entry.clone() });
        Some(entry)
    }

    /// Wait until no job (anyone's render or reformat) is queued or
    /// running, so watched reels never pile onto the machine. `false`
    /// when the watcher was switched off meanwhile.
    async fn wait_for_quiet(&self) -> bool {
        loop {
            if !self.enabled().await {
                return false;
            }
            let busy = self.shared.jobs.list().await.iter().any(|j| !j.state.is_finished());
            if !busy {
                return true;
            }
            tokio::time::sleep(self.shared.settings.job_poll * 2).await;
        }
    }

    /// Render the oldest queued entry, start to finish. `false` when there
    /// was nothing to do (or the watcher is off).
    pub async fn work_once<M: ReelMaker>(&self, maker: &M) -> bool {
        let next = {
            let g = self.shared.inner.lock().await;
            if !g.enabled {
                return false;
            }
            g.file.next_queued().cloned()
        };
        let Some(entry) = next else {
            return false;
        };
        if !self.wait_for_quiet().await {
            return false;
        }
        // Still queued (not dismissed meanwhile)? Then it's ours.
        let started = {
            let mut g = self.shared.inner.lock().await;
            match g.file.get_mut(&entry.id) {
                Some(e) if e.status == EntryStatus::Queued => {
                    e.attempts += 1;
                    e.job_id = None;
                    e.set(EntryStatus::Rendering, None);
                    e.stage = Some("Reading the clips".into());
                    e.progress = Some(0.0);
                    let e = e.clone();
                    self.save(&g);
                    Some(e)
                }
                _ => None,
            }
        };
        let Some(entry) = started else {
            return true;
        };
        self.emit(WatchEvent::Entry { entry: entry.clone() });
        tracing::info!(folder = %entry.path, "watched folder: making a reel");

        let proposal = match maker.propose(&entry).await {
            Ok(p) => p,
            Err(msg) => return self.fail(&entry.id, msg).await,
        };
        self.update(&entry.id, true, |e| {
            e.proposal = Some(proposal.summary.clone());
            e.stage = Some("Waiting to render".into());
        })
        .await;
        let job_id = match maker.submit(&proposal.config_path).await {
            Ok(id) => id,
            Err(msg) => return self.fail(&entry.id, msg).await,
        };
        self.update(&entry.id, true, |e| e.job_id = Some(job_id.clone())).await;

        // Follow the job like any client would, passing its progress on.
        let mut shown = (-1i32, String::new());
        let finished = loop {
            match self.shared.jobs.get(&job_id).await {
                None => break None,
                Some(st) if st.is_finished() => break Some(st),
                Some(JobState::Running { progress: Some(p), .. }) => {
                    let now = ((p.overall * 100.0).round() as i32, p.label.to_string());
                    if now != shown {
                        shown = now;
                        self.update(&entry.id, false, |e| {
                            e.progress = Some(p.overall);
                            e.stage = Some(p.label.to_string());
                        })
                        .await;
                    }
                }
                Some(_) => {}
            }
            tokio::time::sleep(self.shared.settings.job_poll).await;
        };
        match finished {
            Some(JobState::Done { result_json, .. }) => {
                let poster = maker.poster(&result_json).await;
                let done = self
                    .update(&entry.id, true, |e| {
                        e.render = Some(result_json.clone());
                        e.poster_path = poster;
                        e.set(EntryStatus::Done, None);
                    })
                    .await;
                if let Some(entry) = done {
                    tracing::info!(folder = %entry.path, "watched folder: reel ready");
                    self.emit(WatchEvent::ReelReady { entry });
                }
                true
            }
            Some(JobState::Cancelled { .. }) => {
                self.update(&entry.id, true, |e| e.set(EntryStatus::Stopped, Some("Render stopped.".into()))).await;
                true
            }
            Some(JobState::Failed { error, .. }) => self.fail(&entry.id, error).await,
            _ => self.fail(&entry.id, "The render job went missing.".into()).await,
        }
    }

    async fn fail(&self, id: &str, msg: String) -> bool {
        tracing::warn!(error = %msg, "watched folder: no reel");
        self.update(id, true, |e| e.set(EntryStatus::Failed, Some(msg))).await;
        true
    }

    /// Start the two loops: one looks at the folders every
    /// `poll`, the other renders queued entries one at a time.
    pub fn spawn<M: ReelMaker>(&self, maker: M) {
        let scanner = self.clone();
        tokio::spawn(async move {
            loop {
                scanner.tick(Instant::now()).await;
                let poll = scanner.shared.settings.poll;
                tokio::select! {
                    _ = tokio::time::sleep(poll) => {}
                    _ = scanner.shared.wake_scan.notified() => {}
                }
            }
        });
        let worker = self.clone();
        tokio::spawn(async move {
            loop {
                if !worker.work_once(&maker).await {
                    let poll = worker.shared.settings.poll;
                    tokio::select! {
                        _ = tokio::time::sleep(poll) => {}
                        _ = worker.shared.wake_worker.notified() => {}
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests;
