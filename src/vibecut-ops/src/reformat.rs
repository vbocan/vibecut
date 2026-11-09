//! Fast reformat of a finished render to a platform preset — a vertical
//! Reel, a square post, a WhatsApp-sized clip — in one ffmpeg pass over
//! the finished video, with the same reframe the render applies to its
//! `[[outputs]]` (see `vibecut::output_format`). Seconds, not a re-render.
//!
//! The version lands next to the source as `<stem>.<preset>.mp4`
//! (`<stem>.<preset>-<fit>.mp4` for a non-default fit), so the output
//! file route serves it like any render. A version that is newer than its
//! source is reused instead of being made again. Asked to reformat a
//! version (`<stem>.square.mp4`), it works from the render next to it
//! (`vibecut::output_format::reformat_source`).
//!
//! [`submit_async`] runs it as a job in the shared [`JobRegistry`], with
//! kind `reformat` in the jobs list — poll it like a render
//! (`render_status`, the WebSocket feed), stop it like one (its cancel
//! token kills the ffmpeg pass, and the last good version stays). A second
//! request for a version already being made joins that job. The result
//! JSON is [`reformat_sync`]'s. The version is written under a staging
//! name and renamed into place when complete (`vibecut::staging`), the
//! same as a render's `[[outputs]]`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use camino::{Utf8Path, Utf8PathBuf};
use vibecut::output_format::{manifest_card_windows, reformat_source, Fit, OutputPreset, ReformatOptions};
pub use vibecut::output_format::version_path;
use vibecut::progress::{Progress, Stage};

use crate::error::OpsError;
use crate::jobs::{JobRegistry, JobState, ReformatJob};
use crate::paths::resolve_projects;

/// Containers a reformat reads.
const VIDEO_EXTS: &[&str] = &["mp4", "m4v", "mov"];

/// A checked request: where the version comes from and where it goes.
#[derive(Debug, Clone, PartialEq)]
pub struct ReformatTarget {
    pub source: PathBuf,
    pub target: PathBuf,
    pub preset: OutputPreset,
    pub fit: Option<Fit>,
}

pub fn parse_preset(s: &str) -> Result<OutputPreset, OpsError> {
    OutputPreset::parse(s).ok_or_else(|| {
        OpsError::invalid_with_hint(
            format!("unknown preset {s:?}"),
            format!("use one of: {}", OutputPreset::names()),
        )
    })
}

pub fn parse_fit(s: Option<&str>) -> Result<Option<Fit>, OpsError> {
    match s.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(f) => Fit::parse(f)
            .map(Some)
            .ok_or_else(|| OpsError::invalid_with_hint(format!("unknown fit {f:?}"), "use blur, crop or pad")),
    }
}

/// Resolve and check a request without running anything.
pub fn prepare(output_path: &str, preset: &str, fit: Option<&str>) -> Result<ReformatTarget, OpsError> {
    let preset = parse_preset(preset)?;
    let fit = parse_fit(fit)?;
    let source = resolve_projects(output_path);
    let ext = source
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if !VIDEO_EXTS.contains(&ext.as_str()) {
        return Err(OpsError::invalid_with_hint(
            format!("not a video: {output_path:?}"),
            "pass a rendered .mp4 (output_path or version_path of a render)",
        ));
    }
    if !source.is_file() {
        return Err(OpsError::not_found_with("video", source.to_string_lossy().into_owned()));
    }
    // A version (`reel.square.mp4`) is made over from its render, never
    // reframed again into the render's own `<stem>.<preset>.mp4`.
    let source = reformat_source(&source);
    let target = version_path(&source, preset, fit);
    Ok(ReformatTarget { source, target, preset, fit })
}

/// The target exists and is at least as new as the source. Both are dated
/// by when they were committed (`vibecut::staging` stamps the commit
/// time), so a version made from the previous render is older than the
/// render that replaced it even if its pass finished before that render's
/// encode did; and `reformat_video` never commits a pass whose source was
/// replaced under it.
fn is_fresh(source: &Path, target: &Path) -> bool {
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (mtime(source), mtime(target)) {
        (Some(s), Some(t)) => t >= s && std::fs::metadata(target).is_ok_and(|m| m.len() > 0),
        _ => false,
    }
}

/// One lock per target, so two requests for the same version share one
/// pass (the second finds it fresh).
fn target_lock(target: &Path) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let mut map = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(target.to_path_buf()).or_default().clone()
}

fn utf8(p: &Path) -> anyhow::Result<Utf8PathBuf> {
    Utf8PathBuf::from_path_buf(p.to_path_buf()).map_err(|p| anyhow::anyhow!("non-UTF-8 path: {}", p.display()))
}

/// Make (or reuse) the version. Blocking: probes and runs ffmpeg. The pass
/// writes under a staging name and commits at the end
/// (`reformat_video`), and stops with `vibecut::cancel::Cancelled` when the
/// cancel token installed on this thread is cancelled.
fn run_blocking(t: &ReformatTarget, on_progress: &mut dyn FnMut(f32)) -> anyhow::Result<serde_json::Value> {
    let source = utf8(&t.source)?;
    let target = utf8(&t.target)?;
    let opts = ReformatOptions {
        preset: t.preset,
        fit: t.fit,
        // The render's title spans, so a narrower version keeps titles
        // whole and inside its safe area.
        card_windows: manifest_card_windows(&t.source),
    };
    vibecut::cancel::check()?;
    let src = vibecut::output_format::probe_source(&source)?;
    let plan = vibecut::output_format::plan_reformat(&src, &opts);
    let (cached, encoder) = if t.source == t.target || is_fresh(&t.source, &t.target) {
        on_progress(1.0);
        (true, None)
    } else {
        let o = vibecut::output_format::reformat_video(&source, &target, &opts, on_progress)?;
        (false, Some(o.encoder))
    };
    let made = vibecut::output_format::probe_source(Utf8Path::new(&target))?;
    Ok(serde_json::json!({
        "ok": true,
        "source_path": t.source.to_string_lossy(),
        "output_path": t.target.to_string_lossy(),
        "preset": t.preset.name(),
        "width": made.width,
        "height": made.height,
        "fit": plan.fit.map(Fit::name),
        "copied": plan.copy,
        "cached": cached,
        "encoder": encoder,
        "duration_secs": made.duration_secs,
        "source_duration_secs": src.duration_secs,
        "size_bytes": std::fs::metadata(&t.target).map(|m| m.len()).unwrap_or(0),
    }))
}

/// Make the version and wait for it (seconds for a 30 s clip).
pub async fn reformat_sync(
    output_path: &str,
    preset: &str,
    fit: Option<&str>,
) -> Result<serde_json::Value, OpsError> {
    let t = prepare(output_path, preset, fit)?;
    let lock = target_lock(&t.target);
    let _guard = lock.lock().await;
    tokio::task::spawn_blocking(move || run_blocking(&t, &mut |_| {}))
        .await
        .map_err(|e| OpsError::internal(format!("reformat task panicked: {e}")))?
        .map_err(|e| OpsError::internal(format!("{e:#}")))
}

/// Progress report for a reformat job: one stage, the whole bar.
fn progress_report(t: &ReformatTarget, fraction: f32) -> Progress {
    let spec = t.preset.spec();
    Progress {
        stage: Stage::Encoding,
        label: Stage::Encoding.label(),
        stage_fraction: fraction,
        overall: fraction,
        detail: Some(format!("{} · {}×{}", t.preset.name(), spec.width, spec.height)),
    }
}

/// A submitted reformat job.
#[derive(Debug, Clone)]
pub struct SubmittedReformat {
    pub job_id: String,
    /// The checked request; the version's path is known up front.
    pub target: ReformatTarget,
    /// The same version was already being made: this is that job.
    pub reused: bool,
}

/// The registry's record of a request: what the jobs list shows, and the
/// key a second request for the same version is matched on.
fn job_record(t: &ReformatTarget) -> ReformatJob {
    ReformatJob {
        source_path: t.source.to_string_lossy().into_owned(),
        preset: t.preset.name().to_string(),
        fit: t.fit.map(|f| f.name().to_string()),
        output_path: t.target.to_string_lossy().into_owned(),
    }
}

/// How a reformat job ended.
fn final_state(
    outcome: Result<anyhow::Result<serde_json::Value>, tokio::task::JoinError>,
    token: &vibecut::cancel::CancelToken,
    started_at: String,
    finished_at: String,
    elapsed_ms: u64,
) -> JobState {
    match outcome {
        Ok(Ok(result_json)) => JobState::Done { started_at, finished_at, elapsed_ms, result_json },
        // Killing ffmpeg can surface as some other failure first; a job
        // whose token was cancelled counts as stopped either way.
        Ok(Err(e)) if token.is_cancelled() || vibecut::cancel::is_cancelled(&e) => {
            JobState::Cancelled { started_at: Some(started_at), finished_at, elapsed_ms }
        }
        Ok(Err(e)) => JobState::Failed { started_at, finished_at, elapsed_ms, error: format!("{e:#}") },
        Err(e) => JobState::Failed {
            started_at,
            finished_at,
            elapsed_ms,
            error: format!("reformat task panicked: {e}"),
        },
    }
}

/// Submit a reformat job (kind `reformat` in the jobs list). Poll like a
/// render; stop it with the jobs cancel route. A request for a version
/// that a queued or running job is already making returns that job
/// (`reused`) instead of starting a second ffmpeg.
pub async fn submit_async(
    jobs: &JobRegistry,
    output_path: &str,
    preset: &str,
    fit: Option<&str>,
) -> Result<SubmittedReformat, OpsError> {
    let t = prepare(output_path, preset, fit)?;
    let (job_id, reused) = jobs.submit_reformat(job_record(&t)).await;
    if reused {
        return Ok(SubmittedReformat { job_id, target: t, reused });
    }
    let cancel = jobs
        .cancel_token(&job_id)
        .await
        .ok_or_else(|| OpsError::internal("job vanished right after submit"))?;
    let jobs_handle = jobs.clone();
    let id = job_id.clone();
    let task_target = t.clone();
    tokio::spawn(async move {
        let t = task_target;
        // Stays queued while a request for the same file (a reformat the
        // registry didn't join, e.g. one being stopped) finishes.
        let lock = target_lock(&t.target);
        let _guard = lock.lock().await;
        let started_at = chrono::Utc::now().to_rfc3339();
        // Stopped while it waited: it never starts.
        if !jobs_handle.begin(&id, &started_at).await {
            return;
        }
        let t0 = std::time::Instant::now();
        let progress_jobs = jobs_handle.clone();
        let progress_id = id.clone();
        let token = cancel.clone();
        let inner = tokio::task::spawn_blocking(move || {
            progress_jobs.set_progress_blocking(&progress_id, progress_report(&t, 0.0));
            // The job's token rides along, so a Stop kills the ffmpeg pass.
            vibecut::progress::with_sink_and_cancel(
                |_| {},
                token,
                || {
                    let mut last = -1.0_f32;
                    run_blocking(&t, &mut |f| {
                        // ffmpeg reports about twice a second; skip repeats.
                        if f - last >= 0.01 || f >= 1.0 {
                            last = f;
                            progress_jobs.set_progress_blocking(&progress_id, progress_report(&t, f));
                        }
                    })
                },
            )
        })
        .await;
        let elapsed_ms = t0.elapsed().as_millis() as u64;
        let finished_at = chrono::Utc::now().to_rfc3339();
        let state = final_state(inner, &cancel, started_at, finished_at, elapsed_ms);
        jobs_handle.set(&id, state).await;
    });
    Ok(SubmittedReformat { job_id, target: t, reused })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vibecut_reformat_test_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn requests_are_checked() {
        let d = tmp("prepare");
        let video = d.join("reel.mp4");
        std::fs::write(&video, b"x").unwrap();
        let v = video.to_string_lossy();
        let t = prepare(&v, "reels", None).unwrap();
        assert_eq!(t.target, d.join("reel.reels.mp4"));
        assert!(matches!(prepare(&v, "reel", None), Err(OpsError::InvalidInput { .. })));
        assert!(matches!(prepare(&v, "reels", Some("zoom")), Err(OpsError::InvalidInput { .. })));
        assert_eq!(prepare(&v, "square", Some("crop")).unwrap().fit, Some(Fit::Crop));
        let missing = d.join("gone.mp4");
        assert!(matches!(prepare(&missing.to_string_lossy(), "reels", None), Err(OpsError::NotFound { .. })));
        let manifest = d.join("reel.manifest.json");
        std::fs::write(&manifest, b"{}").unwrap();
        assert!(matches!(prepare(&manifest.to_string_lossy(), "reels", None), Err(OpsError::InvalidInput { .. })));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_version_of_a_version_is_made_from_the_render() {
        let d = tmp("nested");
        std::fs::write(d.join("coast.mp4"), b"render").unwrap();
        std::fs::write(d.join("coast.square.mp4"), b"square").unwrap();
        std::fs::write(d.join("coast.reels-crop.mp4"), b"crop").unwrap();
        // "Now make the square one vertical too": made from the render, into
        // the render's reels slot — the version a request from the render
        // reuses, so it must hold the render's picture.
        let t = prepare(&d.join("coast.square.mp4").to_string_lossy(), "reels", None).unwrap();
        assert_eq!((t.source.clone(), t.target.clone()), (d.join("coast.mp4"), d.join("coast.reels.mp4")));
        assert_eq!(t, prepare(&d.join("coast.mp4").to_string_lossy(), "reels", None).unwrap());
        // A crop version asked for as a blur reel: from the render, not a
        // same-shape stream copy of the crop.
        let t = prepare(&d.join("coast.reels-crop.mp4").to_string_lossy(), "reels", None).unwrap();
        assert_eq!(t.source, d.join("coast.mp4"));
        // Render gone: the version is the source, and the result keeps its
        // whole stem instead of taking `coast.reels.mp4`.
        std::fs::remove_file(d.join("coast.mp4")).unwrap();
        let t = prepare(&d.join("coast.square.mp4").to_string_lossy(), "reels", None).unwrap();
        assert_eq!((t.source, t.target), (d.join("coast.square.mp4"), d.join("coast.square.reels.mp4")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_version_newer_than_its_source_is_reused() {
        let d = tmp("fresh");
        let src = d.join("reel.mp4");
        let dst = d.join("reel.reels.mp4");
        std::fs::write(&src, b"render").unwrap();
        assert!(!is_fresh(&src, &dst));
        std::fs::write(&dst, b"version").unwrap();
        assert!(is_fresh(&src, &dst));
        // Re-rendering the source makes the version stale.
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(&src).unwrap().set_modified(later).unwrap();
        assert!(!is_fresh(&src, &dst));
        // An empty file is never a finished version.
        std::fs::write(&dst, b"").unwrap();
        std::fs::File::options().write(true).open(&dst).unwrap().set_modified(later).unwrap();
        assert!(!is_fresh(&src, &dst));
        let _ = std::fs::remove_dir_all(&d);
    }

    fn ffmpeg_available() -> bool {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// A 16:9 "render" `secs` long.
    fn render_clip(d: &Path, secs: u32) -> PathBuf {
        let clip = d.join("coast.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-nostdin", "-f", "lavfi", "-i"])
            .arg(format!("testsrc2=size=640x360:rate=30:duration={secs}"))
            .args(["-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=330:duration={secs}"))
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-shortest"])
            .arg(&clip)
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "generating {}", clip.display());
        clip
    }

    async fn wait_finished(jobs: &JobRegistry, id: &str) -> JobState {
        for _ in 0..600 {
            let st = jobs.get(id).await.unwrap();
            if st.is_finished() {
                return st;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("job {id} never finished");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reformat_jobs_are_listed_joined_and_stoppable() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not on PATH; skipping");
            return;
        }
        let d = tmp("job");
        let clip = render_clip(&d, 20);
        let target = d.join("coast.reels.mp4");
        std::fs::write(&target, b"the last good version").unwrap();
        // Make the old version look stale, so the job re-makes it.
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(&clip).unwrap().set_modified(later).unwrap();
        let jobs = JobRegistry::new();
        let src = clip.to_string_lossy().into_owned();
        let a = submit_async(&jobs, &src, "reels", None).await.unwrap();
        assert!(!a.reused);
        assert_eq!(a.target.target, target);
        // A second click while it runs joins the same job.
        let b = submit_async(&jobs, &src, "reels", None).await.unwrap();
        assert!(b.reused && b.job_id == a.job_id);
        let listed = jobs.info(&a.job_id).await.unwrap();
        assert_eq!(listed.kind, crate::jobs::JobKind::Reformat);
        let rec = listed.reformat.unwrap();
        assert_eq!((rec.preset.as_str(), rec.output_path.as_str()), ("reels", target.to_str().unwrap()));
        // Wait until ffmpeg is under way, then stop it.
        for _ in 0..400 {
            if let Some(JobState::Running { progress: Some(p), .. }) = jobs.get(&a.job_id).await {
                if p.overall > 0.0 {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        jobs.cancel(&a.job_id).await.unwrap();
        let st = wait_finished(&jobs, &a.job_id).await;
        assert!(matches!(st, JobState::Cancelled { started_at: Some(_), .. }), "{st:?}");
        assert_eq!(std::fs::read(&target).unwrap(), b"the last good version");
        assert!(vibecut::staging::staged_files(Utf8Path::from_path(&target).unwrap()).is_empty());
        // A finished reformat job carries the version it made.
        let c = submit_async(&jobs, &src, "whatsapp", None).await.unwrap();
        assert!(!c.reused);
        let st = wait_finished(&jobs, &c.job_id).await;
        match st {
            JobState::Done { result_json, .. } => {
                assert_eq!((result_json["width"].as_u64(), result_json["height"].as_u64()), (Some(720), Some(1280)));
                assert_eq!(result_json["cached"], false);
            }
            other => panic!("expected done, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
