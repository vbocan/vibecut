//! Render a TOML config to an mp4, both synchronously and via
//! fire-and-fetch (`submit_async` + [`crate::jobs::JobRegistry`]).
//! Transport-free counterpart of the old `render_impl` /
//! `render_async_impl` / `render_status_impl`.

use std::path::Path;

use camino::{Utf8Path, Utf8PathBuf};
use vibecut::project_config::Config;

use crate::error::OpsError;
use crate::jobs::{JobRegistry, JobState};
use crate::paths::{check_config_paths_in, resolve_projects_checked, ConfigRoots};

/// What [`load_for_render`] found at a config path.
#[derive(Debug)]
enum Loaded {
    /// Loaded, and every path inside it is within the mounts. The
    /// pipeline runs this value rather than re-reading the file, so a
    /// config swapped on disk after the check can't slip past it.
    Ready(Box<Config>),
    /// Didn't parse or validate. Reported as a failed render, the same
    /// as when the pipeline loaded the file itself.
    Unloadable(String),
}

/// Load the config and confine the paths written inside it (see
/// [`crate::paths::check_config_paths`]). A path that escapes is an
/// [`OpsError::InvalidInput`] (a 400), raised before any render starts.
fn load_for_render(cfg_path: &Utf8Path) -> Result<Loaded, OpsError> {
    load_for_render_in(cfg_path, &ConfigRoots::mounts()?)
}

fn load_for_render_in(cfg_path: &Utf8Path, roots: &ConfigRoots) -> Result<Loaded, OpsError> {
    let cfg = match Config::load(cfg_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            let e = anyhow::Error::new(e).context(format!("loading config {cfg_path}"));
            return Ok(Loaded::Unloadable(format!("{e:#}")));
        }
    };
    check_config_paths_in(&cfg, roots)?;
    Ok(Loaded::Ready(Box::new(cfg)))
}

/// Render `config_path` to an mp4. Blocking under the hood — the
/// pipeline is a sync, CPU/GPU-bound function that spawns ffmpeg — but
/// wrapped in `spawn_blocking` so it doesn't tie up the async runtime.
/// Takes 1–10 minutes. Returns the same result shape whether called
/// here or polled via [`render_status`] after [`submit_async`].
pub async fn render_sync(config_path: &str) -> Result<serde_json::Value, OpsError> {
    let cfg = resolve_projects_checked(config_path)?;
    if !cfg.exists() {
        return Err(OpsError::not_found_with(
            "config",
            cfg.to_string_lossy().into_owned(),
        ));
    }
    let cfg_camino = Utf8PathBuf::from_path_buf(cfg.clone())
        .map_err(|_| OpsError::invalid(format!("non-UTF-8 config path: {}", cfg.display())))?;
    let config = match load_for_render(&cfg_camino)? {
        Loaded::Ready(config) => config,
        Loaded::Unloadable(msg) => {
            return Ok(build_render_result_json(&cfg, None, false, Some(&msg)));
        }
    };
    let primary_output = config.primary_output_path().as_std_path().to_path_buf();

    let join = tokio::task::spawn_blocking(move || -> Result<(), String> {
        vibecut::run_pipeline::run_config(
            *config,
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
    Ok(build_render_result_json(&cfg, Some(&primary_output), ok, error_msg.as_deref()))
}

/// Submit a render job and return its `job_id` immediately. The render
/// runs in the background via `tokio::spawn` + `spawn_blocking`; poll
/// with [`render_status`].
pub async fn submit_async(jobs: &JobRegistry, config_path: &str) -> Result<String, OpsError> {
    let cfg = resolve_projects_checked(config_path)?;
    if !cfg.exists() {
        return Err(OpsError::not_found_with(
            "config",
            cfg.to_string_lossy().into_owned(),
        ));
    }
    let cfg_camino = Utf8PathBuf::from_path_buf(cfg.clone())
        .map_err(|_| OpsError::invalid(format!("non-UTF-8 config path: {}", cfg.display())))?;
    let config = match load_for_render(&cfg_camino)? {
        Loaded::Ready(config) => config,
        Loaded::Unloadable(msg) => {
            // Same contract as when the job loaded the file itself: a
            // job is issued, and it fails.
            let job_id = jobs.submit().await;
            let now = chrono::Utc::now().to_rfc3339();
            let failed = JobState::Failed {
                started_at: now.clone(),
                finished_at: now,
                elapsed_ms: 0,
                error: msg,
            };
            jobs.set(&job_id, failed).await;
            return Ok(job_id);
        }
    };
    let primary_output = config.primary_output_path().as_std_path().to_path_buf();

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
        let inner = tokio::task::spawn_blocking(move || -> Result<(), String> {
            vibecut::run_pipeline::run_config(
                *config,
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

        let final_state = match inner {
            Ok(Ok(())) => {
                let result_json = build_render_result_json(&cfg, Some(&primary_output), true, None);
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
///
/// `primary_output` is the first `[[outputs]].path` of the config that
/// was checked and rendered, not a re-read of the file, so the manifest
/// summarised here is the one [`crate::paths::check_config_paths`]
/// confined. `None` when the config didn't load.
fn build_render_result_json(
    cfg_std: &Path,
    primary_output: Option<&Path>,
    ok: bool,
    error_msg: Option<&str>,
) -> serde_json::Value {
    let output_path = primary_output.map(Path::to_path_buf);
    let manifest_path = primary_output
        .and_then(Utf8Path::from_path)
        .map(|p| vibecut::run_pipeline::manifest_path_for(p).into_std_path_buf());
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

fn load_json(p: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn config_paths_outside_projects_are_rejected() {
        let jobs = JobRegistry::new();
        for p in ["/etc/passwd", "../etc/passwd", "..\\..\\etc\\passwd"] {
            assert!(matches!(
                render_sync(p).await,
                Err(OpsError::InvalidInput { .. })
            ));
            assert!(matches!(
                submit_async(&jobs, p).await,
                Err(OpsError::InvalidInput { .. })
            ));
        }
    }

    /// Temp `footage/` and `projects/` mounts with the cwd in a third
    /// dir, and a config written under `projects/` whose output is
    /// `output` (`{projects}` stands for the temp projects dir).
    fn roots_and_config(tag: &str, output: &str) -> (ConfigRoots, Utf8PathBuf) {
        let base = std::env::temp_dir().join(format!("sv_render_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for d in ["footage", "projects", "elsewhere"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        let base = base.canonicalize().unwrap();
        let roots = ConfigRoots {
            footage: base.join("footage"),
            projects: base.join("projects"),
            cwd: base.join("elsewhere"),
        };
        let output = output.replace("{projects}", &roots.projects.to_string_lossy());
        let toml_text = format!(
            r#"
schema_version = 1
[project]
name = "x"
target_duration = "10s"
fps = 30
[input]
folder = '{}'
[[outputs]]
path = '{output}'
resolution = "1920x1080"
"#,
            roots.footage.display()
        );
        let cfg_path = roots.projects.join("cfg.toml");
        std::fs::write(&cfg_path, toml_text).unwrap();
        (roots, Utf8PathBuf::from_path_buf(cfg_path).unwrap())
    }

    #[test]
    fn a_config_that_writes_outside_projects_is_refused_before_rendering() {
        let (roots, cfg_path) = roots_and_config("escape", "/etc/whatever.mp4");
        match load_for_render_in(&cfg_path, &roots) {
            Err(OpsError::InvalidInput { message, .. }) => {
                assert!(message.contains("outputs[0].path"), "{message}")
            }
            other => panic!("expected a 400, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(roots.footage.parent().unwrap());
    }

    #[test]
    fn a_config_inside_the_mounts_loads_once_for_rendering() {
        let (roots, cfg_path) = roots_and_config("ok", "{projects}/out/x.mp4");
        let output = roots.projects.join("out").join("x.mp4");
        match load_for_render_in(&cfg_path, &roots) {
            Ok(Loaded::Ready(cfg)) => {
                assert_eq!(cfg.primary_output_path().as_std_path(), output)
            }
            other => panic!("expected a loaded config, got {other:?}"),
        }

        // Unparseable stays a failed render (not a 400), as before.
        std::fs::write(&cfg_path, "not = [valid").unwrap();
        match load_for_render_in(&cfg_path, &roots) {
            Ok(Loaded::Unloadable(msg)) => assert!(msg.contains("loading config"), "{msg}"),
            other => panic!("expected an unloadable config, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(roots.footage.parent().unwrap());
    }

    #[test]
    fn render_result_reads_the_manifest_beside_the_checked_output() {
        let dir = std::env::temp_dir().join(format!("sv_render_result_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("x.mp4");
        std::fs::write(dir.join("x.manifest.json"), r#"{"segments": [{}, {}]}"#).unwrap();

        let r = build_render_result_json(&dir.join("cfg.toml"), Some(&output), true, None);
        assert_eq!(r["output_path"], output.to_str().unwrap());
        assert_eq!(r["manifest_summary"]["n_segments"], 2);

        let r = build_render_result_json(&dir.join("cfg.toml"), None, false, Some("boom"));
        assert!(r.get("output_path").is_none() && r.get("manifest_path").is_none());
        assert_eq!(r["error"], "boom");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
