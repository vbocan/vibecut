//! ffmpeg orchestration for the staged render pipeline.
//!
//! Each stage runs ffmpeg once and produces a checkpoint file in the
//! workspace, so failures are localized and the pipeline is resumable.
//! ffmpeg is reached through the [`FfmpegRunner`] trait — [`NativeRunner`]
//! shells out to the system binary; a future `DockerRunner` could ship
//! without disturbing callers.
//!
//! # Render stages (in pipeline order)
//!
//! 1. **Hardware probe** ([`HwSupport::detect`] → [`HwConfig`]) — runs once,
//!    reads `ffmpeg -hwaccels` / `-encoders` / `-decoders` and resolves an
//!    [`EncoderPref`] against detected support. Drives NVENC / NVDEC vs.
//!    libx264 / libx265 selection with transparent fallback.
//!
//! 2. **Stabilize-detect** ([`stabilize_detect`]) *(optional)* — for each
//!    selected segment, runs vidstab pass-1 with `-f null -` to write a
//!    `.trf` transforms file the next stage consumes.
//!
//! 3. **Normalize** ([`normalize_segment`]) — extracts one segment from its
//!    chapter file, conforms to project res / fps / pix_fmt, optionally
//!    applies vidstabtransform + LUT3D + intro/outro [`OverlayDirective`]s.
//!    Encodes via the resolved encoder. Output is mp4 / h264 / aac /
//!    yuv420p so the concat stage can stream-copy.
//!
//! 4. **Card rendering** ([`render_text_card`]) *(optional)* — generates a
//!    standalone clip with solid-color background + drawtext layers + silent
//!    audio. Used for `[intro] mode = "card"` / `[outro] mode = "card"`.
//!    For `mode = "overlay"`, the text is instead baked into stage (3)'s
//!    first / last segment via `OverlayDirective`.
//!
//! 5. **Concat** ([`concat_segments`] hard-cuts, or [`concat_with_xfade`]
//!    with transitions) — combines intro + segments + outro into one body
//!    file. The xfade variant runs video-xfade + audio-acrossfade in one
//!    ffmpeg pass and applies optional video fade-in / fade-out.
//!
//! 6. **Mux soundtrack** — two flavors:
//!    - [`mux_soundtrack`] — straight replacement of body audio with
//!      soundtrack audio (`-c:v copy -c:a aac -shortest`).
//!    - [`mux_with_ducking`] — full mixer with timeline-controlled volume
//!      curves. Mixes source + soundtrack with per-window trapezoidal ducks,
//!      smart-loop seam crossfades, and tail fade-out. The most complex
//!      filter graph in the project.
//!
//! All filter expressions use forward-slash paths and quoted strings so they
//! work reliably on Windows.

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use std::process::Command;

// ---- Audio parameters ----
/// Shared sample rate (Hz) for all synthesised and normalised audio streams.
/// Every ffmpeg `anullsrc`, `-ar`, and `aresample` uses this value so the
/// audio chain never needs a resample at the merge stage.
const AUDIO_SAMPLE_RATE: u32 = 48_000;

// ---- Ken Burns / cinematic motion constants ----
/// zoompan zoom-increment per frame for slow deep-zoom effects (ZoomIn/ZoomOut).
const ZOOM_STEP_FAST: f32 = 0.0015;
/// zoompan zoom-increment per frame for slower, subtler diagonal motions.
const ZOOM_STEP_SLOW: f32 = 0.001;
/// Maximum zoom level for ZoomIn / ZoomOut (1.5× = 50 % crop).
const ZOOM_MAX_DEEP: f32 = 1.5;
/// Maximum zoom level for Diagonal (1.3× = 30 % crop).
const ZOOM_MAX_MED: f32 = 1.3;
/// Fixed zoom level for PanLeft / PanRight (1.1× gives pixel headroom for the
/// horizontal sweep while keeping the crop minimal).
const ZOOM_PAN: f32 = 1.1;
/// Clockwise rotation speed in radians per second for the RotateZoom effect.
const ROTATE_SPEED_RAD_PER_SEC: f32 = 0.008;

pub trait FfmpegRunner: Send + Sync {
    /// Run ffmpeg with the given arguments. Stderr is captured and surfaced on failure.
    fn run(&self, args: &[&str]) -> Result<()>;

    /// Return the first line of `ffmpeg -version`. Used to confirm availability.
    fn version(&self) -> Result<String>;
}

#[derive(Debug, Clone)]
pub struct NativeRunner {
    binary: String,
}

impl Default for NativeRunner {
    fn default() -> Self {
        Self {
            binary: "ffmpeg".into(),
        }
    }
}

impl NativeRunner {
    pub fn new() -> Self {
        Self::default()
    }
    #[allow(dead_code)] // hook for tests / non-default ffmpeg binary paths
    pub fn with_binary(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
        }
    }
}

impl FfmpegRunner for NativeRunner {
    fn run(&self, args: &[&str]) -> Result<()> {
        tracing::debug!(binary = %self.binary, ?args, "ffmpeg invoke");
        let output = Command::new(&self.binary)
            .args(args)
            .output()
            .with_context(|| format!("spawning {} (is it on PATH?)", self.binary))?;
        // Surface non-empty stderr at warn level even on success — ffmpeg
        // prints warnings (e.g. "input X ended before xfade range") with a
        // 0 exit code that would otherwise be invisible.
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            for line in stderr.lines() {
                tracing::warn!(line = %line, "ffmpeg");
            }
        }
        if !output.status.success() {
            // ffmpeg's last-line is usually the most actionable error.
            let tail: Vec<&str> = stderr.lines().rev().take(8).collect();
            let mut tail_msg = tail.into_iter().rev().collect::<Vec<_>>().join("\n  ");
            if tail_msg.is_empty() {
                tail_msg = "<no stderr>".into();
            }
            anyhow::bail!(
                "ffmpeg failed (exit {}):\n  {tail_msg}",
                output.status.code().unwrap_or(-1)
            );
        }
        Ok(())
    }

    fn version(&self) -> Result<String> {
        let output = Command::new(&self.binary)
            .arg("-version")
            .output()
            .with_context(|| format!("spawning {} (is it on PATH?)", self.binary))?;
        if !output.status.success() {
            anyhow::bail!("{} -version failed", self.binary);
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .to_string())
    }
}

// ---------- Hardware-acceleration detection & config ----------

/// What hardware-accelerated codecs ffmpeg reports at startup. Computed once,
/// passed to every render function so each invocation can decide independently.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct HwSupport {
    pub cuda_hwaccel: bool,
    pub h264_nvenc: bool,
    pub hevc_nvenc: bool,
    pub hevc_cuvid: bool,
    pub h264_cuvid: bool,
    pub amf: bool,
    pub h264_amf: bool,
    pub hevc_amf: bool,
    pub qsv: bool,
    pub h264_qsv: bool,
    pub hevc_qsv: bool,
    /// `scale_cuda` GPU-side downscale filter, used by visual analysis to keep
    /// big 4K decoded frames on the GPU and only transfer the downscaled
    /// 480×270 output to CPU memory. Eliminates the 24 MB-per-frame PCIe
    /// memcpy that the CPU `scale` filter forces.
    pub scale_cuda: bool,
}

impl HwSupport {
    /// Run `ffmpeg -hwaccels` and `ffmpeg -encoders` / `-decoders` once and
    /// parse the output to populate this struct. When any hardware encoder is
    /// listed, a 1-frame smoke-test encode is run to confirm the underlying
    /// driver DLL is actually loadable. Slow path is ~80–200 ms per encoder
    /// probe; callers should cache the result.
    pub fn detect(binary: &str) -> Result<Self> {
        let hwaccels = capture(binary, &["-v", "error", "-hwaccels"])?;
        let encoders = capture(binary, &["-v", "error", "-encoders"])?;
        let decoders = capture(binary, &["-v", "error", "-decoders"])?;
        let filters = capture(binary, &["-v", "error", "-filters"])?;
        let hw = hwaccels.to_lowercase();
        let enc = encoders.to_lowercase();
        let dec = decoders.to_lowercase();
        let filt = filters.to_lowercase();

        // Whether an encoder appears in the ffmpeg `-encoders` list is not
        // sufficient: on laptops / RDP sessions / virtualised hosts the
        // vendor DLL may fail to load at first use (nvcuda.dll, amfrt64.dll,
        // libmfx, …). Run a 1-frame smoke test for each listed HW encoder;
        // if it fails, treat that whole vendor stack as unavailable.
        let cuda_works = !enc.contains(" h264_nvenc ") || probe_encoder(binary, "h264_nvenc");
        let amf_works  = !enc.contains(" h264_amf ")   || probe_encoder(binary, "h264_amf");
        let qsv_works  = !enc.contains(" h264_qsv ")   || probe_encoder(binary, "h264_qsv");

        Ok(HwSupport {
            cuda_hwaccel: cuda_works && hw.lines().any(|l| l.trim() == "cuda"),
            h264_nvenc:   cuda_works && enc.contains(" h264_nvenc "),
            hevc_nvenc:   cuda_works && enc.contains(" hevc_nvenc "),
            hevc_cuvid:   cuda_works && dec.contains(" hevc_cuvid "),
            h264_cuvid:   cuda_works && dec.contains(" h264_cuvid "),
            amf:          amf_works  && hw.lines().any(|l| l.trim() == "amf"),
            h264_amf:     amf_works  && enc.contains(" h264_amf "),
            hevc_amf:     amf_works  && enc.contains(" hevc_amf "),
            qsv:          qsv_works  && hw.lines().any(|l| l.trim() == "qsv"),
            h264_qsv:     qsv_works  && enc.contains(" h264_qsv "),
            hevc_qsv:     qsv_works  && enc.contains(" hevc_qsv "),
            scale_cuda:   cuda_works && filt.lines().any(|l| l.contains(" scale_cuda ")),
        })
    }

    pub fn summary(&self) -> String {
        let mut bits = Vec::new();
        if self.cuda_hwaccel {
            bits.push("CUDA decode".to_string());
        }
        if self.h264_nvenc || self.hevc_nvenc {
            bits.push("NVENC".to_string());
        }
        if self.amf {
            bits.push("AMF".to_string());
        }
        if self.qsv {
            bits.push("QSV".to_string());
        }
        if bits.is_empty() {
            "none (software only)".into()
        } else {
            bits.join(" + ")
        }
    }
}

fn capture(binary: &str, args: &[&str]) -> Result<String> {
    let out = std::process::Command::new(binary)
        .args(args)
        .output()
        .with_context(|| format!("spawning {binary}"))?;
    if !out.status.success() {
        anyhow::bail!(
            "{binary} {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Verify that a hardware video encoder actually works by encoding a single
/// 1×1 black frame with it.
///
/// ffmpeg's `-encoders` list reflects what was compiled in, not what the host
/// OS can load at runtime. On laptops / RDP sessions / VMs the vendor DLL
/// (nvcuda.dll, amfrt64.dll, libmfx, …) may be absent or inaccessible. This
/// smoke-test catches that before the pipeline commits to a broken encoder.
///
/// Returns `true` if the encoder is genuinely usable, `false` otherwise.
fn probe_encoder(binary: &str, encoder: &str) -> bool {
    let result = std::process::Command::new(binary)
        .args([
            "-v", "error",
            "-f", "lavfi",
            "-i", "color=black:s=1x1:r=1",
            "-frames:v", "1",
            "-c:v", encoder,
            "-f", "null",
            "-",
        ])
        .output();
    match result {
        Ok(o) => {
            if !o.status.success() {
                tracing::debug!(encoder, exit = %o.status, "HW encoder smoke-test failed; will skip");
                return false;
            }
            // ffmpeg can exit 0 but still print driver-load errors to stderr
            // on some configurations — treat those as failure too.
            let stderr = String::from_utf8_lossy(&o.stderr);
            let hw_error = stderr.contains("nvcuda")
                || stderr.contains("No device available")
                || stderr.contains("Cannot load")
                || stderr.contains("failed to open")   // AMF: "DLL amfrt64.dll failed to open"
                || stderr.contains("Failed to create"); // AMF: "Failed to create hardware device"
            if hw_error {
                tracing::debug!(encoder, "HW encoder smoke-test stderr indicates driver failure; will skip");
            }
            !hw_error
        }
        Err(e) => {
            tracing::debug!(encoder, error = %e, "HW encoder smoke-test could not run; will skip");
            false
        }
    }
}

/// User's encoder preference, before resolving against hardware support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // `HevcNvenc` is reachable from custom configs but not from the current schema's `Encoder` enum
pub enum EncoderPref {
    Auto,
    Libx264,
    Libx265,
    Nvenc,    // h264_nvenc
    HevcNvenc,
    Amf,      // h264_amf
    Qsv,      // h264_qsv
}

/// Resolved encoder choice — what the renderer actually emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderChoice {
    Libx264,
    Libx265,
    H264Nvenc,
    HevcNvenc,
    H264Amf,
    H264Qsv,
}

impl EncoderChoice {
    pub fn name(self) -> &'static str {
        match self {
            EncoderChoice::Libx264 => "libx264",
            EncoderChoice::Libx265 => "libx265",
            EncoderChoice::H264Nvenc => "h264_nvenc",
            EncoderChoice::HevcNvenc => "hevc_nvenc",
            EncoderChoice::H264Amf => "h264_amf",
            EncoderChoice::H264Qsv => "h264_qsv",
        }
    }
    pub fn is_hw(self) -> bool {
        !matches!(self, EncoderChoice::Libx264 | EncoderChoice::Libx265)
    }
}

/// Bundle of detected support + resolved encoder, plus a flag for hwaccel decode.
#[derive(Debug, Clone)]
pub struct HwConfig {
    pub support: HwSupport,
    pub encoder: EncoderChoice,
    /// Whether to add `-hwaccel cuda` to decode-side invocations on HEVC/H.264 input.
    pub use_cuda_decode: bool,
}

impl HwConfig {
    pub fn resolve(support: HwSupport, pref: EncoderPref) -> Self {
        let encoder = match pref {
            EncoderPref::Auto => {
                if support.h264_nvenc {
                    EncoderChoice::H264Nvenc
                } else if support.h264_amf {
                    EncoderChoice::H264Amf
                } else if support.h264_qsv {
                    EncoderChoice::H264Qsv
                } else {
                    EncoderChoice::Libx264
                }
            }
            EncoderPref::Nvenc => {
                if support.h264_nvenc {
                    EncoderChoice::H264Nvenc
                } else {
                    tracing::warn!("NVENC requested but unavailable; falling back to libx264");
                    EncoderChoice::Libx264
                }
            }
            EncoderPref::HevcNvenc => {
                if support.hevc_nvenc {
                    EncoderChoice::HevcNvenc
                } else {
                    EncoderChoice::Libx264
                }
            }
            EncoderPref::Amf => {
                if support.h264_amf {
                    EncoderChoice::H264Amf
                } else {
                    EncoderChoice::Libx264
                }
            }
            EncoderPref::Qsv => {
                if support.h264_qsv {
                    EncoderChoice::H264Qsv
                } else {
                    EncoderChoice::Libx264
                }
            }
            EncoderPref::Libx264 => EncoderChoice::Libx264,
            EncoderPref::Libx265 => EncoderChoice::Libx265,
        };
        // Pair NVDEC decode with NVENC encode (and only with NVENC) — otherwise
        // ffmpeg has to download GPU frames to host memory before the (CPU) encoder,
        // which is still a win for analysis but adds memcpy overhead for renders.
        let use_cuda_decode = support.cuda_hwaccel && encoder.is_hw();
        Self {
            support,
            encoder,
            use_cuda_decode,
        }
    }

    /// Args to insert *before* `-i input` on the ffmpeg command line for
    /// hardware-accelerated decode. Empty when running fully software.
    pub fn decode_prefix(&self) -> Vec<&str> {
        if self.use_cuda_decode {
            vec!["-hwaccel", "cuda"]
        } else {
            vec![]
        }
    }

    /// Encoder args (codec + preset + quality control). `crf` is mapped to `-cq`
    /// for NVENC, `-crf` for libx264/5, `-quality` for AMF/QSV.
    pub fn encode_args(&self, crf: u32) -> Vec<String> {
        let crf_s = crf.to_string();
        match self.encoder {
            EncoderChoice::Libx264 => vec![
                arg("-c:v"), arg("libx264"),
                arg("-preset"), arg("veryfast"),
                arg("-crf"), crf_s,
            ],
            EncoderChoice::Libx265 => vec![
                arg("-c:v"), arg("libx265"),
                arg("-preset"), arg("medium"),
                arg("-crf"), crf_s,
            ],
            EncoderChoice::H264Nvenc => vec![
                arg("-c:v"), arg("h264_nvenc"),
                // p1 fastest .. p7 slowest. p5 = balanced quality / speed.
                arg("-preset"), arg("p5"),
                arg("-tune"), arg("hq"),
                arg("-rc"), arg("vbr"),
                arg("-cq"), crf_s,
                arg("-b:v"), arg("0"), // pure CQ mode (no bitrate cap)
            ],
            EncoderChoice::HevcNvenc => vec![
                arg("-c:v"), arg("hevc_nvenc"),
                arg("-preset"), arg("p5"),
                arg("-tune"), arg("hq"),
                arg("-rc"), arg("vbr"),
                arg("-cq"), crf_s,
                arg("-b:v"), arg("0"),
            ],
            EncoderChoice::H264Amf => vec![
                arg("-c:v"), arg("h264_amf"),
                arg("-quality"), arg("balanced"),
                arg("-rc"), arg("cqp"),
                arg("-qp_i"), crf_s.clone(), arg("-qp_p"), crf_s.clone(), arg("-qp_b"), crf_s,
            ],
            EncoderChoice::H264Qsv => vec![
                arg("-c:v"), arg("h264_qsv"),
                arg("-preset"), arg("medium"),
                arg("-global_quality"), crf_s,
            ],
        }
    }
}

/// Convert a `&str` to `String`. Named `arg` because it is used exclusively
/// to build ffmpeg argument lists where every element must be an owned `String`.
fn arg(x: &str) -> String {
    x.to_string()
}

/// Append image-overlay compositing stages to a filter-graph string `fc`.
///
/// Each overlay directive in `overlays` is scaled, optionally alpha-faded, and
/// composited onto the running video stream. The input stream entering this
/// section must be labelled `[base]`; the final output stream is labelled `[v]`.
///
/// `first_input_offset` is the ffmpeg input index of the first image overlay
/// (varies by call site because the number of preceding inputs differs between
/// `normalize_image_segment` and `normalize_segment`).
///
/// `clip_duration_secs` clamps the overlay's visibility window so it never
/// extends past the clip's end.
fn append_image_overlay_chain(
    fc: &mut String,
    overlays: &[ImageOverlayDirective<'_>],
    first_input_offset: usize,
    clip_duration_secs: f32,
) {
    if overlays.is_empty() {
        fc.push_str(";[base]null[v]");
        return;
    }
    let mut prev_label = "base".to_string();
    let last = overlays.len() - 1;
    for (k, d) in overlays.iter().enumerate() {
        let im = d.overlay;
        let appear = d.start_secs.max(0.0);
        let disappear = (d.start_secs + d.duration_secs).min(clip_duration_secs);
        let img_in = format!("[{}:v]", k + first_input_offset);
        let prep = format!("[i{k}p]");
        fc.push(';');
        fc.push_str(&format!(
            "{img_in}scale=-1:{h}:flags=lanczos,format=rgba",
            h = im.height,
        ));
        if (im.opacity - 1.0).abs() > 1e-3 {
            fc.push_str(&format!(
                ",colorchannelmixer=aa={:.4}",
                im.opacity.clamp(0.0, 1.0)
            ));
        }
        if im.fade_in_secs > 0.0 {
            fc.push_str(&format!(
                ",fade=t=in:st={st:.4}:d={d:.4}:alpha=1",
                st = appear,
                d = im.fade_in_secs,
            ));
        }
        if im.fade_out_secs > 0.0 {
            let st = (disappear - im.fade_out_secs).max(0.0);
            fc.push_str(&format!(
                ",fade=t=out:st={st:.4}:d={d:.4}:alpha=1",
                st = st,
                d = im.fade_out_secs,
            ));
        }
        fc.push_str(&prep);

        let out_label = if k == last { "v".to_string() } else { format!("ic{k}") };
        let (x, y) = image_overlay_xy(&im.position, im.x_offset, im.y_offset);
        fc.push(';');
        fc.push_str(&format!(
            "[{prev}]{prep}overlay=x={x}:y={y}:format=auto:enable='between(t,{a:.4},{b:.4})'[{out}]",
            prev = prev_label,
            prep = prep,
            a = appear,
            b = disappear,
            out = out_label,
        ));
        prev_label = out_label;
    }
}

/// Standard letterbox (preserve-aspect-ratio) filter fragment.
///
/// Scales the source to fit within `w × h` using Lanczos resampling, then
/// pads to fill the target rectangle exactly. The result is always `w × h`
/// regardless of the source aspect ratio. Used in normalize, stabilize-detect,
/// transcode, and the Ken Burns `Static` effect.
fn letterbox_filter(w: u32, h: u32) -> String {
    format!(
        "scale={w}:{h}:flags=lanczos:force_original_aspect_ratio=decrease,\
         pad={w}:{h}:(ow-iw)/2:(oh-ih)/2"
    )
}

// ---------- Stage 1: normalize ----------

#[derive(Debug, Clone)]
pub struct OverlayDirective<'a> {
    pub overlay: &'a TextOverlay,
    /// When the text appears, measured from the segment's start (t=0 is the
    /// start of this segment in the output). For an intro overlay use 0.0;
    /// for an outro overlay use `segment_duration - card_duration`.
    pub start_secs: f32,
    /// How long the text stays on screen (inclusive of its own fades).
    pub duration_secs: f32,
}

/// Image overlay baked into a segment in overlay-mode intro/outro (the
/// "logo-on-footage" pattern). Same shape as [`OverlayDirective`] but for
/// pixel content rather than rasterised text.
#[derive(Debug, Clone)]
pub struct ImageOverlayDirective<'a> {
    pub overlay: &'a ImageOverlay,
    pub start_secs: f32,
    pub duration_secs: f32,
}

/// Outgoing speed-ramp ("whip-up") spec: the last `duration_secs` of the
/// segment plays at `factor`× speed before the next transition. The output
/// clip's total duration is preserved at `NormalizeArgs::duration_secs` —
/// the renderer extends the input window by `duration_secs * (1 - 1/factor)`
/// so the sped-up tail consumes the same wall-clock time as before.
#[derive(Debug, Clone, Copy)]
pub struct SpeedRamp {
    pub duration_secs: f32,
    pub factor: f32,
    /// GoPro-Quik-style colour treatment baked into the sped-up tail: a
    /// subtle brightness warm-up across the whole ramp + a fade-to-white
    /// over the last few frames. The white flash effectively becomes the
    /// transition into the next clip, so the seam reads as a kinetic punch
    /// rather than a hard cut. `false` keeps the speed-up neutral.
    pub flash: bool,
}

#[derive(Debug, Clone)]
pub struct NormalizeArgs<'a> {
    pub input: &'a Utf8Path,
    pub output: &'a Utf8Path,
    pub start_secs: f64,
    pub duration_secs: f64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub crf: u32,
    /// Optional 3D LUT (.cube) applied before final scale.
    pub lut3d: Option<&'a Utf8Path>,
    /// Optional stabilization transforms file produced by
    /// `stabilize_detect`. When set, `vidstabtransform` is added to the
    /// filter chain.
    pub stabilize_transforms: Option<&'a Utf8Path>,
    /// Drawtext overlays baked into this segment (for intro/outro
    /// "overlay" mode where text rides on top of real footage, no separate card).
    pub overlays: &'a [OverlayDirective<'a>],
    /// Image overlays composited on top of this segment (logo-on-footage
    /// pattern in overlay mode). Empty in the common case; when populated
    /// triggers the `-filter_complex` path that admits multiple inputs.
    pub image_overlays: &'a [ImageOverlayDirective<'a>],
    /// Whether the source chapter has an audio stream. When `false`, we
    /// synthesise silent stereo via `anullsrc` so every normalised clip
    /// presents the same shape to the downstream `concat_with_xfade` (which
    /// references `[N:a]` per input — failing if any single clip is missing
    /// audio). GoPro HERO5/6 files where the mic was off, or any source
    /// without an audio track, fall in this bucket.
    pub input_has_audio: bool,
    /// When `Some`, apply an outgoing speed-ramp to the last
    /// `duration_secs` of this segment. The renderer reads
    /// `extra = duration_secs * (1 - 1/factor)` extra seconds from the
    /// source so the output's total length equals
    /// `NormalizeArgs::duration_secs`. When the chapter doesn't have that
    /// extra footage available past `start_secs + duration_secs`, the ramp
    /// is silently dropped for that segment.
    pub outgoing_ramp: Option<SpeedRamp>,
    /// When `true`, `input` is a still image file rather than a video.
    /// The renderer applies a Ken Burns / cinematic motion effect to produce
    /// a video clip of `duration_secs` length.  `start_secs` is ignored.
    pub is_image: bool,
    /// Which animation effect to apply when `is_image = true`.
    /// `None` is treated the same as `Auto` (seed-driven effect selection).
    pub image_effect: Option<crate::project_config::ImageEffect>,
    /// Per-segment seed for deterministic `Auto` effect selection.
    /// Typically `project_seed ^ segment_index`.
    pub image_effect_seed: u64,
}

/// Compute the Ken Burns / cinematic-motion filter expression for a still image.
///
/// The returned string is a raw ffmpeg filter chain fragment that reads from
/// `[0:v]` and outputs a stream conforming to `WxH @ fps`. It is used inside
/// a `-filter_complex` invocation so downstream stages (LUT, drawtext, image
/// overlays) can be chained after it.
///
/// All effects upscale the source first (2× or 4×) to give `zoompan` pixel
/// headroom before it crops back to the target dimensions.
fn build_image_animation_filter(
    effect: crate::project_config::ImageEffect,
    duration_secs: f64,
    fps: u32,
    width: u32,
    height: u32,
    seed: u64,
) -> String {
    use crate::project_config::ImageEffect::*;
    let n = (duration_secs * fps as f64).ceil().max(2.0) as u32;
    let nm = n.saturating_sub(1).max(1); // (n-1) pre-computed to avoid div-by-zero in filter exprs
    let w = width;
    let h = height;
    let f = fps;

    let resolved = if effect == Auto {
        // Deterministically rotate through the 6 animated effects.
        let effects = [ZoomIn, ZoomOut, PanLeft, PanRight, Diagonal, RotateZoom];
        effects[(seed as usize) % effects.len()]
    } else {
        effect
    };

    // Letterbox to the target canvas first so that every subsequent filter
    // (zoompan, rotate) receives an input whose aspect ratio already matches
    // the output dimensions.  Without this, a portrait photo fed into
    // `zoompan=...s={w}x{h}` would be stretched to fill the landscape canvas.
    let lb = letterbox_filter(w, h);

    match resolved {
        // Classic Ken Burns: slow zoom toward center.
        ZoomIn => format!(
            "{lb},scale=iw*4:ih*4,\
             zoompan=z='min(zoom+{zf},{zm})':x='iw/2-(iw/zoom/2)':y='ih/2-(ih/zoom/2)':d={n}:s={w}x{h}:fps={f},\
             setsar=1",
            zf = ZOOM_STEP_FAST, zm = ZOOM_MAX_DEEP,
        ),
        // Reverse Ken Burns: start close, slowly pull back.
        ZoomOut => format!(
            "{lb},scale=iw*4:ih*4,\
             zoompan=z='if(eq(in,0),{zm},max(1,zoom-{zf}))':x='iw/2-(iw/zoom/2)':y='ih/2-(ih/zoom/2)':d={n}:s={w}x{h}:fps={f},\
             setsar=1",
            zf = ZOOM_STEP_FAST, zm = ZOOM_MAX_DEEP,
        ),
        // Horizontal pan: right-to-left sweep.
        PanLeft => format!(
            "{lb},scale=iw*2:ih*2,\
             zoompan=z={zp}:x='max(0,iw*(1-1/zoom)*(1-in/{nm}))':y='ih/2-(ih/zoom/2)':d={n}:s={w}x{h}:fps={f},\
             setsar=1",
            zp = ZOOM_PAN,
        ),
        // Horizontal pan: left-to-right sweep.
        PanRight => format!(
            "{lb},scale=iw*2:ih*2,\
             zoompan=z={zp}:x='min(iw*(1-1/zoom),iw*(1-1/zoom)*in/{nm})':y='ih/2-(ih/zoom/2)':d={n}:s={w}x{h}:fps={f},\
             setsar=1",
            zp = ZOOM_PAN,
        ),
        // Diagonal zoom + pan from top-left corner toward center.
        Diagonal => format!(
            "{lb},scale=iw*4:ih*4,\
             zoompan=z='min(zoom+{zs},{zm})':x='iw*(1-1/zoom)/2*(1-in/{nm})':y='ih*(1-1/zoom)/2*(1-in/{nm})':d={n}:s={w}x{h}:fps={f},\
             setsar=1",
            zs = ZOOM_STEP_SLOW, zm = ZOOM_MAX_MED,
        ),
        // Slow clockwise rotation — upscale 3× to hide black corners at the edges.
        RotateZoom => {
            let w3 = w * 3;
            let h3 = h * 3;
            format!(
                "{lb},scale={w3}:{h3},\
                 rotate=angle='{rs}*t':ow={w}:oh={h}:fillcolor=black,\
                 fps={f},setsar=1",
                rs = ROTATE_SPEED_RAD_PER_SEC,
            )
        }
        // Static letterboxed hold — no motion.
        Static => format!("{lb},fps={f},setsar=1", lb = letterbox_filter(w, h)),
        Auto => unreachable!("resolved above"),
    }
}

/// Render a still image as a video clip with a cinematic motion effect.
///
/// Uses `-loop 1 -framerate fps -i <image>` to feed an infinite image stream
/// into ffmpeg, then applies the animation filter chain (zoompan / rotate)
/// and caps the output with `-t duration`. Always synthesises a silent stereo
/// audio track via `anullsrc`.
fn normalize_image_segment(
    runner: &dyn FfmpegRunner,
    args: &NormalizeArgs<'_>,
    hw: &HwConfig,
) -> Result<()> {
    if let Some(parent) = args.output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating output dir {parent}"))?;
    }

    let effect = args.image_effect.unwrap_or(crate::project_config::ImageEffect::Auto);
    let animation = build_image_animation_filter(
        effect,
        args.duration_secs,
        args.fps,
        args.width,
        args.height,
        args.image_effect_seed,
    );

    let mut cmd: Vec<String> = vec![
        arg("-y"), arg("-hide_banner"), arg("-loglevel"), arg("error"),
        // -loop 1: repeat the single image frame indefinitely; -framerate: decode speed.
        arg("-loop"), arg("1"),
        arg("-framerate"), args.fps.to_string(),
        arg("-i"), args.input.to_string(),
        // Always need a silent audio track (images have no audio stream).
        arg("-f"), arg("lavfi"),
        arg("-i"), format!("anullsrc=channel_layout=stereo:sample_rate={AUDIO_SAMPLE_RATE}"),
    ];

    // Optional image overlay inputs (intro/outro "overlay" mode — logos, etc.)
    // These come AFTER anullsrc so the audio is always at input index 1.
    for d in args.image_overlays {
        cmd.push(arg("-loop"));
        cmd.push(arg("1"));
        cmd.push(arg("-i"));
        cmd.push(d.overlay.path.to_string());
    }

    let dur = args.duration_secs;

    // Build filter_complex.
    //   [0:v] = main image (the still to animate)
    //   [1:a] = anullsrc (silent audio source)
    //   [2:v]+ = image overlay inputs (if any)
    let mut fc = format!("[0:v]{animation}");

    // Optional LUT (colour grading).
    if let Some(lut) = args.lut3d {
        fc.push_str(&format!(",lut3d='{}'", lut.as_str().replace('\\', "/")));
    }

    // Text overlays (intro/outro "overlay" mode drawtext).
    for d in args.overlays {
        if let Some(dt) = build_overlay_drawtext(d) {
            fc.push(',');
            fc.push_str(&dt);
        }
    }

    // Intermediate label after the base chain.
    fc.push_str("[base]");

    // Image overlay compositing (logo-on-image, intro/outro overlay-mode).
    // Slot 0 = main image, slot 1 = anullsrc, slots 2+ = overlay images.
    append_image_overlay_chain(&mut fc, args.image_overlays, 2, dur as f32);

    // Trim the infinite anullsrc to match the exact video duration.
    fc.push_str(&format!(";[1:a]atrim=0:{dur:.4},asetpts=PTS-STARTPTS[a]", dur = dur));

    cmd.extend([
        arg("-filter_complex"), fc,
        arg("-map"), arg("[v]"),
        arg("-map"), arg("[a]"),
        // Hard cap: ensures the output never exceeds the configured hold duration.
        arg("-t"), format!("{dur:.3}"),
        arg("-pix_fmt"), arg("yuv420p"),
    ]);
    cmd.extend(hw.encode_args(args.crf));
    cmd.extend([
        arg("-c:a"), arg("aac"),
        arg("-ar"), AUDIO_SAMPLE_RATE.to_string(),
        arg("-ac"), arg("2"),
        arg("-movflags"), arg("+faststart"),
        args.output.to_string(),
    ]);

    let refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
    runner.run(&refs)
}

/// Extract one segment from `input` and conform it to the project profile.
/// Output is mp4 / aac / yuv420p / target res+fps, encoded by `hw.encoder`.
/// All normalized clips share these parameters so the concat stage can stream-copy.
pub fn normalize_segment(
    runner: &dyn FfmpegRunner,
    args: &NormalizeArgs<'_>,
    hw: &HwConfig,
) -> Result<()> {
    if args.is_image {
        return normalize_image_segment(runner, args, hw);
    }
    if let Some(parent) = args.output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating output dir {parent}"))?;
    }

    // Letterbox (preserve aspect): scale to fit, then pad to exact target.
    // We stay on CPU for scale+pad here because xfade and drawtext later need
    // CPU frames anyway — the win from `scale_cuda` is small and adds a
    // hwdownload roundtrip per segment.
    //
    // Filter order:
    //   scale → pad → [stabilize] → [lut3d] → setsar → fps
    // Stabilize after scale so its transforms file (built at the same scale)
    // matches; LUT after stabilize so any pixel-cropping from zoom doesn't
    // affect the colour mapping.
    let mut chain = letterbox_filter(args.width, args.height);
    if let Some(trf) = args.stabilize_transforms {
        // vidstab requires forward slashes and quoted paths.
        chain.push_str(&format!(
            ",vidstabtransform=input='{}':smoothing=10:zoom=1:interpol=bilinear",
            trf.as_str().replace('\\', "/")
        ));
    }
    if let Some(lut) = args.lut3d {
        chain.push_str(&format!(
            ",lut3d='{}'",
            lut.as_str().replace('\\', "/")
        ));
    }
    // Overlays: any drawtext directives baked into this segment.
    for d in args.overlays {
        if let Some(dt) = build_overlay_drawtext(d) {
            chain.push(',');
            chain.push_str(&dt);
        }
    }
    chain.push_str(&format!(",setsar=1,fps={fps}", fps = args.fps));

    // Speed-ramp arithmetic. We extend the input window by `extra` seconds so
    // the OUTPUT length stays exactly `duration_secs` even after compressing
    // the tail by `factor`. extra = ramp_dur × (1 − 1/factor).
    // E.g. for factor=2.5, ramp=350ms: extra=210ms; the last 350ms of source
    // play in 140ms of output, so the segment still ends at duration_secs.
    let ramp_extra: f32 = args
        .outgoing_ramp
        .map(|r| r.duration_secs * (1.0 - 1.0 / r.factor.max(1.0)))
        .unwrap_or(0.0)
        .max(0.0);
    let input_t_secs = args.duration_secs as f32 + ramp_extra;

    let ss = format!("{:.3}", args.start_secs);
    let t = format!("{:.3}", input_t_secs);

    let mut cmd: Vec<String> = vec![
        arg("-y"),
        arg("-hide_banner"),
        arg("-loglevel"), arg("error"),
        arg("-ss"), ss,
    ];
    for d in hw.decode_prefix() {
        cmd.push(d.to_string());
    }
    cmd.extend([
        arg("-i"), args.input.to_string(),
        arg("-t"), t,
    ]);

    // Image overlays in overlay-mode (logo on footage) need separate inputs
    // and a filter graph that can reference them — `-vf` only sees `[0:v]`.
    // When there are no image overlays we use the historical `-vf` path
    // (lighter filter graph, identical behaviour); otherwise we add each
    // image as an extra `-i` input and switch to `-filter_complex`.
    //
    // When the source has no audio (mic-off GoPro recordings, some HERO5/6
    // captures), we add an `anullsrc` input so the output still has a
    // silent stereo track. Without this, downstream `acrossfade` chains in
    // `concat_with_xfade` fail because some `[N:a]` references would resolve
    // to no stream. ALL extra inputs (image overlays, then anullsrc if
    // needed) must be added BEFORE `-vf` / `-filter_complex` — otherwise
    // ffmpeg treats `-vf` as an input option for the next-following `-i`
    // and fails with "Option vf cannot be applied to input url ...".
    let n_image_inputs = args.image_overlays.len();
    let silent_audio_idx: Option<usize> = if !args.input_has_audio {
        Some(1 + n_image_inputs)
    } else {
        None
    };
    // Image inputs (always added when present, regardless of which filter
    // path we'll use below).
    for d in args.image_overlays {
        cmd.push(arg("-loop"));
        cmd.push(arg("1"));
        cmd.push(arg("-i"));
        cmd.push(d.overlay.path.to_string());
    }
    // Silent audio input: must come BEFORE -vf/-filter_complex.
    if silent_audio_idx.is_some() {
        cmd.extend([
            arg("-f"), arg("lavfi"),
            arg("-i"), format!("anullsrc=channel_layout=stereo:sample_rate={AUDIO_SAMPLE_RATE}"),
        ]);
    }
    // We need `-filter_complex` whenever the graph references inputs other
    // than `0:v` (image overlays, anullsrc), or when we splice the video
    // and/or audio stream (speed-ramp). Otherwise stay on the simpler `-vf`
    // path — same behaviour, lighter graph.
    let use_filter_complex = !args.image_overlays.is_empty() || args.outgoing_ramp.is_some();

    if !use_filter_complex {
        cmd.extend([arg("-vf"), chain]);
    } else {
        // Speed-ramp split point (in source-content seconds, post-`-ss`).
        let normal_end = (input_t_secs - args.outgoing_ramp.map(|r| r.duration_secs).unwrap_or(0.0))
            .max(0.0);

        // Stage 1: video chain on [0:v]. With a ramp, split off the tail,
        // setpts the fast portion, and concat the two halves into [base].
        // Without a ramp, the chain output goes directly to [base].
        let mut fc = format!("[0:v]{chain}");
        if let Some(ramp) = args.outgoing_ramp {
            // After `setpts` shifts each frame's timestamp, the effective
            // frame rate of the fast portion changes (e.g. 30 fps × 2.5 →
            // 75 fps). Without an explicit `fps={N}` resample on each side,
            // `concat` falls back to naive frame matching and produces
            // stuttering at the seam. Locking both halves to the project's
            // fps gives smooth playback through the speed change.
            //
            // When `ramp.flash` is set, the sped-up tail receives a Quik-style
            // colour treatment: `eq=brightness` ramps from 0 to ~0.05 across
            // the ramp (subtle warm-up that lands on a brighter frame just
            // before the cut), then `fade=t=out:color=white` blows out the
            // last ~3 frames to pure white. The white frame BECOMES the
            // visible transition into the next clip — that's the kinetic
            // punch you see in GoPro Quik / Reels whip-ups.
            let ramp_out_secs = ramp.duration_secs / ramp.factor.max(1.01);
            let flash_dur = (ramp_out_secs * 0.20).min(0.10).max(2.0 / args.fps as f32);
            let flash_filter = if ramp.flash {
                format!(
                    ",eq=brightness='0.05*t/{r:.4}':eval=frame,\
                     fade=t=out:st={fst:.4}:d={fd:.4}:color=white",
                    r = ramp_out_secs,
                    fst = (ramp_out_secs - flash_dur).max(0.0),
                    fd = flash_dur,
                )
            } else {
                String::new()
            };
            fc.push_str(&format!(
                ",split[vf1][vf2];\
                 [vf1]trim=0:{ne:.4},setpts=PTS-STARTPTS,fps={fps}[vn];\
                 [vf2]trim={ne:.4}:{tot:.4},setpts=(PTS-STARTPTS)/{f:.4},fps={fps}{flash}[vfast];\
                 [vn][vfast]concat=n=2:v=1:a=0[base]",
                ne = normal_end,
                tot = input_t_secs,
                f = ramp.factor,
                fps = args.fps,
                flash = flash_filter,
            ));
        } else {
            fc.push_str("[base]");
        }

        // Stage 2: image overlays composite onto [base], producing [v].
        // Slot 0 = main video, slots 1..n = image overlays (n_image_inputs).
        // anullsrc (if present) is at slot 1 + n_image_inputs — AFTER the images.
        append_image_overlay_chain(&mut fc, args.image_overlays, 1, args.duration_secs as f32);

        // Stage 3: audio. Source audio (or anullsrc fallback) → optional
        // ramp split/concat → [a]. atempo on the tail matches the video's
        // speed-up so audio and picture stay in sync.
        let audio_in = match silent_audio_idx {
            Some(idx) => format!("[{}:a]", idx),
            None => "[0:a]".to_string(),
        };
        if let Some(ramp) = args.outgoing_ramp {
            if args.input_has_audio {
                fc.push_str(&format!(
                    ";{ai}asplit[af1][af2];\
                     [af1]atrim=0:{ne:.4},asetpts=PTS-STARTPTS[an];\
                     [af2]atrim={ne:.4}:{tot:.4},asetpts=PTS-STARTPTS,atempo={f:.4}[afast];\
                     [an][afast]concat=n=2:v=0:a=1[a]",
                    ai = audio_in,
                    ne = normal_end,
                    tot = input_t_secs,
                    f = ramp.factor,
                ));
            } else {
                // Sped-up silence is still silence — just trim to the output
                // duration so the (infinite) anullsrc doesn't run forever.
                fc.push_str(&format!(
                    ";{ai}atrim=0:{out_t:.4},asetpts=PTS-STARTPTS[a]",
                    ai = audio_in,
                    out_t = args.duration_secs,
                ));
                let _ = ramp; // silence unused
            }
        } else {
            fc.push_str(&format!(";{ai}anull[a]", ai = audio_in));
        }

        cmd.extend([
            arg("-filter_complex"), fc,
            arg("-map"), arg("[v]"),
            arg("-map"), arg("[a]"),
        ]);
    }

    // Silent-audio fallback: only the simple `-vf` path needs explicit -map
    // emission (filter_complex handles it inside the graph). `-shortest`
    // caps the infinite anullsrc at the video's length on both paths.
    if !use_filter_complex {
        if let Some(idx) = silent_audio_idx {
            // Map only the FIRST video stream of input 0. Some sources (DJI
            // drones) carry a secondary MJPEG thumbnail stream that `0:v`
            // would also match, breaking MP4 muxing with
            // "Could not find tag for codec h264 in stream #1".
            cmd.extend([
                arg("-shortest"),
                arg("-map"), arg("0:v:0"),
                arg("-map"), format!("{idx}:a"),
            ]);
        }
    } else if silent_audio_idx.is_some() {
        cmd.push(arg("-shortest"));
    }

    cmd.extend([arg("-pix_fmt"), arg("yuv420p")]);
    cmd.extend(hw.encode_args(args.crf));
    cmd.extend([
        arg("-c:a"), arg("aac"),
        arg("-ar"), AUDIO_SAMPLE_RATE.to_string(),
        arg("-ac"), arg("2"),
        arg("-movflags"), arg("+faststart"),
        args.output.to_string(),
    ]);

    let refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
    runner.run(&refs)
}

// ---------- Stabilization detect pass ----------

/// First pass of `vidstab`: analyse the segment range of `input` and write
/// transform vectors to `transforms_out` (a `.trf` file). The caller then
/// passes that path to `normalize_segment` via `stabilize_transforms`.
///
/// We scale to the same `(w, h)` as the eventual normalize so the transforms
/// match; `-f null -` discards the video output (we just want the .trf).
pub fn stabilize_detect(
    runner: &dyn FfmpegRunner,
    input: &Utf8Path,
    start_secs: f64,
    duration_secs: f64,
    width: u32,
    height: u32,
    transforms_out: &Utf8Path,
    hw: &HwConfig,
) -> Result<()> {
    if let Some(parent) = transforms_out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let trf_path = transforms_out.as_str().replace('\\', "/");
    let vf = format!(
        "{lb},vidstabdetect=stepsize=6:shakiness=5:accuracy=15:result='{trf_path}'",
        lb = letterbox_filter(width, height),
    );
    let ss = format!("{:.3}", start_secs);
    let t = format!("{:.3}", duration_secs);

    let mut cmd: Vec<String> = vec![
        arg("-y"),
        arg("-hide_banner"),
        arg("-loglevel"), arg("error"),
        arg("-ss"), ss,
    ];
    for d in hw.decode_prefix() {
        cmd.push(d.to_string());
    }
    cmd.extend([
        arg("-i"), input.to_string(),
        arg("-t"), t,
        arg("-vf"), vf,
        arg("-f"), arg("null"),
        arg("-"),
    ]);
    let refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
    runner.run(&refs)
}

// ---------- Stage 2: concat ----------

/// Concatenate normalized segments via the concat demuxer (stream copy — no re-encode).
pub fn concat_segments(
    runner: &dyn FfmpegRunner,
    segments: &[Utf8PathBuf],
    workspace: &Utf8Path,
    output: &Utf8Path,
) -> Result<()> {
    if segments.is_empty() {
        anyhow::bail!("concat_segments: no segments provided");
    }
    std::fs::create_dir_all(workspace)
        .with_context(|| format!("creating workspace {workspace}"))?;
    let list_path = workspace.join("concat.txt");

    let body = segments
        .iter()
        .map(|p| {
            // ffmpeg's concat list requires forward slashes and single-quoted paths,
            // with literal single quotes escaped as '\''.
            let abs = std::fs::canonicalize(p.as_std_path())
                .map(|pb| pb.to_string_lossy().replace('\\', "/").trim_start_matches("//?/").to_string())
                .unwrap_or_else(|_| p.as_str().replace('\\', "/"));
            let escaped = abs.replace('\'', "'\\''");
            format!("file '{escaped}'")
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&list_path, body)
        .with_context(|| format!("writing concat list {list_path}"))?;

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    runner.run(&[
        "-y",
        "-hide_banner",
        "-loglevel", "error",
        "-f", "concat",
        "-safe", "0",
        "-i", list_path.as_str(),
        "-c", "copy",
        "-movflags", "+faststart",
        output.as_str(),
    ])
}

// ---------- Stage 3: mux soundtrack ----------

/// Replace the body video's audio with the soundtrack. `-shortest` cuts at whichever
/// stream ends first.
pub fn mux_soundtrack(
    runner: &dyn FfmpegRunner,
    body: &Utf8Path,
    soundtrack: &Utf8Path,
    output: &Utf8Path,
) -> Result<()> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    runner.run(&[
        "-y",
        "-hide_banner",
        "-loglevel", "error",
        "-i", body.as_str(),
        "-i", soundtrack.as_str(),
        "-map", "0:v:0",
        "-map", "1:a:0",
        "-c:v", "copy",
        "-c:a", "aac",
        "-b:a", "192k",
        "-shortest",
        "-movflags", "+faststart",
        output.as_str(),
    ])
}

/// Smart-loop plan for `mux_with_ducking`.
///
/// Controls how the soundtrack is looped under the (potentially longer) body
/// video. Setting `loop_secs = 0.0` disables looping and plays the soundtrack
/// as-is, capped by `-shortest`.
///
/// All timestamps should fall on beat boundaries (caller's responsibility) so
/// every splice is musical.
#[derive(Debug, Clone, Copy, Default)]
pub struct SoundtrackLoopPlan {
    /// End of the natural intro. The first playthrough plays
    /// `source[0 .. first_play_end_secs]` so the authentic intro plays once.
    /// Set to `0.0` to skip the intro pre-roll and just loop from `loop_start`.
    pub first_play_end_secs: f32,
    /// Start of the repeated loud-region section in the source track.
    pub loop_start_secs: f32,
    /// Length of the repeated section. `0.0` = no looping.
    pub loop_secs: f32,
}

/// Mux the soundtrack with a list of "duck windows" where the soundtrack
/// fades out and the source audio fades in. Both audio streams are mixed
/// continuously; outside duck windows source is silent and soundtrack plays
/// at full volume; inside duck windows source plays and soundtrack drops to
/// `ducking_db` dB.
///
/// `windows` should be sorted by `start_secs` and non-overlapping (with
/// ramp_secs of margin). Empty `windows` is equivalent to `mux_soundtrack`
/// — the caller can use either path safely.
pub fn mux_with_ducking(
    runner: &dyn FfmpegRunner,
    body: &Utf8Path,
    soundtrack: &Utf8Path,
    windows: &[(f32, f32, f32)], // (start, end, ramp)
    ducking_db: f32,
    // Total output duration (for fade-out positioning). 0 = no fade-out.
    body_duration_secs: f32,
    soundtrack_fade_out_secs: f32,
    loop_plan: SoundtrackLoopPlan,
    output: &Utf8Path,
) -> Result<()> {
    let soundtrack_first_play_end_secs = loop_plan.first_play_end_secs;
    let soundtrack_loop_start_secs = loop_plan.loop_start_secs;
    let soundtrack_loop_secs = loop_plan.loop_secs;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    if windows.is_empty()
        && soundtrack_fade_out_secs <= 0.0
        && soundtrack_loop_secs <= 0.0
        && soundtrack_first_play_end_secs <= 0.0
    {
        return mux_soundtrack(runner, body, soundtrack, output);
    }

    // Build duck mask `D(t)` ∈ [0,1] as a sum of trapezoids — each duck window
    // ramps up over `ramp_secs` to 1.0, holds, then ramps back to 0.
    // We use the difference of two step-clamped ramps:
    //   trap(t) = clamp((t - (T1-R))/R, 0, 1) - clamp((t - T2)/R, 0, 1)
    // ffmpeg eval has min/max but no clamp, so we spell it out.
    let traps: Vec<String> = windows
        .iter()
        .map(|&(t1, t2, ramp)| {
            let r = ramp.max(1e-3);
            // One trapezoid expression. min/max are pairwise.
            format!(
                "(max(0,min(1,(t-({s}))/{r}))-max(0,min(1,(t-{e})/{r})))",
                s = format_f(t1 - r),
                e = format_f(t2),
                r = format_f(r)
            )
        })
        .collect();
    // Sum of trapezoids → "duck mask" D(t). When there are no duck windows,
    // `traps.join("+")` is "" — which would produce invalid filter
    // expressions like `volume=''` and `volume='1-(0.874)*()'`. Substitute a
    // literal "0" so D(t) ≡ 0 and the soundtrack always plays at full
    // volume, source audio is always silent. Same effective behaviour as
    // skipping the volume filter entirely, but keeps the rest of the graph
    // (smart-loop, fade-out) intact.
    let duck_expr = if traps.is_empty() {
        "0".to_string()
    } else {
        traps.join("+")
    };

    // duck_factor = 10^(ducking_db / 20). For -18 dB ≈ 0.126.
    let duck_factor = 10.0_f32.powf(ducking_db / 20.0);

    // Soundtrack volume: 1.0 outside, duck_factor inside. Linear blend:
    //   V_st(t) = 1 - (1 - duck_factor) * D(t)
    // Source volume: 0 outside, 1 inside.
    //   V_src(t) = D(t)
    let bg_vol_expr = format!(
        "1-({df})*({d})",
        df = format_f(1.0 - duck_factor),
        d = duck_expr
    );
    let src_vol_expr = duck_expr.clone();

    // Build the filter graph. We need amix with normalize=0 so volumes don't
    // get auto-halved. If a soundtrack fade-out is configured, append an
    // `afade=t=out` to the mixed result so the soundtrack tail dies cleanly.
    //
    // Soundtrack pre-processing: if `soundtrack_loop_secs > 0`, trim the
    // soundtrack to that length (a beat-aligned multiple of the beat
    // interval, computed by the caller) and `aloop` it indefinitely so the
    // body audio is continuous regardless of body length. The loop seam is
    // musical because the trim ends on a beat.
    let mut filtergraph = String::new();
    let bg_label = if soundtrack_loop_secs > 0.0 && soundtrack_first_play_end_secs > 0.0 {
        // Smart loop: natural intro + N crossfaded loop cycles.
        //
        // Compute how many loop cycles cover the body, then build a chain:
        //   intro = source[0..first_play_end]
        //   loopN = source[loop_start..loop_end]   (one copy per cycle)
        //   chain = intro × loop1 × loop2 × ... × loopN  (× = acrossfade with
        //          a long enough ramp that the loop seam morphs in/out
        //          gradually instead of presenting a hard splice)
        //
        // Crossfade ramp ~2 s (≈ 4 beats at 120 BPM, a full musical phrase),
        // clamped to ≤ 20 % of the loop length so very short loops still
        // have meaningful loop content between seams. `qsin` curves are
        // equal-power: perceived loudness stays roughly constant through the
        // fade. Tri (linear) curves dip ~3 dB in the middle, which is
        // audible and is the "I can still hear the loop point" complaint.
        let loop_end = soundtrack_loop_start_secs + soundtrack_loop_secs;
        let xfade_secs: f32 = 2.0_f32.min(soundtrack_loop_secs * 0.2).max(0.25);
        // How many full loop cycles do we need beyond the natural intro to
        // cover body_duration_secs (with crossfade overlaps)?
        let needed_after_intro = (body_duration_secs - soundtrack_first_play_end_secs).max(0.0);
        // Each loop cycle adds (loop_secs - xfade_secs) to the running total.
        let cycle_advance = (soundtrack_loop_secs - xfade_secs).max(0.1);
        let n_cycles = ((needed_after_intro / cycle_advance).ceil() as usize).max(1);
        // Total stream copies = 1 (intro) + n_cycles (loops). Cap at a safety
        // limit so we don't build an unbounded filter graph for crazy targets.
        let total_copies = (1 + n_cycles).min(64);
        let split_targets: Vec<String> = (0..total_copies)
            .map(|i| format!("[stcp{}]", i))
            .collect();
        filtergraph.push_str(&format!(
            "[1:a]asplit={n}{labels};",
            n = total_copies,
            labels = split_targets.join(""),
        ));
        // Trim each copy: copy 0 = intro, the rest = loud region.
        for i in 0..total_copies {
            let (start, end) = if i == 0 {
                (0.0, soundtrack_first_play_end_secs)
            } else {
                (soundtrack_loop_start_secs, loop_end)
            };
            filtergraph.push_str(&format!(
                "[stcp{i}]atrim={start:.4}:{end:.4},aresample={sr},asetpts=N/SR/TB[sttr{i}];",
                sr = AUDIO_SAMPLE_RATE,
            ));
        }
        // Chain acrossfades: sttr0 × sttr1 → sx1 ; sx1 × sttr2 → sx2 ; ...
        if total_copies == 1 {
            // Edge case (shouldn't happen): no looping cycles, just the intro.
            // Forward sttr0 as the soundtrack stream.
            filtergraph.push_str("[sttr0]anull[stout];");
        } else {
            for i in 1..total_copies {
                let prev_label = if i == 1 {
                    "[sttr0]".to_string()
                } else {
                    format!("[sx{}]", i - 1)
                };
                let next_label = if i == total_copies - 1 {
                    "[stout]".to_string()
                } else {
                    format!("[sx{}]", i)
                };
                filtergraph.push_str(&format!(
                    "{prev_label}[sttr{i}]acrossfade=d={dur:.4}:c1=qsin:c2=qsin{next_label};",
                    dur = xfade_secs,
                ));
            }
        }
        tracing::info!(
            xfade_secs = xfade_secs,
            n_cycles = n_cycles,
            loop_secs = soundtrack_loop_secs,
            "soundtrack loop crossfade"
        );
        "[stout]"
    } else if soundtrack_loop_secs > 0.0 {
        // Simple loop (no natural intro): trim to loud region, loop forever.
        let loop_end = soundtrack_loop_start_secs + soundtrack_loop_secs;
        let samples = (soundtrack_loop_secs * AUDIO_SAMPLE_RATE as f32) as i64;
        filtergraph.push_str(&format!(
            "[1:a]atrim={start:.4}:{end:.4},aresample={sr},aloop=loop=-1:size={samples}:start=0,asetpts=N/SR/TB[stloop];",
            start = soundtrack_loop_start_secs,
            end = loop_end,
            sr = AUDIO_SAMPLE_RATE,
        ));
        "[stloop]"
    } else {
        "[1:a]"
    };
    filtergraph.push_str(&format!(
        "[0:a]volume=eval=frame:volume='{src_vol}'[src];\
         {bg_in}volume=eval=frame:volume='{bg_vol}'[bg];\
         [src][bg]amix=inputs=2:duration=first:dropout_transition=0:normalize=0",
        bg_in = bg_label,
        src_vol = src_vol_expr,
        bg_vol = bg_vol_expr,
    ));
    if soundtrack_fade_out_secs > 0.0 && body_duration_secs > soundtrack_fade_out_secs {
        let fade_start = body_duration_secs - soundtrack_fade_out_secs;
        filtergraph.push_str(&format!(
            "[mixed];[mixed]afade=t=out:st={st:.3}:d={d:.3}",
            st = fade_start,
            d = soundtrack_fade_out_secs,
        ));
    }
    filtergraph.push_str("[a]");

    // Hard-cap the muxed output to the actual body video duration.
    // `-shortest` is unreliable with `-c:v copy` (the muxer can still emit
    // audio frames past the video EOF), and AAC frame quantisation in the
    // upstream `acrossfade` chain often leaves the body's audio stream
    // longer than its video stream by ~10-20 ms per seam — accumulated
    // across 40+ seams that's enough to leave the player holding the last
    // frame for several seconds at the very end. Pass `body_duration_secs`
    // explicitly via `-t` so audio is truncated to the same wall-clock
    // length the video actually has.
    let body_dur = if body_duration_secs > 0.0 {
        format!("{body_duration_secs:.3}")
    } else {
        // No declared duration: fall back to -shortest only.
        String::new()
    };
    let mut args: Vec<&str> = vec![
        "-y",
        "-hide_banner",
        "-loglevel", "error",
        "-i", body.as_str(),
        "-i", soundtrack.as_str(),
        "-filter_complex", &filtergraph,
        "-map", "0:v:0",
        "-map", "[a]",
        "-c:v", "copy",
        "-c:a", "aac",
        "-b:a", "192k",
        "-shortest",
    ];
    if !body_dur.is_empty() {
        args.push("-t");
        args.push(&body_dur);
    }
    args.extend(["-movflags", "+faststart", output.as_str()]);
    runner.run(&args)
}

fn format_f(v: f32) -> String {
    // ffmpeg eval doesn't accept locale-dependent decimal separators; force `.`.
    let s = format!("{v:.4}");
    s
}

/// Concat clips with the ffmpeg `concat` filter (re-encode), producing one
/// output with clean frame timing across the seams. Use this — instead of
/// the demuxer-based [`concat_segments`] — when you need each input's stream
/// timestamps reset relative to the cumulative output (e.g. when stitching
/// sub-second clips whose codec frame boundaries don't align with clip
/// boundaries; demuxer concat then yields non-monotonic timestamps that
/// crash downstream filters like `xfade`).
///
/// `silent_audio = true` discards the inputs' audio entirely and emits a
/// silent stereo track of the same total duration. Used for the beat-burst
/// fold so the rapid-cut sequence rides only the soundtrack instead of
/// stitching 12 quarter-second source-audio fragments (which produces
/// click/pop artefacts at every cut).
pub fn concat_re_encode(
    runner: &dyn FfmpegRunner,
    inputs: &[Utf8PathBuf],
    width: u32,
    height: u32,
    fps: u32,
    crf: u32,
    silent_audio: bool,
    hw: &HwConfig,
    output: &Utf8Path,
) -> Result<()> {
    if inputs.is_empty() {
        anyhow::bail!("concat_re_encode: no inputs provided");
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let n = inputs.len();
    let mut cmd: Vec<String> = vec![arg("-y"), arg("-hide_banner"), arg("-loglevel"), arg("error")];
    for inp in inputs {
        cmd.push(arg("-i"));
        cmd.push(inp.to_string());
    }
    if silent_audio {
        // Add the silent source as the (n+1)-th input so we can map from it.
        // anullsrc generates an infinite stream; -shortest below caps it.
        cmd.extend([
            arg("-f"), arg("lavfi"),
            arg("-i"), format!("anullsrc=channel_layout=stereo:sample_rate={AUDIO_SAMPLE_RATE}"),
        ]);
    }
    // Build the video concat filter; audio is either concatenated through
    // (default) or replaced with the anullsrc input (silent_audio = true).
    let mut filt = String::new();
    for k in 0..n {
        filt.push_str(&format!("[{k}:v]"));
        if !silent_audio {
            filt.push_str(&format!("[{k}:a]"));
        }
    }
    if silent_audio {
        filt.push_str(&format!("concat=n={n}:v=1:a=0[v]"));
    } else {
        filt.push_str(&format!("concat=n={n}:v=1:a=1[v][a]"));
    }
    cmd.extend([arg("-filter_complex"), filt]);
    if silent_audio {
        cmd.extend([
            arg("-map"), arg("[v]"),
            arg("-map"), format!("{n}:a"),
        ]);
    } else {
        cmd.extend([arg("-map"), arg("[v]"), arg("-map"), arg("[a]")]);
    }
    cmd.extend([
        arg("-r"), fps.to_string(),
        arg("-pix_fmt"), arg("yuv420p"),
    ]);
    cmd.extend(hw.encode_args(crf));
    cmd.extend([
        arg("-c:a"), arg("aac"),
        arg("-ar"), AUDIO_SAMPLE_RATE.to_string(),
        arg("-ac"), arg("2"),
        arg("-movflags"), arg("+faststart"),
    ]);
    if silent_audio {
        // anullsrc is an infinite stream; cap output to the video length.
        cmd.extend([arg("-shortest")]);
    }
    cmd.push(output.to_string());
    let _ = (width, height); // currently inputs are pre-normalized; left here to match other concat helpers' shape
    let refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
    runner.run(&refs)
}

/// Transcode the muxed body (with soundtrack) to one final output: scale to
/// `width × height`, re-encode video at `crf` with the encoder configured by
/// `hw`, and re-encode audio as AAC at `audio_bitrate`. Letterboxes if the
/// aspect ratios differ.
pub fn transcode_output(
    runner: &dyn FfmpegRunner,
    input: &Utf8Path,
    output: &Utf8Path,
    width: u32,
    height: u32,
    crf: u32,
    audio_bitrate: &str,
    hw: &HwConfig,
) -> Result<()> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let vf = format!("{lb},setsar=1", lb = letterbox_filter(width, height));
    let mut cmd: Vec<String> = vec![
        arg("-y"), arg("-hide_banner"), arg("-loglevel"), arg("error"),
        arg("-i"), input.to_string(),
        arg("-vf"), vf,
        arg("-pix_fmt"), arg("yuv420p"),
    ];
    cmd.extend(hw.encode_args(crf));
    cmd.extend([
        arg("-c:a"), arg("aac"),
        arg("-b:a"), audio_bitrate.to_string(),
        arg("-ar"), AUDIO_SAMPLE_RATE.to_string(),
        arg("-ac"), arg("2"),
        arg("-movflags"), arg("+faststart"),
        output.to_string(),
    ]);
    let refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
    runner.run(&refs)
}

// =====================================================================
// Intro/outro text cards + xfade transitions
// =====================================================================

/// One text overlay to bake into a card. Mirrors `crate::project_config::Text` but at the
/// render layer we don't depend on sv-config to keep this crate slim.
#[derive(Debug, Clone)]
pub struct TextOverlay {
    pub content: String,
    pub font_path: Option<Utf8PathBuf>,
    pub size: u32,
    /// `#RRGGBB` or `#RRGGBBAA`.
    pub color_hex: String,
    /// One of: center, top-left, top-right, top-center, bottom-left,
    /// bottom-right, bottom-center.
    pub position: String,
    /// Pixel offset applied AFTER the position anchor.
    #[allow(dead_code)]
    pub x_offset: i32,
    #[allow(dead_code)]
    pub y_offset: i32,
    pub fade_in_secs: f32,
    pub fade_out_secs: f32,
    /// Visibility window in card-relative seconds. The drawtext is bounded by
    /// `enable='between(t, appear_at, disappear_at)'` and its fade alpha is
    /// computed inside that window.
    pub appear_at_secs: f32,
    pub disappear_at_secs: f32,
    pub bg_color_hex: Option<String>,
    pub bg_padding_px: Option<u32>,
}

/// One image overlay (logo / watermark) to composite onto a card.
#[derive(Debug, Clone)]
pub struct ImageOverlay {
    pub path: Utf8PathBuf,
    pub position: String,
    pub height: u32,
    pub x_offset: i32,
    pub y_offset: i32,
    /// 0..1
    pub opacity: f32,
    pub fade_in_secs: f32,
    pub fade_out_secs: f32,
    /// Visibility window in card-relative (or segment-relative for overlay
    /// mode) seconds. The overlay filter uses `enable='between(t, appear, disappear)'`.
    pub appear_at_secs: f32,
    pub disappear_at_secs: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct CardSpec<'a> {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub duration_secs: f32,
    pub bg_color_hex: &'a str, // e.g. "#000000"
    pub crf: u32,
}

/// Render a text card to `output`: solid background + zero or more drawtext
/// layers + zero or more image overlays + silent stereo audio. The output
/// matches the project's res/fps/pix_fmt so it concatenates cleanly with
/// normalized segments.
pub fn render_text_card(
    runner: &dyn FfmpegRunner,
    output: &Utf8Path,
    spec: &CardSpec<'_>,
    texts: &[TextOverlay],
    images: &[ImageOverlay],
    hw: &HwConfig,
) -> Result<()> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let bg = ffmpeg_color(spec.bg_color_hex);
    let video_src = format!(
        "color=c={bg}:size={w}x{h}:rate={fps}:duration={dur}",
        bg = bg,
        w = spec.width,
        h = spec.height,
        fps = spec.fps,
        dur = format!("{:.3}", spec.duration_secs),
    );
    let audio_src = format!(
        "anullsrc=channel_layout=stereo:sample_rate={AUDIO_SAMPLE_RATE}:duration={dur:.3}",
        dur = spec.duration_secs,
    );

    // Build filter chain. Phases:
    //  (a) drawtext chain on [0:v] — produces [vt] (or copies through).
    //  (b) for each image: scale + format + alpha curve on [k:v], overlay onto running video stream.
    //  Final stream must be labeled [v].
    let n_text = texts.len();
    let n_img = images.len();
    let mut filter = String::new();

    let after_text_label: String = if n_text == 0 {
        // No text: pass through. When there are also no image overlays
        // (a blank outro card, e.g. for a quiet fade-to-black ending), this
        // is the FINAL stage — emit `[v]` directly so `-map [v]` resolves.
        // Otherwise emit `[vt]` so the image-overlay chain below can pick
        // it up.
        if n_img == 0 {
            filter.push_str("[0:v]null[v]");
            "v".to_string()
        } else {
            filter.push_str("[0:v]null[vt]");
            "vt".to_string()
        }
    } else {
        let last = n_text - 1;
        for (i, t) in texts.iter().enumerate() {
            let in_label = if i == 0 {
                "[0:v]".to_string()
            } else {
                format!("[t{}]", i - 1)
            };
            let out_label = if i == last && n_img == 0 {
                "[v]".to_string()
            } else if i == last {
                "[vt]".to_string()
            } else {
                format!("[t{i}]")
            };
            let dt = build_drawtext(t, spec.duration_secs);
            filter.push_str(&format!("{in_label}{dt}{out_label}"));
            if i != last {
                filter.push(';');
            }
        }
        if n_img == 0 { "v".to_string() } else { "vt".to_string() }
    };

    // Image overlays. Each image is the (k+2)-th input (after color + anullsrc).
    if n_img > 0 {
        let mut prev = after_text_label.clone();
        let last = n_img - 1;
        for (k, im) in images.iter().enumerate() {
            let img_in = format!("[{}:v]", k + 2);
            // Visibility window — defaults to (0, card_dur) so old configs are
            // unaffected. fade-in is anchored at `appear`; fade-out ends at
            // `disappear`. The overlay's `enable=` clause prevents per-frame
            // composite work outside this range.
            let appear = im.appear_at_secs.max(0.0);
            let disappear = im.disappear_at_secs.min(spec.duration_secs).max(appear + 0.05);
            let prep_label = format!("[i{k}p]");
            filter.push(';');
            filter.push_str(&format!(
                "{img_in}scale=-1:{h}:flags=lanczos,format=rgba",
                h = im.height,
            ));
            // Apply opacity multiplier on alpha channel.
            if (im.opacity - 1.0).abs() > 1e-3 {
                filter.push_str(&format!(
                    ",colorchannelmixer=aa={:.4}",
                    im.opacity.clamp(0.0, 1.0)
                ));
            }
            // Alpha fade-in / fade-out anchored to the visibility window.
            if im.fade_in_secs > 0.0 {
                filter.push_str(&format!(
                    ",fade=t=in:st={st:.4}:d={d:.4}:alpha=1",
                    st = appear,
                    d = im.fade_in_secs
                ));
            }
            if im.fade_out_secs > 0.0 {
                let st = (disappear - im.fade_out_secs).max(0.0);
                filter.push_str(&format!(
                    ",fade=t=out:st={:.4}:d={:.4}:alpha=1",
                    st, im.fade_out_secs
                ));
            }
            filter.push_str(&prep_label);

            // Overlay step (with `enable` for the visibility window).
            let out_label = if k == last { "[v]".to_string() } else { format!("[ic{k}]") };
            let (x, y) = image_overlay_xy(&im.position, im.x_offset, im.y_offset);
            filter.push(';');
            filter.push_str(&format!(
                "[{prev}]{prep}overlay=x={x}:y={y}:format=auto:enable='between(t,{a:.4},{b:.4})'{out}",
                prev = prev,
                prep = prep_label,
                x = x,
                y = y,
                a = appear,
                b = disappear,
                out = out_label,
            ));
            prev = out_label.trim_matches(|c| c == '[' || c == ']').to_string();
        }
    }

    let mut cmd: Vec<String> = vec![
        arg("-y"), arg("-hide_banner"), arg("-loglevel"), arg("error"),
        arg("-f"), arg("lavfi"), arg("-i"), video_src,
        arg("-f"), arg("lavfi"), arg("-i"), audio_src,
    ];
    for im in images {
        cmd.push(arg("-i"));
        cmd.push(im.path.to_string());
    }
    cmd.extend([
        arg("-filter_complex"), filter,
        arg("-map"), arg("[v]"),
        arg("-map"), arg("1:a"),
        arg("-pix_fmt"), arg("yuv420p"),
    ]);
    cmd.extend(hw.encode_args(spec.crf));
    cmd.extend([
        arg("-c:a"), arg("aac"),
        arg("-ar"), AUDIO_SAMPLE_RATE.to_string(),
        arg("-ac"), arg("2"),
        arg("-movflags"), arg("+faststart"),
        output.to_string(),
    ]);
    let refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
    runner.run(&refs)
}

fn image_overlay_xy(pos: &str, x_off: i32, y_off: i32) -> (String, String) {
    // Anchor expressions match position_to_xy but for an arbitrary overlay W/H.
    let (ax, ay) = match pos {
        "top-left" => ("20", "20"),
        "top-right" => ("W-w-20", "20"),
        "top-center" => ("(W-w)/2", "20"),
        "bottom-left" => ("20", "H-h-20"),
        "bottom-right" => ("W-w-20", "H-h-20"),
        "bottom-center" => ("(W-w)/2", "H-h-20"),
        _ => ("(W-w)/2", "(H-h)/2"),
    };
    let x = if x_off == 0 { ax.to_string() } else { format!("({ax})+({x_off})") };
    let y = if y_off == 0 { ay.to_string() } else { format!("({ay})+({y_off})") };
    (x, y)
}

/// Concat clips with `xfade` between every adjacent pair (video) and
/// `acrossfade` (audio). Each clip is re-encoded in one ffmpeg pass —
/// quality cost vs. the stream-copy concat is offset by getting smooth
/// transitions and a single muxed output.
///
/// `inputs[i].duration_secs` must be accurate (used to compute xfade offsets).
/// `transitions[i]` is the ffmpeg xfade name to use between input[i] and
/// input[i+1] (e.g. "fade", "dissolve"). Length = inputs.len() - 1.
pub fn concat_with_xfade(
    runner: &dyn FfmpegRunner,
    inputs: &[(Utf8PathBuf, f32)], // (path, duration_secs)
    // Per-pair `(name, duration_secs)`. `transitions[k]` is the xfade between
    // `inputs[k]` and `inputs[k+1]`. Use a tiny `duration_secs` (e.g. 0.05)
    // to approximate a hard cut — useful when the outgoing clip already has
    // a speed-ramp tail and you don't want the xfade masking it.
    transitions: &[(String, f32)],
    width: u32,
    height: u32,
    fps: u32,
    crf: u32,
    video_fade_in_secs: f32,
    // 0 = no fade. Otherwise a fade-to-black is applied to the last `fade_out_secs`.
    video_fade_out_secs: f32,
    hw: &HwConfig,
    output: &Utf8Path,
) -> Result<()> {
    if inputs.len() < 2 {
        anyhow::bail!("concat_with_xfade requires at least two inputs");
    }
    if transitions.len() != inputs.len() - 1 {
        anyhow::bail!(
            "transitions count {} must equal inputs.len()-1 ({})",
            transitions.len(),
            inputs.len() - 1
        );
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let n = inputs.len();
    // Per-pair durations, clamped to ffmpeg's safe minimum. We used
    // 0.05 s for a long time but ffmpeg's chained acrossfade quietly
    // truncates the audio chain when many pairs use a sub-AAC-frame
    // duration (1024 samples / 48 000 Hz ≈ 21.3 ms). 0.10 s is well
    // above one AAC frame and still visually a hard cut.
    let durs: Vec<f32> = transitions.iter().map(|(_, d)| d.max(0.10)).collect();

    // Build the xfade chain. Cumulative output length grows by (D_k - dur_t)
    // for each new input after the first, using THIS pair's transition
    // duration (not a single global value) so a near-zero transition can
    // collapse the seam between a ramped clip and the next.
    //
    //  [0:v][1:v]xfade=transition=T1:duration=D1:offset=O1[v1];
    //  [v1] [2:v]xfade=transition=T2:duration=D2:offset=O2[v2];
    //  ...
    //
    // O_k = cum_k - dur_t_k  where cum_k is the running output total after
    // applying all prior xfades.
    let mut filter = String::new();
    let mut cum: f32 = inputs[0].1;
    for k in 1..n {
        let in_left = if k == 1 {
            "[0:v]".to_string()
        } else {
            format!("[v{}]", k - 1)
        };
        let in_right = format!("[{}:v]", k);
        let out_label = format!("[v{}]", k);
        let dur_t = durs[k - 1];
        let offset = cum - dur_t;
        let trans = &transitions[k - 1].0;
        filter.push_str(&format!(
            "{in_left}{in_right}xfade=transition={trans}:duration={dur:.3}:offset={off:.3}{out_label};",
            dur = dur_t,
            off = offset,
        ));
        cum += inputs[k].1 - dur_t;
    }
    let mut final_v = format!("v{}", n - 1);

    // Optional fade-IN from black at t=0.
    if video_fade_in_secs > 0.0 {
        filter.push_str(&format!(
            "[{prev}]fade=t=in:st=0:d={d:.3}[vfi];",
            prev = final_v,
            d = video_fade_in_secs,
        ));
        final_v = "vfi".to_string();
    }
    // Optional fade-to-black on the last `video_fade_out_secs`. `cum` is the
    // running total = full body duration after the final xfade.
    if video_fade_out_secs > 0.0 && cum > video_fade_out_secs {
        let fade_start = cum - video_fade_out_secs;
        filter.push_str(&format!(
            "[{prev}]fade=t=out:st={st:.3}:d={d:.3}[vfo];",
            prev = final_v,
            st = fade_start,
            d = video_fade_out_secs,
        ));
        final_v = "vfo".to_string();
    }

    // Audio acrossfade chain — uses the same per-pair durations as the
    // video xfade so audio and picture stay aligned at near-zero ("hard
    // cut") seams as well as at long crossfades.
    for k in 1..n {
        let in_left = if k == 1 {
            "[0:a]".to_string()
        } else {
            format!("[a{}]", k - 1)
        };
        let in_right = format!("[{}:a]", k);
        let out_label = format!("[a{}]", k);
        filter.push_str(&format!(
            "{in_left}{in_right}acrossfade=d={dur:.3}:c1=tri:c2=tri{out_label}",
            dur = durs[k - 1],
        ));
        if k != n - 1 {
            filter.push(';');
        }
    }
    let final_a = format!("a{}", n - 1);

    // Spawn ffmpeg with all inputs. We use `warning` here (instead of
    // `error` like the other ffmpeg invocations) so xfade's "input
    // X ended before its xfade range" complaints surface in the log.
    // Those warnings catch drift bugs between concatenated inputs.
    let mut args: Vec<String> = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "warning".into(),
    ];
    for (p, _) in inputs {
        args.push("-i".into());
        args.push(p.to_string());
    }
    args.push("-filter_complex".into());
    args.push(filter);
    args.push("-map".into());
    args.push(format!("[{final_v}]"));
    args.push("-map".into());
    args.push(format!("[{final_a}]"));
    args.push("-pix_fmt".into());
    args.push("yuv420p".into());
    args.extend(hw.encode_args(crf));
    args.push("-r".into());
    args.push(fps.to_string());
    args.push("-s".into());
    args.push(format!("{width}x{height}"));
    args.push("-c:a".into());
    args.push("aac".into());
    args.push("-ar".into());
    args.push(AUDIO_SAMPLE_RATE.to_string());
    args.push("-ac".into());
    args.push("2".into());
    args.push("-movflags".into());
    args.push("+faststart".into());
    args.push(output.to_string());

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    runner.run(&arg_refs)
}

// ---------- drawtext expression construction ----------

fn build_drawtext(t: &TextOverlay, card_dur: f32) -> String {
    let (rgb, alpha) = parse_hex_color(&t.color_hex);
    let alpha_factor = alpha.unwrap_or(1.0);

    // Visibility window. Defaults of (0, card_dur) yield exactly the
    // pre-window-feature behaviour; non-default values stage the layer in
    // and out of view (e.g. for two-stage GoPro-Quik-style title sequences).
    let appear = t.appear_at_secs.max(0.0);
    let disappear = t.disappear_at_secs.min(card_dur).max(appear + 0.05);
    let win_dur = disappear - appear;

    // Fade alpha INSIDE the [appear, disappear] window. Same expression
    // shape as `fade_alpha_expr_window` so the ramp is anchored to t=appear.
    let fade_expr = fade_alpha_expr_window(appear, win_dur, t.fade_in_secs, t.fade_out_secs);
    let full_alpha = format!("({fade_expr})*{alpha_factor:.4}");

    let (x, y) = position_to_xy_with_offset(&t.position, t.x_offset, t.y_offset);

    let mut s = format!(
        "drawtext=text='{txt}':fontsize={size}:fontcolor={rgb}:alpha='{alpha_expr}':x={x}:y={y}",
        txt = escape_drawtext(&t.content),
        size = t.size,
        alpha_expr = full_alpha,
    );

    if let Some(font) = &t.font_path {
        s.push_str(&format!(
            ":fontfile='{}'",
            escape_drawtext(font.as_str())
        ));
    }

    if let Some(bg) = &t.bg_color_hex {
        let (bg_rgb, bg_alpha) = parse_hex_color(bg);
        let bg_pad = t.bg_padding_px.unwrap_or(20);
        let bg_color_arg = match bg_alpha {
            Some(a) => format!("{bg_rgb}@{a:.4}"),
            None => bg_rgb,
        };
        s.push_str(&format!(":box=1:boxcolor={bg_color_arg}:boxborderw={bg_pad}"));
    }

    // Final `enable` so ffmpeg short-circuits the (potentially expensive)
    // text raster outside the visibility window. Saves CPU on long cards
    // with several staged layers.
    s.push_str(&format!(
        ":enable='between(t,{:.4},{:.4})'",
        appear, disappear
    ));
    s
}

fn position_to_xy_with_offset(pos: &str, x_off: i32, y_off: i32) -> (String, String) {
    let (ax, ay) = position_to_xy(pos);
    let x = if x_off == 0 { ax.to_string() } else { format!("({ax})+({x_off})") };
    let y = if y_off == 0 { ay.to_string() } else { format!("({ay})+({y_off})") };
    (x, y)
}

fn parse_hex_color(s: &str) -> (String, Option<f32>) {
    let s = s.trim_start_matches('#');
    if s.len() == 6 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        (format!("0x{}", s), None)
    } else if s.len() == 8 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        let alpha = u8::from_str_radix(&s[6..8], 16).unwrap_or(255) as f32 / 255.0;
        (format!("0x{}", &s[..6]), Some(alpha))
    } else {
        // Pass through named colors as-is (white, black, red, ...).
        (s.to_string(), None)
    }
}

fn position_to_xy(pos: &str) -> (&'static str, &'static str) {
    match pos {
        "top-left" => ("20", "20"),
        "top-right" => ("w-text_w-20", "20"),
        "top-center" => ("(w-text_w)/2", "20"),
        "bottom-left" => ("20", "h-text_h-20"),
        "bottom-right" => ("w-text_w-20", "h-text_h-40"),
        "bottom-center" => ("(w-text_w)/2", "h-text_h-40"),
        _ => ("(w-text_w)/2", "(h-text_h)/2"), // center, default
    }
}

/// Same as `fade_alpha_expr` but for an arbitrary on-screen window
/// `[start, start+duration]`. Returns 0 outside the window, 1 in the middle,
/// linearly ramped at the edges.
fn fade_alpha_expr_window(start: f32, duration: f32, fade_in: f32, fade_out: f32) -> String {
    let end = start + duration;
    let fi = fade_in.max(1e-3);
    let fo = fade_out.max(1e-3);
    let ramp_in = format!("max(0,min(1,(t-({:.4}))/{:.4}))", start, fi);
    let ramp_down = format!("max(0,min(1,(({:.4})-t)/{:.4}))", end, fo);
    format!("min({},{})", ramp_in, ramp_down)
}

/// Build a drawtext filter for an overlay placed at `start`..`start+duration`
/// within the segment's local time. Returns `Some(s)` only when the overlay
/// has positive duration.
fn build_overlay_drawtext(d: &OverlayDirective<'_>) -> Option<String> {
    if d.duration_secs <= 0.0 {
        return None;
    }
    let t = d.overlay;
    let (rgb, alpha) = parse_hex_color(&t.color_hex);
    let alpha_factor = alpha.unwrap_or(1.0);
    let fade_expr = fade_alpha_expr_window(
        d.start_secs,
        d.duration_secs,
        t.fade_in_secs,
        t.fade_out_secs,
    );
    let full_alpha = format!("({fade_expr})*{alpha_factor:.4}");
    let (x, y) = position_to_xy(&t.position);
    let mut s = format!(
        "drawtext=text='{txt}':fontsize={size}:fontcolor={rgb}:alpha='{alpha_expr}':x={x}:y={y}",
        txt = escape_drawtext(&t.content),
        size = t.size,
        alpha_expr = full_alpha,
    );
    if let Some(font) = &t.font_path {
        s.push_str(&format!(":fontfile='{}'", escape_drawtext(font.as_str())));
    }
    if let Some(bg) = &t.bg_color_hex {
        let (bg_rgb, bg_alpha) = parse_hex_color(bg);
        let bg_pad = t.bg_padding_px.unwrap_or(20);
        let bg_color_arg = match bg_alpha {
            Some(a) => format!("{bg_rgb}@{a:.4}"),
            None => bg_rgb,
        };
        s.push_str(&format!(":box=1:boxcolor={bg_color_arg}:boxborderw={bg_pad}"));
    }
    // Bound the text to its window with `enable=` so ffmpeg fully skips it elsewhere.
    s.push_str(&format!(
        ":enable='between(t,{:.4},{:.4})'",
        d.start_secs,
        d.start_secs + d.duration_secs,
    ));
    Some(s)
}

#[cfg(test)]
fn fade_alpha_expr(fade_in: f32, fade_out: f32, dur: f32) -> String {
    // Returns an ffmpeg eval expression that produces 0..1 alpha vs. t.
    let fi = fade_in.max(0.0);
    let fo = fade_out.max(0.0);
    if fi <= 0.0 && fo <= 0.0 {
        return "1".into();
    }
    // Piecewise: ramp up, hold, ramp down.
    // alpha(t) = clamp(t/fi, 0, 1) - clamp((t - (D-fo))/fo, 0, 1)
    //   first term goes 0 → 1 over [0, fi]
    //   second term subtracts a 0 → 1 ramp at the end
    let fi_eff = if fi > 0.0 { fi } else { 1e-3 };
    let fo_eff = if fo > 0.0 { fo } else { 1e-3 };
    let ramp_in = if fi > 0.0 {
        format!("max(0,min(1,t/{fi_eff:.4}))")
    } else {
        "1".into()
    };
    let ramp_out = if fo > 0.0 {
        format!(
            "max(0,min(1,(t-({d_minus_fo:.4}))/{fo_eff:.4}))",
            d_minus_fo = dur - fo
        )
    } else {
        "0".into()
    };
    format!("({ramp_in}-{ramp_out})")
}

fn escape_drawtext(s: &str) -> String {
    // ffmpeg drawtext: escape backslashes, single quotes, colons.
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\\\\\"),
            '\'' => out.push_str("\\\\'"),
            ':' => out.push_str("\\:"),
            _ => out.push(c),
        }
    }
    out
}

fn ffmpeg_color(hex: &str) -> String {
    let (rgb, alpha) = parse_hex_color(hex);
    match alpha {
        Some(a) => format!("{rgb}@{a:.4}"),
        None => rgb,
    }
}

#[cfg(test)]
mod phase5_tests {
    use super::*;

    #[test]
    fn parses_hex_with_and_without_alpha() {
        let (rgb, a) = parse_hex_color("#FFFFFF");
        assert_eq!(rgb, "0xFFFFFF");
        assert!(a.is_none());

        let (rgb, a) = parse_hex_color("#000000AA");
        assert_eq!(rgb, "0x000000");
        let av = a.unwrap();
        assert!((av - 0xAA as f32 / 255.0).abs() < 1e-3);
    }

    #[test]
    fn position_known_anchors() {
        assert_eq!(position_to_xy("center"), ("(w-text_w)/2", "(h-text_h)/2"));
        assert_eq!(position_to_xy("bottom-right").0, "w-text_w-20");
        assert_eq!(position_to_xy("top-center").1, "20");
    }

    #[test]
    fn fade_expr_handles_corners() {
        assert_eq!(fade_alpha_expr(0.0, 0.0, 2.0), "1");
        let s = fade_alpha_expr(0.4, 0.4, 2.5);
        assert!(s.contains("0.4000"));
        assert!(s.contains("2.1000")); // dur - fade_out
    }

    fn make_text_overlay(content: &str, size: u32) -> TextOverlay {
        TextOverlay {
            content: content.into(),
            font_path: None,
            size,
            color_hex: "#FF8800".into(),
            position: "center".into(),
            x_offset: 0,
            y_offset: 0,
            fade_in_secs: 0.3,
            fade_out_secs: 0.3,
            appear_at_secs: 0.0,
            disappear_at_secs: 2.5,
            bg_color_hex: None,
            bg_padding_px: None,
        }
    }

    #[test]
    fn drawtext_includes_text_and_position() {
        let t = make_text_overlay("Hello World", 64);
        let dt = build_drawtext(&t, 2.5);
        assert!(dt.contains("text='Hello World'"));
        assert!(dt.contains("fontsize=64"));
        assert!(dt.contains("0xFF8800"));
        assert!(dt.contains("(w-text_w)/2"));
        // Default window covers the whole card; enable should reflect that.
        assert!(dt.contains("between(t,0.0000,2.5000)"));
    }

    #[test]
    fn drawtext_visibility_window_appears_in_enable() {
        let mut t = make_text_overlay("Stage 2", 60);
        t.appear_at_secs = 1.5;
        t.disappear_at_secs = 3.5;
        let dt = build_drawtext(&t, 5.0);
        assert!(dt.contains("enable='between(t,1.5000,3.5000)'"), "got: {dt}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct MockRunner {
        calls: RefCell<Vec<Vec<String>>>,
    }
    impl MockRunner {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
            }
        }
    }
    // Safety: tests are single-threaded; RefCell is fine.
    unsafe impl Send for MockRunner {}
    unsafe impl Sync for MockRunner {}

    impl FfmpegRunner for MockRunner {
        fn run(&self, args: &[&str]) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|s| s.to_string()).collect());
            Ok(())
        }
        fn version(&self) -> Result<String> {
            Ok("mock".into())
        }
    }

    #[test]
    fn normalize_emits_expected_args() {
        let runner = MockRunner::new();
        let dir = std::env::temp_dir().join("sv_render_test_norm");
        std::fs::create_dir_all(&dir).unwrap();
        let input = camino::Utf8PathBuf::from_path_buf(dir.join("in.mp4")).unwrap();
        let output = camino::Utf8PathBuf::from_path_buf(dir.join("out.mp4")).unwrap();
        let hw = HwConfig::resolve(HwSupport::default(), EncoderPref::Libx264);
        normalize_segment(
            &runner,
            &NormalizeArgs {
                input: &input,
                output: &output,
                start_secs: 1.5,
                duration_secs: 3.0,
                width: 1920,
                height: 1080,
                fps: 30,
                crf: 20,
                lut3d: None,
                stabilize_transforms: None,
                overlays: &[],
                image_overlays: &[],
                input_has_audio: true,
                outgoing_ramp: None,
                is_image: false,
                image_effect: None,
                image_effect_seed: 0,
            },
            &hw,
        )
        .unwrap();
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 1);
        let args = &calls[0];
        assert!(args.contains(&"-ss".into()));
        assert!(args.iter().any(|a| a == "1.500"));
        assert!(args.iter().any(|a| a.contains("scale=1920:1080")));
        assert!(args.iter().any(|a| a.contains("fps=30")));
        assert!(args.contains(&"libx264".into()));
    }

    #[test]
    fn hwconfig_auto_picks_nvenc_when_available() {
        let s = HwSupport {
            cuda_hwaccel: true,
            h264_nvenc: true,
            ..Default::default()
        };
        let hw = HwConfig::resolve(s, EncoderPref::Auto);
        assert_eq!(hw.encoder, EncoderChoice::H264Nvenc);
        assert!(hw.use_cuda_decode);
    }

    #[test]
    fn hwconfig_falls_back_to_libx264_when_no_hw() {
        let hw = HwConfig::resolve(HwSupport::default(), EncoderPref::Auto);
        assert_eq!(hw.encoder, EncoderChoice::Libx264);
        assert!(!hw.use_cuda_decode);
    }

    #[test]
    fn normalize_with_outgoing_ramp_extends_input_and_emits_filter_complex() {
        // 3.0 s segment + 350 ms ramp at 2.5× → input window extended by
        // 350 * (1 - 1/2.5) = 210 ms so the OUTPUT stays exactly 3.0 s.
        let runner = MockRunner::new();
        let dir = std::env::temp_dir().join("sv_render_test_ramp");
        std::fs::create_dir_all(&dir).unwrap();
        let input = camino::Utf8PathBuf::from_path_buf(dir.join("in.mp4")).unwrap();
        let output = camino::Utf8PathBuf::from_path_buf(dir.join("out.mp4")).unwrap();
        let hw = HwConfig::resolve(HwSupport::default(), EncoderPref::Libx264);
        normalize_segment(
            &runner,
            &NormalizeArgs {
                input: &input,
                output: &output,
                start_secs: 0.0,
                duration_secs: 3.0,
                width: 1920,
                height: 1080,
                fps: 30,
                crf: 20,
                lut3d: None,
                stabilize_transforms: None,
                overlays: &[],
                image_overlays: &[],
                input_has_audio: true,
                outgoing_ramp: Some(SpeedRamp { duration_secs: 0.350, factor: 2.5, flash: false }),
                is_image: false,
                image_effect: None,
                image_effect_seed: 0,
            },
            &hw,
        )
        .unwrap();
        let args = &runner.calls.borrow()[0];
        // Input duration extended to 3.210 s.
        assert!(args.iter().any(|a| a == "3.210"), "args = {args:?}");
        // Filter graph went through filter_complex (not -vf).
        assert!(args.iter().any(|a| a == "-filter_complex"));
        // The split + setpts + concat appears in the filter graph.
        let fc = args
            .iter()
            .skip_while(|a| *a != "-filter_complex")
            .nth(1)
            .expect("filter_complex value");
        assert!(fc.contains("split[vf1][vf2]"), "expected video split: {fc}");
        assert!(fc.contains("setpts=(PTS-STARTPTS)/2.5000"), "expected speed-up factor: {fc}");
        assert!(fc.contains("atempo=2.5000"), "expected audio atempo: {fc}");
        assert!(fc.contains("concat=n=2:v=1:a=0[base]"), "expected video concat: {fc}");
        // Both halves of the ramp split are resampled to the project fps so
        // the concat seam doesn't stutter.
        assert!(fc.contains(",fps=30[vn]"), "expected fps lock on normal half: {fc}");
        assert!(fc.contains(",fps=30[vfast]"), "expected fps lock on fast half: {fc}");
    }

    #[test]
    fn normalize_with_flash_ramp_emits_eq_and_white_fade() {
        let runner = MockRunner::new();
        let dir = std::env::temp_dir().join("sv_render_test_flash");
        std::fs::create_dir_all(&dir).unwrap();
        let input = camino::Utf8PathBuf::from_path_buf(dir.join("in.mp4")).unwrap();
        let output = camino::Utf8PathBuf::from_path_buf(dir.join("out.mp4")).unwrap();
        let hw = HwConfig::resolve(HwSupport::default(), EncoderPref::Libx264);
        normalize_segment(
            &runner,
            &NormalizeArgs {
                input: &input,
                output: &output,
                start_secs: 0.0,
                duration_secs: 3.000,
                width: 1920,
                height: 1080,
                fps: 30,
                crf: 20,
                lut3d: None,
                stabilize_transforms: None,
                overlays: &[],
                image_overlays: &[],
                input_has_audio: true,
                outgoing_ramp: Some(SpeedRamp { duration_secs: 0.600, factor: 3.0, flash: true }),
                is_image: false,
                image_effect: None,
                image_effect_seed: 0,
            },
            &hw,
        )
        .unwrap();
        let args = &runner.calls.borrow()[0];
        let fc = args
            .iter()
            .skip_while(|a| *a != "-filter_complex")
            .nth(1)
            .expect("filter_complex value");
        // Output ramp = 600ms / 3.0 = 200ms. flash_dur = max(min(0.04, 0.10), 2/30) = 0.0667.
        // Brightness ramp scaled over the whole ramp_out (0.2 s).
        assert!(
            fc.contains("eq=brightness='0.05*t/0.2000':eval=frame"),
            "expected brightness ramp: {fc}"
        );
        // Fade-to-white over the last 67 ms of the sped-up tail.
        assert!(
            fc.contains("fade=t=out:st=0.1333:d=0.0667:color=white"),
            "expected fade-to-white at ramp tail: {fc}"
        );
        // The flash filter feeds straight into the [vfast] label.
        assert!(
            fc.contains(":color=white[vfast]"),
            "expected flash filter output to feed [vfast]: {fc}"
        );
    }

    #[test]
    fn nvenc_encode_args_use_cq() {
        let s = HwSupport {
            cuda_hwaccel: true,
            h264_nvenc: true,
            ..Default::default()
        };
        let hw = HwConfig::resolve(s, EncoderPref::Auto);
        let args = hw.encode_args(20);
        assert!(args.iter().any(|a| a == "h264_nvenc"));
        assert!(args.iter().any(|a| a == "-cq"));
        assert!(args.iter().any(|a| a == "20"));
        assert!(!args.iter().any(|a| a == "-crf"));
    }
}
