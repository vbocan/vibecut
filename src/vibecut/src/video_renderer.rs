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
//! 5. **Concat** ([`concat_with_cuts`] hard cuts, or [`concat_with_xfade`]
//!    with transitions) — combines intro + segments + outro into one body
//!    file. Both run in one ffmpeg pass and apply the optional video
//!    fade-in / fade-out; the xfade variant adds video-xfade +
//!    audio-acrossfade at every seam.
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
        // Cancellable: a stopped render kills this ffmpeg mid-encode.
        use crate::cancel::CancellableCommand as _;
        let output = match Command::new(&self.binary).args(args).cancellable_output() {
            Ok(o) => o,
            Err(e) if crate::cancel::is_cancelled(&e) => {
                remove_killed_output(args);
                return Err(e);
            }
            Err(e) => return Err(e.context(format!("spawning {} (is it on PATH?)", self.binary))),
        };
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

/// The output file a killed ffmpeg was writing, if it wrote one: by
/// ffmpeg's argument grammar the last argument is the output (`-` for
/// stdout / `-f null -`, which have nothing to clean up).
fn killed_output<'a>(args: &[&'a str]) -> Option<&'a str> {
    let last = *args.last()?;
    (last != "-" && !last.starts_with('-') && !last.contains("://")).then_some(last)
}

/// Delete what a killed ffmpeg left half-written, so a later render can't
/// take it for a finished intermediate.
fn remove_killed_output(args: &[&str]) {
    if let Some(p) = killed_output(args) {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod cancel_tests {
    use super::*;
    use crate::cancel::{is_cancelled, CancelToken};
    use std::time::{Duration, Instant};

    fn tmp(name: &str) -> Utf8PathBuf {
        let d = std::env::temp_dir().join(format!("vibecut_runner_cancel_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Utf8PathBuf::from_path_buf(d).unwrap()
    }

    #[test]
    fn killed_output_is_the_last_argument_unless_stdout() {
        assert_eq!(killed_output(&["-i", "in.mp4", "-c", "copy", "out.mp4"]), Some("out.mp4"));
        assert_eq!(killed_output(&["-i", "in.mp4", "-f", "null", "-"]), None);
        assert_eq!(killed_output(&[]), None);
    }

    #[test]
    fn a_cancelled_encode_is_killed_and_its_partial_output_removed() {
        if NativeRunner::new().version().is_err() {
            eprintln!("ffmpeg not on PATH; skipping");
            return;
        }
        let d = tmp("encode");
        let out = d.join("body.mp4");
        let out_s = out.to_string();
        let token = CancelToken::new();
        let stop = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(700));
            stop.cancel();
        });
        let started = Instant::now();
        let r = crate::progress::with_sink_and_cancel(|_| {}, token, || {
            NativeRunner::new().run(&[
                "-y", "-v", "error", "-nostdin",
                "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30",
                "-t", "600",
                "-c:v", "libx264", "-preset", "ultrafast",
                &out_s,
            ])
        });
        assert!(is_cancelled(&r.unwrap_err()));
        assert!(started.elapsed() < Duration::from_secs(5), "encode wasn't stopped");
        assert!(!out.exists(), "half-written output left behind");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A runner that writes a partial transforms file and then fails, as a
    /// vidstabdetect killed mid-pass does.
    struct HalfTrfRunner(Utf8PathBuf);
    impl FfmpegRunner for HalfTrfRunner {
        fn run(&self, _args: &[&str]) -> Result<()> {
            std::fs::write(&self.0, b"VID.STAB 1\nFrame 1 (List 0 [])\n").unwrap();
            Err(crate::cancel::Cancelled.into())
        }
        fn version(&self) -> Result<String> {
            Ok("mock".into())
        }
    }

    /// Detection runs at the start of every render (several ffmpeg runs
    /// and encoder smoke tests): a Stop must not wait for it to finish.
    #[test]
    fn hardware_detection_obeys_a_stop() {
        let token = CancelToken::new();
        token.cancel();
        let r = crate::progress::with_sink_and_cancel(|_| {}, token, || HwSupport::detect("ffmpeg"));
        assert!(is_cancelled(&r.expect_err("detection ran its ffmpeg probes after a Stop")));
    }

    #[test]
    fn stabilize_detect_never_leaves_a_partial_trf() {
        let d = tmp("trf");
        let trf = d.join("seg_001_abcd.trf");
        let hw = HwConfig::resolve(HwSupport::default(), EncoderPref::Libx264);
        let r = stabilize_detect(&HalfTrfRunner(trf.clone()), Utf8Path::new("in.mp4"), 0.0, 2.0, 640, 360, &trf, &hw);
        assert!(r.is_err());
        assert!(!trf.exists(), "a partial .trf would be a cache hit next render");
        let _ = std::fs::remove_dir_all(&d);
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
    /// `zscale` (zimg) + `tonemap` filters: normalize tone-maps HDR (HLG /
    /// PQ) sources to BT.709 SDR with them. See [`plan_color_conversion`].
    pub zscale: bool,
    pub tonemap: bool,
    /// Native `colorspace` filter — the approximate fallback when `zscale`
    /// is missing.
    pub colorspace: bool,
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
            zscale:       lists_filter(&filt, "zscale"),
            tonemap:      lists_filter(&filt, "tonemap"),
            colorspace:   lists_filter(&filt, "colorspace"),
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

/// Whether `ffmpeg -filters` output lists the filter `name` exactly. Rows
/// look like ` .SC zscale            V->V       Apply resizing, …`: flags,
/// then the name — so `tonemap` does not match `tonemap_opencl`, and a
/// name inside another filter's description does not count.
fn lists_filter(filters: &str, name: &str) -> bool {
    filters
        .lines()
        .any(|l| l.split_whitespace().nth(1) == Some(name))
}

fn capture(binary: &str, args: &[&str]) -> Result<String> {
    // Runs at the start of every render: a Stop must not wait for it.
    use crate::cancel::CancellableCommand as _;
    let out = std::process::Command::new(binary)
        .args(args)
        .cancellable_output()
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
    // Cancellable (a stuck driver can hold it for seconds). A killed probe
    // reads as "unusable", but the pipeline's check right after detection
    // stops the render before that answer is used.
    use crate::cancel::CancellableCommand as _;
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
        .cancellable_output();
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

// ---------- Colour conversion (HDR → SDR) ----------

/// A source's colour parameters in zscale / colorspace option syntax.
/// Untagged fields fall back to what HDR cameras record (BT.2020 primaries,
/// BT.2020 non-constant-luminance matrix, limited range).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorIn {
    pub transfer: &'static str,
    pub primaries: &'static str,
    pub matrix: &'static str,
    /// `tv` (limited) or `pc` (full).
    pub range: &'static str,
}

/// How normalize brings a clip into the shared 8-bit BT.709 working space
/// every normalized clip is in. Decided per chapter by
/// [`plan_color_conversion`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorConversion {
    /// 8-bit SDR (or untagged): nothing to do.
    #[default]
    None,
    /// High-bit-depth SDR (10-bit BT.709, GoPro Flat / Log): only the pixel
    /// format drops to 8-bit — the `-pix_fmt yuv420p` every clip gets.
    /// Same filter graph, and cache key, as `None`.
    PixelFormat,
    /// HDR (HLG / PQ) → BT.709 SDR: zscale linearises (100-nit reference,
    /// the scale `tonemap` assumes), maps BT.2020 → BT.709 primaries,
    /// `tonemap=mobius` rolls highlights off above the SDR range, zscale
    /// re-applies the BT.709 curve.
    ToneMap(ColorIn),
    /// SDR on BT.2020 primaries: zscale maps the gamut to BT.709; no tone
    /// mapping.
    Gamut(ColorIn),
    /// HDR or BT.2020 without zscale / tonemap: the native `colorspace`
    /// filter converts primaries + matrix to BT.709 and reads an HDR
    /// transfer as BT.2020 gamma. HLG keeps about the right brightness
    /// (it is built to look passable on SDR screens) but stays muted; PQ
    /// comes out flat and dim.
    Approximate(ColorIn),
    /// HDR, but ffmpeg has none of zscale / colorspace: left as-is apart
    /// from the pixel format (renders washed out).
    Unsupported,
}

/// Decide how normalize converts a chapter with colour `info`, given the
/// filters this ffmpeg has (`support.zscale` / `tonemap` / `colorspace`).
pub fn plan_color_conversion(
    info: &crate::media_probe::ColorInfo,
    support: &HwSupport,
) -> ColorConversion {
    let hdr = info.hdr_transfer();
    if hdr.is_none() && !info.is_bt2020() {
        return if info.is_high_bit_depth() {
            ColorConversion::PixelFormat
        } else {
            ColorConversion::None
        };
    }
    let mut cin = ColorIn {
        transfer: match (hdr, info.transfer.as_deref()) {
            (Some(t), _) => t.ffmpeg_name(),
            (None, Some("bt2020-12")) => "bt2020-12",
            (None, Some("bt2020-10")) => "bt2020-10",
            (None, _) => "bt709",
        },
        primaries: match info.primaries.as_deref() {
            Some("bt709") => "bt709",
            _ => "bt2020",
        },
        matrix: match info.matrix.as_deref() {
            Some("bt709") => "bt709",
            Some("bt2020c") => "bt2020c",
            _ => "bt2020nc",
        },
        range: match info.range.as_deref() {
            Some("pc") => "pc",
            _ => "tv",
        },
    };
    if support.zscale && (hdr.is_none() || support.tonemap) {
        return match hdr {
            Some(_) => ColorConversion::ToneMap(cin),
            None => ColorConversion::Gamut(cin),
        };
    }
    if support.colorspace {
        if hdr.is_some() {
            // `colorspace` has no HLG / PQ curve; BT.2020 gamma is the
            // SDR-compatible reading of HLG.
            cin.transfer = "bt2020-10";
        }
        if cin.matrix == "bt2020c" {
            cin.matrix = "bt2020nc";
        }
        return ColorConversion::Approximate(cin);
    }
    match hdr {
        Some(_) => ColorConversion::Unsupported,
        None => ColorConversion::PixelFormat,
    }
}

/// The colour filters this process's `ffmpeg` has — only `zscale`,
/// `tonemap` and `colorspace` are filled in — detected once, with one
/// `ffmpeg -filters` (none of [`HwSupport::detect`]'s encoder smoke tests).
/// Enough for [`plan_color_conversion`] outside a render: the UI's frame
/// grabs ([`frame_color_conversion`]).
pub fn color_filter_support() -> HwSupport {
    static SUPPORT: std::sync::OnceLock<HwSupport> = std::sync::OnceLock::new();
    SUPPORT
        .get_or_init(|| match capture("ffmpeg", &["-v", "error", "-filters"]) {
            Ok(filters) => {
                let f = filters.to_lowercase();
                HwSupport {
                    zscale: lists_filter(&f, "zscale"),
                    tonemap: lists_filter(&f, "tonemap"),
                    colorspace: lists_filter(&f, "colorspace"),
                    ..HwSupport::default()
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "ffmpeg -filters failed; HDR frame grabs stay unconverted");
                HwSupport::default()
            }
        })
        .clone()
}

/// How a still frame of `path` (a thumbnail, a frame the UI or an LLM
/// inspects) is converted so it looks like the clip normalize makes of it:
/// the same [`plan_color_conversion`], so HDR (HLG / PQ) footage isn't shown
/// washed out. [`ColorConversion::None`] for SDR and for files that don't
/// probe.
pub fn frame_color_conversion(path: &Utf8Path) -> ColorConversion {
    match crate::media_probe::probe_metadata(path).ok().and_then(|p| p.media?.video) {
        Some(v) => plan_color_conversion(&v.color, &color_filter_support()),
        None => ColorConversion::None,
    }
}

impl ColorConversion {
    /// Filter-chain fragment that converts the clip, inserted right after
    /// the letterbox scale (so the float tone-map runs at output size).
    /// `None` when the clip needs no colour filter.
    pub fn filter(&self) -> Option<String> {
        match self {
            ColorConversion::None | ColorConversion::PixelFormat | ColorConversion::Unsupported => {
                None
            }
            ColorConversion::ToneMap(c) => Some(format!(
                "zscale=tin={t}:pin={p}:min={m}:rin={r}:t=linear:npl=100,\
                 format=gbrpf32le,zscale=p=bt709,\
                 tonemap=tonemap=mobius:desat=0,\
                 zscale=t=bt709:m=bt709:r=tv,format=yuv420p",
                t = c.transfer,
                p = c.primaries,
                m = c.matrix,
                r = c.range,
            )),
            ColorConversion::Gamut(c) => Some(format!(
                "zscale=tin={t}:pin={p}:min={m}:rin={r}:t=bt709:p=bt709:m=bt709:r=tv,\
                 format=yuv420p",
                t = c.transfer,
                p = c.primaries,
                m = c.matrix,
                r = c.range,
            )),
            ColorConversion::Approximate(c) => Some(format!(
                "colorspace=all=bt709:range=tv:itrc={t}:iprimaries={p}:ispace={m}:irange={r}:\
                 format=yuv420p",
                t = c.transfer,
                p = c.primaries,
                m = c.matrix,
                r = c.range,
            )),
        }
    }

    /// Encoder options that tag a converted clip's bitstream BT.709. ffmpeg 7
    /// already copies the tags zscale / colorspace set on the frames; older
    /// builds may not, so they are spelled out. Empty when nothing is
    /// converted, so SDR clips keep their exact command line.
    pub fn output_tags(&self) -> &'static [&'static str] {
        if self.filter().is_none() {
            return &[];
        }
        &[
            "-color_primaries", "bt709",
            "-color_trc", "bt709",
            "-colorspace", "bt709",
            "-color_range", "tv",
        ]
    }

    /// Extra input for the normalize cache key: `Some` only when the
    /// conversion changes the pixels, so unaffected clips keep their key.
    pub fn cache_tag(&self) -> Option<String> {
        self.filter().map(|f| format!("color-v1:{f}"))
    }

    /// One-line description for the run log.
    pub fn describe(&self) -> &'static str {
        match self {
            ColorConversion::None => "8-bit SDR, as-is",
            ColorConversion::PixelFormat => "high bit depth SDR → 8-bit",
            ColorConversion::ToneMap(_) => "HDR → BT.709 SDR (zscale + tonemap)",
            ColorConversion::Gamut(_) => "BT.2020 → BT.709 gamut (zscale)",
            ColorConversion::Approximate(_) => {
                "→ BT.709 via colorspace filter (approximate: ffmpeg has no zscale/tonemap)"
            }
            ColorConversion::Unsupported => {
                "HDR left unconverted: ffmpeg has neither zscale nor colorspace"
            }
        }
    }
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
    /// Colour conversion into 8-bit BT.709 (HDR tone mapping); see
    /// [`plan_color_conversion`].
    pub color: ColorConversion,
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

    // Hard cap at an exact frame count (see `output_frame_count`).
    let frames = output_frame_count(dur, args.fps);
    cmd.extend([
        arg("-filter_complex"), fc,
        arg("-map"), arg("[v]"),
        arg("-map"), arg("[a]"),
        arg("-frames:v"), frames.to_string(),
        arg("-t"), format!("{:.6}", frames as f64 / args.fps.max(1) as f64),
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
    //   scale → pad → [tone-map] → [stabilize] → [lut3d] → setsar → fps
    // Stabilize after scale so its transforms file (built at the same scale)
    // matches; LUT after stabilize so any pixel-cropping from zoom doesn't
    // affect the colour mapping. HDR tone mapping right after the scale:
    // the float maths runs at output size, and a LUT then grades SDR
    // footage the same way it grades every other clip.
    let mut chain = letterbox_filter(args.width, args.height);
    if let Some(f) = args.color.filter() {
        chain.push(',');
        chain.push_str(&f);
    }
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
    // `start_time=0`: after an `-ss` seek the first source frame usually sits
    // a fraction of a frame in, and plain `fps` rounds it into slot 1 —
    // every clip started one frame late. Pin the first output frame to 0
    // (duplicating the first frame into slot 0 when needed).
    chain.push_str(&format!(",setsar=1,fps=fps={fps}:start_time=0", fps = args.fps));

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

    // Frame-exact output: `-ss`/`-t` plus the `fps` filter round the last
    // frame away (a 4.000 s request used to come out as 119 frames), and the
    // beat-locked layout places every cut by absolute frame counts. Read two
    // frames past the window, pad by cloning if the source ends early, then
    // cap video at exactly `frames` and audio at the same length.
    let frames = output_frame_count(args.duration_secs, args.fps);
    let out_t = frames as f64 / args.fps.max(1) as f64;
    let read_pad = 2.0 / args.fps.max(1) as f32;
    chain.push_str(",tpad=stop_mode=clone:stop=2");

    let ss = format!("{:.3}", args.start_secs);
    let t = format!("{:.3}", input_t_secs + read_pad);

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
                 [vn][vfast]concat=n=2:v=1:a=0,tpad=stop_mode=clone:stop=2[base]",
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

    cmd.extend([
        arg("-frames:v"), frames.to_string(),
        arg("-t"), format!("{out_t:.6}"),
        arg("-pix_fmt"), arg("yuv420p"),
    ]);
    // Tone-mapped / gamut-mapped clips are tagged BT.709 explicitly.
    cmd.extend(args.color.output_tags().iter().map(|s| s.to_string()));
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
    let r = runner.run(&refs);
    // vidstabdetect writes the .trf as it goes, and a non-empty .trf is a
    // cache hit next run: never leave a partial one (failed or cancelled).
    if r.is_err() {
        let _ = std::fs::remove_file(transforms_out.as_std_path());
    }
    r
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

/// Concat clips back to back with straight cuts (hard-cut mode), re-encoded
/// in one pass so the project fade-in / fade-out apply over the cards and
/// footage — the stream-copy [`concat_segments`] can't fade.
///
/// `inputs[k]` is `(path, frames)`: the frames the output timeline gives
/// input `k` (a `ChainLayout` with zero-length seams). Each input adds
/// exactly that many frames, and exactly the audio samples under them
/// (padded with silence or trimmed), so every input starts on the frame the
/// timeline puts it on and the body's audio is exactly as long as its
/// picture. Picture and sound are concatenated separately: a clip whose AAC
/// track runs a few ms past its last frame can't push the next clip's
/// picture late.
pub fn concat_with_cuts(
    runner: &dyn FfmpegRunner,
    inputs: &[(Utf8PathBuf, u32)],
    width: u32,
    height: u32,
    fps: u32,
    crf: u32,
    video_fade_in_secs: f32,
    // 0 = no fade. Otherwise a fade-to-black over the last `fade_out_secs`.
    video_fade_out_secs: f32,
    hw: &HwConfig,
    output: &Utf8Path,
) -> Result<()> {
    if inputs.is_empty() {
        anyhow::bail!("concat_with_cuts: no inputs provided");
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let frames: Vec<u32> = inputs.iter().map(|(_, f)| *f).collect();
    let filter = cut_concat_filter(&frames, fps, video_fade_in_secs, video_fade_out_secs);
    let mut args: Vec<String> = vec![arg("-y"), arg("-hide_banner"), arg("-loglevel"), arg("error")];
    for (p, _) in inputs {
        args.push(arg("-i"));
        args.push(p.to_string());
    }
    args.extend([
        arg("-filter_complex"), filter,
        arg("-map"), arg("[vout]"),
        arg("-map"), arg("[aout]"),
        arg("-pix_fmt"), arg("yuv420p"),
    ]);
    args.extend(hw.encode_args(crf));
    args.extend([
        arg("-r"), fps.to_string(),
        arg("-s"), format!("{width}x{height}"),
        arg("-c:a"), arg("aac"),
        arg("-ar"), AUDIO_SAMPLE_RATE.to_string(),
        arg("-ac"), arg("2"),
        arg("-movflags"), arg("+faststart"),
        output.to_string(),
    ]);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    runner.run(&refs)
}

/// The `-filter_complex` graph of [`concat_with_cuts`] for inputs
/// `frames[k]` frames long; its outputs are `[vout]` and `[aout]`.
fn cut_concat_filter(frames: &[u32], fps: u32, fade_in_secs: f32, fade_out_secs: f32) -> String {
    let fps = fps.max(1) as u64;
    let sr = AUDIO_SAMPLE_RATE as u64;
    // Audio sample at a frame boundary, rounded per boundary (not per clip)
    // so the rounding never adds up.
    let sample_at = |frame: u64| (frame * sr + fps / 2) / fps;
    let mut f = String::new();
    let mut start = 0u64;
    for (k, &len) in frames.iter().enumerate() {
        let len = len.max(1) as u64;
        let samples = sample_at(start + len) - sample_at(start);
        start += len;
        f.push_str(&format!(
            "[{k}:v]setsar=1,trim=end_frame={len},setpts=N/({fps}*TB)[v{k}];\
             [{k}:a]aformat=sample_rates={sr}:channel_layouts=stereo,\
             apad=whole_len={samples},atrim=end_sample={samples},asetpts=N/SR/TB[a{k}];"
        ));
    }
    let n = frames.len();
    for k in 0..n {
        f.push_str(&format!("[v{k}]"));
    }
    f.push_str(&format!("concat=n={n}:v=1:a=0[vcat];"));
    for k in 0..n {
        f.push_str(&format!("[a{k}]"));
    }
    f.push_str(&format!("concat=n={n}:v=0:a=1[aout];"));
    // The project fades, over the whole body (cards included), as on the
    // crossfade path.
    let total = start as f64 / fps as f64;
    let mut fades: Vec<String> = Vec::new();
    if fade_in_secs > 0.0 {
        fades.push(format!("fade=t=in:st=0:d={:.6}", fade_in_secs));
    }
    if fade_out_secs > 0.0 && total > fade_out_secs as f64 {
        fades.push(format!(
            "fade=t=out:st={:.6}:d={:.6}",
            total - fade_out_secs as f64,
            fade_out_secs
        ));
    }
    if fades.is_empty() {
        fades.push("null".into());
    }
    f.push_str(&format!("[vcat]{}[vout]", fades.join(",")));
    f
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
    /// The soundtrack's beat interval, `0.0` if unknown. When set, the
    /// smart-loop crossfade is a whole number of beats, so the two copies'
    /// beats coincide through every loop seam (no flamming kicks) and the
    /// beat grid runs on unbroken.
    pub beat_interval_secs: f32,
}

/// Frames a normalized clip of `duration_secs` has: the nearest whole
/// number, at least one. Every normalized clip is exactly this long, so the
/// beat-locked layout can place cuts by frame arithmetic.
pub fn output_frame_count(duration_secs: f64, fps: u32) -> u64 {
    ((duration_secs * fps as f64).round() as u64).max(1)
}

/// Shortest crossfade [`concat_with_xfade`] will build; shorter requests are
/// raised to this (one AAC frame is ~21 ms; chained acrossfades below that
/// silently truncate the audio).
pub const MIN_XFADE_SECS: f32 = 0.10;

/// Most smart-loop copies `mux_with_ducking` chains (natural intro + loops).
const MAX_SOUNDTRACK_COPIES: usize = 64;

/// One stretch of the output during which a single copy of the soundtrack
/// is the one you hear: output time `t` in `[output_start, output_end)`
/// plays soundtrack time `source_start + (t − output_start)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SoundtrackSpan {
    pub output_start: f32,
    pub output_end: f32,
    pub source_start: f32,
}

impl SoundtrackLoopPlan {
    fn is_smart(&self) -> bool {
        self.loop_secs > 0.0 && self.first_play_end_secs > 0.0
    }

    /// Crossfade between consecutive smart-loop copies: ~2 s (a phrase at
    /// 120 BPM), at most 20 % of the loop so short loops keep real content
    /// between seams. With a known beat interval it is rounded to whole
    /// beats: the loop's start and end sit on beats, so a whole-beat
    /// crossfade overlays beat on beat.
    pub fn crossfade_secs(&self) -> f32 {
        let raw = 2.0_f32.min(self.loop_secs * 0.2).max(0.25);
        let beat = self.beat_interval_secs;
        if beat <= 0.0 {
            return raw;
        }
        let max_beats = ((self.loop_secs * 0.2) / beat).floor().max(1.0);
        (raw / beat).round().clamp(1.0, max_beats) * beat
    }

    /// Soundtrack copies the smart loop chains to cover `body_secs`:
    /// the natural intro plus enough loop cycles, capped.
    fn smart_copies(&self, body_secs: f32) -> usize {
        let needed_after_intro = (body_secs - self.first_play_end_secs).max(0.0);
        let cycle_advance = (self.loop_secs - self.crossfade_secs()).max(0.1);
        let n_cycles = ((needed_after_intro / cycle_advance).ceil() as usize).max(1);
        (1 + n_cycles).min(MAX_SOUNDTRACK_COPIES)
    }

    /// Where each soundtrack copy plays in the output, exactly as
    /// [`mux_with_ducking`] lays them out. Inside a smart-loop crossfade the
    /// span boundary is the crossfade's midpoint, where the incoming copy
    /// takes over. `track_secs` is the soundtrack's own length; spans stop at
    /// `body_secs`.
    pub fn spans(&self, body_secs: f32, track_secs: f32) -> Vec<SoundtrackSpan> {
        let mut spans = Vec::new();
        let mut push = |output_start: f32, output_end: f32, source_start: f32| {
            let output_end = output_end.min(body_secs);
            if output_end > output_start {
                spans.push(SoundtrackSpan { output_start, output_end, source_start });
            }
        };
        if self.is_smart() {
            let x = self.crossfade_secs();
            let copies = self.smart_copies(body_secs);
            // Copy i ≥ 1 starts at fpe + (i − 1)·L − i·X: each acrossfade
            // overlaps the incoming copy with the last X seconds of the chain.
            let start_of = |i: usize| {
                self.first_play_end_secs + (i as f32 - 1.0) * self.loop_secs - i as f32 * x
            };
            let first_end = if copies > 1 { start_of(1) + x * 0.5 } else { self.first_play_end_secs };
            push(0.0, first_end, 0.0);
            for i in 1..copies {
                let s = start_of(i);
                let begin = s + x * 0.5;
                let end = if i + 1 < copies { start_of(i + 1) + x * 0.5 } else { s + self.loop_secs };
                push(begin, end, self.loop_start_secs + (begin - s));
            }
        } else if self.loop_secs > 0.0 {
            let mut i = 0usize;
            while (i as f32) * self.loop_secs < body_secs {
                let s = i as f32 * self.loop_secs;
                push(s, s + self.loop_secs, self.loop_start_secs);
                i += 1;
            }
        } else {
            push(0.0, track_secs, 0.0);
        }
        spans
    }
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
    // Total output duration (for fade-out positioning and the `-t` cap).
    // 0 = no fade-out. f64: past 256 s an f32 is only good to ~30 µs, and a
    // cap that falls half an audio sample short loses the last video frame.
    body_duration_secs: f64,
    soundtrack_fade_out_secs: f32,
    loop_plan: SoundtrackLoopPlan,
    output: &Utf8Path,
) -> Result<()> {
    // Fade and loop-copy arithmetic is in f32 like the rest of the graph;
    // only the `-t` cap needs the full precision.
    let body_secs_f32 = body_duration_secs as f32;
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
    // How many times the soundtrack file is opened as an input (inputs 1..).
    let mut soundtrack_inputs = 1usize;
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
        let xfade_secs: f32 = loop_plan.crossfade_secs();
        // Copies = 1 (natural intro) + enough loop cycles to cover the body,
        // each cycle adding (loop_secs − xfade_secs); capped so we don't
        // build an unbounded filter graph for crazy targets. Shared with
        // `SoundtrackLoopPlan::spans`, which the beat-sync check relies on.
        let total_copies = loop_plan.smart_copies(body_secs_f32);
        let n_cycles = total_copies - 1;
        // Each copy reads its own input of the soundtrack file. Splitting
        // one input with `asplit` does NOT work here: `acrossfade` reads its
        // first input to the end before touching the second, and the later
        // `asplit` branches come out empty — the soundtrack stopped after
        // the first play-through (ffmpeg 7.1; see `SoundtrackLoopPlan`).
        soundtrack_inputs = total_copies;
        // Trim each copy: copy 0 = intro, the rest = loud region.
        for i in 0..total_copies {
            let (start, end) = if i == 0 {
                (0.0, soundtrack_first_play_end_secs)
            } else {
                (soundtrack_loop_start_secs, loop_end)
            };
            filtergraph.push_str(&format!(
                "[{input}:a]atrim={start:.4}:{end:.4},aresample={sr},asetpts=N/SR/TB[sttr{i}];",
                input = 1 + i,
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
    if soundtrack_fade_out_secs > 0.0 && body_secs_f32 > soundtrack_fade_out_secs {
        let fade_start = body_secs_f32 - soundtrack_fade_out_secs;
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
    // length the video actually has. Microseconds, not milliseconds: 865
    // frames at 30 fps written as "28.833" cut the audio a third of a
    // millisecond early, and `-shortest` then dropped the last video frame.
    // The same happened past 256 s while the length was carried as an f32
    // (7700 frames = 256.666667 s printed as "256.666656"), hence the f64.
    let body_dur = if body_duration_secs > 0.0 {
        format!("{body_duration_secs:.6}")
    } else {
        // No declared duration: fall back to -shortest only.
        String::new()
    };
    let mut args: Vec<&str> = vec![
        "-y",
        "-hide_banner",
        "-loglevel", "error",
        "-i", body.as_str(),
    ];
    for _ in 0..soundtrack_inputs {
        args.extend(["-i", soundtrack.as_str()]);
    }
    args.extend([
        "-filter_complex", &filtergraph,
        "-map", "0:v:0",
        "-map", "[a]",
        "-c:v", "copy",
        "-c:a", "aac",
        "-b:a", "192k",
        "-shortest",
    ]);
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

/// Transcode the muxed body (with soundtrack) to one final output: bring
/// the picture to the output's shape with `graph` (a filter graph from
/// `[0:v]` to `[v]`, see [`crate::output_format::reframe_graph`]),
/// re-encode video with `video_args` (see
/// [`crate::output_format::output_encode_args`]), and re-encode audio as
/// AAC at `audio_bitrate`.
pub fn transcode_output(
    runner: &dyn FfmpegRunner,
    input: &Utf8Path,
    output: &Utf8Path,
    graph: &str,
    video_args: &[String],
    audio_bitrate: &str,
) -> Result<()> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let cmd = crate::output_format::transcode_args(input, output, graph, video_args, audio_bitrate);
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
    // Frame-exact: the lavfi colour source rounds a fractional duration up
    // (2.867 s at 30 fps came out as 87 frames, not 86), which shifted every
    // beat-locked cut after the card by a frame. Cap video and audio at
    // exactly the card's frame count.
    let frames = output_frame_count(spec.duration_secs as f64, spec.fps);
    cmd.extend([
        arg("-filter_complex"), filter,
        arg("-map"), arg("[v]"),
        arg("-map"), arg("1:a"),
        arg("-frames:v"), frames.to_string(),
        arg("-t"), format!("{:.6}", frames as f64 / spec.fps.max(1) as f64),
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
    let durs: Vec<f32> = transitions.iter().map(|(_, d)| d.max(MIN_XFADE_SECS)).collect();

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
    // applying all prior xfades — rounded to a whole frame and written with
    // enough digits to land exactly on it, by the same layout the manifest
    // timeline and the beat-sync check use (see `output_timeline`).
    let layout = crate::output_timeline::ChainLayout::new(
        &inputs.iter().map(|(_, d)| *d as f64).collect::<Vec<_>>(),
        &durs.iter().map(|&d| d as f64).collect::<Vec<_>>(),
        fps,
    );
    let mut filter = String::new();
    let cum = layout.duration_secs() as f32;
    for k in 1..n {
        let in_left = if k == 1 {
            "[0:v]".to_string()
        } else {
            format!("[v{}]", k - 1)
        };
        let in_right = format!("[{}:v]", k);
        let out_label = format!("[v{}]", k);
        let dur_t = durs[k - 1];
        let offset = layout.start_secs(k);
        let trans = &transitions[k - 1].0;
        filter.push_str(&format!(
            "{in_left}{in_right}xfade=transition={trans}:duration={dur:.3}:offset={off:.6}{out_label};",
            dur = dur_t,
            off = offset,
        ));
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
                color: ColorConversion::None,
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
                color: ColorConversion::None,
                outgoing_ramp: Some(SpeedRamp { duration_secs: 0.350, factor: 2.5, flash: false }),
                is_image: false,
                image_effect: None,
                image_effect_seed: 0,
            },
            &hw,
        )
        .unwrap();
        let args = &runner.calls.borrow()[0];
        // Input window extended to 3.210 s for the ramp, plus a two-frame
        // read pad (3.210 + 2/30 = 3.277).
        assert!(args.iter().any(|a| a == "3.277"), "args = {args:?}");
        // Output is frame-exact: 3.000 s = 90 frames, audio capped to match.
        let after = |flag: &str| args.iter().skip_while(|a| *a != flag).nth(1).cloned();
        assert_eq!(after("-frames:v").as_deref(), Some("90"), "args = {args:?}");
        assert!(args.iter().any(|a| a == "3.000000"), "args = {args:?}");
        // Filter graph went through filter_complex (not -vf).
        assert!(args.iter().any(|a| a == "-filter_complex"));
        // The split + setpts + concat appears in the filter graph.
        let fc = args
            .iter()
            .skip_while(|a| *a != "-filter_complex")
            .nth(1)
            .expect("filter_complex value");
        assert!(fc.contains("trim=2.8600:3.2100"), "ramp reads to 3.210 s: {fc}");
        assert!(fc.contains("fps=fps=30:start_time=0"), "first frame pinned to 0: {fc}");
        assert!(fc.contains("split[vf1][vf2]"), "expected video split: {fc}");
        assert!(fc.contains("setpts=(PTS-STARTPTS)/2.5000"), "expected speed-up factor: {fc}");
        assert!(fc.contains("atempo=2.5000"), "expected audio atempo: {fc}");
        assert!(
            fc.contains("concat=n=2:v=1:a=0,tpad=stop_mode=clone:stop=2[base]"),
            "expected video concat (with end pad): {fc}"
        );
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
                color: ColorConversion::None,
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

    fn color(depth: u32, transfer: Option<&str>, primaries: Option<&str>) -> crate::media_probe::ColorInfo {
        crate::media_probe::ColorInfo {
            pix_fmt: Some(if depth > 8 { "yuv420p10le" } else { "yuv420p" }.into()),
            bit_depth: Some(depth),
            transfer: transfer.map(str::to_string),
            primaries: primaries.map(str::to_string),
            matrix: primaries.map(|p| if p == "bt2020" { "bt2020nc" } else { p }.to_string()),
            range: Some("tv".into()),
        }
    }

    fn full_filters() -> HwSupport {
        HwSupport { zscale: true, tonemap: true, colorspace: true, ..Default::default() }
    }

    #[test]
    fn colour_plan_leaves_sdr_alone_and_tone_maps_hdr() {
        let s = full_filters();
        // 8-bit SDR, tagged or not: nothing.
        assert_eq!(plan_color_conversion(&color(8, None, None), &s), ColorConversion::None);
        assert_eq!(
            plan_color_conversion(&color(8, Some("bt709"), Some("bt709")), &s),
            ColorConversion::None
        );
        // 10-bit BT.709 (GoPro 10-bit SDR, Flat/Log): pixel format only.
        assert_eq!(
            plan_color_conversion(&color(10, Some("bt709"), Some("bt709")), &s),
            ColorConversion::PixelFormat
        );
        assert_eq!(plan_color_conversion(&color(10, None, None), &s), ColorConversion::PixelFormat);
        // HLG and PQ on BT.2020: tone-mapped with the source's own tags.
        let hlg = plan_color_conversion(&color(10, Some("arib-std-b67"), Some("bt2020")), &s);
        assert_eq!(
            hlg,
            ColorConversion::ToneMap(ColorIn {
                transfer: "arib-std-b67",
                primaries: "bt2020",
                matrix: "bt2020nc",
                range: "tv",
            })
        );
        let pq = plan_color_conversion(&color(10, Some("smpte2084"), Some("bt2020")), &s);
        assert!(matches!(pq, ColorConversion::ToneMap(c) if c.transfer == "smpte2084"));
        // HLG tagged on the transfer only: primaries / matrix default to BT.2020.
        let partial = plan_color_conversion(&color(10, Some("arib-std-b67"), None), &s);
        assert!(matches!(
            partial,
            ColorConversion::ToneMap(c) if c.primaries == "bt2020" && c.matrix == "bt2020nc"
        ));
        // SDR on BT.2020 primaries: gamut map, no tone map.
        assert!(matches!(
            plan_color_conversion(&color(10, Some("bt2020-10"), Some("bt2020")), &s),
            ColorConversion::Gamut(c) if c.transfer == "bt2020-10"
        ));
    }

    #[test]
    fn colour_plan_falls_back_without_zscale() {
        let hlg = color(10, Some("arib-std-b67"), Some("bt2020"));
        // No zscale (or no tonemap): colorspace filter, transfer read as BT.2020 gamma.
        for s in [
            HwSupport { colorspace: true, ..Default::default() },
            HwSupport { zscale: true, colorspace: true, ..Default::default() },
        ] {
            assert_eq!(
                plan_color_conversion(&hlg, &s),
                ColorConversion::Approximate(ColorIn {
                    transfer: "bt2020-10",
                    primaries: "bt2020",
                    matrix: "bt2020nc",
                    range: "tv",
                })
            );
        }
        // Nothing usable: HDR is left alone, SDR BT.2020 just changes pixel format.
        let none = HwSupport::default();
        assert_eq!(plan_color_conversion(&hlg, &none), ColorConversion::Unsupported);
        assert_eq!(
            plan_color_conversion(&color(10, Some("bt2020-10"), Some("bt2020")), &none),
            ColorConversion::PixelFormat
        );
        assert_eq!(ColorConversion::Unsupported.filter(), None);
    }

    #[test]
    fn colour_filter_strings() {
        let s = full_filters();
        let tm = plan_color_conversion(&color(10, Some("arib-std-b67"), Some("bt2020")), &s);
        assert_eq!(
            tm.filter().unwrap(),
            "zscale=tin=arib-std-b67:pin=bt2020:min=bt2020nc:rin=tv:t=linear:npl=100,\
             format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=mobius:desat=0,\
             zscale=t=bt709:m=bt709:r=tv,format=yuv420p"
        );
        let approx = plan_color_conversion(
            &color(10, Some("smpte2084"), Some("bt2020")),
            &HwSupport { colorspace: true, ..Default::default() },
        );
        assert_eq!(
            approx.filter().unwrap(),
            "colorspace=all=bt709:range=tv:itrc=bt2020-10:iprimaries=bt2020:ispace=bt2020nc:\
             irange=tv:format=yuv420p"
        );
        let gamut = plan_color_conversion(&color(10, Some("bt2020-10"), Some("bt2020")), &s);
        assert!(gamut.filter().unwrap().starts_with("zscale=tin=bt2020-10:pin=bt2020:"));
        // Only converting plans touch the cache key.
        assert_eq!(ColorConversion::None.cache_tag(), None);
        assert_eq!(ColorConversion::PixelFormat.cache_tag(), None);
        assert!(tm.cache_tag().unwrap().contains("tonemap=mobius"));
        assert_ne!(tm.cache_tag(), approx.cache_tag());
        // Only converting plans retag the output.
        assert!(ColorConversion::None.output_tags().is_empty());
        assert!(ColorConversion::PixelFormat.output_tags().is_empty());
        assert!(ColorConversion::Unsupported.output_tags().is_empty());
        for plan in [tm, approx, gamut] {
            assert!(plan.output_tags().windows(2).any(|w| w == ["-color_trc", "bt709"]));
        }
    }

    fn normalize_args_for<'a>(
        input: &'a camino::Utf8Path,
        output: &'a camino::Utf8Path,
        color: ColorConversion,
    ) -> NormalizeArgs<'a> {
        NormalizeArgs {
            input,
            output,
            start_secs: 0.0,
            duration_secs: 2.0,
            width: 1920,
            height: 1080,
            fps: 30,
            crf: 20,
            lut3d: None,
            stabilize_transforms: None,
            overlays: &[],
            image_overlays: &[],
            input_has_audio: true,
            color,
            outgoing_ramp: None,
            is_image: false,
            image_effect: None,
            image_effect_seed: 0,
        }
    }

    #[test]
    fn normalize_tone_maps_hdr_after_the_scale() {
        let dir = std::env::temp_dir().join("sv_render_test_tonemap");
        std::fs::create_dir_all(&dir).unwrap();
        let input = camino::Utf8PathBuf::from_path_buf(dir.join("in.mp4")).unwrap();
        let output = camino::Utf8PathBuf::from_path_buf(dir.join("out.mp4")).unwrap();
        let hw = HwConfig::resolve(HwSupport::default(), EncoderPref::Libx264);
        let plan = plan_color_conversion(&color(10, Some("arib-std-b67"), Some("bt2020")), &full_filters());

        let runner = MockRunner::new();
        normalize_segment(&runner, &normalize_args_for(&input, &output, plan), &hw).unwrap();
        let args = &runner.calls.borrow()[0];
        let vf = args.iter().skip_while(|a| *a != "-vf").nth(1).expect("-vf value");
        let scale_at = vf.find("scale=1920:1080").expect("letterbox scale");
        let tm_at = vf.find("tonemap=tonemap=mobius").expect("tone map in chain");
        assert!(scale_at < tm_at, "tone map runs after the downscale: {vf}");
        assert!(tm_at < vf.find("fps=fps=30").unwrap(), "{vf}");
        // The chain ends on BT.709 8-bit frames (zscale sets the frame tags
        // the encoder writes).
        assert!(vf.contains("zscale=t=bt709:m=bt709:r=tv,format=yuv420p"), "{vf}");
        let after = |flag: &str| args.iter().skip_while(|a| *a != flag).nth(1).cloned();
        assert_eq!(after("-pix_fmt").as_deref(), Some("yuv420p"));
        // ...and the bitstream is tagged BT.709 explicitly.
        assert_eq!(after("-color_primaries").as_deref(), Some("bt709"));
        assert_eq!(after("-color_trc").as_deref(), Some("bt709"));
        assert_eq!(after("-colorspace").as_deref(), Some("bt709"));
        assert_eq!(after("-color_range").as_deref(), Some("tv"));
        // Output options: after the input, before the output path.
        let tag_at = args.iter().position(|a| a == "-color_trc").unwrap();
        assert!(tag_at > args.iter().position(|a| a == "-i").unwrap());
        assert_eq!(args.last().map(String::as_str), Some(output.as_str()));

        // SDR (8-bit or 10-bit) gets exactly the old command line.
        let plain = MockRunner::new();
        normalize_segment(&plain, &normalize_args_for(&input, &output, ColorConversion::None), &hw).unwrap();
        let deep = MockRunner::new();
        normalize_segment(&deep, &normalize_args_for(&input, &output, ColorConversion::PixelFormat), &hw)
            .unwrap();
        let plain_args = plain.calls.borrow()[0].clone();
        assert_eq!(plain_args, deep.calls.borrow()[0]);
        assert!(!plain_args.iter().any(|a| a.contains("zscale") || a.contains("colorspace")));
        assert!(!plain_args.iter().any(|a| a.starts_with("-color_")));
    }

    #[test]
    fn filter_list_parsing_matches_whole_names() {
        // Excerpt of `ffmpeg -filters` (7.1), lower-cased as `detect` does.
        let full = " T.. = Timeline support\n\
                    ... = Source or sink filter\n\
                    TS. colorspace        V->V       Convert between colorspaces.\n\
                    ... colorspace_cuda   V->V       CUDA accelerated video color converter\n\
                    .S. tonemap           V->V       Conversion to/from different dynamic ranges.\n\
                    ... tonemap_opencl    V->V       Perform HDR to SDR conversion with tonemapping.\n\
                    .SC zscale            V->V       Apply resizing, colorspace and bit depth conversion.\n";
        assert!(lists_filter(full, "zscale"));
        assert!(lists_filter(full, "tonemap"));
        assert!(lists_filter(full, "colorspace"));
        // A build without zimg: only the GPU tone mappers and words in descriptions.
        let no_zimg = " ... tonemap_opencl    V->V       Perform HDR to SDR conversion with tonemapping.\n\
                       ... tonemap_vaapi     V->V       VAAPI VPP for tone-mapping\n\
                       TS. colorspace        V->V       Convert between colorspaces.\n\
                       ... scale             V->V       Scale the input video size, not zscale.\n";
        assert!(!lists_filter(no_zimg, "zscale"));
        assert!(!lists_filter(no_zimg, "tonemap"));
        assert!(lists_filter(no_zimg, "colorspace"));
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

    #[test]
    fn xfade_offsets_sit_exactly_on_frames() {
        // Three 1 s clips: a 0.1 s near-cut, then a 13-frame crossfade. Input
        // 1 starts at frame 27, input 2 at frame 44 (1.4667 s). Written to
        // three decimals that is "1.467", past the frame boundary — ffmpeg
        // starts a transition on the first frame at or after its offset, so
        // the clip came in a frame late. Offsets carry six decimals.
        let runner = MockRunner::new();
        let out = camino::Utf8PathBuf::from("/tmp/sv_xfade_offsets/body.mp4");
        let hw = HwConfig::resolve(HwSupport::default(), EncoderPref::Libx264);
        let inputs: Vec<(camino::Utf8PathBuf, f32)> = vec![
            ("a.mp4".into(), 1.0),
            ("b.mp4".into(), 1.0),
            ("c.mp4".into(), 1.0),
        ];
        let trans = vec![("fade".to_string(), 0.1), ("fade".to_string(), 1.0 - 17.0 / 30.0)];
        concat_with_xfade(&runner, &inputs, &trans, 64, 36, 30, 20, 0.0, 0.0, &hw, &out).unwrap();
        let calls = runner.calls.borrow();
        let fc = calls[0].iter().find(|a| a.contains("xfade=")).unwrap();
        assert!(fc.contains("offset=0.900000[v1]"), "{fc}");
        assert!(fc.contains("offset=1.466667[v2]"), "{fc}");
    }

    #[test]
    fn hard_cut_concat_keeps_the_cards_and_the_project_fades() {
        // Hard-cut mode used to stream-copy the clips alone: no intro or
        // outro card, no fade from or to black. Intro card (90 frames), a
        // clip (61), an outro card (45), straight cuts, 0.2 s in / 1.5 s out.
        let runner = MockRunner::new();
        let out = camino::Utf8PathBuf::from("/tmp/sv_cut_concat/body.mp4");
        let hw = HwConfig::resolve(HwSupport::default(), EncoderPref::Libx264);
        let inputs: Vec<(camino::Utf8PathBuf, u32)> =
            vec![("intro.mp4".into(), 90), ("seg_001.mp4".into(), 61), ("outro.mp4".into(), 45)];
        concat_with_cuts(&runner, &inputs, 64, 36, 30, 20, 0.2, 1.5, &hw, &out).unwrap();
        let calls = runner.calls.borrow();
        let args = &calls[0];
        let inputs_passed: Vec<&str> = args
            .windows(2)
            .filter(|w| w[0] == "-i")
            .map(|w| w[1].as_str())
            .collect();
        assert_eq!(inputs_passed, vec!["intro.mp4", "seg_001.mp4", "outro.mp4"]);
        let fc = args.iter().find(|a| a.contains("concat=")).unwrap();
        // Straight cuts: no crossfade anywhere.
        assert!(!fc.contains("xfade") && !fc.contains("acrossfade"), "{fc}");
        // 196 frames = 6.5333 s; the fade-out ends on the last frame.
        assert!(fc.contains("[vcat]fade=t=in:st=0:d=0.200000,fade=t=out:st=5.033333:d=1.500000[vout]"), "{fc}");
        assert!(args.windows(2).any(|w| w[0] == "-map" && w[1] == "[vout]"));
        assert!(args.windows(2).any(|w| w[0] == "-map" && w[1] == "[aout]"));
    }

    #[test]
    fn hard_cut_concat_holds_every_input_to_its_timeline_frames() {
        // Each input is trimmed to the frames the timeline gives it, and its
        // audio padded/trimmed to exactly the samples under those frames, so
        // a long AAC tail can't push the next clip late.
        let fc = cut_concat_filter(&[90, 61, 45], 30, 0.0, 0.0);
        for (k, (frames, samples)) in [(90, 144_000), (61, 97_600), (45, 72_000)].iter().enumerate() {
            assert!(fc.contains(&format!("[{k}:v]setsar=1,trim=end_frame={frames},")), "{fc}");
            assert!(
                fc.contains(&format!("apad=whole_len={samples},atrim=end_sample={samples},asetpts=N/SR/TB[a{k}]")),
                "{fc}"
            );
        }
        // Picture and sound concatenated separately; no fades asked for.
        assert!(fc.contains("[v0][v1][v2]concat=n=3:v=1:a=0[vcat]"), "{fc}");
        assert!(fc.contains("[a0][a1][a2]concat=n=3:v=0:a=1[aout]"), "{fc}");
        assert!(fc.ends_with("[vcat]null[vout]"), "{fc}");
    }

    #[test]
    fn hard_cut_audio_rounding_does_not_accumulate() {
        // At 7 fps a frame is 6857.14 samples: per-clip rounding would drift,
        // per-boundary rounding keeps the total at round(frames × 48000 / 7).
        // Five 2-frame clips: 5 × round(13714.29) = 68570, one sample short.
        let fc = cut_concat_filter(&[2, 2, 2, 2, 2], 7, 0.0, 0.0);
        let total: u64 = fc
            .split("atrim=end_sample=")
            .skip(1)
            .map(|s| s.split(',').next().unwrap().parse::<u64>().unwrap())
            .sum();
        assert_eq!(total, 68_571);
    }

    #[test]
    fn mux_caps_at_the_full_body_length() {
        // A 865-frame body (28.8333 s) capped at "-t 28.833" lost its last
        // frame in the mux; the cap must not fall short of the body.
        let runner = MockRunner::new();
        let out = camino::Utf8PathBuf::from("/tmp/sv_mux_cap/out.mp4");
        let body_secs = 865.0_f64 / 30.0;
        mux_with_ducking(
            &runner,
            camino::Utf8Path::new("body.mp4"),
            camino::Utf8Path::new("song.mp3"),
            &[(5.0, 9.0, 0.3)],
            -18.0,
            body_secs,
            1.5,
            SoundtrackLoopPlan::default(),
            &out,
        )
        .unwrap();
        let calls = runner.calls.borrow();
        let t: f64 = calls[0]
            .windows(2)
            .find(|w| w[0] == "-t")
            .map(|w| w[1].parse().unwrap())
            .expect("-t cap");
        // Within a microsecond of the true end (well under one audio sample).
        assert!((t - 865.0 / 30.0).abs() < 2e-6, "-t {t}");
    }

    #[test]
    fn mux_cap_keeps_the_last_frame_of_long_bodies() {
        // Past 256 s an f32 length is only good to ~30 µs: 7700 frames
        // (256.666667 s) came out as "-t 256.666656", 11 µs short — over
        // half an audio sample, so the mux lost the last video frame. The
        // cap must not fall short by half a sample at any length.
        let half_sample = 0.5 / AUDIO_SAMPLE_RATE as f64;
        for (frames, fps) in [(7700u32, 30u32), (8008, 30), (15_001, 25), (35_999, 60)] {
            let runner = MockRunner::new();
            let out = camino::Utf8PathBuf::from("/tmp/sv_mux_cap_long/out.mp4");
            let body_secs = frames as f64 / fps as f64;
            mux_with_ducking(
                &runner,
                camino::Utf8Path::new("body.mp4"),
                camino::Utf8Path::new("song.mp3"),
                &[(5.0, 9.0, 0.3)],
                -18.0,
                body_secs,
                1.5,
                SoundtrackLoopPlan::default(),
                &out,
            )
            .unwrap();
            let calls = runner.calls.borrow();
            let t: f64 = calls[0]
                .windows(2)
                .find(|w| w[0] == "-t")
                .map(|w| w[1].parse().unwrap())
                .expect("-t cap");
            assert!(t > body_secs - half_sample, "{frames} frames at {fps} fps: -t {t} < {body_secs}");
            assert!(t - body_secs < 1e-6, "{frames} frames at {fps} fps: -t {t}");
        }
    }
}
