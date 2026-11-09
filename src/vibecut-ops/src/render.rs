//! Render a TOML config to an mp4, both synchronously and via
//! fire-and-fetch (`submit_async` + [`crate::jobs::JobRegistry`]).
//! Transport-free counterpart of the old `render_impl` /
//! `render_async_impl` / `render_status_impl`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use camino::{Utf8Path, Utf8PathBuf};
use vibecut::project_config::{Config, Quality};

use crate::error::OpsError;
use crate::jobs::{JobInfo, JobRegistry, JobState};
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

/// `config_path` resolved under `/projects`, required to exist.
fn existing_config(config_path: &str) -> Result<(PathBuf, Utf8PathBuf), OpsError> {
    let cfg = resolve_projects_checked(config_path)?;
    if !cfg.exists() {
        return Err(OpsError::not_found_with("config", cfg.to_string_lossy().into_owned()));
    }
    let cfg_camino = Utf8PathBuf::from_path_buf(cfg.clone())
        .map_err(|_| OpsError::invalid(format!("non-UTF-8 config path: {}", cfg.display())))?;
    Ok((cfg, cfg_camino))
}

/// The first `[[outputs]].path` of a checked config.
fn primary_output(config: &Config) -> Option<PathBuf> {
    config.outputs.first().map(|o| o.path.as_std_path().to_path_buf())
}

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

/// What a render request asks for beyond its config.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderRequest {
    /// `Some(Draft)`: a fast 540p preview of the same edit, written to
    /// `<stem>.draft.mp4` beside the final. `None`: the config's
    /// `[project].quality`, final unless it says otherwise.
    pub quality: Option<Quality>,
}

impl RenderRequest {
    /// A request's `quality` field: absent, `"draft"` or `"final"`.
    pub fn from_quality_field(quality: Option<&str>) -> Result<Self, OpsError> {
        let quality = match quality.map(str::trim).filter(|q| !q.is_empty()) {
            None => None,
            Some(q) => Some(Quality::parse(q).ok_or_else(|| {
                OpsError::invalid_with_hint(
                    format!("quality must be \"draft\" or \"final\", got {q:?}"),
                    "draft = a fast 540p preview beside the final; final = the full render",
                )
            })?),
        };
        Ok(RenderRequest { quality })
    }
}

/// Render `config_path` to an mp4. Blocking under the hood — the
/// pipeline is a sync, CPU/GPU-bound function that spawns ffmpeg — but
/// wrapped in `spawn_blocking` so it doesn't tie up the async runtime.
/// Takes 1–10 minutes. Returns the same result shape whether called
/// here or polled via [`render_status`] after [`submit_async`].
pub async fn render_sync(config_path: &str) -> Result<serde_json::Value, OpsError> {
    render_sync_with(config_path, RenderRequest::default()).await
}

/// [`render_sync`] at the requested quality.
pub async fn render_sync_with(config_path: &str, req: RenderRequest) -> Result<serde_json::Value, OpsError> {
    let (cfg, cfg_camino) = existing_config(config_path)?;
    let config = match load_for_render(&cfg_camino)? {
        Loaded::Ready(config) => config,
        Loaded::Unloadable(msg) => {
            let quality = req.quality.unwrap_or_default();
            return Ok(build_render_result_json(&cfg, None, quality, false, Some(&msg)));
        }
    };
    let quality = config.render_quality(req.quality);
    let primary = primary_output(&config);

    let published = crate::plan::RenderPlan::begin(&cfg);
    let lock = project_lock(&cfg);
    let _guard = lock.lock().await;
    let join = tokio::task::spawn_blocking(move || -> Result<(), String> {
        published.started();
        vibecut::run_pipeline::run_config(*config, run_options(req, published))
            .map(|_| ())
            .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| OpsError::internal(format!("render task panicked: {e}")))?;

    let (ok, error_msg) = match join {
        Ok(()) => (true, None),
        Err(msg) => (false, Some(msg)),
    };
    Ok(build_render_result_json(&cfg, primary.as_deref(), quality, ok, error_msg.as_deref()))
}

/// How the pipeline runs a web / MCP render: everything at its default
/// but the requested quality, and the plan published for storyboards.
fn run_options(req: RenderRequest, published: crate::plan::RenderPlan) -> vibecut::run_pipeline::RunOptions {
    vibecut::run_pipeline::RunOptions {
        quality: req.quality,
        on_plan: Some(published.hook()),
        ..Default::default()
    }
}

/// Submit a render job and return its `job_id` immediately. The render
/// runs in the background via `tokio::spawn` + `spawn_blocking`; poll
/// with [`render_status`].
pub async fn submit_async(jobs: &JobRegistry, config_path: &str) -> Result<String, OpsError> {
    submit_async_with(jobs, config_path, RenderRequest::default()).await
}

/// [`submit_async`] at the requested quality. The render publishes its
/// plan as soon as it has one, for [`crate::plan::plan`].
pub async fn submit_async_with(
    jobs: &JobRegistry,
    config_path: &str,
    req: RenderRequest,
) -> Result<String, OpsError> {
    let (cfg, cfg_camino) = existing_config(config_path)?;
    match load_for_render(&cfg_camino)? {
        Loaded::Ready(config) => submit_loaded(jobs, cfg, *config, req).await,
        Loaded::Unloadable(msg) => {
            // Same contract as when the job loaded the file itself: a
            // job is issued, and it fails.
            let job_id = jobs.submit_for(&cfg.to_string_lossy()).await;
            let now = chrono::Utc::now().to_rfc3339();
            let failed = JobState::Failed {
                started_at: now.clone(),
                finished_at: now,
                elapsed_ms: 0,
                error: msg,
            };
            jobs.set(&job_id, failed).await;
            Ok(job_id)
        }
    }
}

/// Queue the render of `config`, already loaded from `cfg` and checked.
async fn submit_loaded(
    jobs: &JobRegistry,
    cfg: PathBuf,
    config: Config,
    req: RenderRequest,
) -> Result<String, OpsError> {
    let quality = config.render_quality(req.quality);
    let primary = primary_output(&config);
    let job_id = jobs.submit_for(&cfg.to_string_lossy()).await;
    // From now on a storyboard request waits for this render's plan.
    let published = crate::plan::RenderPlan::begin(&cfg);
    let cancel = jobs
        .cancel_token(&job_id)
        .await
        .ok_or_else(|| OpsError::internal("job vanished right after submit"))?;
    let jobs_handle = jobs.clone();
    let job_id_for_task = job_id.clone();

    tokio::spawn(async move {
        // Stays "queued" while another render of this project finishes.
        let lock = project_lock(&cfg);
        let _guard = lock.lock().await;
        let started_at = chrono::Utc::now().to_rfc3339();
        // Cancelled while it waited: it never starts.
        if !jobs_handle.begin(&job_id_for_task, &started_at).await {
            return;
        }
        let t0 = std::time::Instant::now();
        let progress_jobs = jobs_handle.clone();
        let progress_id = job_id_for_task.clone();
        let render_cancel = cancel.clone();
        let inner = tokio::task::spawn_blocking(move || -> Result<(), RenderStop> {
            // Every stage report lands on the job, where the status
            // endpoint and the WebSocket feed pick it up; the job's cancel
            // token rides along, so Stop kills the pipeline's ffmpeg runs.
            vibecut::progress::with_sink_and_cancel(
                move |p| progress_jobs.set_progress_blocking(&progress_id, p.clone()),
                render_cancel.clone(),
                || {
                    // Stopped between `begin` and here.
                    vibecut::cancel::check()?;
                    published.started();
                    vibecut::run_pipeline::run_config(config, run_options(req, published)).map(|_| ())
                },
            )
            .map_err(|e| RenderStop::classify(&e, &render_cancel))
        })
        .await;
        let elapsed_ms = t0.elapsed().as_millis() as u64;
        let finished_at = chrono::Utc::now().to_rfc3339();

        let final_state = match inner {
            Ok(Ok(())) => {
                let mut result_json =
                    build_render_result_json(&cfg, primary.as_deref(), quality, true, None);
                // Each async render also keeps its own copy, so a UI can
                // compare and restore earlier versions.
                let tag = format!(
                    "{}_{}",
                    chrono::Utc::now().format("%Y%m%dT%H%M%S"),
                    job_id_for_task
                );
                let out = primary.as_deref().map(|o| quality_output_path(o, quality));
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
            Ok(Err(RenderStop::Cancelled)) => JobState::Cancelled {
                started_at: Some(started_at),
                finished_at,
                elapsed_ms,
            },
            Ok(Err(RenderStop::Failed(msg))) => JobState::Failed {
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

/// Why an async render's pipeline returned early.
#[derive(Debug, PartialEq)]
enum RenderStop {
    /// Stopped on request.
    Cancelled,
    /// A real failure, as a human-readable message.
    Failed(String),
}

impl RenderStop {
    /// A render whose token was cancelled counts as cancelled whatever
    /// error it surfaced with: killing ffmpeg can show up as some other
    /// stage failing first.
    fn classify(e: &anyhow::Error, token: &vibecut::cancel::CancelToken) -> Self {
        if token.is_cancelled() || vibecut::cancel::is_cancelled(e) {
            RenderStop::Cancelled
        } else {
            RenderStop::Failed(crate::error::without_source_text(&format!("{e:#}")))
        }
    }
}

/// Look up a submitted job's current state. `None` means the id was
/// never issued (or was evicted — see [`JobRegistry`]'s retention cap).
pub async fn render_status(jobs: &JobRegistry, job_id: &str) -> Option<JobState> {
    jobs.get(job_id).await
}

/// Stop a job: a queued one never starts, a running one has its ffmpeg
/// children killed and turns `cancelled` within about a second (poll it,
/// or watch the progress feed). The last good output stays in place.
/// Returns the job's JSON (see [`job_info_json`]) right after the
/// request; `None` for an unknown id.
pub async fn cancel(jobs: &JobRegistry, job_id: &str) -> Option<serde_json::Value> {
    jobs.cancel(job_id).await.map(|info| job_info_json(&info))
}

/// Every retained job, newest first, as `{ "jobs": [...] }`.
pub async fn list_jobs(jobs: &JobRegistry) -> serde_json::Value {
    let list: Vec<serde_json::Value> = jobs.list().await.iter().map(job_info_json).collect();
    serde_json::json!({ "jobs": list })
}

/// [`job_state_json`] plus what the jobs list needs: the job's `kind`
/// (`render` or `reformat`), the config it renders, when it was submitted,
/// and whether a Stop is pending. A reformat job also names the render it
/// works from (`source_path`), its `preset` and `fit`, and the version it
/// writes (`output_path`, known before it is done).
pub fn job_info_json(info: &JobInfo) -> serde_json::Value {
    let mut v = job_state_json(&info.id, info.state.clone());
    if let serde_json::Value::Object(map) = &mut v {
        map.insert("kind".into(), info.kind.as_str().into());
        map.insert("config_path".into(), info.config_path.clone().into());
        map.insert("submitted_at".into(), info.submitted_at.clone().into());
        map.insert("cancel_requested".into(), info.cancel_requested.into());
        if let Some(r) = &info.reformat {
            map.insert("source_path".into(), r.source_path.clone().into());
            map.insert("preset".into(), r.preset.clone().into());
            map.insert("fit".into(), r.fit.clone().into());
            // A done job's result already has it (the same path).
            map.entry("output_path").or_insert_with(|| r.output_path.clone().into());
        }
    }
    v
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
        JobState::Cancelled {
            started_at,
            finished_at,
            elapsed_ms,
        } => serde_json::json!({
            "job_id": job_id,
            "state": "cancelled",
            "started_at": started_at,
            "finished_at": finished_at,
            "elapsed_ms": elapsed_ms,
        }),
    }
}

// ---------- shared result-building ----------

/// Build the JSON body returned by a sync render and (after
/// completion) by an async one. Same shape either way so callers see
/// one contract regardless of how the call was made.
///
/// `primary` is the first `[[outputs]].path` of the config that was
/// checked and rendered, not a re-read of the file, so the manifest
/// summarised here is one [`crate::paths::check_config_paths`] confined.
/// `None` when the config didn't load.
fn build_render_result_json(
    cfg_std: &Path,
    primary: Option<&Path>,
    quality: Quality,
    ok: bool,
    error_msg: Option<&str>,
) -> serde_json::Value {
    // A draft's output and manifest sit beside the final's.
    let output_path = primary.map(|o| quality_output_path(o, quality));
    let manifest_path = output_path.as_ref().map(manifest_for_output);
    let manifest = manifest_path.as_deref().and_then(load_json);

    let mut result = serde_json::json!({
        "ok": ok,
        "config_path": cfg_std.to_string_lossy(),
        "quality": quality.as_str(),
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

/// Where a render at `quality` writes the configured output `out`.
fn quality_output_path(out: &Path, quality: Quality) -> PathBuf {
    match camino::Utf8Path::from_path(out) {
        Some(p) => vibecut::run_pipeline::output_path_for(p, quality).into_std_path_buf(),
        None => out.to_path_buf(),
    }
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
    fn a_stopped_render_is_cancelled_not_failed() {
        let token = vibecut::cancel::CancelToken::new();
        let boom = anyhow::anyhow!("ffmpeg failed (exit 1)");
        assert_eq!(
            RenderStop::classify(&boom, &token),
            RenderStop::Failed("ffmpeg failed (exit 1)".into())
        );
        let stopped = anyhow::Error::from(vibecut::cancel::Cancelled).context("normalize segment 2");
        assert_eq!(RenderStop::classify(&stopped, &token), RenderStop::Cancelled);
        // Once Stop was pressed, whatever the kill surfaced as is a cancel.
        token.cancel();
        assert_eq!(RenderStop::classify(&boom, &token), RenderStop::Cancelled);
    }

    #[test]
    fn cancelled_state_json() {
        let v = job_state_json(
            "job_1",
            JobState::Cancelled { started_at: None, finished_at: "t1".into(), elapsed_ms: 0 },
        );
        assert_eq!(v["state"], "cancelled");
        assert_eq!(v["job_id"], "job_1");
        assert!(v["started_at"].is_null());
    }

    #[tokio::test]
    async fn the_jobs_list_tells_renders_from_reformats() {
        let jobs = JobRegistry::new();
        let render = jobs.submit_for("/projects/a.toml").await;
        let (reformat, _) = jobs
            .submit_reformat(crate::jobs::ReformatJob {
                source_path: "/projects/out/a.mp4".into(),
                preset: "square".into(),
                fit: None,
                output_path: "/projects/out/a.square.mp4".into(),
            })
            .await;
        let listed = list_jobs(&jobs).await;
        let row = |id: &str| listed["jobs"].as_array().unwrap().iter().find(|j| j["job_id"] == id).unwrap().clone();
        let r = row(&render);
        assert_eq!(r["kind"], "render");
        assert_eq!(r["config_path"], "/projects/a.toml");
        assert!(r.get("preset").is_none() && r.get("source_path").is_none());
        let f = row(&reformat);
        assert_eq!(f["kind"], "reformat");
        assert_eq!(f["preset"], "square");
        assert!(f["fit"].is_null());
        assert_eq!(f["source_path"], "/projects/out/a.mp4");
        assert_eq!(f["output_path"], "/projects/out/a.square.mp4");
        // Done: the result's own output_path is kept, not overwritten.
        jobs.set(
            &reformat,
            JobState::Done {
                started_at: "t0".into(),
                finished_at: "t1".into(),
                elapsed_ms: 5,
                result_json: serde_json::json!({ "ok": true, "output_path": "/projects/out/a.square.mp4", "width": 1080 }),
            },
        )
        .await;
        let f = job_info_json(&jobs.info(&reformat).await.unwrap());
        assert_eq!((f["state"].as_str(), f["kind"].as_str(), f["width"].as_u64()), (Some("done"), Some("reformat"), Some(1080)));
    }

    /// A job cancelled while it waits for the project lock never runs:
    /// its footage folder doesn't exist, so running it would end `failed`.
    #[tokio::test]
    async fn a_cancelled_queued_render_never_starts() {
        let (roots, cfg_camino) = roots_and_config("queued", "{projects}/out/x.mp4");
        let cfg = cfg_camino.as_std_path().to_path_buf();
        let Ok(Loaded::Ready(config)) = load_for_render_in(&cfg_camino, &roots) else {
            panic!("the test config should load");
        };
        let jobs = JobRegistry::new();
        let lock = project_lock(&cfg);
        let held = lock.lock().await; // another render of this project
        let id = submit_loaded(&jobs, cfg.clone(), *config, RenderRequest::default()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(matches!(jobs.get(&id).await.unwrap(), JobState::Queued));
        let after = cancel(&jobs, &id).await.unwrap();
        assert_eq!(after["state"], "cancelled");
        assert_eq!(after["cancel_requested"], true);
        drop(held);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        match jobs.get(&id).await.unwrap() {
            JobState::Cancelled { started_at: None, .. } => {}
            other => panic!("cancelled job ran anyway: {other:?}"),
        }
        let listed = list_jobs(&jobs).await;
        assert_eq!(listed["jobs"][0]["job_id"], id.as_str());
        assert!(listed["jobs"][0]["config_path"].as_str().unwrap().ends_with("cfg.toml"));
        let _ = std::fs::remove_dir_all(roots.footage.parent().unwrap());
    }

    #[test]
    fn the_quality_field_is_draft_or_final() {
        assert_eq!(RenderRequest::from_quality_field(None).unwrap().quality, None);
        assert_eq!(RenderRequest::from_quality_field(Some("")).unwrap().quality, None);
        assert_eq!(RenderRequest::from_quality_field(Some("Draft")).unwrap().quality, Some(Quality::Draft));
        assert_eq!(RenderRequest::from_quality_field(Some("final")).unwrap().quality, Some(Quality::Final));
        let e = RenderRequest::from_quality_field(Some("preview")).unwrap_err();
        assert!(matches!(e, OpsError::InvalidInput { .. }), "{e:?}");
    }

    /// A draft's result names the draft's own files, beside the final's;
    /// the request wins over the config's `[project].quality`.
    #[test]
    fn a_draft_result_names_the_draft_beside_the_final() {
        let d = tmp("draft_result");
        let out = d.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let toml = |quality: &str| {
            format!(
                "schema_version = 1\n[project]\nname = \"x\"\ntarget_duration = \"10s\"\nfps = 30\n{quality}\n\
                 [input]\nfolder = \".\"\n[[outputs]]\npath = \"{}\"\nresolution = \"1920x1080\"\n",
                out.join("reel.mp4").to_string_lossy().replace('\\', "/")
            )
        };
        let cfg = d.join("p.toml");
        std::fs::write(&cfg, toml("")).unwrap();
        std::fs::write(out.join("reel.draft.manifest.json"), r#"{"segments":[1,2,3],"beat_sync":{"n_cuts":2}}"#).unwrap();
        let result = |requested: Option<Quality>| {
            let config = Config::load(Utf8Path::from_path(&cfg).unwrap()).unwrap();
            let quality = config.render_quality(requested);
            build_render_result_json(&cfg, primary_output(&config).as_deref(), quality, true, None)
        };
        let name = |v: &serde_json::Value, k: &str| {
            Path::new(v[k].as_str().unwrap()).file_name().unwrap().to_string_lossy().into_owned()
        };

        let r = result(Some(Quality::Draft));
        assert_eq!(r["quality"], "draft");
        assert_eq!(name(&r, "output_path"), "reel.draft.mp4");
        assert_eq!(name(&r, "manifest_path"), "reel.draft.manifest.json");
        assert_eq!(r["manifest_summary"]["n_segments"], 3);

        let r = result(None);
        assert_eq!(r["quality"], "final");
        assert_eq!(name(&r, "output_path"), "reel.mp4");
        assert_eq!(name(&r, "manifest_path"), "reel.manifest.json");

        std::fs::write(&cfg, toml("quality = \"draft\"")).unwrap();
        assert_eq!(result(None)["quality"], "draft");
        assert_eq!(result(Some(Quality::Final))["quality"], "final");

        // Its kept version is a draft too, never confused with the final's.
        std::fs::write(out.join("reel.draft.mp4"), b"draft").unwrap();
        let (v, m) = keep_version(&out.join("reel.draft.mp4"), "t1").unwrap();
        assert!(v.ends_with("versions/reel.draft__t1.mp4"), "{v:?}");
        assert!(m.ends_with("versions/reel.draft__t1.manifest.json"), "{m:?}");
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

    #[tokio::test]
    async fn config_paths_outside_projects_are_rejected() {
        let jobs = JobRegistry::new();
        for p in ["/etc/passwd", "../etc/passwd", r"..\..\etc\passwd"] {
            assert!(matches!(render_sync(p).await, Err(OpsError::InvalidInput { .. })), "{p}");
            assert!(matches!(submit_async(&jobs, p).await, Err(OpsError::InvalidInput { .. })), "{p}");
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
            extra_read: Vec::new(),
            extra_write: Vec::new(),
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
            roots.footage.join("missing").display()
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
            Ok(Loaded::Ready(cfg)) => assert_eq!(cfg.primary_output_path().as_std_path(), output),
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
    fn an_unloaded_config_has_no_output_in_its_result() {
        let r = build_render_result_json(Path::new("/projects/cfg.toml"), None, Quality::Final, false, Some("boom"));
        assert!(r.get("output_path").is_none() && r.get("manifest_path").is_none());
        assert_eq!(r["error"], "boom");
    }
}
