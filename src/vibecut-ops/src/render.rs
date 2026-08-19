//! Render a TOML config to an mp4, both synchronously and via
//! fire-and-fetch (`submit_async` + [`crate::jobs::JobRegistry`]).
//! Transport-free counterpart of the old `render_impl` /
//! `render_async_impl` / `render_status_impl`.

use std::path::{Path, PathBuf};

use camino::Utf8PathBuf;

use crate::error::OpsError;
use crate::jobs::{JobRegistry, JobState};
use crate::paths::resolve_projects;

/// Render `config_path` to an mp4. Blocking under the hood — the
/// pipeline is a sync, CPU/GPU-bound function that spawns ffmpeg — but
/// wrapped in `spawn_blocking` so it doesn't tie up the async runtime.
/// Takes 1–10 minutes. Returns the same result shape whether called
/// here or polled via [`render_status`] after [`submit_async`].
pub async fn render_sync(config_path: &str) -> Result<serde_json::Value, OpsError> {
    let cfg = resolve_projects(config_path);
    if !cfg.exists() {
        return Err(OpsError::not_found_with(
            "config",
            cfg.to_string_lossy().into_owned(),
        ));
    }
    let cfg_camino = Utf8PathBuf::from_path_buf(cfg.clone())
        .map_err(|_| OpsError::invalid(format!("non-UTF-8 config path: {}", cfg.display())))?;

    let render_target = cfg_camino.clone();
    let join = tokio::task::spawn_blocking(move || -> Result<(), String> {
        vibecut::run_pipeline::run_cmd(
            &render_target,
            /*dry_run*/ false,
            /*no_visual*/ false,
            /*no_cache*/ false,
            /*explain*/ false,
            /*print_effective_config*/ false,
            /*random_seed*/ false,
        )
        .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| OpsError::internal(format!("render task panicked: {e}")))?;

    let (ok, error_msg) = match join {
        Ok(()) => (true, None),
        Err(msg) => (false, Some(msg)),
    };
    Ok(build_render_result_json(&cfg, ok, error_msg.as_deref()))
}

/// Submit a render job and return its `job_id` immediately. The render
/// runs in the background via `tokio::spawn` + `spawn_blocking`; poll
/// with [`render_status`].
pub async fn submit_async(jobs: &JobRegistry, config_path: &str) -> Result<String, OpsError> {
    let cfg = resolve_projects(config_path);
    if !cfg.exists() {
        return Err(OpsError::not_found_with(
            "config",
            cfg.to_string_lossy().into_owned(),
        ));
    }
    let cfg_camino = Utf8PathBuf::from_path_buf(cfg.clone())
        .map_err(|_| OpsError::invalid(format!("non-UTF-8 config path: {}", cfg.display())))?;

    let job_id = jobs.submit().await;
    let jobs_handle = jobs.clone();
    let job_id_for_task = job_id.clone();

    tokio::spawn(async move {
        let started_at = chrono::Utc::now().to_rfc3339();
        jobs_handle
            .set(
                &job_id_for_task,
                JobState::Running {
                    started_at: started_at.clone(),
                },
            )
            .await;
        let t0 = std::time::Instant::now();
        let cfg_for_blocking = cfg_camino.clone();
        let inner = tokio::task::spawn_blocking(move || -> Result<(), String> {
            vibecut::run_pipeline::run_cmd(
                &cfg_for_blocking,
                /*dry_run*/ false,
                /*no_visual*/ false,
                /*no_cache*/ false,
                /*explain*/ false,
                /*print_effective_config*/ false,
                /*random_seed*/ false,
            )
            .map_err(|e| format!("{e:#}"))
        })
        .await;
        let elapsed_ms = t0.elapsed().as_millis() as u64;
        let finished_at = chrono::Utc::now().to_rfc3339();

        let cfg_std = cfg_camino.as_std_path().to_path_buf();
        let final_state = match inner {
            Ok(Ok(())) => {
                let result_json = build_render_result_json(&cfg_std, true, None);
                JobState::Done {
                    started_at,
                    finished_at,
                    elapsed_ms,
                    result_json,
                }
            }
            Ok(Err(msg)) => JobState::Failed {
                started_at,
                finished_at,
                elapsed_ms,
                error: msg,
            },
            Err(e) => JobState::Failed {
                started_at,
                finished_at,
                elapsed_ms,
                error: format!("render task panicked: {e}"),
            },
        };
        jobs_handle.set(&job_id_for_task, final_state).await;
    });

    Ok(job_id)
}

/// Look up a submitted job's current state. `None` means the id was
/// never issued (or was evicted — see [`JobRegistry`]'s retention cap).
pub async fn render_status(jobs: &JobRegistry, job_id: &str) -> Option<JobState> {
    jobs.get(job_id).await
}

/// Render a job's state as one JSON shape, shared by every transport
/// that polls or streams it — the MCP `render_status` tool, the REST
/// `/api/render/{job_id}` endpoint, and the WebSocket progress feed
/// all call this instead of each re-deriving their own shape.
pub fn job_state_json(job_id: &str, state: JobState) -> serde_json::Value {
    match state {
        JobState::Queued => serde_json::json!({
            "job_id": job_id,
            "state": "queued",
        }),
        JobState::Running { started_at } => serde_json::json!({
            "job_id": job_id,
            "state": "running",
            "started_at": started_at,
        }),
        JobState::Done {
            started_at,
            finished_at,
            elapsed_ms,
            result_json,
        } => {
            let mut merged = result_json;
            if let serde_json::Value::Object(map) = &mut merged {
                map.insert("job_id".into(), serde_json::Value::String(job_id.to_string()));
                map.insert("state".into(), serde_json::Value::String("done".into()));
                map.insert("started_at".into(), serde_json::Value::String(started_at));
                map.insert("finished_at".into(), serde_json::Value::String(finished_at));
                map.insert("elapsed_ms".into(), serde_json::Value::from(elapsed_ms));
            }
            merged
        }
        JobState::Failed {
            started_at,
            finished_at,
            elapsed_ms,
            error,
        } => serde_json::json!({
            "job_id": job_id,
            "state": "failed",
            "started_at": started_at,
            "finished_at": finished_at,
            "elapsed_ms": elapsed_ms,
            "error": error,
        }),
    }
}

// ---------- shared result-building ----------

/// Build the JSON body returned by a sync render and (after
/// completion) by an async one. Same shape either way so callers see
/// one contract regardless of how the call was made.
fn build_render_result_json(cfg_std: &Path, ok: bool, error_msg: Option<&str>) -> serde_json::Value {
    let output_path = output_path_from_toml(cfg_std);
    let manifest_path = output_path.as_ref().map(manifest_for_output);
    let manifest = manifest_path.as_deref().and_then(load_json);

    let mut result = serde_json::json!({
        "ok": ok,
        "config_path": cfg_std.to_string_lossy(),
    });
    if let Some(msg) = error_msg {
        result["error"] = serde_json::Value::String(msg.into());
    }
    if let Some(p) = &output_path {
        result["output_path"] = serde_json::Value::String(p.to_string_lossy().into());
        result["output_exists"] = serde_json::Value::Bool(p.exists());
    }
    if let Some(p) = &manifest_path {
        result["manifest_path"] = serde_json::Value::String(p.to_string_lossy().into());
    }
    if let Some(m) = manifest {
        result["manifest_summary"] = serde_json::json!({
            "n_segments": m.get("segments").and_then(|s| s.as_array()).map(|a| a.len()).unwrap_or(0),
            "actual_duration_secs": m.get("actual_duration_secs"),
            "target_duration_secs": m.get("target_duration_secs"),
            "n_sources": m.get("sources").and_then(|s| s.as_array()).map(|a| a.len()).unwrap_or(0),
        });
    }
    result
}

fn output_path_from_toml(cfg_path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(cfg_path).ok()?;
    // First `path = "..."` line wins. The vibe templates put it right
    // after `[[outputs]]`, so this matches the user's intent.
    let re = regex::Regex::new(r#"(?m)^\s*path\s*=\s*"([^"]+)""#).ok()?;
    re.captures(&text)
        .and_then(|c| c.get(1))
        .map(|m| PathBuf::from(m.as_str()))
}

fn manifest_for_output(p: &PathBuf) -> PathBuf {
    p.with_extension("manifest.json")
}

fn load_json(p: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(&text).ok()
}
