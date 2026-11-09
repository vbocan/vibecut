//! Frame extraction for multimodal context.
//!
//! After every render the LLM (or a web UI preview) ideally "sees"
//! what came out — first frame, last frame, an even sampling across
//! the body, and (when the user references something specific) the
//! frame at that timestamp. This module does the ffmpeg-side
//! extraction and stays transport-agnostic: it returns raw JPEG bytes
//! plus metadata, not `rmcp::model::Content`. The MCP layer wraps the
//! result into inline image content for multimodal clients; a REST
//! layer would instead return the bytes as a data URI or a static file.
//!
//! The same calls serve the UI's footage thumbnails (frames of source
//! chapters). HDR (HLG / PQ) footage is grabbed through the colour
//! conversion normalize applies to it (`vibecut::video_renderer::
//! frame_color_conversion`), so its thumbnails aren't washed out.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use tokio::process::Command;

use crate::error::OpsError;
use crate::paths::resolve_projects;

/// Output JPEG width in pixels. 480 px wide at quality 5 (JPEG q≈75)
/// lands around 30–60 KB per frame for typical GoPro content; that
/// keeps responses compact even with 12+ frames inline.
pub const DEFAULT_JPEG_WIDTH: u32 = 480;

/// JPEG quality knob passed to ffmpeg as `-q:v`. The scale is 1..31
/// where lower numbers are higher quality (perceptually inverse of the
/// usual 0..100). 5 ≈ quality 75 — visually clean but compact.
const JPEG_QSCALE: &str = "5";

/// Cap on how many frames a single call can return. Bigger counts blow
/// up the response and slow the client down without helping reasoning
/// much; 16 is comfortably above what the curator needs for an even
/// sampling.
pub const MAX_FRAMES_PER_CALL: usize = 16;

// ---------- timestamp curation ----------

/// Pick `n` evenly spaced timestamps across `[0, duration_secs]`,
/// inset slightly at the head and tail so we land on visible footage
/// rather than fade-in / fade-out black.
pub fn even_timestamps(duration_secs: f32, n: usize) -> Vec<f32> {
    if n == 0 || duration_secs <= 0.0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![duration_secs.max(0.0) / 2.0];
    }
    // Inset 50 ms past the very start (typical fade-in is 200–500 ms,
    // but we accept some risk of black at 50 ms in exchange for
    // covering very-short clips). Inset 100 ms before the end.
    let head = 0.05_f32.min(duration_secs * 0.01);
    let tail = (duration_secs - 0.1).max(head);
    let step = (tail - head) / (n - 1) as f32;
    (0..n).map(|i| head + step * i as f32).collect()
}

/// Clamp a list of user-supplied timestamps to `[0, duration]` and
/// drop NaN / infinite values. Returns the cleaned list and a count of
/// how many entries were rejected.
pub fn clean_timestamps(timestamps: &[f32], duration_secs: f32) -> (Vec<f32>, usize) {
    let mut kept = Vec::with_capacity(timestamps.len());
    let mut rejected = 0usize;
    for &t in timestamps {
        if !t.is_finite() || t < 0.0 || t > duration_secs.max(0.0) {
            rejected += 1;
            continue;
        }
        kept.push(t);
    }
    (kept, rejected)
}

// ---------- raw frame extraction ----------

/// The `-vf` of a frame grab: scale to `width` (height auto, even), then
/// the clip's colour conversion, if it has one (after the scale, so a
/// float tone-map works on the small frame).
fn frame_filter(width: u32, color_filter: Option<&str>) -> String {
    match color_filter {
        Some(c) => format!("scale={width}:-2,{c}"),
        None => format!("scale={width}:-2"),
    }
}

/// The colour conversion frames of `path` need (`None` for SDR): one
/// ffprobe, on a blocking thread.
async fn color_filter_for(path: &Path) -> Option<String> {
    let path = camino::Utf8PathBuf::from_path_buf(path.to_path_buf()).ok()?;
    tokio::task::spawn_blocking(move || vibecut::video_renderer::frame_color_conversion(&path).filter())
        .await
        .ok()
        .flatten()
}

/// Extract a single frame at `t_secs` from `mp4_path` as a JPEG.
///
/// `width` is the OUTPUT width; height is auto-computed and rounded to
/// even pixels (mjpeg requires that). Aspect ratio is preserved.
/// `color_filter` is the clip's colour conversion
/// (`ColorConversion::filter`), applied after the scale; `None` for SDR.
pub async fn extract_frame_jpeg(
    mp4_path: &Path,
    t_secs: f32,
    width: u32,
    color_filter: Option<&str>,
) -> Result<Vec<u8>> {
    let path_str = mp4_path.to_str().context("mp4 path is not valid UTF-8")?;
    // -ss before -i = INPUT seek (fast). -frames:v 1 caps at one frame.
    let out = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-ss",
            &format!("{t_secs:.3}"),
            "-i",
            path_str,
            "-frames:v",
            "1",
            "-vf",
            &frame_filter(width, color_filter),
            "-q:v",
            JPEG_QSCALE,
            "-f",
            "image2pipe",
            "-c:v",
            "mjpeg",
            "-",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .context("spawning ffmpeg for frame extraction")?;
    if !out.status.success() {
        anyhow::bail!(
            "ffmpeg failed at t={t_secs:.3}s: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    if out.stdout.is_empty() {
        anyhow::bail!("ffmpeg produced no bytes at t={t_secs:.3}s (likely past end of stream)");
    }
    Ok(out.stdout)
}

/// Extract `timestamps.len()` frames in parallel. Returns a vector of
/// `(t_secs, Result<jpeg_bytes>)` so the caller can surface partial
/// failures (one bad timestamp shouldn't sink the whole call).
async fn extract_frames_parallel(
    mp4_path: &Path,
    timestamps: &[f32],
    width: u32,
) -> Vec<(f32, Result<Vec<u8>>)> {
    use futures_util::future::join_all;

    // One colour plan for the file, shared by every frame.
    let color = color_filter_for(mp4_path).await;
    let path = mp4_path.to_path_buf();
    let futs = timestamps.iter().map(|&t| {
        let path = path.clone();
        let color = color.clone();
        async move {
            let r = extract_frame_jpeg(&path, t, width, color.as_deref()).await;
            (t, r)
        }
    });
    join_all(futs).await
}

// ---------- op-level results ----------

#[derive(Debug, Clone)]
pub struct ExtractedFrame {
    pub t_secs: f32,
    pub jpeg_bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FrameError {
    pub t_secs: f32,
    pub error: String,
}

/// Result of an inspect op. `frames` carries raw bytes (not JSON —
/// callers decide how to package them: inline MCP image content,
/// base64 for a REST response, a temp file for a static preview URL).
/// `summary_json` is the same JSON-serialisable metadata every caller
/// needs (output path, duration, per-frame timestamps/sizes, errors),
/// kept separate so it can be logged or returned as-is without
/// touching the byte payload.
pub struct FrameExtractionResult {
    pub output_path: String,
    pub duration_secs: f32,
    pub frame_width_px: u32,
    pub frames: Vec<ExtractedFrame>,
    pub errors: Vec<FrameError>,
}

impl FrameExtractionResult {
    /// JSON metadata only — no image bytes. Safe to log or return
    /// alongside inline images without duplicating the payload.
    pub fn summary_json(&self) -> serde_json::Value {
        let frame_meta: Vec<serde_json::Value> = self
            .frames
            .iter()
            .map(|f| {
                serde_json::json!({
                    "t_secs": (f.t_secs * 100.0).round() / 100.0,
                    "size_bytes": f.jpeg_bytes.len(),
                })
            })
            .collect();
        serde_json::json!({
            "output_path": self.output_path,
            "duration_secs": (self.duration_secs * 100.0).round() / 100.0,
            "frame_width_px": self.frame_width_px,
            "frames": frame_meta,
            "errors": self.errors,
        })
    }
}

async fn probe_duration(mp4: &Path) -> Result<f32, OpsError> {
    let mp4_camino = camino::Utf8PathBuf::from_path_buf(mp4.to_path_buf())
        .map_err(|_| OpsError::invalid("non-UTF-8 output path".to_string()))?;
    let duration = vibecut::media_probe::probe_video_duration(&mp4_camino) as f32;
    if duration <= 0.0 {
        return Err(OpsError::internal(format!(
            "could not probe duration of {}",
            mp4.display()
        )));
    }
    Ok(duration)
}

/// Extract a curated overview of frames from a rendered mp4: evenly
/// spaced across the timeline, `max_frames` (default 12, capped at
/// [`MAX_FRAMES_PER_CALL`]) of them.
pub async fn inspect_output(
    output_path: &str,
    max_frames: Option<u32>,
) -> Result<FrameExtractionResult, OpsError> {
    let mp4 = resolve_projects(output_path);
    if !mp4.exists() {
        return Err(OpsError::not_found_with(
            "output mp4",
            mp4.to_string_lossy().into_owned(),
        ));
    }
    let duration = probe_duration(&mp4).await?;

    let n = (max_frames.unwrap_or(12).clamp(1, MAX_FRAMES_PER_CALL as u32)) as usize;
    let timestamps = even_timestamps(duration, n);

    let extracted = extract_frames_parallel(&mp4, &timestamps, DEFAULT_JPEG_WIDTH).await;
    Ok(package(&mp4, duration, DEFAULT_JPEG_WIDTH, extracted))
}

/// Extract frames at user-specified timestamps. Up to
/// [`MAX_FRAMES_PER_CALL`] per call; invalid timestamps are silently
/// dropped (out-of-range, NaN, infinite).
pub async fn inspect_at(
    output_path: &str,
    timestamps: &[f32],
) -> Result<FrameExtractionResult, OpsError> {
    let mp4 = resolve_projects(output_path);
    if !mp4.exists() {
        return Err(OpsError::not_found_with(
            "output mp4",
            mp4.to_string_lossy().into_owned(),
        ));
    }
    let duration = probe_duration(&mp4).await?;

    let (clean, _rejected) = clean_timestamps(timestamps, duration);
    if clean.is_empty() {
        return Err(OpsError::invalid(format!(
            "no valid timestamps in {timestamps:?} (output is {duration:.2}s long)"
        )));
    }
    let take = clean.len().min(MAX_FRAMES_PER_CALL);
    let timestamps = &clean[..take];

    let extracted = extract_frames_parallel(&mp4, timestamps, DEFAULT_JPEG_WIDTH).await;
    Ok(package(&mp4, duration, DEFAULT_JPEG_WIDTH, extracted))
}

fn package(
    mp4: &Path,
    duration: f32,
    width: u32,
    extracted: Vec<(f32, Result<Vec<u8>>)>,
) -> FrameExtractionResult {
    let mut frames = Vec::with_capacity(extracted.len());
    let mut errors = Vec::new();
    for (t, res) in extracted {
        match res {
            Ok(jpeg_bytes) => frames.push(ExtractedFrame { t_secs: t, jpeg_bytes }),
            Err(e) => errors.push(FrameError {
                t_secs: (t * 100.0).round() / 100.0,
                error: format!("{e:#}"),
            }),
        }
    }
    FrameExtractionResult {
        output_path: mp4.to_string_lossy().into_owned(),
        duration_secs: duration,
        frame_width_px: width,
        frames,
        errors,
    }
}

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_filter_adds_the_colour_conversion_after_the_scale() {
        assert_eq!(frame_filter(480, None), "scale=480:-2");
        assert_eq!(frame_filter(480, Some("zscale=t=linear,tonemap=tonemap=mobius")), "scale=480:-2,zscale=t=linear,tonemap=tonemap=mobius");
    }

    fn ffmpeg_ok(args: &[&str]) -> bool {
        std::process::Command::new("ffmpeg").args(args).status().is_ok_and(|s| s.success())
    }

    /// Mean saturation of a JPEG (ffmpeg signalstats `SATAVG`).
    fn saturation(jpeg: &[u8], dir: &Path, name: &str) -> f64 {
        let file = dir.join(name);
        std::fs::write(&file, jpeg).unwrap();
        let out = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-f", "lavfi"])
            .arg(format!("movie={},signalstats", file.to_str().unwrap()))
            .args(["-show_entries", "frame_tags=lavfi.signalstats.SATAVG", "-of", "default=nw=1:nk=1"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }

    /// An HLG clip's thumbnail is tone-mapped like normalize does it: its
    /// colour comes out close to the SDR original, not washed out.
    #[tokio::test]
    async fn hdr_thumbnails_are_tone_mapped_like_normalize() {
        let support = vibecut::video_renderer::color_filter_support();
        if !ffmpeg_ok(&["-version"]) || !(support.zscale && support.tonemap) {
            eprintln!("ffmpeg with zscale + tonemap not available; skipping");
            return;
        }
        let d = std::env::temp_dir().join(format!("vibecut_frames_hdr_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let (hlg, sdr) = (d.join("GX010077.MP4"), d.join("sdr.mp4"));
        // SDR testsrc2 placed at HLG reference white (BT.2408), 10-bit HEVC
        // tagged HLG / BT.2020 — what a GoPro "HLG HDR" clip carries.
        let hlg_vf = "format=yuv444p,setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv,\
                      zscale=t=linear:npl=203,format=gbrpf32le,\
                      zscale=p=bt2020:t=arib-std-b67:m=bt2020nc:r=tv:npl=203,format=yuv420p10le";
        let made = ffmpeg_ok(&[
            "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=640x360:r=30:d=1", "-vf", hlg_vf,
            "-c:v", "libx265", "-preset", "ultrafast",
            "-x265-params", "log-level=error:colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc",
            "-color_primaries", "bt2020", "-color_trc", "arib-std-b67", "-colorspace", "bt2020nc",
            "-color_range", "tv", hlg.to_str().unwrap(),
        ]);
        if !made {
            eprintln!("this ffmpeg can't make an HLG test clip (libx265); skipping");
            return;
        }
        assert!(ffmpeg_ok(&[
            "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=640x360:r=30:d=1",
            "-pix_fmt", "yuv420p", "-c:v", "libx264", "-crf", "16", sdr.to_str().unwrap(),
        ]));
        let color = color_filter_for(&hlg).await;
        assert!(color.as_deref().is_some_and(|c| c.contains("tonemap")), "no tone map planned: {color:?}");
        assert_eq!(color_filter_for(&sdr).await, None, "an SDR clip gets no conversion");

        let raw = extract_frame_jpeg(&hlg, 0.5, 480, None).await.unwrap();
        let shown = extract_frames_parallel(&hlg, &[0.5], 480).await.remove(0).1.unwrap();
        let reference = extract_frames_parallel(&sdr, &[0.5], 480).await.remove(0).1.unwrap();
        let (raw, shown, reference) =
            (saturation(&raw, &d, "raw.jpg"), saturation(&shown, &d, "shown.jpg"), saturation(&reference, &d, "ref.jpg"));
        assert!(raw < reference * 0.6, "the unconverted grab should look washed out: {raw} vs {reference}");
        assert!(
            (shown - reference).abs() < reference * 0.25,
            "the thumbnail should look like the SDR original: {shown} vs {reference} (unconverted {raw})"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn even_timestamps_basic() {
        let ts = even_timestamps(100.0, 5);
        assert_eq!(ts.len(), 5);
        // First is near 0, last is near (duration - 0.1).
        assert!(ts[0] < 1.0);
        assert!(ts[4] > 99.0);
        // Strictly increasing.
        for w in ts.windows(2) {
            assert!(w[1] > w[0]);
        }
    }

    #[test]
    fn even_timestamps_edge_cases() {
        assert!(even_timestamps(0.0, 5).is_empty());
        assert!(even_timestamps(10.0, 0).is_empty());

        // Single frame: roughly the midpoint.
        let one = even_timestamps(10.0, 1);
        assert_eq!(one.len(), 1);
        assert!((one[0] - 5.0).abs() < 0.01);
    }

    #[test]
    fn even_timestamps_short_clip() {
        // 0.5 s clip with 3 requested frames: head inset shrinks
        // proportionally, no overlap.
        let ts = even_timestamps(0.5, 3);
        assert_eq!(ts.len(), 3);
        assert!(ts[0] >= 0.0);
        assert!(ts[2] <= 0.5);
        assert!(ts[1] > ts[0] && ts[2] > ts[1]);
    }

    #[test]
    fn clean_timestamps_drops_invalid() {
        let raw = vec![5.0, -1.0, f32::NAN, f32::INFINITY, 102.0, 50.0];
        let (kept, rejected) = clean_timestamps(&raw, 100.0);
        // Kept: 5.0, 50.0 (102 > duration is rejected; -1, NaN, ∞ rejected).
        assert_eq!(kept, vec![5.0, 50.0]);
        assert_eq!(rejected, 4);
    }
}
