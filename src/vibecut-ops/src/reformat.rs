//! Fast reformat of a finished render to a platform preset — a vertical
//! Reel, a square post, a WhatsApp-sized clip — in one ffmpeg pass over
//! the finished video, with the same reframe the render applies to its
//! `[[outputs]]` (see `vibecut::output_format`). Seconds, not a re-render.
//!
//! The version lands next to the source as `<stem>.<preset>.mp4`
//! (`<stem>.<preset>-<fit>.mp4` for a non-default fit), so the output
//! file route serves it like any render. A version that is newer than its
//! source is reused instead of being made again.
//!
//! [`submit_async`] runs it as a job in the shared [`JobRegistry`] —
//! poll it like a render (`render_status`, the WebSocket feed); the
//! result JSON is [`reformat_sync`]'s.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use camino::{Utf8Path, Utf8PathBuf};
use vibecut::output_format::{manifest_card_windows, Fit, OutputPreset, ReformatOptions};
pub use vibecut::output_format::version_path;
use vibecut::progress::{Progress, Stage};

use crate::error::OpsError;
use crate::jobs::{JobRegistry, JobState};
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
    let target = version_path(&source, preset, fit);
    Ok(ReformatTarget { source, target, preset, fit })
}

/// The target exists and is at least as new as the source.
fn is_fresh(source: &Path, target: &Path) -> bool {
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (mtime(source), mtime(target)) {
        (Some(s), Some(t)) => t >= s && std::fs::metadata(target).is_ok_and(|m| m.len() > 0),
        _ => false,
    }
}

/// Where ffmpeg writes before the version is complete, so a failed or
/// interrupted pass never leaves a truncated `<stem>.<preset>.mp4` that
/// would later look fresh.
fn partial_path(target: &Path) -> PathBuf {
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    target.with_file_name(format!(".{name}.partial.mp4"))
}

/// One lock per target, so two requests for the same version share one
/// pass (the second finds it fresh).
fn target_lock(target: &Path) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let mut map = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(target.to_path_buf()).or_default().clone()
}

fn utf8(p: &Path) -> Result<Utf8PathBuf, String> {
    Utf8PathBuf::from_path_buf(p.to_path_buf()).map_err(|p| format!("non-UTF-8 path: {}", p.display()))
}

/// Make (or reuse) the version. Blocking: probes and runs ffmpeg.
fn run_blocking(t: &ReformatTarget, on_progress: &mut dyn FnMut(f32)) -> Result<serde_json::Value, String> {
    let source = utf8(&t.source)?;
    let target = utf8(&t.target)?;
    let opts = ReformatOptions {
        preset: t.preset,
        fit: t.fit,
        // The render's title spans, so a narrower version keeps titles
        // whole and inside its safe area.
        card_windows: manifest_card_windows(&t.source),
    };
    let src = vibecut::output_format::probe_source(&source).map_err(|e| format!("{e:#}"))?;
    let plan = vibecut::output_format::plan_reformat(&src, &opts);
    let (cached, encoder) = if t.source == t.target || is_fresh(&t.source, &t.target) {
        on_progress(1.0);
        (true, None)
    } else {
        let tmp = utf8(&partial_path(&t.target))?;
        let outcome = vibecut::output_format::reformat_video(&source, &tmp, &opts, on_progress);
        match outcome {
            Ok(o) => {
                std::fs::rename(&tmp, &target).map_err(|e| format!("moving {tmp} to {target}: {e}"))?;
                (false, Some(o.encoder))
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(format!("{e:#}"));
            }
        }
    };
    let made = vibecut::output_format::probe_source(Utf8Path::new(&target)).map_err(|e| format!("{e:#}"))?;
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
        .map_err(OpsError::internal)
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

/// Submit a reformat job; returns its id and the checked request (the
/// version's path is known up front). Poll like a render.
pub async fn submit_async(
    jobs: &JobRegistry,
    output_path: &str,
    preset: &str,
    fit: Option<&str>,
) -> Result<(String, ReformatTarget), OpsError> {
    let t = prepare(output_path, preset, fit)?;
    let job_id = jobs.submit().await;
    let jobs_handle = jobs.clone();
    let id = job_id.clone();
    let task_target = t.clone();
    tokio::spawn(async move {
        let t = task_target;
        let lock = target_lock(&t.target);
        let _guard = lock.lock().await;
        let started_at = chrono::Utc::now().to_rfc3339();
        jobs_handle
            .set(
                &id,
                JobState::Running { started_at: started_at.clone(), progress: Some(progress_report(&t, 0.0)) },
            )
            .await;
        let t0 = std::time::Instant::now();
        let progress_jobs = jobs_handle.clone();
        let progress_id = id.clone();
        let inner = tokio::task::spawn_blocking(move || {
            let mut last = -1.0_f32;
            run_blocking(&t, &mut |f| {
                // ffmpeg reports about twice a second; skip repeats.
                if f - last >= 0.01 || f >= 1.0 {
                    last = f;
                    progress_jobs.set_progress_blocking(&progress_id, progress_report(&t, f));
                }
            })
        })
        .await;
        let elapsed_ms = t0.elapsed().as_millis() as u64;
        let finished_at = chrono::Utc::now().to_rfc3339();
        let state = match inner {
            Ok(Ok(result_json)) => JobState::Done { started_at, finished_at, elapsed_ms, result_json },
            Ok(Err(error)) => JobState::Failed { started_at, finished_at, elapsed_ms, error },
            Err(e) => JobState::Failed {
                started_at,
                finished_at,
                elapsed_ms,
                error: format!("reformat task panicked: {e}"),
            },
        };
        jobs_handle.set(&id, state).await;
    });
    Ok((job_id, t))
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
        assert_eq!(partial_path(&dst), d.join(".reel.reels.mp4.partial.mp4"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
