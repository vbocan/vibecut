//! Render a TOML config to an mp4, both synchronously and via
//! fire-and-fetch (`submit_async` + [`crate::jobs::JobRegistry`]).
//! Transport-free counterpart of the old `render_impl` /
//! `render_async_impl` / `render_status_impl`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use camino::Utf8PathBuf;

use crate::error::OpsError;
use crate::jobs::{JobRegistry, JobState};
use crate::paths::resolve_projects;

/// One lock per project config. Two renders of the same config share a
/// workspace (intermediates with fixed names) and an output path, so the
/// second waits for the first instead of both writing the same files.
fn project_lock(cfg: &Path) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let key = std::fs::canonicalize(cfg).unwrap_or_else(|_| cfg.to_path_buf());
    let mut map = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(key).or_default().clone()
}

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

    let lock = project_lock(&cfg);
    let _guard = lock.lock().await;
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
        // Stays "queued" while another render of this project finishes.
        let lock = project_lock(cfg_camino.as_std_path());
        let _guard = lock.lock().await;
        let started_at = chrono::Utc::now().to_rfc3339();
        jobs_handle
            .set(
                &job_id_for_task,
                JobState::Running {
                    started_at: started_at.clone(),
                    progress: None,
                },
            )
            .await;
        let t0 = std::time::Instant::now();
        let cfg_for_blocking = cfg_camino.clone();
        let progress_jobs = jobs_handle.clone();
        let progress_id = job_id_for_task.clone();
        let inner = tokio::task::spawn_blocking(move || -> Result<(), String> {
            // Every stage report lands on the job, where the status
            // endpoint and the WebSocket feed pick it up.
            vibecut::progress::with_sink(
                move |p| progress_jobs.set_progress_blocking(&progress_id, p.clone()),
                || {
                    vibecut::run_pipeline::run_cmd(
                        &cfg_for_blocking,
                        /*dry_run*/ false,
                        /*no_visual*/ false,
                        /*no_cache*/ false,
                        /*explain*/ false,
                        /*print_effective_config*/ false,
                        /*random_seed*/ false,
                    )
                },
            )
            .map_err(|e| format!("{e:#}"))
        })
        .await;
        let elapsed_ms = t0.elapsed().as_millis() as u64;
        let finished_at = chrono::Utc::now().to_rfc3339();

        let cfg_std = cfg_camino.as_std_path().to_path_buf();
        let final_state = match inner {
            Ok(Ok(())) => {
                let mut result_json = build_render_result_json(&cfg_std, true, None);
                // Each async render also keeps its own copy, so a UI can
                // compare and restore earlier versions.
                let tag = format!(
                    "{}_{}",
                    chrono::Utc::now().format("%Y%m%dT%H%M%S"),
                    job_id_for_task
                );
                let out = output_path_from_toml(&cfg_std);
                let kept = tokio::task::spawn_blocking(move || {
                    out.as_deref().and_then(|o| keep_version(o, &tag))
                })
                .await
                .ok()
                .flatten();
                if let Some((video, manifest)) = kept {
                    result_json["version_path"] = video.to_string_lossy().into_owned().into();
                    result_json["version_manifest_path"] =
                        manifest.to_string_lossy().into_owned().into();
                }
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
        JobState::Running { started_at, progress } => serde_json::json!({
            "job_id": job_id,
            "state": "running",
            "started_at": started_at,
            "progress": progress,
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
        let beat = m.get("beat_sync").map(|b| {
            serde_json::json!({
                "tempo_bpm": b.get("tempo_bpm"),
                "n_cuts": b.get("n_cuts"),
                "within_tolerance": b.get("within_tolerance"),
                "mean_abs_error_ms": b.get("mean_abs_error_ms"),
            })
        });
        result["manifest_summary"] = serde_json::json!({
            "n_segments": m.get("segments").and_then(|s| s.as_array()).map(|a| a.len()).unwrap_or(0),
            "actual_duration_secs": m.get("actual_duration_secs"),
            "target_duration_secs": m.get("target_duration_secs"),
            "n_sources": m.get("sources").and_then(|s| s.as_array()).map(|a| a.len()).unwrap_or(0),
            "beat_sync": beat,
        });
    }
    result
}

/// The primary output (`[[outputs]][0].path`). Parsed as TOML: a
/// first-`path =`-line scan picked up `[[input.images]]` or soundtrack
/// paths whenever those came first in the file.
fn output_path_from_toml(cfg_path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(cfg_path).ok()?;
    let doc: toml::Value = toml::from_str(&text).ok()?;
    doc.get("outputs")?
        .as_array()?
        .first()?
        .get("path")?
        .as_str()
        .map(PathBuf::from)
}

/// Keep this render's output and manifest as their own version under
/// `<output dir>/versions/` — every render overwrites the configured
/// output path, so without this a history of renders (A/B compare,
/// undo) would only ever see the latest. Always a real copy: the
/// pipeline rewrites the output in place (ffmpeg `-y`, `fs::write`), so
/// a hard link would make every version turn into the newest render.
/// Returns `(video, manifest)`.
fn keep_version(output: &Path, tag: &str) -> Option<(PathBuf, PathBuf)> {
    let dir = output.parent()?.join("versions");
    std::fs::create_dir_all(&dir).ok()?;
    let stem = output.file_stem()?.to_string_lossy();
    let ext = output.extension().map(|e| e.to_string_lossy()).unwrap_or_else(|| "mp4".into());
    let video = dir.join(format!("{stem}__{tag}.{ext}"));
    let manifest = dir.join(format!("{stem}__{tag}.manifest.json"));
    let copy = |from: &Path, to: &Path| -> bool {
        let _ = std::fs::remove_file(to);
        std::fs::copy(from, to).is_ok()
    };
    if !copy(output, &video) {
        return None;
    }
    let src_manifest = manifest_for_output(&output.to_path_buf());
    if src_manifest.exists() && !copy(&src_manifest, &manifest) {
        return None;
    }
    Some((video, manifest))
}

fn manifest_for_output(p: &PathBuf) -> PathBuf {
    p.with_extension("manifest.json")
}

fn load_json(p: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vibecut_render_test_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn output_path_is_the_first_output_not_the_first_path_line() {
        let d = tmp("toml");
        let cfg = d.join("p.toml");
        std::fs::write(
            &cfg,
            "schema_version = 1\n[input]\nfolder = \"f\"\n[[input.images]]\npath = \"photo.jpg\"\nduration = \"3s\"\n\
             [[outputs]]\npath = \"/projects/out/reel.mp4\"\nresolution = \"1920x1080\"\n",
        )
        .unwrap();
        assert_eq!(output_path_from_toml(&cfg), Some(PathBuf::from("/projects/out/reel.mp4")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn each_render_keeps_its_own_version() {
        let d = tmp("versions");
        let out = d.join("reel.mp4");
        std::fs::write(&out, b"render one").unwrap();
        std::fs::write(d.join("reel.manifest.json"), b"{}").unwrap();
        let (v1, m1) = keep_version(&out, "a").unwrap();
        // The next render rewrites the output in place (truncate + write),
        // exactly as ffmpeg -y and fs::write do.
        std::fs::write(&out, b"render two").unwrap();
        std::fs::write(d.join("reel.manifest.json"), b"{\"v\":2}").unwrap();
        let (v2, _) = keep_version(&out, "b").unwrap();
        assert_eq!(std::fs::read(&v1).unwrap(), b"render one");
        assert_eq!(std::fs::read(&m1).unwrap(), b"{}");
        assert_eq!(std::fs::read(&v2).unwrap(), b"render two");
        assert!(m1.ends_with("versions/reel__a.manifest.json"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
