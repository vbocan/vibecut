//! Visual analysis: decode subsampled grayscale frames via ffmpeg's stdout pipe
//! and compute per-frame **sharpness** (Laplacian variance), **exposure score**
//! (fraction of non-clipped pixels), and **motion** (frame-difference MSD).
//!
//! All three metrics are produced in a single decode pass, streaming. Memory
//! footprint is two frames at the analysis resolution (default 480×270 grayscale
//! → ~250 KB total).
//!
//! # Why shell out instead of `ffmpeg-next` bindings?
//!
//! Linking `ffmpeg-next` against the system libav* on Windows is famously
//! painful. Piping raw `gray` frames through stdout costs an
//! extra memcpy per frame but keeps the build entirely native.

use anyhow::{Context, Result};
use camino::Utf8Path;
use serde::Serialize;
use std::io::Read;
use std::process::{Command, Stdio};

use crate::video_renderer::ColorConversion;

/// Per-frame visual metrics. Frame `i` corresponds to time `i / sample_rate_hz`
/// since the start of the analyzed video.
#[derive(Debug, Clone, serde::Deserialize, Serialize, Default)]
pub struct VisualAnalysis {
    pub sample_rate_hz: f32,
    pub width: u32,
    pub height: u32,
    pub frame_count: usize,
    /// Laplacian variance per frame. Higher = sharper.
    pub sharpness: Vec<f32>,
    /// Fraction of pixels NOT in clipped bins (`<16` or `>239`). Higher = better exposed.
    pub exposure: Vec<f32>,
    /// Mean-squared difference vs. the previous frame. Higher = more motion / scene change.
    /// First frame is always 0.
    pub motion: Vec<f32>,
}

#[allow(dead_code)] // public time-base helpers — kept on the VisualAnalysis surface
impl VisualAnalysis {
    pub fn time_of(&self, i: usize) -> f32 {
        i as f32 / self.sample_rate_hz
    }
    pub fn duration_secs(&self) -> f32 {
        self.frame_count as f32 / self.sample_rate_hz
    }
}

/// Default settings tuned for 4K source: 2 fps analysis at 480×270.
pub const DEFAULT_FPS: f32 = 2.0;
pub const DEFAULT_W: u32 = 480;
pub const DEFAULT_H: u32 = 270;

/// Cacheable parameter set — included in the cache key so re-analyses with
/// different settings don't collide.
#[derive(Debug, Clone, serde::Serialize)]
struct VisualCacheKey {
    fps: f32,
    w: u32,
    h: u32,
    hwaccel_cuda: bool,
    /// `true` when frames were downscaled on GPU via `scale_cuda` rather than
    /// CPU-side `scale`. Different scalers produce slightly different floats,
    /// so this is part of the key — old caches don't stale-hit when we
    /// switch chains.
    use_scale_cuda: bool,
    /// Bumped when the algorithm changes in a way that invalidates old caches.
    algo_version: u32,
    /// The colour conversion the frames were read through (HDR footage is
    /// tone-mapped like normalize does it); absent for SDR, so SDR keys are
    /// what they always were.
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<String>,
}

const ALGO_VERSION: u32 = 1;

/// Wrap `analyze_visual` with on-disk caching. On cache hit the function
/// returns instantly; on miss it runs the full decode and stores the result.
#[allow(clippy::too_many_arguments)]
pub fn analyze_visual_cached(
    path: &Utf8Path,
    fps: f32,
    w: u32,
    h: u32,
    hwaccel_cuda: bool,
    use_scale_cuda: bool,
    color: &ColorConversion,
    cache: &crate::disk_cache::Cache,
) -> anyhow::Result<VisualAnalysis> {
    let params = VisualCacheKey {
        fps,
        w,
        h,
        hwaccel_cuda,
        use_scale_cuda,
        algo_version: ALGO_VERSION,
        color: color.cache_tag(),
    };
    let key = crate::disk_cache::Cache::key_for_file(path, &params)?;
    if let Some(cached) = cache.load::<VisualAnalysis>(&key, crate::disk_cache::VISUAL) {
        tracing::info!(file = %path.file_name().unwrap_or("?"), "visual cache hit");
        return Ok(cached);
    }
    let started = std::time::Instant::now();
    let result = analyze_visual(path, fps, w, h, hwaccel_cuda, use_scale_cuda, color)?;
    tracing::info!(
        file = %path.file_name().unwrap_or("?"),
        frames = result.frame_count,
        elapsed_s = started.elapsed().as_secs_f32(),
        "visual analysis (uncached)"
    );
    cache.save(&key, crate::disk_cache::VISUAL, &result)?;
    Ok(result)
}

/// Decode `path` to subsampled grayscale frames at `(fps, w, h)` via ffmpeg
/// and compute sharpness / exposure / motion per frame.
///
/// When `hwaccel_cuda = true`, prepends `-hwaccel cuda` to use NVDEC for
/// HEVC/H.264 decoding. When `use_scale_cuda = true` (requires `hwaccel_cuda`
/// as well) the downscale runs on the GPU via `scale_cuda` and only the
/// already-tiny output frame is downloaded to CPU memory — eliminating the
/// per-frame full-resolution PCIe transfer. On HEVC 4K source this typically
/// yields another 2–3× speedup on top of NVDEC alone.
///
/// When the GPU chain fails (typically on older H.264 profiles where NVDEC's
/// `cuvidCreateDecoder` returns `CUDA_ERROR_INVALID_VALUE` and ffmpeg's
/// `auto_scale_0` filter can't bridge the resulting `yuvj420p` software
/// fallback to a `cuda` filter input), we automatically retry the same file
/// with the CPU `scale` chain. This per-file fallback keeps mixed projects
/// working without requiring users to know which clips NVDEC accepts.
///
/// `color` is the clip's colour plan (`plan_color_conversion`, as normalize
/// uses it): HDR footage is tone-mapped to BT.709 before it is measured, so
/// exposure and sharpness are judged on the picture the render shows, not
/// on washed-out HLG / PQ code values. A conversion runs on the CPU chain
/// (it has no `scale_cuda` counterpart); NVDEC decode still applies.
pub fn analyze_visual(
    path: &Utf8Path,
    fps: f32,
    w: u32,
    h: u32,
    hwaccel_cuda: bool,
    use_scale_cuda: bool,
    color: &ColorConversion,
) -> Result<VisualAnalysis> {
    let use_scale_cuda = use_scale_cuda && color.filter().is_none();
    // First attempt: whatever the caller asked for.
    let primary = run_visual_chain(path, fps, w, h, hwaccel_cuda, use_scale_cuda, color);

    // Specifically when the GPU chain failed with a CUDA-init / format-bridge
    // error, retry with the CPU scale chain. We don't blanket-fall-back on
    // every error because legitimate problems (missing file, corrupt MP4)
    // should surface, not be silently downgraded.
    if hwaccel_cuda && use_scale_cuda {
        if let Err(ref e) = primary {
            let msg = format!("{e}");
            let cuda_failure = msg.contains("CUDA_ERROR")
                || msg.contains("cuvidCreateDecoder")
                || msg.contains("Failed setup for format cuda")
                || msg.contains("Impossible to convert between the formats");
            if cuda_failure {
                tracing::warn!(
                    file = %path.file_name().unwrap_or("?"),
                    "GPU scale_cuda chain rejected this source; retrying with CPU scale chain"
                );
                return run_visual_chain(path, fps, w, h, hwaccel_cuda, false, color);
            }
        }
    }
    primary
}

/// The CPU chain's filter: downscale, then (HDR only) the same colour
/// conversion normalize applies, then the frame rate. The conversion runs
/// after the scale so its float tone-map works on the small frame. Note the
/// explicit `format=yuv420p`: it normalises the JPEG-range `yuvj420p` GoPro
/// sometimes emits, so the `pix_fmt gray` output doesn't trip the
/// auto-converter on edge cases. SDR keeps its exact old chain.
fn cpu_chain_filter(w: u32, h: u32, fps: &str, color: &ColorConversion) -> String {
    match color.filter() {
        Some(conv) => format!("scale={w}:{h}:flags=fast_bilinear,{conv},fps={fps}"),
        None => format!("scale={w}:{h}:flags=fast_bilinear,format=yuv420p,fps={fps}"),
    }
}

/// One ffmpeg invocation + per-frame analysis loop. Factored out so
/// `analyze_visual` can call it twice when retrying after a GPU-chain failure.
fn run_visual_chain(
    path: &Utf8Path,
    fps: f32,
    w: u32,
    h: u32,
    hwaccel_cuda: bool,
    use_scale_cuda: bool,
    color: &ColorConversion,
) -> Result<VisualAnalysis> {
    let path_str = path.as_str().to_string();
    let fps_s = format!("{fps}");
    let mut args: Vec<String> = vec!["-v".into(), "error".into()];

    if hwaccel_cuda && use_scale_cuda {
        // GPU-resident chain: decode to CUDA frames, downscale on GPU, then
        // hwdownload only the tiny output. Y plane only via extractplanes.
        args.extend([
            "-hwaccel".into(), "cuda".into(),
            "-hwaccel_output_format".into(), "cuda".into(),
            "-i".into(), path_str.clone(),
            "-vf".into(),
            format!("scale_cuda={w}:{h},hwdownload,format=nv12,extractplanes=y,fps={fps_s}"),
        ]);
    } else {
        // CPU-side scale (+ colour conversion) + fps. Hwaccel decode is
        // optional; when present the decoded frames are auto-downloaded
        // before scale. See `cpu_chain_filter`.
        if hwaccel_cuda {
            args.extend(["-hwaccel".into(), "cuda".into()]);
        }
        args.extend([
            "-i".into(), path_str.clone(),
            "-vf".into(), cpu_chain_filter(w, h, &fps_s, color),
        ]);
    }
    args.extend([
        "-f".into(), "rawvideo".into(),
        "-pix_fmt".into(), "gray".into(),
        "-an".into(),
        "-".into(),
    ]);
    let mut child = Command::new("ffmpeg")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning ffmpeg for visual analysis of {path}"))?;

    let mut stdout = child.stdout.take().expect("stdout configured");
    let frame_size = (w as usize) * (h as usize);

    // Frames are scored on a reader thread while this one waits on ffmpeg,
    // so a cancelled render kills the decode at once instead of whenever
    // the next frame happens to arrive. The kill closes the pipe, which
    // ends the reader.
    let reader = std::thread::spawn(move || -> std::io::Result<(Vec<f32>, Vec<f32>, Vec<f32>)> {
        let mut frame = vec![0u8; frame_size];
        let mut prev = vec![0u8; frame_size];
        let mut have_prev = false;

        let mut sharpness: Vec<f32> = Vec::new();
        let mut exposure: Vec<f32> = Vec::new();
        let mut motion: Vec<f32> = Vec::new();

        loop {
            match stdout.read_exact(&mut frame) {
                Ok(()) => {
                    sharpness.push(laplacian_variance(&frame, w as usize, h as usize));
                    exposure.push(exposure_score(&frame));
                    motion.push(if have_prev {
                        frame_diff_msd(&frame, &prev)
                    } else {
                        0.0
                    });
                    prev.copy_from_slice(&frame);
                    have_prev = true;
                }
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                // Returning drops the pipe, so ffmpeg can't block on it.
                Err(e) => return Err(e),
            }
        }
        Ok((sharpness, exposure, motion))
    });

    let status = crate::cancel::wait_child(&mut child).context("waiting on ffmpeg");
    let frames = reader
        .join()
        .map_err(|_| anyhow::anyhow!("visual-analysis reader panicked"))?;
    let status = status?;
    let (sharpness, exposure, motion) = frames.context("reading raw frames from ffmpeg stdout")?;
    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut e) = child.stderr {
            e.read_to_string(&mut stderr).ok();
        }
        anyhow::bail!(
            "ffmpeg visual decode failed (exit {}): {}",
            status.code().unwrap_or(-1),
            stderr.trim()
        );
    }

    Ok(VisualAnalysis {
        sample_rate_hz: fps,
        width: w,
        height: h,
        frame_count: sharpness.len(),
        sharpness,
        exposure,
        motion,
    })
}

// ---------- per-frame metrics ----------

/// 4-neighbour Laplacian variance: a standard cheap sharpness estimator.
/// Borders are skipped. `frame` is row-major, length = `w * h`.
pub fn laplacian_variance(frame: &[u8], w: usize, h: usize) -> f32 {
    if w < 3 || h < 3 || frame.len() < w * h {
        return 0.0;
    }
    let mut sum = 0.0_f64;
    let mut sum_sq = 0.0_f64;
    let mut count = 0usize;
    for y in 1..h - 1 {
        let row_off = y * w;
        for x in 1..w - 1 {
            let c = frame[row_off + x] as i32;
            let n = frame[row_off - w + x] as i32;
            let s = frame[row_off + w + x] as i32;
            let e = frame[row_off + x + 1] as i32;
            let o = frame[row_off + x - 1] as i32;
            let lap = (4 * c - n - s - e - o) as f64;
            sum += lap;
            sum_sq += lap * lap;
            count += 1;
        }
    }
    if count == 0 {
        return 0.0;
    }
    let mean = sum / count as f64;
    ((sum_sq / count as f64) - mean * mean).max(0.0) as f32
}

/// 1 − fraction of pixels in clipped bins (`<16` or `>239`). 1.0 = ideally exposed.
pub fn exposure_score(frame: &[u8]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let clipped = frame.iter().filter(|&&p| p < 16 || p > 239).count();
    1.0 - (clipped as f32 / frame.len() as f32)
}

/// Mean-squared luminance difference between two equal-size frames.
pub fn frame_diff_msd(curr: &[u8], prev: &[u8]) -> f32 {
    debug_assert_eq!(curr.len(), prev.len());
    if curr.is_empty() {
        return 0.0;
    }
    let mut sum = 0.0_f64;
    for (a, b) in curr.iter().zip(prev) {
        let d = *a as f64 - *b as f64;
        sum += d * d;
    }
    (sum / curr.len() as f64) as f32
}

// ---------- scene changes ----------

/// A frame difference below this (RMS ≈ 17 luma levels) is never a scene
/// change, however calm the frames around it.
pub const SCENE_MIN_MSD: f32 = 300.0;
/// A hard cut: the difference into frame `i` is at least this many times
/// the typical difference around it...
const CUT_RATIO: f32 = 4.0;
/// ...and at least this many times both neighbouring differences (a cut
/// is one jump; a fast pan keeps the difference high frame after frame).
const CUT_ISOLATION: f32 = 2.0;
/// A big change (into a tunnel, under water, lens covered) needs less of
/// a jump when the picture's exposure or sharpness level shifts with it.
const CHANGE_RATIO: f32 = 2.5;
const CHANGE_EXPOSURE_STEP: f32 = 0.25;
const CHANGE_SHARPNESS_STEP: f32 = 3.0;
/// A sharpness shift only counts when the sharper side has real detail
/// (Laplacian variance); near-flat frames swing by large factors on noise.
const CHANGE_SHARPNESS_MIN: f32 = 20.0;
/// Frames either side that make up "the typical difference around it".
const SCENE_NEIGHBOURS: usize = 4;
/// Floor for that typical difference, so near-static footage doesn't
/// turn every flicker into a ratio of 50.
const SCENE_NOISE_FLOOR: f32 = 20.0;

/// What kind of scene change [`detect_scene_changes`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneChangeKind {
    /// An isolated spike in frame difference: an edit, or the camera
    /// switching on into a different scene.
    Cut,
    /// A smaller spike that comes with a lasting shift in exposure or
    /// sharpness.
    Change,
}

/// A scene change between analysed frames `frame - 1` and `frame`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, serde::Deserialize)]
pub struct SceneChange {
    pub frame: usize,
    pub kind: SceneChangeKind,
    /// The frame difference over the typical one around it.
    pub strength: f32,
}

/// Find hard scene cuts and big visual changes in one clip's per-frame
/// metrics (as [`analyze_visual`] returns them: `motion[i]` is the
/// difference between frames `i - 1` and `i`). A frame is a **cut** when
/// its difference is at least [`SCENE_MIN_MSD`], [`CUT_RATIO`] × the
/// median difference of the [`SCENE_NEIGHBOURS`] frames either side and
/// [`CUT_ISOLATION`] × each neighbour's; it is a **change** when the jump
/// is only [`CHANGE_RATIO`] × but exposure moves by
/// [`CHANGE_EXPOSURE_STEP`] or sharpness by a factor of
/// [`CHANGE_SHARPNESS_STEP`] (medians of the three frames before vs the
/// three from it on). Frame 0 (no previous frame) is never a change.
pub fn detect_scene_changes(motion: &[f32], sharpness: &[f32], exposure: &[f32]) -> Vec<SceneChange> {
    fn median(mut v: Vec<f32>) -> Option<f32> {
        if v.is_empty() {
            return None;
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        Some(v[v.len() / 2])
    }
    let level = |metric: &[f32], lo: usize, hi: usize| median(metric.get(lo..hi.min(metric.len()))?.to_vec());
    let n = motion.len();
    let mut out = Vec::new();
    for i in 1..n {
        let m = motion[i];
        if !(m >= SCENE_MIN_MSD) {
            continue;
        }
        let around: Vec<f32> = (i.saturating_sub(SCENE_NEIGHBOURS).max(1)..(i + SCENE_NEIGHBOURS + 1).min(n))
            .filter(|&j| j != i)
            .map(|j| motion[j])
            .collect();
        let typical = median(around).unwrap_or(0.0).max(SCENE_NOISE_FLOOR);
        let ratio = m / typical;
        let neighbour = [i.checked_sub(1).filter(|&j| j >= 1), Some(i + 1).filter(|&j| j < n)]
            .into_iter()
            .flatten()
            .map(|j| motion[j])
            .fold(0.0_f32, f32::max);
        if ratio >= CUT_RATIO && m >= CUT_ISOLATION * neighbour {
            out.push(SceneChange { frame: i, kind: SceneChangeKind::Cut, strength: ratio });
            continue;
        }
        if ratio < CHANGE_RATIO {
            continue;
        }
        let before = i.saturating_sub(3);
        let exposure_step = match (level(exposure, before, i), level(exposure, i, i + 3)) {
            (Some(a), Some(b)) => (b - a).abs() >= CHANGE_EXPOSURE_STEP,
            _ => false,
        };
        let sharpness_step = match (level(sharpness, before, i), level(sharpness, i, i + 3)) {
            (Some(a), Some(b)) if a > 0.0 && b > 0.0 && a.max(b) >= CHANGE_SHARPNESS_MIN => {
                (a / b).max(b / a) >= CHANGE_SHARPNESS_STEP
            }
            _ => false,
        };
        if exposure_step || sharpness_step {
            out.push(SceneChange { frame: i, kind: SceneChangeKind::Change, strength: ratio });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker(w: usize, h: usize, block: usize) -> Vec<u8> {
        let mut v = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let bx = x / block;
                let by = y / block;
                v[y * w + x] = if (bx + by) % 2 == 0 { 220 } else { 30 };
            }
        }
        v
    }

    fn flat(w: usize, h: usize, val: u8) -> Vec<u8> {
        vec![val; w * h]
    }

    #[test]
    fn flat_image_has_zero_sharpness() {
        let f = flat(64, 64, 128);
        assert!(laplacian_variance(&f, 64, 64) < 1e-3);
    }

    #[test]
    fn checker_is_sharper_than_flat() {
        let f1 = flat(64, 64, 128);
        let f2 = checker(64, 64, 4);
        assert!(laplacian_variance(&f2, 64, 64) > laplacian_variance(&f1, 64, 64) + 100.0);
    }

    #[test]
    fn fully_clipped_image_scores_near_zero_exposure() {
        let f = flat(32, 32, 250); // hot
        let s = exposure_score(&f);
        assert!(s < 0.01, "got {s}");
    }

    #[test]
    fn well_exposed_midtone_scores_high() {
        let f = flat(32, 32, 128);
        assert!(exposure_score(&f) > 0.99);
    }

    #[test]
    fn motion_zero_for_identical_frames() {
        let f = flat(32, 32, 100);
        assert!(frame_diff_msd(&f, &f) < 1e-9);
    }

    #[test]
    fn hdr_frames_are_tone_mapped_before_they_are_measured() {
        use crate::video_renderer::ColorIn;
        let sdr = cpu_chain_filter(480, 270, "2", &ColorConversion::None);
        assert_eq!(sdr, "scale=480:270:flags=fast_bilinear,format=yuv420p,fps=2");
        assert_eq!(cpu_chain_filter(480, 270, "2", &ColorConversion::PixelFormat), sdr);
        let hlg = ColorConversion::ToneMap(ColorIn {
            transfer: "arib-std-b67",
            primaries: "bt2020",
            matrix: "bt2020nc",
            range: "tv",
        });
        let f = cpu_chain_filter(480, 270, "2", &hlg);
        assert!(f.starts_with("scale=480:270:flags=fast_bilinear,zscale=tin=arib-std-b67"), "{f}");
        assert!(f.contains("tonemap=tonemap=mobius") && f.ends_with(",format=yuv420p,fps=2"), "{f}");
        // The cache key changes only for converted clips.
        let key = |c: &ColorConversion| {
            serde_json::to_string(&VisualCacheKey {
                fps: 2.0,
                w: 480,
                h: 270,
                hwaccel_cuda: false,
                use_scale_cuda: false,
                algo_version: ALGO_VERSION,
                color: c.cache_tag(),
            })
            .unwrap()
        };
        assert!(!key(&ColorConversion::None).contains("color"));
        assert_eq!(key(&ColorConversion::None), key(&ColorConversion::PixelFormat));
        assert_ne!(key(&ColorConversion::None), key(&hlg));
    }

    /// The tone-mapped chain runs on a real HLG clip (ffmpeg accepts the
    /// graph into `gray` rawvideo) and changes what is measured.
    #[test]
    fn an_hlg_clip_is_analysed_through_its_tone_map() {
        let ok = |args: &[&str]| Command::new("ffmpeg").args(args).status().is_ok_and(|s| s.success());
        let support = crate::video_renderer::color_filter_support();
        if !ok(&["-version"]) || !(support.zscale && support.tonemap) {
            eprintln!("ffmpeg with zscale + tonemap not available; skipping");
            return;
        }
        let d = std::env::temp_dir().join(format!("vibecut_visual_hdr_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let clip = camino::Utf8PathBuf::from_path_buf(d.join("hlg.mp4")).unwrap();
        let hlg_vf = "format=yuv444p,setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv,                      zscale=t=linear:npl=203,format=gbrpf32le,                      zscale=p=bt2020:t=arib-std-b67:m=bt2020nc:r=tv:npl=203,format=yuv420p10le";
        if !ok(&[
            "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=640x360:r=30:d=2", "-vf", hlg_vf,
            "-c:v", "libx265", "-preset", "ultrafast",
            "-x265-params", "log-level=error:colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc",
            "-color_primaries", "bt2020", "-color_trc", "arib-std-b67", "-colorspace", "bt2020nc",
            "-color_range", "tv", clip.as_str(),
        ]) {
            eprintln!("this ffmpeg can't make an HLG test clip (libx265); skipping");
            return;
        }
        let plan = crate::video_renderer::frame_color_conversion(&clip);
        assert!(matches!(plan, ColorConversion::ToneMap(_)), "{plan:?}");
        let mapped = analyze_visual(&clip, 2.0, 160, 90, false, false, &plan).unwrap();
        let raw = analyze_visual(&clip, 2.0, 160, 90, false, false, &ColorConversion::None).unwrap();
        assert!(mapped.frame_count >= 3, "{} frames", mapped.frame_count);
        assert_eq!(mapped.frame_count, raw.frame_count);
        assert_ne!(mapped.exposure, raw.exposure, "the tone map didn't change the measured picture");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Steady handheld-ish motion around 400 with a little wobble.
    fn steady(n: usize) -> Vec<f32> {
        let mut m: Vec<f32> = (0..n).map(|i| 400.0 + 60.0 * ((i as f32) * 0.9).sin()).collect();
        m[0] = 0.0;
        m
    }

    #[test]
    fn an_isolated_frame_difference_spike_is_a_hard_cut() {
        let mut m = steady(40);
        m[20] = 3200.0;
        let flat = vec![500.0; 40];
        let exp = vec![0.98; 40];
        let found = detect_scene_changes(&m, &flat, &exp);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].frame, 20);
        assert_eq!(found[0].kind, SceneChangeKind::Cut);
        assert!(found[0].strength > 6.0);
    }

    #[test]
    fn a_fast_pan_is_not_a_cut() {
        // Several frames in a row of big differences: the camera whipping
        // round, not an edit.
        let mut m = steady(40);
        for v in &mut m[15..21] {
            *v = 2600.0;
        }
        assert!(detect_scene_changes(&m, &vec![500.0; 40], &vec![0.98; 40]).is_empty());
        // Nor is a jump that is big only relative to near-static footage.
        let mut calm = vec![6.0_f32; 40];
        calm[0] = 0.0;
        calm[12] = 180.0;
        assert!(detect_scene_changes(&calm, &vec![500.0; 40], &vec![0.98; 40]).is_empty());
    }

    #[test]
    fn a_smaller_jump_counts_when_exposure_or_sharpness_shifts_with_it() {
        let mut m = steady(40);
        m[20] = 1300.0; // ~3× the typical difference, next to normal frames
        let sharp = vec![500.0; 40];
        let even = vec![0.98; 40];
        // On its own: not enough for a cut.
        assert!(detect_scene_changes(&m, &sharp, &even).is_empty());
        // Into a tunnel: exposure drops and stays down.
        let mut dark = even.clone();
        for v in &mut dark[20..] {
            *v = 0.55;
        }
        let found = detect_scene_changes(&m, &sharp, &dark);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].frame, found[0].kind), (20, SceneChangeKind::Change));
        // Under water: the picture goes soft and stays soft.
        let mut soft = sharp.clone();
        for v in &mut soft[20..] {
            *v = 90.0;
        }
        assert_eq!(detect_scene_changes(&m, &soft, &even).len(), 1);
        // Near-flat frames whose sharpness swings on noise don't count.
        let mut noise = vec![1.5_f32; 40];
        for v in &mut noise[20..] {
            *v = 0.3;
        }
        assert!(detect_scene_changes(&m, &noise, &even).is_empty());
    }

    /// The demo's "Coastline Bike Ride" GX010302 as analysed at 2 fps: a
    /// smooth synthetic sweep with no edits, whose first difference is
    /// ~2.5× its neighbours. No scene change, so its picks don't move.
    #[test]
    fn the_demo_footage_has_no_scene_changes() {
        let motion = [
            0.0, 546.2405, 266.1527, 164.87752, 173.57207, 306.61624, 616.93225, 741.07715, 746.6933,
            676.90656, 345.12677, 187.1774, 156.95885, 236.12846, 483.5955, 736.9893, 754.126,
            737.3765, 458.21335, 225.20586, 158.02612, 190.30605, 365.56223, 687.4366, 747.78625,
            744.253, 593.7099, 295.49454, 168.94331, 166.36998, 279.9097, 570.7323, 738.2128,
            755.15027, 702.1839, 379.93762, 198.07216, 157.34018, 218.0974, 433.89117,
        ];
        let sharpness = [
            1.4579391, 1.4394475, 0.97802556, 0.5669066, 0.9895787, 1.4724599, 1.6355298, 1.0943139,
            1.2102588, 1.4257396, 1.1024011, 0.64827794, 0.7781334, 1.3282723, 1.648668, 1.2243642,
            0.9907333, 1.5044022, 1.3498952, 0.82290953, 0.6713451, 1.1613142, 1.5891461, 1.5011001,
            0.875078, 1.3978169, 1.4701414, 1.0101558, 0.5100231, 0.9209392, 1.439674, 1.6813916,
            1.0452048, 1.1186608, 1.487135, 1.2194935, 0.7499609, 0.75424653, 1.3037064, 1.6743503,
        ];
        let exposure = [1.0_f32; 40];
        assert!(detect_scene_changes(&motion, &sharpness, &exposure).is_empty());
        assert!(detect_scene_changes(&[], &[], &[]).is_empty());
        assert!(detect_scene_changes(&[0.0], &[1.0], &[1.0]).is_empty());
    }

    #[test]
    fn motion_positive_for_different_frames() {
        let a = flat(32, 32, 100);
        let b = flat(32, 32, 110);
        let m = frame_diff_msd(&a, &b);
        assert!((m - 100.0).abs() < 1e-3, "got {m}");
    }
}
