//! Async render via fire-and-fetch.
//!
//! `render` is blocking: 1–10 minutes per call. That's fine when the
//! LLM client is willing to wait, but with conversational UX the user
//! often wants to do something else (look at the previous output,
//! draft a soundtrack swap, …) while a render is in flight. The
//! fire-and-fetch model:
//!
//! 1. `render_async(config_path)` returns a `job_id` immediately.
//! 2. A background task drives `vibecut::run_pipeline::run_config`.
//! 3. `render_status(job_id)` returns the current state (queued /
//!    running / done / failed / cancelled) and, when done, the same
//!    manifest summary the synchronous `render` returns.
//! 4. `cancel(job_id)` stops it: a queued job never starts, a running
//!    one has its ffmpeg children killed through the job's
//!    [`CancelToken`] and ends `Cancelled` (see [`vibecut::cancel`]).
//!
//! State lives in a process-local map; restarts lose in-flight jobs.
//! That's fine — renders are always re-runnable from the same
//! config, by construction.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::Mutex;
use vibecut::cancel::CancelToken;

/// Per-process monotonic counter for job IDs. Hex-encoded so the
/// strings look distinct from numeric segments / durations in logs.
static NEXT_JOB_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Generate the next job ID. Format: `job_<hex>` (e.g. `job_0a3`).
pub fn fresh_job_id() -> String {
    let n = NEXT_JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("job_{n:x}")
}

/// State of one render job. The LLM polls `render_status(job_id)` and
/// branches on this.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JobState {
    /// Submitted but not yet started by the worker. Should be brief.
    Queued,
    /// Worker is running. `progress` is the pipeline's latest stage
    /// report (stage, friendly label, fractions, detail such as
    /// "clip 7 of 24"); `None` until the first report arrives.
    Running {
        started_at: String,
        progress: Option<vibecut::progress::Progress>,
    },
    /// Render completed. `result_json` is the SAME body the
    /// synchronous `render` tool returns (parsed JSON).
    Done {
        started_at: String,
        finished_at: String,
        elapsed_ms: u64,
        result_json: serde_json::Value,
    },
    /// Render failed. `error` is a human-readable message.
    Failed {
        started_at: String,
        finished_at: String,
        elapsed_ms: u64,
        error: String,
    },
    /// Stopped on request. `started_at` is `None` when the job was
    /// cancelled while still queued (it never ran). The previous output
    /// file is untouched: renders only replace it once they succeed.
    Cancelled {
        started_at: Option<String>,
        finished_at: String,
        elapsed_ms: u64,
    },
}

impl JobState {
    /// Done, failed or cancelled: nothing will change any more.
    pub fn is_finished(&self) -> bool {
        matches!(self, JobState::Done { .. } | JobState::Failed { .. } | JobState::Cancelled { .. })
    }
}

/// What a job does. Renders and platform reformats share one registry —
/// one jobs list, one cancel route, one progress feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// A full render of a project config (`render::submit_async`).
    #[default]
    Render,
    /// A platform version of a finished render (`reformat::submit_async`).
    Reformat,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Render => "render",
            JobKind::Reformat => "reformat",
        }
    }
}

/// The request a reformat job runs: which render, which platform preset
/// (and fit), and the version file it writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReformatJob {
    pub source_path: String,
    pub preset: String,
    /// `None`: the preset's default fit.
    pub fit: Option<String>,
    pub output_path: String,
}

/// One job as the jobs list shows it.
#[derive(Debug, Clone)]
pub struct JobInfo {
    pub id: String,
    pub kind: JobKind,
    /// The config the job renders (empty for jobs submitted without one,
    /// and for reformats).
    pub config_path: String,
    /// What a reformat job makes; `None` for renders.
    pub reformat: Option<ReformatJob>,
    pub submitted_at: String,
    pub state: JobState,
    /// A Stop was asked for; the job ends `Cancelled` (or `Done`, if the
    /// render was already past the point of no return).
    pub cancel_requested: bool,
}

/// One retained job.
struct JobRecord {
    id: String,
    kind: JobKind,
    config_path: String,
    reformat: Option<ReformatJob>,
    submitted_at: String,
    state: JobState,
    cancel: CancelToken,
}

impl JobRecord {
    fn new(id: String, kind: JobKind, config_path: String, reformat: Option<ReformatJob>) -> Self {
        JobRecord {
            id,
            kind,
            config_path,
            reformat,
            submitted_at: chrono::Utc::now().to_rfc3339(),
            state: JobState::Queued,
            cancel: CancelToken::new(),
        }
    }

    fn info(&self) -> JobInfo {
        JobInfo {
            id: self.id.clone(),
            kind: self.kind,
            config_path: self.config_path.clone(),
            reformat: self.reformat.clone(),
            submitted_at: self.submitted_at.clone(),
            state: self.state.clone(),
            cancel_requested: self.cancel.is_cancelled(),
        }
    }
}

/// Process-local registry of in-flight + recently-finished jobs.
/// Cap (`MAX_JOBS_RETAINED`) prevents unbounded growth on a
/// long-lived server; oldest finished jobs are evicted.
#[derive(Clone, Default)]
pub struct JobRegistry {
    inner: Arc<Mutex<JobRegistryInner>>,
}

#[derive(Default)]
struct JobRegistryInner {
    /// Insertion-ordered: `entries[i]` was inserted at i-th call to
    /// `insert`. We use a `Vec` rather than `HashMap` because we want
    /// order-preserving eviction and the per-entry access cost is
    /// irrelevant at our scale (< 100 entries).
    entries: Vec<JobRecord>,
}

impl JobRegistryInner {
    fn find(&mut self, id: &str) -> Option<&mut JobRecord> {
        self.entries.iter_mut().find(|r| r.id == id)
    }
}

/// Cap on retained job records. Plenty for any realistic conversation
/// while keeping memory bounded. Once we exceed this we evict the
/// oldest *finished* (Done / Failed / Cancelled) entries; in-flight
/// jobs are never evicted.
const MAX_JOBS_RETAINED: usize = 64;

impl JobRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a new `Queued` job, return its ID.
    pub async fn submit(&self) -> String {
        self.submit_for("").await
    }

    /// Insert a new `Queued` job rendering `config_path`, return its ID.
    pub async fn submit_for(&self, config_path: &str) -> String {
        let id = fresh_job_id();
        let mut g = self.inner.lock().await;
        g.entries.push(JobRecord::new(id.clone(), JobKind::Render, config_path.to_string(), None));
        id
    }

    /// Insert a `Queued` reformat job — unless one for the same render and
    /// preset (and fit) is still queued or running, which is reused: a
    /// second click on a platform joins the pass already making that
    /// version instead of starting another ffmpeg. One being stopped
    /// (cancel requested) is not reused. Returns `(id, reused)`.
    pub async fn submit_reformat(&self, job: ReformatJob) -> (String, bool) {
        let mut g = self.inner.lock().await;
        let running = g.entries.iter().find(|r| {
            r.kind == JobKind::Reformat
                && !r.state.is_finished()
                && !r.cancel.is_cancelled()
                && r.reformat.as_ref().is_some_and(|j| {
                    j.source_path == job.source_path && j.preset == job.preset && j.fit == job.fit
                })
        });
        if let Some(r) = running {
            return (r.id.clone(), true);
        }
        let id = fresh_job_id();
        g.entries.push(JobRecord::new(id.clone(), JobKind::Reformat, String::new(), Some(job)));
        (id, false)
    }

    /// The job's cancel token, to install around its render.
    pub async fn cancel_token(&self, id: &str) -> Option<CancelToken> {
        let mut g = self.inner.lock().await;
        g.find(id).map(|r| r.cancel.clone())
    }

    /// Move a queued job to `Running`. `false` (and no change) when it
    /// isn't queued any more — cancelled while it waited — so the worker
    /// must not start it.
    pub async fn begin(&self, id: &str, started_at: &str) -> bool {
        let mut g = self.inner.lock().await;
        match g.find(id) {
            Some(r) if matches!(r.state, JobState::Queued) && !r.cancel.is_cancelled() => {
                r.state = JobState::Running { started_at: started_at.to_string(), progress: None };
                true
            }
            _ => false,
        }
    }

    /// Ask a job to stop. A queued job becomes `Cancelled` at once and
    /// never starts; a running one is signalled through its token and
    /// turns `Cancelled` when its worker has killed ffmpeg and returned
    /// (well under a second). Finished jobs are left as they are.
    /// Returns the job afterwards; `None` for an unknown id.
    pub async fn cancel(&self, id: &str) -> Option<JobInfo> {
        let mut g = self.inner.lock().await;
        let r = g.find(id)?;
        match r.state {
            JobState::Queued => {
                r.cancel.cancel();
                r.state = JobState::Cancelled {
                    started_at: None,
                    finished_at: chrono::Utc::now().to_rfc3339(),
                    elapsed_ms: 0,
                };
            }
            JobState::Running { .. } => r.cancel.cancel(),
            _ => {}
        }
        Some(r.info())
    }

    /// Update an existing job's state.
    pub async fn set(&self, id: &str, state: JobState) {
        let mut g = self.inner.lock().await;
        if let Some(r) = g.find(id) {
            r.state = state;
        }
        // Evict oldest finished entries if we're over the cap. We
        // never evict Queued / Running entries — losing those would
        // strand a real worker. In practice this rarely fires.
        let over = g.entries.len().saturating_sub(MAX_JOBS_RETAINED);
        if over > 0 {
            let mut removed = 0;
            g.entries.retain(|r| {
                if removed >= over {
                    return true;
                }
                if r.state.is_finished() {
                    removed += 1;
                    false
                } else {
                    true
                }
            });
        }
    }

    /// Record a progress report for a running job. Called from the
    /// render's blocking thread (not an async context), hence
    /// `blocking_lock`. Ignored unless the job is currently `Running`.
    pub fn set_progress_blocking(&self, id: &str, p: vibecut::progress::Progress) {
        let mut g = self.inner.blocking_lock();
        if let Some(JobRecord { state: JobState::Running { progress, .. }, .. }) = g.find(id) {
            *progress = Some(p);
        }
    }

    /// Look up a job's current state.
    pub async fn get(&self, id: &str) -> Option<JobState> {
        let g = self.inner.lock().await;
        g.entries.iter().find(|r| r.id == id).map(|r| r.state.clone())
    }

    /// Look up a job with its config and submit time.
    pub async fn info(&self, id: &str) -> Option<JobInfo> {
        let g = self.inner.lock().await;
        g.entries.iter().find(|r| r.id == id).map(JobRecord::info)
    }

    /// Snapshot of every job currently retained, oldest first.
    pub async fn snapshot(&self) -> Vec<(String, JobState)> {
        let g = self.inner.lock().await;
        g.entries.iter().map(|r| (r.id.clone(), r.state.clone())).collect()
    }

    /// Every retained job, newest first — the jobs list.
    pub async fn list(&self) -> Vec<JobInfo> {
        let g = self.inner.lock().await;
        g.entries.iter().rev().map(JobRecord::info).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn submit_get_roundtrip() {
        let r = JobRegistry::new();
        let id = r.submit().await;
        assert!(id.starts_with("job_"));
        match r.get(&id).await.unwrap() {
            JobState::Queued => {}
            other => panic!("expected Queued, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn state_transitions_visible() {
        let r = JobRegistry::new();
        let id = r.submit().await;
        r.set(
            &id,
            JobState::Done {
                started_at: "t0".into(),
                finished_at: "t1".into(),
                elapsed_ms: 100,
                result_json: serde_json::json!({"ok": true}),
            },
        )
        .await;
        let st = r.get(&id).await.unwrap();
        match st {
            JobState::Done { result_json, elapsed_ms, .. } => {
                assert_eq!(elapsed_ms, 100);
                assert_eq!(result_json["ok"], true);
            }
            other => panic!("unexpected state {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_job_returns_none() {
        let r = JobRegistry::new();
        assert!(r.get("job_does_not_exist").await.is_none());
    }

    #[tokio::test]
    async fn cancelling_a_queued_job_means_it_never_starts() {
        let r = JobRegistry::new();
        let id = r.submit_for("/projects/a.toml").await;
        let token = r.cancel_token(&id).await.unwrap();
        let info = r.cancel(&id).await.unwrap();
        assert!(matches!(info.state, JobState::Cancelled { started_at: None, .. }));
        assert!(info.cancel_requested && token.is_cancelled());
        // The worker reaches the front of the queue and is refused.
        assert!(!r.begin(&id, "t0").await);
        assert!(matches!(r.get(&id).await.unwrap(), JobState::Cancelled { .. }));
    }

    #[tokio::test]
    async fn cancelling_a_running_job_signals_its_worker() {
        let r = JobRegistry::new();
        let id = r.submit_for("/projects/a.toml").await;
        assert!(r.begin(&id, "t0").await);
        // A second begin is refused: a job runs once.
        assert!(!r.begin(&id, "t1").await);
        let token = r.cancel_token(&id).await.unwrap();
        let info = r.cancel(&id).await.unwrap();
        // Still running until the worker has stopped ffmpeg and reports.
        assert!(matches!(info.state, JobState::Running { .. }));
        assert!(info.cancel_requested && token.is_cancelled());
        r.set(
            &id,
            JobState::Cancelled { started_at: Some("t0".into()), finished_at: "t2".into(), elapsed_ms: 5 },
        )
        .await;
        assert!(r.get(&id).await.unwrap().is_finished());
    }

    #[tokio::test]
    async fn cancelling_a_finished_or_unknown_job_changes_nothing() {
        let r = JobRegistry::new();
        let id = r.submit().await;
        let done = JobState::Done {
            started_at: "t0".into(),
            finished_at: "t1".into(),
            elapsed_ms: 1,
            result_json: serde_json::json!({}),
        };
        r.set(&id, done).await;
        let info = r.cancel(&id).await.unwrap();
        assert!(matches!(info.state, JobState::Done { .. }));
        assert!(!info.cancel_requested);
        assert!(r.cancel("job_nope").await.is_none());
    }

    #[tokio::test]
    async fn list_is_newest_first_with_configs() {
        let r = JobRegistry::new();
        let a = r.submit_for("/projects/a.toml").await;
        let b = r.submit_for("/projects/b.toml").await;
        let list = r.list().await;
        let ids: Vec<&str> = list.iter().map(|j| j.id.as_str()).collect();
        assert_eq!(ids, [b.as_str(), a.as_str()]);
        assert_eq!(list[0].config_path, "/projects/b.toml");
        assert!(!list[0].submitted_at.is_empty());
    }

    #[tokio::test]
    async fn eviction_drops_cancelled_jobs_but_never_running_ones() {
        let r = JobRegistry::new();
        let running = r.submit().await;
        assert!(r.begin(&running, "t0").await);
        for _ in 0..MAX_JOBS_RETAINED + 4 {
            let id = r.submit().await;
            r.cancel(&id).await;
            // `set` runs the eviction pass.
            let st = r.get(&id).await.unwrap();
            r.set(&id, st).await;
        }
        let list = r.list().await;
        assert!(list.len() <= MAX_JOBS_RETAINED);
        assert!(list.iter().any(|j| j.id == running));
    }

    fn reel(fit: Option<&str>) -> ReformatJob {
        ReformatJob {
            source_path: "/projects/out/coast.mp4".into(),
            preset: "reels".into(),
            fit: fit.map(str::to_string),
            output_path: "/projects/out/coast.reels.mp4".into(),
        }
    }

    #[tokio::test]
    async fn jobs_carry_their_kind() {
        let r = JobRegistry::new();
        let render = r.submit_for("/projects/a.toml").await;
        let (reformat, reused) = r.submit_reformat(reel(None)).await;
        assert!(!reused);
        let list = r.list().await;
        let get = |id: &str| list.iter().find(|j| j.id == id).unwrap().clone();
        assert_eq!(get(&render).kind, JobKind::Render);
        assert!(get(&render).reformat.is_none());
        assert_eq!(get(&reformat).kind, JobKind::Reformat);
        assert_eq!(get(&reformat).reformat, Some(reel(None)));
        assert_eq!(JobKind::Reformat.as_str(), "reformat");
    }

    #[tokio::test]
    async fn a_second_reformat_of_the_same_version_reuses_the_running_job() {
        let r = JobRegistry::new();
        let (a, _) = r.submit_reformat(reel(None)).await;
        // Queued, then running: the same request joins it.
        assert_eq!(r.submit_reformat(reel(None)).await, (a.clone(), true));
        assert!(r.begin(&a, "t0").await);
        assert_eq!(r.submit_reformat(reel(None)).await, (a.clone(), true));
        // Another preset, or another fit of the same one, is its own job.
        let square = ReformatJob {
            preset: "square".into(),
            output_path: "/projects/out/coast.square.mp4".into(),
            ..reel(None)
        };
        let (b, reused) = r.submit_reformat(square).await;
        assert!(!reused && b != a);
        let (c, reused) = r.submit_reformat(reel(Some("crop"))).await;
        assert!(!reused && c != a);
        // A job being stopped isn't joined; nor is a finished one.
        r.cancel(&a).await;
        let (d, reused) = r.submit_reformat(reel(None)).await;
        assert!(!reused && d != a);
        let failed = JobState::Failed {
            started_at: "t".into(),
            finished_at: "t".into(),
            elapsed_ms: 1,
            error: "x".into(),
        };
        r.set(&d, failed).await;
        let (e, reused) = r.submit_reformat(reel(None)).await;
        assert!(!reused && e != d);
        // Renders are never folded into one another.
        let x = r.submit_for("/projects/a.toml").await;
        let y = r.submit_for("/projects/a.toml").await;
        assert_ne!(x, y);
    }

    #[tokio::test]
    async fn fresh_ids_are_distinct() {
        let r = JobRegistry::new();
        let a = r.submit().await;
        let b = r.submit().await;
        let c = r.submit().await;
        assert!(a != b && b != c && a != c);
    }
}
