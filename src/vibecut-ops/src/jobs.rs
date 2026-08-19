//! Async render via fire-and-fetch.
//!
//! `render` is blocking: 1–10 minutes per call. That's fine when the
//! LLM client is willing to wait, but with conversational UX the user
//! often wants to do something else (look at the previous output,
//! draft a soundtrack swap, …) while a render is in flight. The
//! fire-and-fetch model:
//!
//! 1. `render_async(config_path)` returns a `job_id` immediately.
//! 2. A background task drives `vibecut::run_pipeline::run_cmd`.
//! 3. `render_status(job_id)` returns the current state (queued /
//!    running / done / failed) and, when done, the same manifest
//!    summary the synchronous `render` returns.
//!
//! State lives in a process-local map; restarts lose in-flight jobs.
//! That's fine — renders are always re-runnable from the same
//! config, by construction.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::Mutex;

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
    /// Worker is running. Optional progress hint may land here in
    /// future phases (e.g. "stage 4 of 7"); v1 just reports `running`.
    Running { started_at: String },
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
    /// `insert`. We use a `Vec<(id, state)>` rather than `HashMap`
    /// because we want order-preserving eviction and the per-entry
    /// access cost is irrelevant at our scale (< 100 entries).
    entries: Vec<(String, JobState)>,
}

/// Cap on retained job records. Plenty for any realistic conversation
/// while keeping memory bounded. Once we exceed this we evict the
/// oldest *finished* (Done / Failed) entries; in-flight jobs are
/// never evicted.
const MAX_JOBS_RETAINED: usize = 64;

impl JobRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a new `Queued` job, return its ID.
    pub async fn submit(&self) -> String {
        let id = fresh_job_id();
        let mut g = self.inner.lock().await;
        g.entries.push((id.clone(), JobState::Queued));
        id
    }

    /// Update an existing job's state.
    pub async fn set(&self, id: &str, state: JobState) {
        let mut g = self.inner.lock().await;
        for (k, v) in g.entries.iter_mut() {
            if k == id {
                *v = state;
                break;
            }
        }
        // Evict oldest finished entries if we're over the cap. We
        // never evict Queued / Running entries — losing those would
        // strand a real worker. In practice this rarely fires.
        let over = g.entries.len().saturating_sub(MAX_JOBS_RETAINED);
        if over > 0 {
            let mut removed = 0;
            g.entries.retain(|(_, st)| {
                if removed >= over {
                    return true;
                }
                let finished = matches!(st, JobState::Done { .. } | JobState::Failed { .. });
                if finished {
                    removed += 1;
                    false
                } else {
                    true
                }
            });
        }
    }

    /// Look up a job's current state.
    pub async fn get(&self, id: &str) -> Option<JobState> {
        let g = self.inner.lock().await;
        g.entries.iter().find(|(k, _)| k == id).map(|(_, v)| v.clone())
    }

    /// Snapshot of every job currently retained. Useful for a future
    /// `list_jobs` tool / debugging.
    pub async fn snapshot(&self) -> Vec<(String, JobState)> {
        self.inner.lock().await.entries.clone()
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
    async fn fresh_ids_are_distinct() {
        let r = JobRegistry::new();
        let a = r.submit().await;
        let b = r.submit().await;
        let c = r.submit().await;
        assert!(a != b && b != c && a != c);
    }
}
