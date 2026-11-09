//! Platform output presets and reframing.
//!
//! An `[[outputs]]` entry can name a platform `preset` ("youtube",
//! "reels", "square", "whatsapp", …) instead of spelling out resolution,
//! CRF and bitrates; explicit keys still win over the preset's values
//! ([`resolve_output`]). The body is rendered once, in the master shape
//! ([`body_resolution`]), and every output whose aspect differs from it
//! is reframed in its own encode pass with a [`Fit`]:
//!
//! * `blur` (the default): the whole frame, fitted, over a blurred and
//!   slightly darkened fill made from the same frame;
//! * `crop`: a centre crop that fills the output;
//! * `pad`: the whole frame, fitted, with black bars;
//! * `smart`: a crop that follows the action along a camera path measured
//!   on the video ([`crate::smart_reframe`]), the centre crop where
//!   nothing clearly moves.
//!
//! [`reframe_graph`] builds that filter graph ([`reframe_graph_with_path`]
//! with the path a `smart` fit follows). The same graph drives the
//! render's per-output pass and [`reformat_video`], the fast one-pass
//! reformat of a finished render (the Share sheet's "make a vertical
//! version").
//!
//! Titles: intro/outro cards are rendered once, at the body's resolution
//! (their text sizes are master pixels), and reach other shapes through
//! the reframe, scaled with the rest of the frame. Blur and pad put that
//! frame in the middle band of a 9:16 output (y 656–1264 of 1080×1920),
//! clear of the platform UI at the top and bottom — but a title as wide as
//! the master frame would then run edge to edge, under the side buttons.
//! So during the title spans ([`card_windows`], recorded in the manifest)
//! a portrait output narrower than the master shows the frame at
//! [`TITLE_SAFE_SCALE`] of its fitted width, inside the 9:16 safe area,
//! and eases back to full width over [`TITLE_SAFE_RAMP_SECS`] after the
//! title (and in again before an outro). A centre crop would cut a title
//! outright, so during those spans a crop output shows the blur fit
//! instead. A square output keeps the full fit: no platform UI covers it.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use crate::project_config::{Encoder, RenderOutput};
use crate::smart_reframe::{CameraPath, PanAxis};
use crate::video_renderer::{EncoderChoice, EncoderPref, HwConfig, HwSupport};

/// How a frame of one aspect fills an output of another. Only applies when
/// the aspects differ; a same-aspect output is a plain resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export, rename_all = "snake_case"))]
pub enum Fit {
    /// The whole frame, fitted, over a blurred, darkened fill of itself.
    #[default]
    Blur,
    /// Centre crop that fills the output.
    Crop,
    /// The whole frame, fitted, with black bars.
    Pad,
    /// A crop that follows the action ([`crate::smart_reframe`]); the
    /// centre crop where nothing clearly moves.
    Smart,
}

impl Fit {
    pub fn name(self) -> &'static str {
        match self {
            Fit::Blur => "blur",
            Fit::Crop => "crop",
            Fit::Pad => "pad",
            Fit::Smart => "smart",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "blur" => Some(Fit::Blur),
            "crop" => Some(Fit::Crop),
            "pad" => Some(Fit::Pad),
            "smart" => Some(Fit::Smart),
            _ => None,
        }
    }

    /// Every fit name, for error messages.
    pub const NAMES: &'static str = "blur, crop, pad or smart";
}

/// A platform preset for one output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputPreset {
    /// The master render, as configured (today's behaviour).
    Master,
    Youtube,
    Reels,
    Tiktok,
    Shorts,
    Square,
    Whatsapp,
    /// 4:5, 1080×1350: the tallest a feed post shows uncropped.
    Portrait,
}

/// What a preset fills in for keys the output doesn't set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresetSpec {
    pub width: u32,
    pub height: u32,
    pub crf: u32,
    pub audio_bitrate: &'static str,
    /// Peak video bitrate (bits/s), for platforms with a file-size limit.
    pub max_bitrate: Option<u64>,
    /// A shape derived from the master (vertical, square) rather than the
    /// master itself: it never sets the body's resolution.
    pub derived: bool,
}

impl OutputPreset {
    pub const ALL: [OutputPreset; 8] = [
        OutputPreset::Master,
        OutputPreset::Youtube,
        OutputPreset::Reels,
        OutputPreset::Tiktok,
        OutputPreset::Shorts,
        OutputPreset::Square,
        OutputPreset::Whatsapp,
        OutputPreset::Portrait,
    ];

    pub fn name(self) -> &'static str {
        match self {
            OutputPreset::Master => "master",
            OutputPreset::Youtube => "youtube",
            OutputPreset::Reels => "reels",
            OutputPreset::Tiktok => "tiktok",
            OutputPreset::Shorts => "shorts",
            OutputPreset::Square => "square",
            OutputPreset::Whatsapp => "whatsapp",
            OutputPreset::Portrait => "portrait",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|p| p.name() == s)
    }

    /// Every preset name, for error messages.
    pub fn names() -> String {
        Self::ALL.map(|p| p.name()).join(", ")
    }

    pub fn spec(self) -> PresetSpec {
        let vertical = PresetSpec {
            width: 1080,
            height: 1920,
            crf: DEFAULT_CRF,
            audio_bitrate: DEFAULT_AUDIO_BITRATE,
            max_bitrate: None,
            derived: true,
        };
        match self {
            OutputPreset::Master => PresetSpec {
                width: DEFAULT_RESOLUTION.0,
                height: DEFAULT_RESOLUTION.1,
                crf: DEFAULT_CRF,
                audio_bitrate: DEFAULT_AUDIO_BITRATE,
                max_bitrate: None,
                derived: false,
            },
            OutputPreset::Youtube => PresetSpec {
                width: 1920,
                height: 1080,
                // YouTube re-encodes everything; give it a little more to work with.
                crf: 18,
                audio_bitrate: DEFAULT_AUDIO_BITRATE,
                max_bitrate: None,
                derived: false,
            },
            OutputPreset::Reels | OutputPreset::Tiktok | OutputPreset::Shorts => vertical,
            OutputPreset::Square => PresetSpec { width: 1080, height: 1080, ..vertical },
            OutputPreset::Portrait => PresetSpec { width: 1080, height: 1350, ..vertical },
            // WhatsApp's video limit is 16 MB. A 3 Mbit/s video peak plus
            // 128 kbit/s audio keeps 30 s at or under ~11.7 MB.
            OutputPreset::Whatsapp => PresetSpec {
                width: 720,
                height: 1280,
                crf: 23,
                audio_bitrate: "128k",
                max_bitrate: Some(3_000_000),
                derived: true,
            },
        }
    }
}

pub const DEFAULT_RESOLUTION: (u32, u32) = (1920, 1080);
pub const DEFAULT_CRF: u32 = 20;
pub const DEFAULT_AUDIO_BITRATE: &str = "192k";

/// `-preset` names libx264 and libx265 take.
const X26X_PRESETS: &[&str] = &[
    "ultrafast", "superfast", "veryfast", "faster", "fast", "medium", "slow", "slower",
    "veryslow", "placebo",
];
/// `-preset` names h264_nvenc and hevc_nvenc take.
const NVENC_PRESETS: &[&str] = &[
    "p1", "p2", "p3", "p4", "p5", "p6", "p7", "default", "slow", "medium", "fast", "hp", "hq",
    "bd", "ll", "llhq", "llhp", "lossless", "losslesshp",
];
/// `-quality` names h264_amf takes.
const AMF_PRESETS: &[&str] = &["speed", "balanced", "quality"];
/// `-preset` names h264_qsv takes.
const QSV_PRESETS: &[&str] = &["veryfast", "faster", "fast", "medium", "slow", "slower", "veryslow"];

/// The preset names `encoder` takes (its `-preset`, or AMF's `-quality`).
fn encoder_presets(encoder: EncoderChoice) -> &'static [&'static str] {
    match encoder {
        EncoderChoice::Libx264 | EncoderChoice::Libx265 => X26X_PRESETS,
        EncoderChoice::H264Nvenc | EncoderChoice::HevcNvenc => NVENC_PRESETS,
        EncoderChoice::H264Amf => AMF_PRESETS,
        EncoderChoice::H264Qsv => QSV_PRESETS,
    }
}

/// Whether `encoder` takes `name` as its preset. A name for another
/// encoder (`p5` on libx264, `veryfast` on NVENC) makes ffmpeg fail the
/// pass, and `encoder = "auto"` only settles the encoder at render time.
pub fn encoder_accepts_preset(encoder: EncoderChoice, name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    encoder_presets(encoder).contains(&name.as_str())
}

/// An encoder preset name for any encoder — what `preset` held before it
/// named platforms. Still accepted there, as an `encoder_preset`, so older
/// configs keep loading.
pub fn is_encoder_preset_name(s: &str) -> bool {
    let s = s.trim().to_ascii_lowercase();
    [X26X_PRESETS, NVENC_PRESETS, AMF_PRESETS, QSV_PRESETS]
        .iter()
        .any(|names| names.contains(&s.as_str()))
}

/// One output with every preset default and override applied.
#[derive(Debug, Clone)]
pub struct ResolvedOutput {
    pub path: Utf8PathBuf,
    pub preset: Option<OutputPreset>,
    pub width: u32,
    pub height: u32,
    /// The fit to use if this output's aspect differs from the body's.
    pub fit: Fit,
    pub encoder: Encoder,
    pub crf: u32,
    pub encoder_preset: Option<String>,
    pub audio_bitrate: String,
    /// Peak video bitrate in bits/s.
    pub max_bitrate: Option<u64>,
}

impl ResolvedOutput {
    /// See [`PresetSpec::derived`].
    pub fn is_derived(&self) -> bool {
        self.preset.is_some_and(|p| p.spec().derived)
    }

    pub fn resolution(&self) -> String {
        format!("{}x{}", self.width, self.height)
    }
}

/// Apply `o.preset`'s values to every key `o` leaves unset. Explicit keys
/// always win. Errors name the offending key (the caller adds the index).
pub fn resolve_output(o: &RenderOutput) -> Result<ResolvedOutput, String> {
    let (preset, legacy_encoder_preset) = match o.preset.as_deref().map(str::trim) {
        None | Some("") => (None, None),
        Some(s) => match OutputPreset::parse(s) {
            Some(p) => (Some(p), None),
            None if is_encoder_preset_name(s) => (None, Some(s.to_string())),
            None => {
                return Err(format!(
                    "preset {s:?} is not a platform preset (one of: {}); \
                     encoder presets go in `encoder_preset`",
                    OutputPreset::names()
                ))
            }
        },
    };
    let spec = preset.map(OutputPreset::spec);
    let (width, height) = match o.resolution.as_deref() {
        Some(r) => parse_resolution(r).ok_or_else(|| {
            format!("resolution must look like \"WIDTHxHEIGHT\", got {r:?}")
        })?,
        None => match spec {
            Some(s) => (s.width, s.height),
            None => {
                return Err("needs a `resolution` (e.g. \"1920x1080\") or a `preset`".into())
            }
        },
    };
    let max_bitrate = match o.max_bitrate.as_deref() {
        Some(s) => parse_bitrate_cap(s).ok_or_else(|| {
            format!("max_bitrate must look like \"3M\" or \"2500k\" (or \"none\"), got {s:?}")
        })?,
        None => spec.and_then(|s| s.max_bitrate),
    };
    if let Some(b) = o.audio_bitrate.as_deref() {
        if !is_bitrate_string(b) {
            return Err(format!("audio_bitrate must look like \"192k\", got {b:?}"));
        }
    }
    if let Some(p) = o.encoder_preset.as_deref() {
        if p.is_empty() || !p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return Err(format!("encoder_preset must be a preset name such as \"p5\" or \"veryfast\", got {p:?}"));
        }
    }
    Ok(ResolvedOutput {
        path: o.path.clone(),
        preset,
        width,
        height,
        fit: o.fit.unwrap_or_default(),
        encoder: o.encoder,
        crf: o.crf.or(spec.map(|s| s.crf)).unwrap_or(DEFAULT_CRF),
        encoder_preset: o.encoder_preset.clone().or(legacy_encoder_preset),
        audio_bitrate: o
            .audio_bitrate
            .clone()
            .or(spec.map(|s| s.audio_bitrate.to_string()))
            .unwrap_or_else(|| DEFAULT_AUDIO_BITRATE.to_string()),
        max_bitrate,
    })
}

/// `"WIDTHxHEIGHT"` → `(w, h)`, both non-zero.
pub fn parse_resolution(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x')?;
    let w: u32 = w.trim().parse().ok()?;
    let h: u32 = h.trim().parse().ok()?;
    (w > 0 && h > 0).then_some((w, h))
}

/// A bitrate as ffmpeg takes it: digits with an optional `k` / `M` suffix
/// (`"192k"`, `"3M"`, `"128000"`).
pub fn is_bitrate_string(s: &str) -> bool {
    let digits = s.strip_suffix(['k', 'K', 'm', 'M']).unwrap_or(s);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// `"3M"`, `"2500k"`, `"1.5M"`, `"128000"` → bits/s.
pub fn parse_bitrate(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mult) = match s.chars().last()? {
        'k' | 'K' => (&s[..s.len() - 1], 1_000.0),
        'm' | 'M' => (&s[..s.len() - 1], 1_000_000.0),
        _ => (s, 1.0),
    };
    let v: f64 = num.trim().parse().ok()?;
    (v.is_finite() && v >= 0.0).then(|| (v * mult).round() as u64)
}

/// A `max_bitrate` value: a bitrate, or `"none"` / `"0"` to drop a
/// preset's cap. `None` = not a valid value; `Some(None)` = no cap.
fn parse_bitrate_cap(s: &str) -> Option<Option<u64>> {
    let t = s.trim().to_ascii_lowercase();
    if t == "none" || t == "off" || t.is_empty() {
        return Some(None);
    }
    parse_bitrate(&t).map(|b| (b > 0).then_some(b))
}

/// Bits/s → an ffmpeg bitrate string (`3000000` → `"3M"`).
pub fn format_bitrate(bps: u64) -> String {
    if bps >= 1_000_000 && bps % 1_000_000 == 0 {
        format!("{}M", bps / 1_000_000)
    } else if bps >= 1_000 && bps % 1_000 == 0 {
        format!("{}k", bps / 1_000)
    } else {
        bps.to_string()
    }
}

fn even(x: u32) -> u32 {
    (x & !1).max(2)
}

/// Same aspect ratio, within 1 %.
pub fn same_aspect(a: (u32, u32), b: (u32, u32)) -> bool {
    let (aw, ah) = (a.0.max(1) as f64, a.1.max(1) as f64);
    let (bw, bh) = (b.0.max(1) as f64, b.1.max(1) as f64);
    ((aw / ah) / (bw / bh) - 1.0).abs() <= 0.01
}

/// The body's render resolution: the largest master-shaped output (no
/// preset, `master`, `youtube`), ties going to the first. When every
/// output is a derived shape (vertical, square), a 16:9 master as tall as
/// the largest output's short side, but never smaller than
/// [`DEFAULT_RESOLUTION`]: card text sizes are absolute pixels, set for a
/// 1080p frame, so a smaller body (1280x720 for a WhatsApp-only project)
/// would cut a title off before the reframe ever sees it.
pub fn body_resolution(outs: &[ResolvedOutput]) -> (u32, u32) {
    let mut best: Option<(u32, u32)> = None;
    let mut best_pixels = 0u64;
    for o in outs.iter().filter(|o| !o.is_derived()) {
        let p = o.width as u64 * o.height as u64;
        if p > best_pixels {
            best_pixels = p;
            best = Some((o.width, o.height));
        }
    }
    if let Some(b) = best {
        return b;
    }
    match outs.iter().map(|o| o.width.min(o.height)).max() {
        Some(short) => {
            let short = short.max(DEFAULT_RESOLUTION.1);
            (even((short * 16).div_ceil(9)), even(short))
        }
        None => (0, 0),
    }
}

/// The fit an output gets from a `source` frame: `None` when the aspects
/// match (a plain resize), else the output's fit.
pub fn reframe_fit(source: (u32, u32), target: (u32, u32), fit: Fit) -> Option<Fit> {
    (!same_aspect(source, target)).then_some(fit)
}

/// Whether an output needs its own encode pass, rather than being the
/// body's mux written straight to its path.
pub fn needs_own_pass(body: (u32, u32), o: &ResolvedOutput) -> bool {
    (o.width, o.height) != body || o.max_bitrate.is_some()
}

/// Output-time spans that carry the intro/outro titles, card or overlay
/// mode: `[0, intro]` and `[total − outro, total]`. A card's span
/// includes its crossfade into (or out of) the footage.
pub fn card_windows(intro_secs: f64, outro_secs: f64, total_secs: f64) -> Vec<[f64; 2]> {
    let mut w = Vec::new();
    if intro_secs > 0.0 {
        w.push([0.0, intro_secs.min(total_secs.max(0.0))]);
    }
    if outro_secs > 0.0 && total_secs > 0.0 {
        w.push([(total_secs - outro_secs).max(0.0), total_secs]);
    }
    w
}

/// Blur-fill source scale: the fill is blurred at a quarter of the
/// output's size, then scaled up (cheap, and smooth after the blur).
const BLUR_FILL_DIVISOR: u32 = 4;
/// Gaussian sigma of the fill blur, at the quarter-size scale (80 px at
/// full size): an ambient wash of colour, with no readable shapes — a
/// white title on a card only shows as a soft glow.
const BLUR_FILL_SIGMA: u32 = 20;
/// Luma multiplier that darkens the fill so the fitted frame stands out,
/// applied above the format's black level (`minval`: 16 in limited range).
const BLUR_FILL_DARKEN: f32 = 0.72;
/// Width of the fitted frame during titles, as a share of its full fitted
/// width: leaves 12 % each side of a 9:16 frame, clear of the side UI
/// (TikTok / Reels / Shorts buttons) and inside every platform's safe area.
pub const TITLE_SAFE_SCALE: f64 = 0.76;
/// How long the frame takes to grow back to full width after a title.
pub const TITLE_SAFE_RAMP_SECS: f64 = 0.4;
/// Gaps between title spans shorter than this join the spans.
const MIN_PIECE_SECS: f64 = 0.01;

fn scale_fit(w: u32, h: u32) -> String {
    format!("scale={w}:{h}:flags=lanczos:force_original_aspect_ratio=decrease")
}

fn pad_chain(w: u32, h: u32) -> String {
    format!("{},pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1", scale_fit(w, h))
}

fn crop_chain(w: u32, h: u32) -> String {
    format!("scale={w}:{h}:flags=lanczos:force_original_aspect_ratio=increase,crop={w}:{h},setsar=1")
}

/// [`crop_chain`] with the window following `path` over `[from, to)` of
/// it, in a clock that starts at `from` (the chain resets its input's
/// timestamps to 0). The crop is positioned per frame by
/// [`crate::smart_reframe::offset_expr`], on the frame already scaled to
/// cover the output, so a slow pan moves in output pixels, not in steps
/// of the source's. The plain centre crop where the path stays centred.
fn follow_chain(w: u32, h: u32, path: &CameraPath, from: f64, to: Option<f64>) -> String {
    if path.is_centred(from, to) {
        return crop_chain(w, h);
    }
    let e = crate::smart_reframe::offset_expr(path, from, to);
    let (x, y) = match path.axis {
        PanAxis::X => (format!("'(in_w-out_w)*({e})'"), "(in_h-out_h)/2".to_string()),
        PanAxis::Y => ("(in_w-out_w)/2".to_string(), format!("'(in_h-out_h)*({e})'")),
    };
    format!(
        "setpts=PTS-STARTPTS,scale={w}:{h}:flags=lanczos:force_original_aspect_ratio=increase,\
         crop={w}:{h}:{x}:{y},setsar=1"
    )
}

/// The fill: the frame scaled to cover `w × h`, blurred and darkened (both
/// at the small size, where they cost next to nothing), then scaled up.
fn blur_fill(w: u32, h: u32) -> String {
    let bw = even(w / BLUR_FILL_DIVISOR);
    let bh = even(h / BLUR_FILL_DIVISOR);
    format!(
        "scale={bw}:{bh}:force_original_aspect_ratio=increase,crop={bw}:{bh},\
         gblur=sigma={BLUR_FILL_SIGMA},lutyuv=y=minval+(val-minval)*{BLUR_FILL_DARKEN:.2},\
         scale={w}:{h}:flags=bilinear,setsar=1"
    )
}

/// The fitted frame at its own size, `share` of the full fit (the caller
/// centres it).
fn fitted(w: u32, h: u32, share: f64) -> String {
    let (bw, bh) = if share >= 1.0 {
        (w, h)
    } else {
        (even((w as f64 * share) as u32), even((h as f64 * share) as u32))
    };
    format!("{}:force_divisible_by=2,setsar=1", scale_fit(bw, bh))
}

/// ffmpeg expression for the fitted frame's scale at time `t`: 1 away from
/// the title spans, [`TITLE_SAFE_SCALE`] inside them, linear in between.
fn title_scale_expr(windows: &[[f64; 2]]) -> String {
    let dist = windows
        .iter()
        .map(|[a, b]| format!("max(max({a:.3}-t,t-{b:.3}),0)"))
        .reduce(|acc, d| format!("min({acc},{d})"))
        .unwrap_or_else(|| "1".into());
    let safe = TITLE_SAFE_SCALE;
    format!("({safe:.2}+{:.2}*min(1,{dist}/{TITLE_SAFE_RAMP_SECS:.2}))", 1.0 - safe)
}

/// The fitted frame on a `w × h` canvas, sized per frame by
/// [`title_scale_expr`] over `windows` (piece-relative seconds):
/// transparent around it with `alpha`, black otherwise. A per-frame size
/// re-creates the scaler on every frame, so the graph only runs this over
/// the stretches around the titles ([`reframe_graph`]).
fn titled_canvas(w: u32, h: u32, windows: &[[f64; 2]], alpha: bool) -> String {
    format!(
        "scale=w='trunc(iw*min({w}/iw,{h}/ih)*{s}/2)*2':h=-2:eval=frame:flags=lanczos,\
         {fmt}pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color={color}:eval=frame,setsar=1",
        s = title_scale_expr(windows),
        fmt = if alpha { "format=yuva420p," } else { "" },
        color = if alpha { "black@0" } else { "black" },
    )
}

/// `[input]` → `[output]`: the frame through `fg` over a blurred, darkened
/// fill of itself. `fg` gives the fitted frame at its own size (centred
/// here), or with `canvas` a whole `w × h` canvas, transparent around it.
/// `p` prefixes the internal labels so several chains can share a graph.
fn blur_chain(input: &str, output: &str, w: u32, h: u32, p: &str, fg: &str, canvas: bool) -> String {
    format!(
        "[{input}]split=2[{p}fg0][{p}bg0];\
         [{p}bg0]{fill}[{p}bg];\
         [{p}fg0]{fg}[{p}fg];\
         [{p}bg][{p}fg]overlay={at},setsar=1[{output}]",
        fill = blur_fill(w, h),
        at = if canvas { "0:0" } else { "(W-w)/2:(H-h)/2" },
    )
}

/// A stretch of the timeline the reframe treats one way.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Piece {
    start: f64,
    /// `None`: to the end of the video.
    end: Option<f64>,
    /// Carries a title (or the ease around one).
    titled: bool,
}

/// `windows` (finite, non-empty ones) widened by `widen` each side,
/// clamped at 0, sorted and merged.
fn merged_spans(windows: &[[f64; 2]], widen: f64) -> Vec<[f64; 2]> {
    let mut spans: Vec<[f64; 2]> = windows
        .iter()
        .filter(|[a, b]| a.is_finite() && b.is_finite() && b > a)
        .map(|[a, b]| [(a - widen).max(0.0), b + widen])
        .collect();
    spans.sort_by(|x, y| x[0].total_cmp(&y[0]));
    let mut merged: Vec<[f64; 2]> = Vec::with_capacity(spans.len());
    for s in spans {
        match merged.last_mut() {
            Some(last) if s[0] <= last[1] + MIN_PIECE_SECS => last[1] = last[1].max(s[1]),
            _ => merged.push(s),
        }
    }
    merged
}

/// Cut the timeline at the edges of `spans` (from [`merged_spans`]): the
/// first piece starts at 0, the last runs to the end of the video.
fn pieces(spans: &[[f64; 2]]) -> Vec<Piece> {
    let mut out = Vec::with_capacity(spans.len() * 2 + 1);
    let mut at = 0.0_f64;
    for &[a, b] in spans {
        if a - at >= MIN_PIECE_SECS {
            out.push(Piece { start: at, end: Some(a), titled: false });
            at = a;
        }
        out.push(Piece { start: at, end: Some(b), titled: true });
        at = b;
    }
    out.push(Piece { start: at, end: None, titled: false });
    out
}

/// The title spans near `piece`, in its own time (it starts at 0).
fn piece_windows(windows: &[[f64; 2]], piece: &Piece) -> Vec<[f64; 2]> {
    let r = TITLE_SAFE_RAMP_SECS;
    windows
        .iter()
        .filter(|[a, b]| *b + r >= piece.start && piece.end.map_or(true, |e| *a - r <= e))
        .map(|[a, b]| [a - piece.start, b - piece.start])
        .collect()
}

/// `[input]` → `[output]` with the timeline cut into `pieces`, each run
/// through its own chain: `chain(i, piece, in, out)` builds piece `i`'s
/// subgraph from label `in` to label `out`, in piece-relative time. Every
/// chain must end at the same size and pixel format. `concat` leaves the
/// last frame without a duration (the file would come out a frame short),
/// so `fps` re-times the joined stream at the source rate — and the join
/// must be the graph's last step: frames that pass through another
/// filter after it (an overlay) lose that duration again.
fn piecewise(
    input: &str,
    output: &str,
    p: &str,
    pieces: &[Piece],
    fps: &str,
    chain: impl Fn(usize, &Piece, &str, &str) -> String,
) -> String {
    let n = pieces.len();
    let mut g = format!("[{input}]split={n}");
    for i in 0..n {
        g.push_str(&format!("[{p}s{i}]"));
    }
    for (i, piece) in pieces.iter().enumerate() {
        let trim = match piece.end {
            Some(e) => format!("trim=start={:.3}:end={e:.3}", piece.start),
            None => format!("trim=start={:.3}", piece.start),
        };
        g.push_str(&format!(";[{p}s{i}]{trim},setpts=PTS-STARTPTS[{p}t{i}];"));
        g.push_str(&chain(i, piece, &format!("{p}t{i}"), &format!("{p}q{i}")));
    }
    g.push(';');
    for i in 0..n {
        g.push_str(&format!("[{p}q{i}]"));
    }
    g.push_str(&format!("concat=n={n}:v=1:a=0,fps={fps}[{output}]"));
    g
}

/// A frame rate as an exact `fps` filter value: `30`, `30000/1001`
/// (NTSC rates), else three decimals.
pub fn fps_literal(fps: f64) -> String {
    if !(fps.is_finite() && fps > 0.0) {
        return "30".into();
    }
    if (fps - fps.round()).abs() < 1e-3 {
        return format!("{}", fps.round() as u64);
    }
    let ntsc = fps * 1.001;
    if (ntsc - ntsc.round()).abs() < 1e-2 {
        return format!("{}/1001", ntsc.round() as u64 * 1000);
    }
    format!("{fps:.3}")
}

/// Filter graph from `[0:v]` (a `src`-sized frame at `fps`) to `[v]` at
/// `w × h`. `fit = None` is a plain resize (same aspect; letterboxes any
/// rounding). `titles` are the title spans ([`card_windows`]):
///
/// * a portrait output narrower than `src` (9:16 from 16:9) shows the
///   frame at [`TITLE_SAFE_SCALE`] during them, inside the safe area, and
///   eases back to full size over [`TITLE_SAFE_RAMP_SECS`];
/// * a crop to any narrower shape would cut a title, so during them it
///   shows the blur fit instead (title-safe in portrait).
///
/// Only the stretches around the titles get that treatment; the rest of
/// the video runs through the plain, fast chain. A `smart` fit here has no
/// camera path, so it is the centre crop; see [`reframe_graph_with_path`].
pub fn reframe_graph(
    src: (u32, u32),
    w: u32,
    h: u32,
    fit: Option<Fit>,
    titles: &[[f64; 2]],
    fps: f64,
) -> String {
    reframe_graph_with_path(src, w, h, fit, titles, fps, None)
}

/// [`reframe_graph`], with the camera `path` a `smart` fit follows
/// ([`crate::smart_reframe::Follower`], in the video's own seconds). A
/// smart fit is a crop that moves: during the title spans it shows the
/// title-safe blur fit like `crop` does, and elsewhere the window follows
/// `path`.
pub fn reframe_graph_with_path(
    src: (u32, u32),
    w: u32,
    h: u32,
    fit: Option<Fit>,
    titles: &[[f64; 2]],
    fps: f64,
    path: Option<&CameraPath>,
) -> String {
    // The crop of an untitled stretch `[from, to)`.
    let cropped = |f: Fit, from: f64, to: Option<f64>| match (f, path) {
        (Fit::Smart, Some(p)) => follow_chain(w, h, p, from, to),
        _ => crop_chain(w, h),
    };
    let narrower = (w as f64 / h.max(1) as f64) < (src.0 as f64 / src.1.max(1) as f64) * 0.99;
    let portrait = h > w;
    let titles: Vec<[f64; 2]> = if narrower {
        titles.iter().copied().filter(|[a, b]| a.is_finite() && b.is_finite() && b > a).collect()
    } else {
        Vec::new()
    };
    let shrink = portrait && !titles.is_empty();
    let fps = fps_literal(fps);
    match fit {
        None => format!("[0:v]{}[v]", pad_chain(w, h)),
        Some(Fit::Pad) if !shrink => format!("[0:v]{}[v]", pad_chain(w, h)),
        Some(Fit::Blur) if !shrink => blur_chain("0:v", "v", w, h, "", &fitted(w, h, 1.0), false),
        Some(f @ (Fit::Crop | Fit::Smart)) if titles.is_empty() => format!("[0:v]{}[v]", cropped(f, 0.0, None)),
        // Each piece is composited on its own fill: the fitted frame sized
        // per frame around the titles, the plain fit elsewhere.
        Some(Fit::Blur) => {
            let pieces = pieces(&merged_spans(&titles, TITLE_SAFE_RAMP_SECS));
            let body = piecewise("src", "v", "b", &pieces, &fps, |k, piece, i, o| {
                let p = format!("b{k}");
                if piece.titled {
                    let fg = titled_canvas(w, h, &piece_windows(&titles, piece), true);
                    blur_chain(i, o, w, h, &p, &fg, true)
                } else {
                    blur_chain(i, o, w, h, &p, &fitted(w, h, 1.0), false)
                }
            });
            format!("[0:v]setpts=PTS-STARTPTS[src];{body}")
        }
        Some(Fit::Pad) => {
            let pieces = pieces(&merged_spans(&titles, TITLE_SAFE_RAMP_SECS));
            let body = piecewise("src", "v", "p", &pieces, &fps, |_, piece, i, o| {
                let canvas = if piece.titled {
                    titled_canvas(w, h, &piece_windows(&titles, piece), false)
                } else {
                    pad_chain(w, h)
                };
                format!("[{i}]{canvas}[{o}]")
            });
            format!("[0:v]setpts=PTS-STARTPTS[src];{body}")
        }
        Some(f @ (Fit::Crop | Fit::Smart)) => {
            let pieces = pieces(&merged_spans(&titles, 0.0));
            let share = if portrait { TITLE_SAFE_SCALE } else { 1.0 };
            let body = piecewise("src", "v", "c", &pieces, &fps, |k, piece, i, o| {
                if piece.titled {
                    blur_chain(i, o, w, h, &format!("g{k}"), &fitted(w, h, share), false)
                } else {
                    format!("[{i}]{}[{o}]", cropped(f, piece.start, piece.end))
                }
            });
            format!("[0:v]setpts=PTS-STARTPTS[src];{body}")
        }
    }
}

/// Video encoder args for an output's own pass: the encoder's quality
/// settings from `hw`, with the encoder preset swapped in and a peak
/// bitrate cap applied, when given. An encoder preset the resolved
/// encoder doesn't take ([`encoder_accepts_preset`]) is skipped with a
/// warning, keeping that encoder's default: configs that never had their
/// preset applied (`preset = "p5"` with `encoder = "auto"`, rendered on a
/// machine without NVENC) must keep rendering.
pub fn output_encode_args(
    hw: &HwConfig,
    crf: u32,
    encoder_preset: Option<&str>,
    max_bitrate: Option<u64>,
) -> Vec<String> {
    let mut a = hw.encode_args(crf);
    if let Some(p) = encoder_preset.map(str::trim).filter(|p| !p.is_empty()) {
        if encoder_accepts_preset(hw.encoder, p) {
            let key = if hw.encoder == EncoderChoice::H264Amf { "-quality" } else { "-preset" };
            if let Some(i) = a.iter().position(|x| x == key) {
                a[i + 1] = p.to_ascii_lowercase();
            }
        } else {
            tracing::warn!(
                "encoder preset {p:?} is not one {} takes (one of: {}); using its default",
                hw.encoder.name(),
                encoder_presets(hw.encoder).join(", ")
            );
        }
    }
    if let Some(cap) = max_bitrate.filter(|&c| c > 0) {
        let remove = |a: &mut Vec<String>, key: &str| {
            if let Some(i) = a.iter().position(|x| x == key) {
                a.drain(i..(i + 2).min(a.len()));
            }
        };
        match hw.encoder {
            // Capped quality mode: CRF / CQ with a VBV peak.
            EncoderChoice::Libx264
            | EncoderChoice::Libx265
            | EncoderChoice::H264Nvenc
            | EncoderChoice::HevcNvenc => {}
            // Constant-QP / ICQ ignore a peak, so these switch to peak VBR.
            EncoderChoice::H264Amf => {
                for k in ["-qp_i", "-qp_p", "-qp_b"] {
                    remove(&mut a, k);
                }
                if let Some(i) = a.iter().position(|x| x == "-rc") {
                    a[i + 1] = "vbr_peak".into();
                }
                a.extend(["-b:v".into(), format_bitrate(cap * 4 / 5)]);
            }
            EncoderChoice::H264Qsv => {
                remove(&mut a, "-global_quality");
                a.extend(["-b:v".into(), format_bitrate(cap * 4 / 5)]);
            }
        }
        a.extend([
            "-maxrate".into(),
            format_bitrate(cap),
            "-bufsize".into(),
            format_bitrate(cap * 2),
        ]);
    }
    a
}

/// The full ffmpeg argument list for one output pass over a finished
/// video: reframe with `graph`, encode video with `video_args`, AAC audio
/// at `audio_bitrate` (any audio stream; none is fine).
pub fn transcode_args(
    input: &Utf8Path,
    output: &Utf8Path,
    graph: &str,
    video_args: &[String],
    audio_bitrate: &str,
) -> Vec<String> {
    let mut cmd: Vec<String> = [
        "-y", "-hide_banner", "-loglevel", "error", "-i", input.as_str(),
        "-filter_complex", graph, "-map", "[v]", "-map", "0:a?", "-pix_fmt", "yuv420p",
    ]
    .map(String::from)
    .to_vec();
    cmd.extend(video_args.iter().cloned());
    cmd.extend(
        [
            "-c:a", "aac", "-b:a", audio_bitrate, "-ar", "48000", "-ac", "2",
            "-movflags", "+faststart", output.as_str(),
        ]
        .map(String::from),
    );
    cmd
}

/// Stream-copy arguments: the source already is the target shape.
fn copy_args(input: &Utf8Path, output: &Utf8Path) -> Vec<String> {
    [
        "-y", "-hide_banner", "-loglevel", "error", "-i", input.as_str(), "-map", "0:v:0",
        "-map", "0:a?", "-c", "copy", "-movflags", "+faststart", output.as_str(),
    ]
    .map(String::from)
    .to_vec()
}

/// The source's shape, as [`reformat_video`] needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceVideo {
    pub width: u32,
    pub height: u32,
    pub codec: String,
    pub duration_secs: f64,
    /// Frames per second (the stream's `r_frame_rate`).
    pub fps: f64,
}

/// A reformat request: the preset, an optional fit override, and the
/// title spans of the source (its manifest's `card_windows`), which a
/// narrower shape keeps whole and inside its safe area.
#[derive(Debug, Clone)]
pub struct ReformatOptions {
    pub preset: OutputPreset,
    pub fit: Option<Fit>,
    pub card_windows: Vec<[f64; 2]>,
}

/// What a reformat will do, decided before any ffmpeg runs.
#[derive(Debug, Clone, PartialEq)]
pub struct ReformatPlan {
    pub width: u32,
    pub height: u32,
    /// `None` = same aspect as the source.
    pub fit: Option<Fit>,
    /// The source already is this shape and needs no bitrate cap.
    pub copy: bool,
    pub crf: u32,
    pub audio_bitrate: String,
    pub max_bitrate: Option<u64>,
}

pub fn plan_reformat(src: &SourceVideo, opts: &ReformatOptions) -> ReformatPlan {
    let spec = opts.preset.spec();
    let fit = reframe_fit((src.width, src.height), (spec.width, spec.height), opts.fit.unwrap_or_default());
    let copy = (src.width, src.height) == (spec.width, spec.height)
        && spec.max_bitrate.is_none()
        && src.codec.eq_ignore_ascii_case("h264");
    ReformatPlan {
        width: spec.width,
        height: spec.height,
        fit,
        copy,
        crf: spec.crf,
        audio_bitrate: spec.audio_bitrate.to_string(),
        max_bitrate: spec.max_bitrate,
    }
}

/// ffmpeg `-progress` line → output seconds written so far.
pub fn parse_progress_secs(line: &str) -> Option<f64> {
    // `out_time_ms` is (despite its name) microseconds, same as `out_time_us`.
    let v = line
        .strip_prefix("out_time_us=")
        .or_else(|| line.strip_prefix("out_time_ms="))?;
    let us: i64 = v.trim().parse().ok()?;
    (us >= 0).then(|| us as f64 / 1_000_000.0)
}

/// Run ffmpeg with `args`, calling `on_progress` with 0..1 of `total_secs`
/// written as it goes.
///
/// Cancellable like the render's ffmpeg runs (see [`crate::cancel`]): once
/// the cancel token installed on this thread is cancelled, ffmpeg is
/// killed and this returns [`crate::cancel::Cancelled`] within a poll
/// interval, even between two progress lines.
pub fn run_ffmpeg_with_progress(
    args: &[String],
    total_secs: f64,
    on_progress: &mut dyn FnMut(f32),
) -> Result<()> {
    crate::cancel::check()?;
    let mut child = Command::new("ffmpeg")
        .args(["-progress", "pipe:1", "-nostats"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning ffmpeg (is it on PATH?)")?;
    let mut stderr = child.stderr.take().context("ffmpeg stderr")?;
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    // Progress lines are read on their own thread, so a Stop is noticed
    // between them instead of only when ffmpeg next writes one.
    let (tx, rx) = std::sync::mpsc::channel::<f64>();
    let out_reader = child.stdout.take().map(|stdout| {
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(|l| l.ok()) {
                if let Some(secs) = parse_progress_secs(&line) {
                    if tx.send(secs).is_err() {
                        break;
                    }
                }
            }
        })
    });
    let token = crate::progress::cancel_token();
    loop {
        if token.as_ref().is_some_and(|t| t.is_cancelled()) {
            break;
        }
        match rx.recv_timeout(std::time::Duration::from_millis(25)) {
            Ok(secs) => {
                if total_secs > 0.0 {
                    on_progress((secs / total_secs).clamp(0.0, 1.0) as f32);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    // Kills and reaps ffmpeg when the token was cancelled.
    let status = crate::cancel::wait_child(&mut child);
    drop(rx);
    if let Some(h) = out_reader {
        let _ = h.join();
    }
    let stderr = err_reader.join().unwrap_or_default();
    let status = status.context("waiting for ffmpeg")?;
    if !status.success() {
        let tail: Vec<&str> = stderr.lines().rev().take(8).collect();
        let tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n  ");
        anyhow::bail!("ffmpeg failed (exit {}):\n  {tail}", status.code().unwrap_or(-1));
    }
    on_progress(1.0);
    Ok(())
}

/// Probe a finished video for [`plan_reformat`].
pub fn probe_source(input: &Utf8Path) -> Result<SourceVideo> {
    let p = crate::media_probe::probe_metadata(input).with_context(|| format!("probing {input}"))?;
    let media = p
        .media
        .with_context(|| format!("ffprobe failed for {input}: {}", p.probe_error.unwrap_or_default()))?;
    let v = media.video.with_context(|| format!("{input} has no video stream"))?;
    let duration_secs = match crate::media_probe::probe_video_duration(input) {
        d if d > 0.0 => d,
        _ => media.duration_secs,
    };
    Ok(SourceVideo { width: v.width, height: v.height, codec: v.codec, duration_secs, fps: v.fps as f64 })
}

/// Hardware support, detected once per process (detection runs ffmpeg).
fn hw_support() -> HwSupport {
    static SUPPORT: std::sync::OnceLock<HwSupport> = std::sync::OnceLock::new();
    SUPPORT
        .get_or_init(|| HwSupport::detect("ffmpeg").unwrap_or_default())
        .clone()
}

/// Result of [`reformat_video`].
#[derive(Debug, Clone, PartialEq)]
pub struct ReformatOutcome {
    pub plan: ReformatPlan,
    pub encoder: &'static str,
    pub source_duration_secs: f64,
    /// The camera path a `smart` fit followed.
    pub path: Option<CameraPath>,
}

/// Share of a smart reformat's progress bar the motion analysis takes
/// (it decodes the video once, small; the encode is the rest).
pub const SMART_ANALYSIS_SHARE: f32 = 0.25;

/// Where a platform version of `source` goes: `<stem>.<preset>.mp4` next
/// to it, `<stem>.<preset>-<fit>.mp4` for a non-default fit. Callers pass
/// [`reformat_source`]'s path, so a version's versions come from its
/// render (`reel.square.mp4` → `reel.mp4` → `reel.reels.mp4`). A version
/// whose render is gone keeps its whole stem (`reel.square.reels.mp4`): a
/// reframe of a reframe never takes the render's own `reel.reels.mp4`,
/// which a later request from the render would reuse as fresh.
pub fn version_path(source: &Path, preset: OutputPreset, fit: Option<Fit>) -> PathBuf {
    let fit_tag = match fit {
        Some(f) if f != Fit::default() => format!("-{}", f.name()),
        _ => String::new(),
    };
    source.with_file_name(format!("{}.{}{fit_tag}.mp4", file_stem(source), preset.name()))
}

/// Containers a render (and so a version's source) can be.
const RENDER_EXTS: &[&str] = &["mp4", "m4v", "mov"];

/// The render a platform version of `source` is made from. A version
/// (`reel.square.mp4`, `reel.reels-crop.mp4`) is already reframed, so when
/// its render (`reel.mp4`) sits next to it, that render is the source —
/// a vertical version of a square one would otherwise carry two layers of
/// fill bands, and a same-shape request would copy the wrong fit. Without
/// the render, `source` itself.
pub fn reformat_source(source: &Path) -> PathBuf {
    let mut src = source.to_path_buf();
    while let Some(base) = version_base_stem(&src) {
        let own_ext = src.extension().map(|e| e.to_string_lossy().into_owned());
        let render = own_ext
            .into_iter()
            .chain(RENDER_EXTS.iter().map(|e| e.to_string()))
            .map(|ext| src.with_file_name(format!("{base}.{ext}")))
            .find(|p| p.is_file());
        match render {
            Some(r) => src = r,
            None => break,
        }
    }
    src
}

fn file_stem(source: &Path) -> String {
    source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "video".into())
}

/// `reel` for a version such as `reel.reels.mp4` or `reel.square-crop.mp4`;
/// `None` when `source` isn't one.
pub fn version_base_stem(source: &Path) -> Option<String> {
    let stem = file_stem(source);
    let (base, suffix) = stem.rsplit_once('.')?;
    let preset = suffix.split('-').next().unwrap_or(suffix);
    (!base.is_empty() && OutputPreset::parse(preset).is_some()).then(|| base.to_string())
}

/// The title spans `source`'s manifest recorded (`card_windows`), for
/// [`ReformatOptions::card_windows`]. A version (`reel.reels.mp4`, or
/// `reel.square.reels.mp4`) has the same timeline as its render, so it
/// reads that render's manifest. Empty when there's no manifest.
pub fn manifest_card_windows(source: &Path) -> Vec<[f64; 2]> {
    let Some(m) = read_manifest(source) else {
        return Vec::new();
    };
    m.get("card_windows")
        .and_then(|w| serde_json::from_value::<Vec<[f64; 2]>>(w.clone()).ok())
        .unwrap_or_default()
}

/// `source`'s manifest (a version reads its render's), parsed.
fn read_manifest(source: &Path) -> Option<serde_json::Value> {
    let mut manifest = source.with_extension("manifest.json");
    let mut at = source.to_path_buf();
    while !manifest.is_file() {
        let Some(base) = version_base_stem(&at) else { break };
        at = at.with_file_name(format!("{base}.mp4"));
        manifest = at.with_extension("manifest.json");
    }
    let text = std::fs::read_to_string(manifest).ok()?;
    serde_json::from_str::<serde_json::Value>(&text).ok()
}

/// Every cut of `source`, output seconds, from its manifest's exact
/// `timeline` (a version reads its render's): where a smart reframe snaps
/// instead of panning. Empty without a manifest or a timeline (older
/// renders); the smart reframe then finds the hard cuts in the pictures.
pub fn manifest_cuts(source: &Path) -> Vec<f64> {
    read_manifest(source)
        .and_then(|m| m.get("timeline").cloned())
        .and_then(|t| serde_json::from_value::<crate::output_timeline::OutputTimeline>(t).ok())
        .map(|t| crate::smart_reframe::timeline_cuts(&t))
        .unwrap_or_default()
}

/// One ffmpeg pass over a finished render: reframe `input` to `opts.preset`
/// with the same filter graph the render's per-output pass uses, and write
/// it to `output`. Stream-copies when the source already is that shape.
///
/// Like a render's `[[outputs]]`, the version is written under its staging
/// name ([`crate::staging`]) and only renamed to `output` once ffmpeg has
/// finished, so a failed or stopped pass ([`crate::cancel`]) never leaves a
/// truncated file there that would later look like a finished version.
///
/// A render can replace `input` while the pass runs (its outputs land by
/// rename, possibly from another process). The version is only committed
/// when `input` is still the file the pass started from; otherwise the pass
/// runs once more over the new one, so a version never shows the cut
/// before the one next to it.
pub fn reformat_video(
    input: &Utf8Path,
    output: &Utf8Path,
    opts: &ReformatOptions,
    on_progress: &mut dyn FnMut(f32),
) -> Result<ReformatOutcome> {
    reformat_video_with(input, output, opts, on_progress, &mut || {})
}

/// [`reformat_video`], with `before_commit` run between the pass and the
/// check that `input` is unchanged (tests replace the input there).
fn reformat_video_with(
    input: &Utf8Path,
    output: &Utf8Path,
    opts: &ReformatOptions,
    on_progress: &mut dyn FnMut(f32),
    before_commit: &mut dyn FnMut(),
) -> Result<ReformatOutcome> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    const ATTEMPTS: usize = 2;
    for attempt in 1..=ATTEMPTS {
        let started_from = file_identity(input)?;
        let src = probe_source(input)?;
        let plan = plan_reformat(&src, opts);
        let staged = crate::staging::StagedOutputs::new([output]);
        let outcome = write_reformat(input, staged.path(0), &src, plan, opts, on_progress)?;
        before_commit();
        // Last chance to stop: past this point the version replaces the old one.
        crate::cancel::check()?;
        if file_identity(input)? == started_from {
            staged.commit()?;
            return Ok(outcome);
        }
        tracing::info!(%input, attempt, "the video changed while its version was being made; making it again");
    }
    anyhow::bail!("{input} kept changing while its version was being made (a render is writing it?); try again")
}

/// What tells a replaced file from the one read: size and modification
/// time (a render's commit stamps both anew).
fn file_identity(p: &Utf8Path) -> Result<(u64, Option<std::time::SystemTime>)> {
    let m = std::fs::metadata(p).with_context(|| format!("reading {p}"))?;
    Ok((m.len(), m.modified().ok()))
}

/// [`reformat_video`]'s ffmpeg pass, straight into `output`.
fn write_reformat(
    input: &Utf8Path,
    output: &Utf8Path,
    src: &SourceVideo,
    plan: ReformatPlan,
    opts: &ReformatOptions,
    on_progress: &mut dyn FnMut(f32),
) -> Result<ReformatOutcome> {
    // A smart fit first measures where the action is (the first part of
    // the progress bar), with the render's cuts to snap at.
    let path = match plan.fit {
        Some(Fit::Smart) if !plan.copy => {
            let cuts = manifest_cuts(input.as_std_path());
            let mut follower = crate::smart_reframe::Follower::new(input, (src.width, src.height), cuts);
            follower.path_for((plan.width, plan.height), &mut |f| on_progress(f * SMART_ANALYSIS_SHARE))?
        }
        _ => None,
    };
    let (args, encoder) = if plan.copy {
        (copy_args(input, output), "copy")
    } else {
        let hw = HwConfig::resolve(hw_support(), EncoderPref::Auto);
        let graph = reframe_graph_with_path(
            (src.width, src.height),
            plan.width,
            plan.height,
            plan.fit,
            &opts.card_windows,
            src.fps,
            path.as_ref(),
        );
        let video = output_encode_args(&hw, plan.crf, None, plan.max_bitrate);
        (transcode_args(input, output, &graph, &video, &plan.audio_bitrate), hw.encoder.name())
    };
    tracing::debug!(?args, "reformat");
    let done = if path.is_some() { SMART_ANALYSIS_SHARE } else { 0.0 };
    run_ffmpeg_with_progress(&args, src.duration_secs, &mut |f| on_progress(done + (1.0 - done) * f))?;
    Ok(ReformatOutcome { plan, encoder, source_duration_secs: src.duration_secs, path })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(toml_body: &str) -> RenderOutput {
        toml::from_str(&format!("path = \"out/x.mp4\"\n{toml_body}")).unwrap()
    }

    fn sw() -> HwConfig {
        HwConfig::resolve(HwSupport::default(), EncoderPref::Libx264)
    }

    #[test]
    fn preset_table() {
        let s = |p: &str| OutputPreset::parse(p).unwrap().spec();
        assert_eq!((s("youtube").width, s("youtube").height), (1920, 1080));
        for vertical in ["reels", "tiktok", "shorts"] {
            assert_eq!((s(vertical).width, s(vertical).height), (1080, 1920), "{vertical}");
            assert!(s(vertical).derived);
        }
        assert_eq!((s("square").width, s("square").height), (1080, 1080));
        assert_eq!((s("whatsapp").width, s("whatsapp").height), (720, 1280));
        assert_eq!(s("master").width, 1920);
        assert!(!s("master").derived && !s("youtube").derived);
        assert_eq!(OutputPreset::parse(" Reels "), Some(OutputPreset::Reels));
        assert_eq!(OutputPreset::parse("reel"), None);
    }

    #[test]
    fn whatsapp_cap_keeps_30_seconds_under_16_mb() {
        let s = OutputPreset::Whatsapp.spec();
        let audio = parse_bitrate(s.audio_bitrate).unwrap();
        let peak_bytes = (s.max_bitrate.unwrap() + audio) as f64 * 30.0 / 8.0;
        assert!(peak_bytes < 16.0e6 * 0.8, "{peak_bytes} bytes at the peak rate");
    }

    #[test]
    fn preset_fills_unset_keys() {
        let r = resolve_output(&out("preset = \"reels\"")).unwrap();
        assert_eq!((r.width, r.height, r.crf), (1080, 1920, 20));
        assert_eq!(r.audio_bitrate, "192k");
        assert_eq!(r.fit, Fit::Blur);
        assert_eq!(r.max_bitrate, None);
        let w = resolve_output(&out("preset = \"whatsapp\"")).unwrap();
        assert_eq!((w.width, w.height, w.crf), (720, 1280, 23));
        assert_eq!((w.audio_bitrate.as_str(), w.max_bitrate), ("128k", Some(3_000_000)));
    }

    #[test]
    fn explicit_keys_override_the_preset() {
        let r = resolve_output(&out(
            "preset = \"whatsapp\"\nresolution = \"1080x1920\"\ncrf = 18\n\
             audio_bitrate = \"96k\"\nmax_bitrate = \"2500k\"\nfit = \"crop\"",
        ))
        .unwrap();
        assert_eq!((r.width, r.height, r.crf), (1080, 1920, 18));
        assert_eq!(r.audio_bitrate, "96k");
        assert_eq!(r.max_bitrate, Some(2_500_000));
        assert_eq!(r.fit, Fit::Crop);
        // "none" drops a preset's cap.
        let n = resolve_output(&out("preset = \"whatsapp\"\nmax_bitrate = \"none\"")).unwrap();
        assert_eq!(n.max_bitrate, None);
    }

    #[test]
    fn no_preset_keeps_todays_defaults() {
        let r = resolve_output(&out("resolution = \"3840x2160\"")).unwrap();
        assert_eq!((r.width, r.height, r.crf), (3840, 2160, 20));
        assert_eq!((r.audio_bitrate.as_str(), r.max_bitrate, r.preset), ("192k", None, None));
        let e = resolve_output(&out("crf = 20")).unwrap_err();
        assert!(e.contains("resolution"), "{e}");
        let m = resolve_output(&out("preset = \"master\"")).unwrap();
        assert_eq!((m.width, m.height), (1920, 1080));
    }

    #[test]
    fn legacy_encoder_preset_in_preset_still_loads() {
        let r = resolve_output(&out("resolution = \"1920x1080\"\npreset = \"p5\"")).unwrap();
        assert_eq!(r.preset, None);
        assert_eq!(r.encoder_preset.as_deref(), Some("p5"));
        // An explicit encoder_preset wins over the legacy spelling.
        let r = resolve_output(&out(
            "resolution = \"1920x1080\"\npreset = \"veryfast\"\nencoder_preset = \"slow\"",
        ))
        .unwrap();
        assert_eq!(r.encoder_preset.as_deref(), Some("slow"));
        let e = resolve_output(&out("preset = \"instagram\"")).unwrap_err();
        assert!(e.contains("reels") && e.contains("encoder_preset"), "{e}");
    }

    #[test]
    fn bad_values_are_rejected() {
        assert!(resolve_output(&out("resolution = \"1920\"")).unwrap_err().contains("WIDTHxHEIGHT"));
        assert!(resolve_output(&out("preset = \"reels\"\nmax_bitrate = \"fast\""))
            .unwrap_err()
            .contains("max_bitrate"));
    }

    #[test]
    fn bitrates_parse_and_format() {
        assert_eq!(parse_bitrate("3M"), Some(3_000_000));
        assert_eq!(parse_bitrate("2500k"), Some(2_500_000));
        assert_eq!(parse_bitrate("1.5M"), Some(1_500_000));
        assert_eq!(parse_bitrate("128000"), Some(128_000));
        assert_eq!(parse_bitrate("fast"), None);
        assert_eq!(format_bitrate(3_000_000), "3M");
        assert_eq!(format_bitrate(2_400_000), "2400k");
        assert_eq!(format_bitrate(1_234), "1234");
    }

    #[test]
    fn body_takes_the_largest_master_shape() {
        let r = |t: &str| resolve_output(&out(t)).unwrap();
        // A vertical output never turns the body vertical.
        assert_eq!(body_resolution(&[r("resolution = \"1920x1080\""), r("preset = \"reels\"")]), (1920, 1080));
        assert_eq!(body_resolution(&[r("preset = \"reels\""), r("resolution = \"1280x720\"")]), (1280, 720));
        assert_eq!(body_resolution(&[r("preset = \"youtube\""), r("resolution = \"3840x2160\"")]), (3840, 2160));
        // Only derived shapes: a 16:9 master as tall as the largest short side.
        assert_eq!(body_resolution(&[r("preset = \"reels\"")]), (1920, 1080));
        assert_eq!(body_resolution(&[r("preset = \"whatsapp\""), r("preset = \"square\"")]), (1920, 1080));
        assert_eq!(body_resolution(&[r("preset = \"reels\"\nresolution = \"2160x3840\"")]), (3840, 2160));
        // ... but never below 1080p, where card text sizes are calibrated:
        // a 1280x720 body would cut a 110 px title before the reframe.
        assert_eq!(body_resolution(&[r("preset = \"whatsapp\"")]), (1920, 1080));
        assert_eq!(body_resolution(&[r("preset = \"square\"\nresolution = \"640x640\"")]), (1920, 1080));
        // An explicit vertical resolution without a preset still sets the body (as before).
        assert_eq!(body_resolution(&[r("resolution = \"1080x1920\"")]), (1080, 1920));
    }

    #[test]
    fn fit_only_applies_when_the_aspect_differs() {
        assert_eq!(reframe_fit((1920, 1080), (1280, 720), Fit::Blur), None);
        assert_eq!(reframe_fit((3840, 2160), (1920, 1080), Fit::Crop), None);
        assert_eq!(reframe_fit((1920, 1080), (1080, 1920), Fit::Blur), Some(Fit::Blur));
        assert_eq!(reframe_fit((1920, 1080), (1080, 1080), Fit::Pad), Some(Fit::Pad));
    }

    #[test]
    fn own_pass_when_shape_or_cap_differs() {
        let r = |t: &str| resolve_output(&out(t)).unwrap();
        assert!(!needs_own_pass((1920, 1080), &r("resolution = \"1920x1080\"")));
        assert!(needs_own_pass((1920, 1080), &r("preset = \"reels\"")));
        assert!(needs_own_pass((1920, 1080), &r("resolution = \"1920x1080\"\nmax_bitrate = \"8M\"")));
    }

    const HD: (u32, u32) = (1920, 1080);

    #[test]
    fn same_aspect_graph_is_the_old_letterbox() {
        assert_eq!(
            reframe_graph(HD, 1280, 720, None, &[], 30.0),
            "[0:v]scale=1280:720:flags=lanczos:force_original_aspect_ratio=decrease,\
             pad=1280:720:(ow-iw)/2:(oh-ih)/2,setsar=1[v]"
        );
        assert_eq!(
            reframe_graph(HD, 1080, 1920, Some(Fit::Pad), &[], 30.0),
            reframe_graph(HD, 1080, 1920, None, &[], 30.0)
        );
        // Titles don't change a same-aspect resize.
        assert_eq!(
            reframe_graph(HD, 1280, 720, None, &[[0.0, 3.0]], 30.0),
            reframe_graph(HD, 1280, 720, None, &[], 30.0)
        );
    }

    #[test]
    fn blur_graph_fits_over_a_blurred_darkened_fill() {
        let g = reframe_graph(HD, 1080, 1920, Some(Fit::Blur), &[], 30.0);
        assert_eq!(
            g,
            "[0:v]split=2[fg0][bg0];\
             [bg0]scale=270:480:force_original_aspect_ratio=increase,crop=270:480,\
             gblur=sigma=20,lutyuv=y=minval+(val-minval)*0.72,scale=1080:1920:flags=bilinear,setsar=1[bg];\
             [fg0]scale=1080:1920:flags=lanczos:force_original_aspect_ratio=decrease:force_divisible_by=2,setsar=1[fg];\
             [bg][fg]overlay=(W-w)/2:(H-h)/2,setsar=1[v]"
        );
    }

    #[test]
    fn blur_graph_keeps_titles_inside_the_safe_area() {
        let g = reframe_graph(HD, 1080, 1920, Some(Fit::Blur), &[[0.0, 3.0]], 30.0);
        // Cut in two, each piece on its own fill: up to the end of the ease
        // (3 s + 0.4 s) the frame is sized per frame on a transparent
        // canvas, then the plain fit; joined last and re-timed.
        let fill = "scale=270:480:force_original_aspect_ratio=increase,crop=270:480,\
                    gblur=sigma=20,lutyuv=y=minval+(val-minval)*0.72,scale=1080:1920:flags=bilinear,setsar=1";
        assert_eq!(
            g,
            format!(
                "[0:v]setpts=PTS-STARTPTS[src];[src]split=2[bs0][bs1];\
                 [bs0]trim=start=0.000:end=3.400,setpts=PTS-STARTPTS[bt0];\
                 [bt0]split=2[b0fg0][b0bg0];[b0bg0]{fill}[b0bg];\
                 [b0fg0]scale=w='trunc(iw*min(1080/iw,1920/ih)*(0.76+0.24*min(1,max(max(0.000-t,t-3.000),0)/0.40))/2)*2'\
                 :h=-2:eval=frame:flags=lanczos,format=yuva420p,\
                 pad=1080:1920:(ow-iw)/2:(oh-ih)/2:color=black@0:eval=frame,setsar=1[b0fg];\
                 [b0bg][b0fg]overlay=0:0,setsar=1[bq0];\
                 [bs1]trim=start=3.400,setpts=PTS-STARTPTS[bt1];\
                 [bt1]split=2[b1fg0][b1bg0];[b1bg0]{fill}[b1bg];\
                 [b1fg0]scale=1080:1920:flags=lanczos:force_original_aspect_ratio=decrease:force_divisible_by=2,setsar=1[b1fg];\
                 [b1bg][b1fg]overlay=(W-w)/2:(H-h)/2,setsar=1[bq1];\
                 [bq0][bq1]concat=n=2:v=1:a=0,fps=30[v]"
            )
        );
    }

    #[test]
    fn per_frame_sizing_only_runs_around_the_titles() {
        // A per-frame size re-creates the scaler every frame (a pass about
        // 2.5x slower), so only the two title pieces may use it.
        let g = reframe_graph(HD, 1080, 1920, Some(Fit::Blur), &[[0.0, 3.0], [27.5, 30.0]], 30.0);
        assert_eq!(g.matches("scale=w='").count(), 2, "{g}");
        assert!(g.contains("[bs0]trim=start=0.000:end=3.400,"), "{g}");
        assert!(g.contains("[bs1]trim=start=3.400:end=27.100,"), "{g}");
        assert!(g.contains("[bs2]trim=start=27.100:end=30.400,"), "{g}");
        assert!(g.contains("[bs3]trim=start=30.400,"), "{g}");
        // Each title piece's expression is in its own time and only names
        // its own title.
        assert!(g.contains("*(0.76+0.24*min(1,max(max(0.400-t,t-2.900),0)/0.40))"), "{g}");
        assert!(!g.contains("27.500"), "{g}");
        // The join is the last step (see `piecewise`).
        assert!(g.ends_with("[bq0][bq1][bq2][bq3]concat=n=4:v=1:a=0,fps=30[v]"), "{g}");
    }

    #[test]
    fn titles_shrink_only_in_portrait_outputs() {
        let titles = [[0.0, 3.0]];
        // A wider output (a 16:9 version of a vertical render) has room.
        assert_eq!(
            reframe_graph((1080, 1920), 1920, 1080, Some(Fit::Blur), &titles, 30.0),
            reframe_graph((1080, 1920), 1920, 1080, Some(Fit::Blur), &[], 30.0)
        );
        // A square post has no platform UI over it: the plain fit.
        assert_eq!(
            reframe_graph(HD, 1080, 1080, Some(Fit::Blur), &titles, 30.0),
            reframe_graph(HD, 1080, 1080, Some(Fit::Blur), &[], 30.0)
        );
        assert_eq!(
            reframe_graph(HD, 1080, 1080, Some(Fit::Pad), &titles, 30.0),
            reframe_graph(HD, 1080, 1080, Some(Fit::Pad), &[], 30.0)
        );
        // Broken spans are ignored.
        assert_eq!(
            reframe_graph(HD, 1080, 1920, Some(Fit::Blur), &[[3.0, 1.0], [f64::NAN, 2.0]], 30.0),
            reframe_graph(HD, 1080, 1920, Some(Fit::Blur), &[], 30.0)
        );
    }

    #[test]
    fn pad_graph_shrinks_titles_over_black() {
        let p = reframe_graph(HD, 720, 1280, Some(Fit::Pad), &[[0.0, 3.0]], 29.97);
        assert!(p.starts_with("[0:v]setpts=PTS-STARTPTS[src];[src]split=2[ps0][ps1];"), "{p}");
        assert!(p.contains("pad=720:1280:(ow-iw)/2:(oh-ih)/2:color=black:eval=frame,setsar=1[pq0]"), "{p}");
        assert!(p.contains(&format!("[pt1]{}[pq1]", pad_chain(720, 1280))), "{p}");
        assert!(p.ends_with("[pq0][pq1]concat=n=2:v=1:a=0,fps=30000/1001[v]"), "{p}");
        assert!(!p.contains("yuva"), "{p}");
    }

    #[test]
    fn title_scale_is_safe_inside_spans_and_full_away_from_them() {
        // Evaluate the expression the way ffmpeg would, for a few times.
        let at = |t: f64| {
            let d = [[0.0_f64, 3.0], [27.5, 30.0]]
                .iter()
                .map(|[a, b]| (a - t).max(t - b).max(0.0))
                .fold(f64::INFINITY, f64::min);
            TITLE_SAFE_SCALE + (1.0 - TITLE_SAFE_SCALE) * (d / TITLE_SAFE_RAMP_SECS).min(1.0)
        };
        assert_eq!(at(1.0), TITLE_SAFE_SCALE);
        assert!((at(3.2) - 0.88).abs() < 1e-9);
        assert_eq!(at(10.0), 1.0);
        assert_eq!(at(28.0), TITLE_SAFE_SCALE);
        // A full-width master title (1880 of 1920 px) lands inside the safe width.
        let title_px = 1880.0 * (1080.0 / 1920.0) * at(1.0);
        assert!(title_px <= 1080.0 * 0.76, "{title_px}");
        assert!(title_scale_expr(&[[0.0, 3.0]]).contains("max(max(0.000-t,t-3.000),0)"));
    }

    #[test]
    fn crop_graph_fills_and_guards_titles_with_blur() {
        assert_eq!(
            reframe_graph(HD, 1080, 1080, Some(Fit::Crop), &[], 30.0),
            "[0:v]scale=1080:1080:flags=lanczos:force_original_aspect_ratio=increase,crop=1080:1080,setsar=1[v]"
        );
        let g = reframe_graph(HD, 1080, 1920, Some(Fit::Crop), &[[0.0, 3.0], [27.5, 30.0]], 30.0);
        assert!(g.starts_with("[0:v]setpts=PTS-STARTPTS[src];[src]split=4[cs0][cs1][cs2][cs3];"), "{g}");
        // The titles themselves (no ease) get the blur fit; the rest is cropped.
        assert!(
            g.contains("[cs0]trim=start=0.000:end=3.000,setpts=PTS-STARTPTS[ct0];[ct0]split=2[g0fg0][g0bg0]"),
            "{g}"
        );
        assert!(
            g.contains(&format!(
                "[cs1]trim=start=3.000:end=27.500,setpts=PTS-STARTPTS[ct1];[ct1]{}[cq1]",
                crop_chain(1080, 1920)
            )),
            "{g}"
        );
        assert!(
            g.contains("[cs2]trim=start=27.500:end=30.000,setpts=PTS-STARTPTS[ct2];[ct2]split=2[g2fg0][g2bg0]"),
            "{g}"
        );
        // The guard shows the title-safe fit: 76 % of 1080x1920 is 820x1458.
        assert!(g.contains("[g0fg0]scale=820:1458:flags=lanczos:force_original_aspect_ratio=decrease"), "{g}");
        assert!(g.ends_with("[cq0][cq1][cq2][cq3]concat=n=4:v=1:a=0,fps=30[v]"), "{g}");
        // A square crop guards titles too (it would cut them), at the full fit.
        let sq = reframe_graph(HD, 1080, 1080, Some(Fit::Crop), &[[0.0, 3.0]], 30.0);
        assert!(sq.contains("[g0fg0]scale=1080:1080:flags=lanczos:force_original_aspect_ratio=decrease"), "{sq}");
    }

    /// A 9:16 camera path over 30 s: left of centre, a pan to the right
    /// from 10 to 14 s, then held there.
    fn panning_path() -> CameraPath {
        use crate::smart_reframe::PathPiece;
        let w = (9.0 / 16.0) / (16.0 / 9.0);
        let p = |t0, t1, c0, c1| PathPiece { t0, t1, c0, c1 };
        CameraPath {
            axis: PanAxis::X,
            window: w,
            duration_secs: 30.0,
            pieces: vec![p(0.0, 10.0, 0.3, 0.3), p(10.0, 14.0, 0.3, 0.7), p(14.0, 30.0, 0.7, 0.7)],
            shots: 1,
            followed_shots: 1,
        }
    }

    #[test]
    fn smart_graph_crops_along_the_path_and_guards_titles() {
        let crop = reframe_graph(HD, 1080, 1920, Some(Fit::Crop), &[], 30.0);
        // No path (or one that never leaves the centre): the centre crop.
        assert_eq!(reframe_graph(HD, 1080, 1920, Some(Fit::Smart), &[], 30.0), crop);
        let mut centred = panning_path();
        centred.pieces = vec![crate::smart_reframe::PathPiece { t0: 0.0, t1: 30.0, c0: 0.5, c1: 0.5 }];
        assert_eq!(reframe_graph_with_path(HD, 1080, 1920, Some(Fit::Smart), &[], 30.0, Some(&centred)), crop);
        // A crop / blur fit ignores a path.
        let path = panning_path();
        assert_eq!(reframe_graph_with_path(HD, 1080, 1920, Some(Fit::Crop), &[], 30.0, Some(&path)), crop);
        // The window slides along x over the frame scaled to cover 1080x1920.
        let g = reframe_graph_with_path(HD, 1080, 1920, Some(Fit::Smart), &[], 30.0, Some(&path));
        let e = crate::smart_reframe::offset_expr(&path, 0.0, None);
        assert_eq!(
            g,
            format!(
                "[0:v]setpts=PTS-STARTPTS,scale=1080:1920:flags=lanczos:force_original_aspect_ratio=increase,\
                 crop=1080:1920:'(in_w-out_w)*({e})':(in_h-out_h)/2,setsar=1[v]"
            )
        );
        assert!(e.starts_with("lt(t,10.000)*0.2074+gte(t,10.000)*lt(t,14.000)*(0.2074+0.146"), "{e}");
        // Titles: the guard (title-safe blur) during them, the following
        // crop elsewhere, on each piece's own clock.
        let g = reframe_graph_with_path(HD, 1080, 1920, Some(Fit::Smart), &[[0.0, 3.0], [27.5, 30.0]], 30.0, Some(&path));
        assert!(g.contains("[cs0]trim=start=0.000:end=3.000,setpts=PTS-STARTPTS[ct0];[ct0]split=2[g0fg0][g0bg0]"), "{g}");
        let body = crate::smart_reframe::offset_expr(&path, 3.0, Some(27.5));
        assert!(body.starts_with("lt(t,7.000)*0.2074+gte(t,7.000)*lt(t,11.000)*"), "{body}");
        assert!(g.contains(&format!("[ct1]setpts=PTS-STARTPTS,scale=1080:1920:flags=lanczos:force_original_aspect_ratio=increase,crop=1080:1920:'(in_w-out_w)*({body})':(in_h-out_h)/2,setsar=1[cq1]")), "{g}");
        assert!(g.contains("[cs2]trim=start=27.500:end=30.000,setpts=PTS-STARTPTS[ct2];[ct2]split=2[g2fg0][g2bg0]"), "{g}");
        // Past the end of the path (no frames left): the plain crop.
        assert!(g.ends_with("[cq0][cq1][cq2][cq3]concat=n=4:v=1:a=0,fps=30[v]"), "{g}");
        // A wider shape slides along y.
        let mut tall = panning_path();
        tall.axis = PanAxis::Y;
        let g = reframe_graph_with_path((1080, 1920), 1920, 1080, Some(Fit::Smart), &[], 30.0, Some(&tall));
        assert!(g.contains("crop=1920:1080:(in_w-out_w)/2:'(in_h-out_h)*("), "{g}");
    }

    #[test]
    fn portrait_is_four_by_five_and_smart_is_a_fit() {
        let s = OutputPreset::Portrait.spec();
        assert_eq!((s.width, s.height, s.derived), (1080, 1350, true));
        assert_eq!(OutputPreset::parse("portrait"), Some(OutputPreset::Portrait));
        assert!(OutputPreset::names().contains("portrait"));
        let r = resolve_output(&out("preset = \"portrait\"\nfit = \"smart\"")).unwrap();
        assert_eq!((r.width, r.height, r.fit), (1080, 1350, Fit::Smart));
        assert_eq!(Fit::parse(" Smart "), Some(Fit::Smart));
        assert_eq!(Fit::Smart.name(), "smart");
        // A portrait-only project still renders a 1080p master.
        assert_eq!(body_resolution(&[r]), (1920, 1080));
        assert_eq!(
            version_path(Path::new("/projects/out/reel.mp4"), OutputPreset::Portrait, Some(Fit::Smart)),
            PathBuf::from("/projects/out/reel.portrait-smart.mp4")
        );
        let master = SourceVideo { width: 1920, height: 1080, codec: "h264".into(), duration_secs: 30.0, fps: 30.0 };
        let plan = plan_reformat(
            &master,
            &ReformatOptions { preset: OutputPreset::Portrait, fit: Some(Fit::Smart), card_windows: vec![] },
        );
        assert_eq!((plan.width, plan.height, plan.fit), (1080, 1350, Some(Fit::Smart)));
    }

    #[test]
    fn cuts_come_from_the_manifest_timeline() {
        use crate::output_timeline::{ChainItem, ChainLayout, ClipSource, OutputTimeline};
        let d = std::env::temp_dir().join(format!("vibecut_output_format_cuts_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let video = d.join("reel.mp4");
        assert!(manifest_cuts(&video).is_empty());
        let clip = |k: usize| {
            ChainItem::Clip(ClipSource {
                segment_index: k,
                source_id: format!("GX01000{k}"),
                source_path: format!("/footage/GX01000{k}.MP4").into(),
                source_in_secs: 1.0,
                duration_secs: 2.0,
                ramp: None,
            })
        };
        let layout = ChainLayout::new(&[3.0, 2.0, 2.0, 2.0], &[0.5, 0.0, 0.3], 30);
        let timeline = OutputTimeline::build(&layout, &[ChainItem::IntroCard, clip(0), clip(1), clip(2)]);
        let manifest = serde_json::json!({ "card_windows": [[0.0, 2.75]], "timeline": timeline });
        std::fs::write(d.join("reel.manifest.json"), manifest.to_string()).unwrap();
        let cuts = manifest_cuts(&video);
        let want: Vec<f64> = timeline.cuts().iter().map(|c| c.output_secs as f64).collect();
        assert_eq!(cuts.len(), 3);
        assert!(cuts.iter().zip(&want).all(|(a, b)| (a - b).abs() < 1e-5), "{cuts:?} vs {want:?}");
        // The intro's crossfade cut, then a hard cut on a frame boundary.
        assert!((cuts[0] - 2.75).abs() < 1e-9 && (cuts[1] - 4.5).abs() < 1e-9, "{cuts:?}");
        // A version reads its render's; a manifest without a timeline has none.
        assert_eq!(manifest_cuts(&d.join("reel.reels-smart.mp4")), cuts);
        std::fs::write(d.join("reel.manifest.json"), br#"{"card_windows": []}"#).unwrap();
        assert!(manifest_cuts(&video).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn timeline_pieces_cover_the_titles() {
        let p = |start: f64, end: Option<f64>, titled: bool| Piece { start, end, titled };
        let close = |a: &[[f64; 2]], b: &[[f64; 2]]| {
            a.len() == b.len()
                && a.iter().zip(b).all(|(x, y)| (x[0] - y[0]).abs() < 1e-9 && (x[1] - y[1]).abs() < 1e-9)
        };
        // Widened, clamped at 0, merged when they touch, sorted.
        let m = merged_spans(&[[27.5, 30.0], [0.0, 3.0]], 0.4);
        assert!(close(&m, &[[0.0, 3.4], [27.1, 30.4]]), "{m:?}");
        let m = merged_spans(&[[0.0, 3.0], [3.5, 5.0]], 0.4);
        assert!(close(&m, &[[0.0, 5.4]]), "{m:?}");
        assert!(merged_spans(&[[2.0, 2.0], [f64::INFINITY, 1.0]], 0.4).is_empty());
        assert_eq!(
            pieces(&[[0.0, 3.4], [27.1, 30.4]]),
            vec![p(0.0, Some(3.4), true), p(3.4, Some(27.1), false), p(27.1, Some(30.4), true), p(30.4, None, false)]
        );
        // A title that starts later leaves an untitled lead-in.
        assert_eq!(pieces(&[[1.6, 5.4]]), vec![p(0.0, Some(1.6), false), p(1.6, Some(5.4), true), p(5.4, None, false)]);
        assert_eq!(pieces(&[]), vec![p(0.0, None, false)]);
        // Windows move into the piece's own time; far ones drop out.
        let w = [[0.0, 3.0], [27.5, 30.0]];
        let pw = piece_windows(&w, &p(27.1, Some(30.4), true));
        assert!(close(&pw, &[[0.4, 2.9]]), "{pw:?}");
        assert_eq!(piece_windows(&w, &p(0.0, Some(3.4), true)), vec![[0.0, 3.0]]);
    }

    #[test]
    fn frame_rates_are_exact() {
        assert_eq!(fps_literal(30.0), "30");
        assert_eq!(fps_literal(25.0), "25");
        assert_eq!(fps_literal(29.97), "30000/1001");
        assert_eq!(fps_literal(29.970_03), "30000/1001");
        assert_eq!(fps_literal(23.976), "24000/1001");
        assert_eq!(fps_literal(59.94), "60000/1001");
        assert_eq!(fps_literal(12.5), "12.500");
        assert_eq!(fps_literal(f64::NAN), "30");
        assert_eq!(fps_literal(0.0), "30");
    }

    #[test]
    fn card_windows_cover_intro_and_outro() {
        assert_eq!(card_windows(3.0, 0.0, 30.0), vec![[0.0, 3.0]]);
        assert_eq!(card_windows(3.0, 2.0, 30.0), vec![[0.0, 3.0], [28.0, 30.0]]);
        assert!(card_windows(0.0, 0.0, 30.0).is_empty());
    }

    #[test]
    fn encode_args_cap_and_preset() {
        let a = output_encode_args(&sw(), 23, None, Some(3_000_000));
        assert_eq!(
            a.join(" "),
            "-c:v libx264 -preset veryfast -crf 23 -maxrate 3M -bufsize 6M"
        );
        let a = output_encode_args(&sw(), 20, Some("slow"), None);
        assert_eq!(a.join(" "), "-c:v libx264 -preset slow -crf 20");
        let mut amf = sw();
        amf.encoder = EncoderChoice::H264Amf;
        let a = output_encode_args(&amf, 23, None, Some(3_000_000)).join(" ");
        assert!(a.contains("-rc vbr_peak") && !a.contains("-qp_i"), "{a}");
        assert!(a.ends_with("-b:v 2400k -maxrate 3M -bufsize 6M"), "{a}");
        let mut qsv = sw();
        qsv.encoder = EncoderChoice::H264Qsv;
        let a = output_encode_args(&qsv, 23, None, Some(3_000_000)).join(" ");
        assert!(!a.contains("-global_quality"), "{a}");
        let mut nv = sw();
        nv.encoder = EncoderChoice::H264Nvenc;
        let a = output_encode_args(&nv, 23, None, Some(3_000_000)).join(" ");
        assert!(a.contains("-cq 23") && a.ends_with("-maxrate 3M -bufsize 6M"), "{a}");
    }

    #[test]
    fn encoder_presets_only_reach_an_encoder_that_takes_them() {
        // An NVENC preset with `encoder = "auto"` on a machine without
        // NVENC (the Docker image): libx264 keeps its own, instead of
        // ffmpeg failing on `-preset p5`.
        assert_eq!(output_encode_args(&sw(), 20, Some("p5"), None).join(" "), "-c:v libx264 -preset veryfast -crf 20");
        let mut nv = sw();
        nv.encoder = EncoderChoice::H264Nvenc;
        assert!(output_encode_args(&nv, 20, Some("p7"), None).join(" ").contains("-preset p7"));
        // ... and the other way round.
        let a = output_encode_args(&nv, 20, Some("veryfast"), None).join(" ");
        assert!(a.contains("-preset p5") && !a.contains("veryfast"), "{a}");
        let mut amf = sw();
        amf.encoder = EncoderChoice::H264Amf;
        assert!(output_encode_args(&amf, 20, Some("quality"), None).join(" ").contains("-quality quality"));
        assert!(output_encode_args(&amf, 20, Some("slow"), None).join(" ").contains("-quality balanced"));
        let mut qsv = sw();
        qsv.encoder = EncoderChoice::H264Qsv;
        assert!(output_encode_args(&qsv, 20, Some("veryslow"), None).join(" ").contains("-preset veryslow"));
        assert!(output_encode_args(&qsv, 20, Some("ultrafast"), None).join(" ").contains("-preset medium"));
        // Case doesn't matter; the name goes to ffmpeg in its own case.
        assert_eq!(output_encode_args(&sw(), 20, Some(" Slow "), None).join(" "), "-c:v libx264 -preset slow -crf 20");
        // Every per-encoder name is still a legacy `preset` spelling.
        for e in [EncoderChoice::Libx264, EncoderChoice::H264Nvenc, EncoderChoice::H264Amf, EncoderChoice::H264Qsv] {
            assert!(encoder_presets(e).iter().all(|n| is_encoder_preset_name(n)), "{e:?}");
        }
        assert!(!is_encoder_preset_name("reels"));
    }

    #[test]
    fn transcode_args_map_the_graph_and_audio() {
        let a = transcode_args(
            Utf8Path::new("in.mp4"),
            Utf8Path::new("out.mp4"),
            "[0:v]null[v]",
            &["-c:v".into(), "libx264".into()],
            "128k",
        )
        .join(" ");
        assert_eq!(
            a,
            "-y -hide_banner -loglevel error -i in.mp4 -filter_complex [0:v]null[v] -map [v] \
             -map 0:a? -pix_fmt yuv420p -c:v libx264 -c:a aac -b:a 128k -ar 48000 -ac 2 \
             -movflags +faststart out.mp4"
        );
    }

    #[test]
    fn reformat_plan() {
        let master = SourceVideo { width: 1920, height: 1080, codec: "h264".into(), duration_secs: 30.0, fps: 30.0 };
        let opts = |p| ReformatOptions { preset: p, fit: None, card_windows: vec![] };
        let reels = plan_reformat(&master, &opts(OutputPreset::Reels));
        assert_eq!((reels.width, reels.height, reels.fit, reels.copy), (1080, 1920, Some(Fit::Blur), false));
        // Already the right shape, no cap: a stream copy.
        assert!(plan_reformat(&master, &opts(OutputPreset::Youtube)).copy);
        let wa = plan_reformat(&master, &opts(OutputPreset::Whatsapp));
        assert_eq!((wa.copy, wa.max_bitrate), (false, Some(3_000_000)));
        let crop = plan_reformat(
            &master,
            &ReformatOptions { preset: OutputPreset::Square, fit: Some(Fit::Crop), card_windows: vec![] },
        );
        assert_eq!(crop.fit, Some(Fit::Crop));
    }

    #[test]
    fn versions_sit_next_to_the_source() {
        let p = |s: &str| PathBuf::from(s);
        let v = |s: &str, preset, fit| version_path(&p(s), preset, fit);
        assert_eq!(v("/projects/out/reel.mp4", OutputPreset::Reels, None), p("/projects/out/reel.reels.mp4"));
        assert_eq!(
            v("/projects/out/versions/reel__20261109T101010_job_3.mp4", OutputPreset::Square, None),
            p("/projects/out/versions/reel__20261109T101010_job_3.square.mp4")
        );
        // A version made from a version (its render gone, see
        // `reformat_source`) keeps the whole stem: it never takes the
        // render's own `reel.reels.mp4`.
        assert_eq!(
            v("/projects/out/reel.reels.mp4", OutputPreset::Whatsapp, None),
            p("/projects/out/reel.reels.whatsapp.mp4")
        );
        assert_eq!(
            v("/projects/out/reel.square-crop.mp4", OutputPreset::Reels, None),
            p("/projects/out/reel.square-crop.reels.mp4")
        );
        // The default fit isn't spelled out; the others are.
        assert_eq!(v("/projects/out/reel.mp4", OutputPreset::Reels, Some(Fit::Blur)), p("/projects/out/reel.reels.mp4"));
        assert_eq!(v("/projects/out/reel.mp4", OutputPreset::Reels, Some(Fit::Crop)), p("/projects/out/reel.reels-crop.mp4"));
        // A dotted stem that isn't a preset keeps its dots.
        assert_eq!(v("/projects/out/trip.2026.mp4", OutputPreset::Reels, None), p("/projects/out/trip.2026.reels.mp4"));
    }

    #[test]
    fn card_windows_come_from_the_manifest() {
        let d = std::env::temp_dir().join(format!("vibecut_output_format_windows_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let video = d.join("reel.mp4");
        assert!(manifest_card_windows(&video).is_empty());
        std::fs::write(d.join("reel.manifest.json"), br#"{"card_windows": [[0.0, 3.0], [27.5, 30.0]]}"#).unwrap();
        assert_eq!(manifest_card_windows(&video), vec![[0.0, 3.0], [27.5, 30.0]]);
        // A version reads its render's manifest; a dotted name that isn't a
        // version doesn't.
        assert_eq!(manifest_card_windows(&d.join("reel.reels.mp4")), vec![[0.0, 3.0], [27.5, 30.0]]);
        assert_eq!(manifest_card_windows(&d.join("reel.square.reels.mp4")), vec![[0.0, 3.0], [27.5, 30.0]]);
        assert!(manifest_card_windows(&d.join("reel.2026.mp4")).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn versions_are_made_from_the_render() {
        let d = std::env::temp_dir().join(format!("vibecut_output_format_source_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let touch = |n: &str| std::fs::write(d.join(n), b"x").unwrap();
        touch("coast.mp4");
        touch("coast.square.mp4");
        touch("coast.reels-crop.mp4");
        touch("coast.square.whatsapp.mp4");
        touch("trip.2026.mp4");
        // A version's versions come from the render next to it, so a
        // vertical version of the square one lands in the render's own
        // slot with the render's picture (not a 9:16 blur of a 1:1 blur).
        assert_eq!(reformat_source(&d.join("coast.square.mp4")), d.join("coast.mp4"));
        assert_eq!(
            version_path(&reformat_source(&d.join("coast.square.mp4")), OutputPreset::Reels, None),
            d.join("coast.reels.mp4")
        );
        // A crop version asked for as reels must not stream-copy into the blur slot.
        assert_eq!(reformat_source(&d.join("coast.reels-crop.mp4")), d.join("coast.mp4"));
        // Down a chain of versions, too.
        assert_eq!(reformat_source(&d.join("coast.square.whatsapp.mp4")), d.join("coast.mp4"));
        // The render itself, and names that aren't versions, stay as they are.
        assert_eq!(reformat_source(&d.join("coast.mp4")), d.join("coast.mp4"));
        assert_eq!(reformat_source(&d.join("trip.2026.mp4")), d.join("trip.2026.mp4"));
        // Render gone: the version is the source, and its versions keep its stem.
        std::fs::remove_file(d.join("coast.mp4")).unwrap();
        let src = reformat_source(&d.join("coast.square.mp4"));
        assert_eq!(src, d.join("coast.square.mp4"));
        assert_eq!(version_path(&src, OutputPreset::Reels, None), d.join("coast.square.reels.mp4"));
        // A render in another container is found as well.
        touch("coast.mov");
        assert_eq!(reformat_source(&d.join("coast.square.mp4")), d.join("coast.mov"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn progress_lines() {
        assert_eq!(parse_progress_secs("out_time_us=1500000"), Some(1.5));
        assert_eq!(parse_progress_secs("out_time_ms=2000000"), Some(2.0));
        assert_eq!(parse_progress_secs("out_time_us=N/A"), None);
        assert_eq!(parse_progress_secs("frame=12"), None);
    }
}

/// [`reformat_video`] against real ffmpeg: it writes through a staging
/// name, and a Stop kills its pass and leaves the last good version alone.
#[cfg(test)]
mod reformat_job_tests {
    use super::*;
    use crate::cancel::{is_cancelled, CancelToken};
    use crate::progress::with_sink_and_cancel;
    use crate::staging::staged_files;

    fn ffmpeg_available() -> bool {
        Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// A small 16:9 "render" with sound, `secs` long.
    fn render_clip(name: &str, secs: u32) -> (Utf8PathBuf, Utf8PathBuf) {
        let d = std::env::temp_dir().join(format!("vibecut_reformat_job_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let d = Utf8PathBuf::from_path_buf(d).unwrap();
        let clip = d.join("coast.mp4");
        let ok = Command::new("ffmpeg")
            .args([
                "-y", "-v", "error", "-nostdin",
                "-f", "lavfi", "-i", &format!("testsrc2=size=640x360:rate=30:duration={secs}"),
                "-f", "lavfi", "-i", &format!("sine=frequency=330:duration={secs}"),
                "-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-shortest",
                clip.as_str(),
            ])
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "generating {clip}");
        (d, clip)
    }

    fn opts(preset: OutputPreset) -> ReformatOptions {
        ReformatOptions { preset, fit: None, card_windows: Vec::new() }
    }

    #[test]
    fn a_finished_reformat_is_committed_from_its_staging_name() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not on PATH; skipping");
            return;
        }
        let (d, clip) = render_clip("commit", 1);
        let target = d.join("coast.whatsapp.mp4");
        let mut saw_staged = false;
        reformat_video(&clip, &target, &opts(OutputPreset::Whatsapp), &mut |_| {
            saw_staged |= !staged_files(&target).is_empty() && !target.exists();
        })
        .expect("reformat");
        assert!(saw_staged, "ffmpeg didn't write under the staging name");
        assert!(staged_files(&target).is_empty(), "staging file left behind");
        let made = probe_source(&target).unwrap();
        assert_eq!((made.width, made.height), (720, 1280));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_stopped_reformat_kills_ffmpeg_and_keeps_the_last_good_version() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not on PATH; skipping");
            return;
        }
        let (d, clip) = render_clip("stop", 20);
        let target = d.join("coast.reels.mp4");
        std::fs::write(&target, b"the last good version").unwrap();
        let token = CancelToken::new();
        let stop = token.clone();
        let started = std::time::Instant::now();
        let r = with_sink_and_cancel(|_| {}, token, || {
            reformat_video(&clip, &target, &opts(OutputPreset::Reels), &mut |f| {
                // Stop as soon as ffmpeg is under way.
                if f > 0.0 {
                    stop.cancel();
                }
            })
        });
        let err = r.expect_err("a stopped reformat must not succeed");
        assert!(is_cancelled(&err), "stopped with a non-cancel error: {err:#}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(15),
            "the Stop didn't end the pass early ({:?})",
            started.elapsed()
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"the last good version");
        assert!(staged_files(&target).is_empty(), "partial version left behind");
        // Cancelled before it starts: ffmpeg never runs.
        let token = CancelToken::new();
        token.cancel();
        let r = with_sink_and_cancel(|_| {}, token, || {
            run_ffmpeg_with_progress(&["-version".to_string()], 1.0, &mut |_| {})
        });
        assert!(r.is_err_and(|e| is_cancelled(&e)));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Box speed (source px/s) and start of the synthetic subject.
    const BOX_SPEED: f64 = 95.0;
    const BOX_X0: f64 = 20.0;
    const BOX: f64 = 48.0;

    /// A 16:9 640×360 clip, 6 s: a yellow box crossing a static, textured
    /// grey background (desaturated SMPTE bars) left to right.
    fn moving_subject_clip(d: &Utf8Path) -> Utf8PathBuf {
        let clip = d.join("ride.mp4");
        let graph = format!(
            "smptebars=s=640x360:r=30:d=6,hue=s=0[bg];color=c=yellow:s=48x48:r=30:d=6[box];\
             [bg][box]overlay=x='{BOX_X0}+{BOX_SPEED}*t':y=156:shortest=1,format=yuv420p[v]"
        );
        let ok = Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-nostdin", "-filter_complex", &graph, "-map", "[v]"])
            .args(["-c:v", "libx264", "-preset", "ultrafast", clip.as_str()])
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "generating {clip}");
        clip
    }

    /// One frame of `video` at `t`, as `w × h` RGB.
    fn grab_rgb(video: &Utf8Path, t: f64, w: u32, h: u32) -> Vec<u8> {
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-ss", &format!("{t:.3}"), "-i", video.as_str()])
            .args(["-frames:v", "1", "-vf", &format!("scale={w}:{h}"), "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .output()
            .expect("ffmpeg frame grab");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(out.stdout.len(), (w * h * 3) as usize);
        out.stdout
    }

    /// The yellow box in an RGB frame: its pixel count and bounding box.
    fn find_box(rgb: &[u8], w: u32) -> (usize, [u32; 4]) {
        let mut n = 0;
        let mut bb = [u32::MAX, u32::MAX, 0, 0];
        for (i, px) in rgb.chunks(3).enumerate() {
            if px[0] > 170 && px[1] > 170 && px[2] < 90 {
                let (x, y) = (i as u32 % w, i as u32 / w);
                n += 1;
                bb = [bb[0].min(x), bb[1].min(y), bb[2].max(x), bb[3].max(y)];
            }
        }
        (n, bb)
    }

    #[test]
    fn a_smart_vertical_version_keeps_the_moving_subject_in_frame() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not on PATH; skipping");
            return;
        }
        let (d, _) = render_clip("smart", 1);
        let clip = moving_subject_clip(&d);
        let target = d.join("ride.reels-smart.mp4");
        let opts = ReformatOptions { preset: OutputPreset::Reels, fit: Some(Fit::Smart), card_windows: Vec::new() };
        let mut last = 0.0f32;
        let mut monotonic = true;
        let done = reformat_video(&clip, &target, &opts, &mut |f| {
            monotonic &= f >= last;
            last = f;
        })
        .expect("smart reformat");
        assert!(monotonic && last == 1.0, "progress went backwards or never finished");
        let path = done.path.expect("a smart fit follows a path");
        assert_eq!((path.shots, path.followed_shots), (1, 1), "{}", path.describe());
        let src = probe_source(&clip).unwrap();
        let made = probe_source(&target).unwrap();
        assert_eq!((made.width, made.height), (1080, 1920));
        assert!((made.duration_secs - src.duration_secs).abs() <= 1.0 / 30.0 + 1e-3, "{made:?} vs {src:?}");
        // The crop is 9:16 of the 360-px-tall source: 202.5 px of 640.
        let crop_w = 360.0 * 9.0 / 16.0;
        // Grab at a quarter of the output size; the box is 48 × 1920/360
        // × 1/4 = 64 px square there.
        let (gw, gh) = (270, 480);
        let side = BOX * 1920.0 / 360.0 / 4.0;
        for t in [0.5, 1.5, 2.5, 3.5, 4.5, 5.5] {
            let (n, [x0, y0, x1, y1]) = find_box(&grab_rgb(&target, t, gw, gh), gw);
            // All of it, away from the edges: never cut by the crop.
            assert!(n as f64 >= 0.85 * side * side, "t={t}: only {n} px of the box in frame");
            assert!(x0 > 1 && x1 < gw - 2 && y0 > 1 && y1 < gh - 2, "t={t}: box at {x0}..{x1} x {y0}..{y1}");
            // The centre crop would have lost it at the start and the end.
            let bx = BOX_X0 + BOX_SPEED * t;
            let centre_crop = (320.0 - crop_w / 2.0, 320.0 + crop_w / 2.0);
            if t < 1.0 || t > 5.0 {
                assert!(bx + BOX < centre_crop.0 || bx > centre_crop.1, "t={t}: the centre crop holds it too");
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A render replaced the video while its version was being made: the
    /// pass that read the old cut isn't committed; the version is made
    /// again from the new one.
    #[test]
    fn a_version_of_a_video_replaced_mid_pass_is_made_again() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not on PATH; skipping");
            return;
        }
        let (d, clip) = render_clip("replaced", 1);
        // The next render: 2 s long, so the versions tell the cuts apart.
        let (d2, next_render) = render_clip("replaced_next", 2);
        let target = d.join("coast.whatsapp.mp4");
        let mut passes = 0;
        reformat_video_with(&clip, &target, &opts(OutputPreset::Whatsapp), &mut |_| {}, &mut || {
            passes += 1;
            if passes == 1 {
                // The render commits its new cut by rename, mid-pass.
                std::fs::copy(&next_render, d.join("next.mp4")).unwrap();
                std::fs::rename(d.join("next.mp4"), &clip).unwrap();
            }
        })
        .expect("reformat");
        assert_eq!(passes, 2, "the pass over the old cut was committed");
        let made = probe_source(&target).unwrap();
        assert!((made.duration_secs - 2.0).abs() < 0.2, "the version shows the old cut ({} s)", made.duration_secs);
        assert!(staged_files(&target).is_empty());

        // Replaced on every pass: it gives up, and the last good version stays.
        std::fs::write(&target, b"the last good version").unwrap();
        let mut bump = 0u64;
        let r = reformat_video_with(&clip, &target, &opts(OutputPreset::Whatsapp), &mut |_| {}, &mut || {
            bump += 1;
            let t = std::time::SystemTime::now() + std::time::Duration::from_secs(60 * bump);
            std::fs::File::options().write(true).open(&clip).unwrap().set_modified(t).unwrap();
        });
        let err = r.expect_err("a version of a video that keeps changing");
        assert!(err.to_string().contains("kept changing"), "{err:#}");
        assert_eq!(std::fs::read(&target).unwrap(), b"the last good version");
        assert!(staged_files(&target).is_empty());
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&d2);
    }
}
