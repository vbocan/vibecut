//! TOML configuration schema, defaults, and validation.
//!
//! [`Config::load`] reads a `.toml` file from disk, deserializes via serde
//! with `deny_unknown_fields` (so typos surface as errors, not silent drops),
//! and runs [`Config::validate`] for cross-field invariants (e.g.
//! `min_clip ≤ max_clip`). Sensible defaults live in `Default` impls so an
//! example file with only a few fields still produces a usable pipeline.
//!
//! # Schema sections (mirrors the TOML)
//!
//! ```toml
//! schema_version = 1
//!
//! [project]      # name, output path, target_duration, resolution, fps,
//!                # video fade_in / fade_out
//! [input]        # folder + glob patterns + optional manual time ranges
//! [soundtrack]   # file, beat_align, ducking knobs, smart-loop fade, …
//! [intro]        # one dedicated card or text overlay; mode = card | overlay
//! [outro]        # same shape as [intro]
//! [scoring]      # per-signal weights (hilight, accel, motion, …) + floors
//! [selection]    # min_clip / max_clip / order + max_per_source override
//! [transition]   # default xfade name + duration + palette
//! [grading]      # optional LUT3D + stabilize toggle
//! [ml]           # ONNX Runtime engine knobs (Silero VAD)
//! [render]       # encoder preference (auto/libx264/libx265/nvenc/amf/qsv) + crf
//! ```
//!
//! Domain types like resolution (`"1920x1080"`), color (`"#RRGGBB[AA]"`), and
//! human durations (`"5m"` / `"400ms"`) are parsed at point of use rather
//! than at deserialization — keeps the schema layer simple and validation
//! errors contextual.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    Io {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error(
        "schema_version {found} is not supported (this build understands {supported})"
    )]
    UnsupportedSchemaVersion { found: u32, supported: u32 },
    #[error("validation: {0}")]
    Validation(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct Config {
    pub schema_version: u32,

    pub project: Project,
    pub input: Input,

    #[serde(default)]
    pub soundtrack: Option<Soundtrack>,

    /// Title card before the body. Use `[intro]` in the TOML.
    #[serde(default)]
    pub intro: Option<TextCard>,
    /// End card after the body. Use `[outro]` in the TOML.
    #[serde(default)]
    pub outro: Option<TextCard>,

    #[serde(default)]
    pub scoring: Scoring,
    #[serde(default)]
    pub selection: Selection,
    #[serde(default)]
    pub transition: Transition,

    #[serde(default, rename = "text")]
    #[cfg_attr(feature = "ts-export", ts(rename = "text"))]
    pub texts: Vec<Text>,

    #[serde(default)]
    pub multi_angle: MultiAngle,
    #[serde(default)]
    pub grading: Grading,

    /// One or more output files. The renderer produces the body at the
    /// HIGHEST resolution among these, then transcodes the body to each
    /// output's resolution + encoder settings. Use `[[outputs]]` in TOML.
    #[serde(rename = "outputs")]
    pub outputs: Vec<RenderOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct Project {
    pub name: String,
    #[serde(with = "humantime_serde")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub target_duration: Duration,
    pub fps: u32,
    #[serde(default)]
    pub seed: Option<u64>,
    /// Fade-in from black over this duration at the very start. 0 = none.
    #[serde(with = "humantime_serde", default = "default_video_fade_in")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub fade_in: Duration,
    /// Fade the final video to black over this duration at the very end.
    /// 0 = no fade. Default 0.5s.
    #[serde(with = "humantime_serde", default = "default_video_fade_out")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub fade_out: Duration,
    /// Where the pipeline stages keep their intermediate files (normalized
    /// segments, stabilization transforms, intro/outro cards, body checkpoint).
    /// Defaults to `<first output's parent>/.vibecut_<project_name>` if absent.
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(type = "string | null"))]
    pub temp_dir: Option<Utf8PathBuf>,
    /// Directory holding ONNX model files (`silero_vad.onnx`, future face /
    /// scene-detection nets). The pipeline auto-loads any model present;
    /// when missing it warns and gracefully falls back to non-ML behaviour.
    /// Default `"models"` relative to the working directory. A model not
    /// found here is looked up in `$VIBECUT_MODELS_DIR`, then `/app/models`
    /// (see `ml_engine::ModelDirs`).
    #[serde(default = "default_models_dir")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub models_dir: Utf8PathBuf,
}

fn default_models_dir() -> Utf8PathBuf {
    Utf8PathBuf::from("models")
}

/// One target output file. The body is rendered once in the master shape
/// (see [`Config::body_resolution`]), then transcoded to each output's
/// resolution + encoder settings, reframed when its aspect differs.
/// [`RenderOutput::resolve`] applies the platform `preset`'s defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct RenderOutput {
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub path: Utf8PathBuf,
    /// `WIDTHxHEIGHT`, e.g. `"3840x2160"` or `"1920x1080"`. Required unless
    /// `preset` supplies it; overrides the preset's when both are set.
    #[serde(default)]
    pub resolution: Option<String>,
    #[serde(default)]
    pub encoder: Encoder,
    /// Quality factor: `-crf` for libx264/265, `-cq` for NVENC, etc.
    /// Default: the preset's, else 20.
    #[serde(default)]
    pub crf: Option<u32>,
    /// Platform preset: `master`, `youtube`, `reels`, `tiktok`, `shorts`,
    /// `square` or `whatsapp` (see [`crate::output_format::OutputPreset`]).
    /// An encoder preset name here (`"p5"`, `"veryfast"`) is still read as
    /// `encoder_preset`, for configs written before platform presets.
    #[serde(default)]
    pub preset: Option<String>,
    /// AAC bitrate string, e.g. `"192k"`. Default: the preset's, else `"192k"`.
    #[serde(default)]
    pub audio_bitrate: Option<String>,
    /// How a frame fills this output when its aspect differs from the
    /// body's: `blur` (default), `crop` or `pad`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub fit: Option<crate::output_format::Fit>,
    /// Encoder-specific preset, e.g. `"p5"` (NVENC), `"veryfast"` (libx264),
    /// for this output's own encode pass. `None` → the encoder's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub encoder_preset: Option<String>,
    /// Peak video bitrate, e.g. `"3M"`; `"none"` drops a preset's cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub max_bitrate: Option<String>,
}

impl RenderOutput {
    /// Apply the preset's defaults to every key this output leaves unset.
    pub fn resolve(&self) -> Result<crate::output_format::ResolvedOutput, String> {
        crate::output_format::resolve_output(self)
    }
}

/// Accept either `file = "x"` (single string) or `files = ["x", "y"]`
/// (string array) for `[soundtrack]`. Single → `Vec` with one element.
fn deser_path_or_paths<'de, D>(deserializer: D) -> Result<Vec<Utf8PathBuf>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{Error, Visitor};
    use std::fmt;

    struct PathOrPaths;
    impl<'de> Visitor<'de> for PathOrPaths {
        type Value = Vec<Utf8PathBuf>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a path string or an array of path strings")
        }
        fn visit_str<E: Error>(self, v: &str) -> Result<Self::Value, E> {
            Ok(vec![Utf8PathBuf::from(v)])
        }
        fn visit_string<E: Error>(self, v: String) -> Result<Self::Value, E> {
            Ok(vec![Utf8PathBuf::from(v)])
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(s) = seq.next_element::<String>()? {
                out.push(Utf8PathBuf::from(s));
            }
            Ok(out)
        }
    }
    deserializer.deserialize_any(PathOrPaths)
}

fn default_video_fade_in() -> Duration {
    Duration::from_millis(500)
}
fn default_video_fade_out() -> Duration {
    Duration::from_millis(500)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct Input {
    /// Folder to scan for video files. Optional when `videos` lists files
    /// explicitly. If both are provided the scanned files are merged with the
    /// explicit list (duplicates by path are deduped).
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(type = "string | null"))]
    pub folder: Option<Utf8PathBuf>,
    #[serde(default = "default_includes")]
    pub include: Vec<String>,
    #[serde(default)]
    pub ranges: Vec<InputRange>,
    /// Explicit video file paths to include as source material. Each file is
    /// probed and scored the same way as folder-discovered videos.
    #[serde(default)]
    pub videos: Vec<ExplicitVideo>,
    /// Still images to animate and mix into the output. Each image is rendered
    /// as a video clip with a Ken Burns / cinematic motion effect. Images are
    /// treated as mandatory segments — they always appear in the output,
    /// interleaved with the auto-selected video clips.
    #[serde(default)]
    pub images: Vec<ImageInput>,
}

/// An explicitly listed video file (alternative to folder-scan discovery).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct ExplicitVideo {
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub path: Utf8PathBuf,
}

fn default_includes() -> Vec<String> {
    vec!["**/*.MP4".into(), "**/*.mp4".into()]
}

/// One still image to animate into a video clip.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct ImageInput {
    /// Path to the image file (JPEG, PNG, BMP, WebP, TIFF, …).
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub path: Utf8PathBuf,
    /// How long the animated clip should run in the output. Default 4 s.
    #[serde(with = "humantime_serde", default = "default_image_hold_duration")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub duration: Duration,
    /// Cinematic motion effect applied to the still image. Default `auto`
    /// (picks an effect deterministically from `[project].seed`).
    #[serde(default)]
    pub effect: ImageEffect,
    /// Recording timestamp (ISO-8601 / RFC-3339, e.g. `2026-05-20T08:08:46Z`).
    /// Used when `[selection].order = "strict_chronological"` to merge the
    /// image with video segments at its real recording position instead of
    /// the default evenly-spaced interleave. Ignored under any other order.
    #[serde(default)]
    pub timestamp: Option<String>,
}

fn default_image_hold_duration() -> Duration {
    Duration::from_secs(4)
}

/// Motion effect applied when converting a still image to an animated clip.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export, rename_all = "snake_case"))]
pub enum ImageEffect {
    /// Deterministically choose an effect from the project seed.
    #[default]
    Auto,
    /// Ken Burns: slow zoom toward the image center.
    ZoomIn,
    /// Ken Burns: start zoomed in, slowly pull back to reveal more.
    ZoomOut,
    /// Camera panning slowly from right to left across the image.
    PanLeft,
    /// Camera panning slowly from left to right across the image.
    PanRight,
    /// Diagonal zoom + pan from a corner toward the image center.
    Diagonal,
    /// Slow clockwise rotation combined with a slight zoom.
    RotateZoom,
    /// No movement — static letterboxed hold.
    Static,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct InputRange {
    pub file: String,
    #[serde(with = "humantime_serde")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub start: Duration,
    #[serde(with = "humantime_serde")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub end: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct Soundtrack {
    /// Audio file(s). When more than one is given they are concatenated in
    /// declaration order to form a single virtual track, then beat-detected
    /// and looped as if it were one song. `file = "x.mp3"` is accepted as
    /// shorthand for `files = ["x.mp3"]`.
    #[serde(default, alias = "file", deserialize_with = "deser_path_or_paths")]
    #[cfg_attr(feature = "ts-export", ts(type = "string[]"))]
    pub files: Vec<Utf8PathBuf>,
    #[serde(default = "default_true")]
    pub beat_align: bool,
    /// Clip lengths follow the song: short clips where it is loud and busy,
    /// long holds where it is quiet (needs `beat_align`). See `pacing`.
    #[serde(default = "default_true")]
    pub energy_drives_pacing: bool,
    /// How far `energy_drives_pacing` bends clip lengths, 0..1: 0 = even
    /// lengths, 1 = the loudest music cuts ~2.8× faster than average and
    /// the quietest holds ~2.8× longer (always within `[selection]`
    /// `min_clip`..`max_clip`). Default 0.6.
    #[serde(default = "default_pacing_strength")]
    pub pacing_strength: f32,
    /// End the edit musically (needs `beat_align`): the last cut on the
    /// song's final hit when its own ending fits the target length, else
    /// on the phrase start nearest it, and the music's fade completes at
    /// the end of a bar. Default true.
    #[serde(default = "default_true")]
    pub musical_ending: bool,
    /// Put the best moment on the downbeat that opens the song's drop (or
    /// the biggest section that plays in the video), needs `beat_align`.
    /// `energy_arc` is arranged around it; other orders keep their
    /// sequence. Default true.
    #[serde(default = "default_true")]
    pub hero_on_drop: bool,
    /// Soundtrack volume during a duck (in dB). Default -18 dB.
    #[serde(default = "default_ducking_db")]
    pub ducking_db: f32,
    /// How many segments to "duck" — i.e. fade the soundtrack out so the
    /// original GoPro audio is briefly heard. Default 2. Set to 0 to disable.
    #[serde(default = "default_duck_count")]
    pub duck_count: u32,
    /// Minimum spacing between duck windows in the output timeline.
    #[serde(with = "humantime_serde", default = "default_duck_min_spacing")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub duck_min_spacing: Duration,
    /// Linear fade-in / fade-out ramp around each duck window.
    #[serde(with = "humantime_serde", default = "default_duck_ramp")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub duck_ramp: Duration,
    /// Minimum duck-window length (the source-audio reveal is at least this long,
    /// independent of the segment length the duck started from).
    #[serde(with = "humantime_serde", default = "default_duck_min_dur")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub duck_min_dur: Duration,
    /// Maximum duck-window length. The actual length linearly interpolates
    /// between min and max based on the segment's audio interestingness score.
    #[serde(with = "humantime_serde", default = "default_duck_max_dur")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub duck_max_dur: Duration,
    /// Fade out the soundtrack over this duration at the very end of the
    /// output. Avoids the "abrupt cutoff" feel.
    #[serde(with = "humantime_serde", default = "default_soundtrack_fade_out")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub fade_out: Duration,
    /// Rapid beat-aligned cut burst (a "music-video moment" with
    /// each clip lasting one beat). `auto` = the system enables it iff the
    /// soundtrack's tempo is in [80, 180] BPM, its beat pulse is clearly
    /// detectable, and its loud region is at least 6 s.
    #[serde(default)]
    pub beat_burst: BeatBurstMode,
    /// Minimum number of beats the burst should run for. If suitability
    /// passes but fewer than this many beats fit in the soundtrack's loud
    /// region, the burst is silently skipped.
    #[serde(default = "default_beat_burst_min_beats")]
    pub beat_burst_min_beats: u32,
    /// Maximum beats per burst. The actual count is whatever fits in the
    /// loud region, clamped to [`beat_burst_min_beats`, `beat_burst_max_beats`].
    #[serde(default = "default_beat_burst_max_beats")]
    pub beat_burst_max_beats: u32,
    /// How strongly speech presence (Silero VAD) boosts a segment's
    /// duck-window score. Final score = audio_interestingness ×
    /// (1 + vad_speech_weight × peak_speech_prob). Default 1.5 → a segment
    /// with peak_speech = 1.0 gets 2.5× the duck priority. Only consulted
    /// when `silero_vad.onnx` is found in `[project].models_dir`,
    /// `$VIBECUT_MODELS_DIR` or `/app/models` (the Docker image always has
    /// it there); 0 turns the speech boost off.
    #[serde(default = "default_vad_speech_weight")]
    pub vad_speech_weight: f32,
}

fn default_vad_speech_weight() -> f32 {
    1.5
}

fn default_pacing_strength() -> f32 {
    0.6
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export, rename_all = "snake_case"))]
pub enum BeatBurstMode {
    /// Enable when the suitability gate (tempo / pulse strength / loud-region
    /// length) passes; otherwise silently skip.
    #[default]
    Auto,
    /// Never insert a burst.
    Off,
    /// Always try; surfaces a warning if suitability fails but still attempts.
    On,
}

fn default_beat_burst_min_beats() -> u32 {
    8
}
fn default_beat_burst_max_beats() -> u32 {
    12
}

fn default_true() -> bool {
    true
}
fn default_ducking_db() -> f32 {
    -18.0
}
fn default_duck_count() -> u32 {
    2
}
fn default_duck_min_spacing() -> Duration {
    Duration::from_secs(4)
}
fn default_duck_ramp() -> Duration {
    Duration::from_millis(300)
}
fn default_duck_min_dur() -> Duration {
    Duration::from_secs(4)
}
fn default_duck_max_dur() -> Duration {
    Duration::from_secs(7)
}
fn default_soundtrack_fade_out() -> Duration {
    Duration::from_millis(1500)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct Scoring {
    pub hilight_tags: f32,
    pub accel_peaks: f32,
    pub gps_speed: f32,
    pub faces: f32,
    /// Visual motion peaks weight (frame-difference MSD).
    pub motion_peaks: f32,
    /// Reject frames in the bottom `sharpness_floor` fraction of Laplacian
    /// variance (e.g. 0.2 = bottom 20% rejected). 0 = no floor.
    pub sharpness_floor: f32,
    /// Same idea for exposure score (1 − clipped-pixel fraction). 0 = no floor.
    pub exposure_floor: f32,
    pub diversity_per_source: f32,
}

impl Default for Scoring {
    fn default() -> Self {
        // Sensible out-of-the-box weights so a config without [scoring]
        // produces a useful score curve.
        Self {
            hilight_tags: 1.0,
            accel_peaks: 0.6,
            gps_speed: 0.4,
            faces: 0.3,
            motion_peaks: 0.3,
            sharpness_floor: 0.2,
            exposure_floor: 0.15,
            diversity_per_source: 0.5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct Selection {
    #[serde(with = "humantime_serde", default = "default_min_clip")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub min_clip: Duration,
    #[serde(with = "humantime_serde", default = "default_max_clip")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub max_clip: Duration,
    #[serde(default)]
    pub order: Order,
    /// Optional override for the per-source pick cap. When `None`, the picker
    /// auto-computes `ceil(K / n_sources_with_signal)` — fine for short cuts
    /// over many sources but may underfill long cuts when each source can only
    /// contribute a couple of strong moments. Set this to allow more picks
    /// per source (e.g. 5) when target duration is large vs. source count.
    #[serde(default)]
    pub max_per_source: Option<u32>,
    /// Selection-time score jitter strength. Each candidate moment's score is
    /// multiplied by `1 + r * variety`, where `r` is a deterministic random
    /// number in `[-1, 1]` driven by `[project].seed`. Strong moments still
    /// usually win, but close-second candidates sometimes leapfrog them — so
    /// changing the seed (or this value) produces a different cut each time.
    ///
    /// `0.0` (default) = strict deterministic top-K; same seed → same picks.
    /// `0.15` = "two clips might swap occasionally".
    /// `0.30` = "noticeable shake-up while quality stays good".
    /// `0.50+` = real variety; you'll occasionally see a much weaker moment win.
    #[serde(default)]
    pub variety: f32,
    /// Source-window exclusions. The MCP server's
    /// `apply_feedback(DropSegment)` writes one entry per "drop the bit
    /// at M:SS" feedback. The picker zeros the score curve in any
    /// matching range, so the next render won't pick that range.
    /// Hand-edits work too: just add a `[[selection.exclude]]` entry.
    #[serde(default)]
    pub exclude: Vec<SelectionExclude>,
    /// Per-source scoring weights. The picker multiplies
    /// each source's score curve by its weight before composing the
    /// pick set. The MCP server's `apply_feedback(MoreOf)` /
    /// `apply_feedback(LessOf)` actions double / halve entries here
    /// (between 0.125 and 8; never 0). Hand-edits
    /// work too — drop in a `[[selection.source_weights]]` entry to
    /// tilt the picker for a given render.
    #[serde(default)]
    pub source_weights: Vec<SourceWeight>,
}

/// One per-source scoring multiplier. `weight = 1.5` means "give this
/// source 50 % more weight" — the picker multiplies its score curve
/// by 1.5 before composing the pick set. `weight = 0.0` effectively
/// drops the source from candidacy entirely (it only fills slots
/// nothing else can). Last-write-wins if the
/// same `source_id` appears more than once. `source_id` matches the
/// source's id ignoring letter case (`"dji_0001"` weights `DJI_0001`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct SourceWeight {
    /// Source ID (the trailing-digits-of-GoPro-filename grouping —
    /// e.g. `"0320"` for chapters `GH010320.MP4`, `GH020320.MP4`).
    pub source_id: String,
    /// Multiplier on the score curve. 1.0 = no change. Clamped to
    /// `[0.0, 10.0]` at apply time (10× is well into "this user
    /// really insists" territory; anything higher is almost certainly
    /// a typo).
    pub weight: f32,
}

/// One source-window the picker must avoid. Matches by chapter
/// filename suffix (so `"GH010320.MP4"` matches whatever absolute
/// path the source resolved to). Multiple entries are additive.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct SelectionExclude {
    /// Filename or filename-suffix to match. Compared case-insensitively
    /// against the basename of each chapter path. Examples:
    /// `"GH010320.MP4"` (exact basename) or `"0320.MP4"` (suffix).
    pub file: String,
    /// Start of the excluded range, in *chapter-local* seconds.
    #[serde(with = "humantime_serde")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub start: Duration,
    /// End of the excluded range, in chapter-local seconds. Must be > `start`.
    #[serde(with = "humantime_serde")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub end: Duration,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            min_clip: default_min_clip(),
            max_clip: default_max_clip(),
            order: Order::default(),
            max_per_source: None,
            exclude: Vec::new(),
            source_weights: Vec::new(),
            variety: 0.0,
        }
    }
}

fn default_min_clip() -> Duration {
    Duration::from_millis(1500)
}
fn default_max_clip() -> Duration {
    Duration::from_secs(6)
}

/// How the picker arranges its chosen segments along the output timeline.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export, rename_all = "snake_case"))]
pub enum Order {
    /// By absolute recording time (`creation_time` + offset). Default. The
    /// safe choice for multi-day / multi-event projects where a backwards
    /// jump (sunset → morning) feels jarring.
    #[default]
    Chronological,
    /// Arrange clips so the energy/score curve forms a deliberate arc —
    /// weakest at the edges, strongest in the middle. Strongest pick lands
    /// at output position `K/2`, second-strongest just to one side, third
    /// to the other, alternating outward. Result: triangular score profile
    /// across the timeline, building to a peak and falling away. Best for
    /// single-event highlight reels where pacing-driven cuts beat strict
    /// chronology. Ignores recording time entirely.
    EnergyArc,
    /// Like `chronological`, but treats images as first-class chronological
    /// events: each image's `[[input.images]].timestamp` is honored and the
    /// image is merged with the video segments at its real recording position,
    /// instead of being inserted at an evenly-spaced slot. Useful for
    /// reportage-style edits mixing many photos with sparse video where the
    /// natural reading order is "by the clock". Requires `timestamp` on every
    /// listed image.
    StrictChronological,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct Transition {
    /// ffmpeg xfade name used at every seam. `"cut"` / `"none"` (or a zero
    /// `duration`) join the clips with straight cuts; the intro/outro cards
    /// and the project fade-in / fade-out are kept either way.
    #[serde(default = "default_transition")]
    pub default: String,
    #[serde(with = "humantime_serde", default = "default_transition_duration")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub duration: Duration,
    #[serde(default)]
    pub palette: Vec<String>,
    /// Speed-ramp ("whip-up") effect on outgoing transitions: the last
    /// `speed_ramp_duration` of the outgoing clip plays at `speed_ramp_factor`
    /// speed before the cut/xfade. Probability `[0..1]` controls how often a
    /// transition gets the ramp — `0.0` = never (default), `1.0` = every
    /// transition, `0.4` = "some, like a typical reel". Per-transition
    /// decision is keyed by `[project].seed` for reproducibility. Audio
    /// (when present) is sped up via `atempo` to match.
    #[serde(default)]
    pub speed_ramp_probability: f32,
    #[serde(with = "humantime_serde", default = "default_speed_ramp_duration")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub speed_ramp_duration: Duration,
    #[serde(default = "default_speed_ramp_factor")]
    pub speed_ramp_factor: f32,
    /// GoPro-Quik-style "whip-up" punch on the ramp itself: a subtle
    /// brightness warm-up across the sped-up tail, then a brief fade-to-white
    /// over the last ~3 frames before the cut. Reads as a kinetic flash of
    /// light right at the seam. Default `true` whenever speed-ramps are
    /// enabled — set `false` for a clean speed-up with no colour treatment.
    #[serde(default = "default_speed_ramp_flash")]
    pub speed_ramp_flash: bool,
}

impl Default for Transition {
    fn default() -> Self {
        Self {
            default: default_transition(),
            duration: default_transition_duration(),
            palette: Vec::new(),
            speed_ramp_probability: 0.0,
            speed_ramp_duration: default_speed_ramp_duration(),
            speed_ramp_factor: default_speed_ramp_factor(),
            speed_ramp_flash: default_speed_ramp_flash(),
        }
    }
}

impl Transition {
    /// Hard-cut mode: straight cuts instead of crossfades, because `default`
    /// is `"cut"` / `"none"` or `duration` is zero. The concat stage, the
    /// beat lock and the feedback actions all go by this.
    pub fn hard_cut(&self) -> bool {
        self.default == "cut" || self.default == "none" || self.duration.is_zero()
    }
}

fn default_transition() -> String {
    "fade".into()
}
fn default_transition_duration() -> Duration {
    Duration::from_millis(400)
}
fn default_speed_ramp_duration() -> Duration {
    Duration::from_millis(350)
}
fn default_speed_ramp_factor() -> f32 {
    2.5
}
fn default_speed_ramp_flash() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct Text {
    pub kind: TextKind,
    pub content: String,
    #[serde(default)]
    pub font: Option<String>,
    #[serde(default = "default_font_size")]
    pub size: u32,
    #[serde(default = "default_color")]
    pub color: String,
    #[serde(default = "default_position")]
    pub position: String,
    #[serde(with = "humantime_serde", default = "default_text_duration")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub duration: Duration,
    #[serde(with = "humantime_serde", default = "zero_duration")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub fade_in: Duration,
    #[serde(with = "humantime_serde", default = "zero_duration")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub fade_out: Duration,
    #[serde(default)]
    pub background: Option<TextBackground>,
}

fn default_font_size() -> u32 {
    72
}
fn default_color() -> String {
    "#FFFFFF".into()
}
fn default_position() -> String {
    "center".into()
}
fn default_text_duration() -> Duration {
    Duration::from_secs(2)
}
fn zero_duration() -> Duration {
    Duration::from_secs(0)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export, rename_all = "snake_case"))]
pub enum TextKind {
    Intro,
    Outro,
    LowerThird,
}

/// Intro / outro card. The `[[<intro|outro>.layers]]` array stacks one or
/// more text and/or image layers on top of a `bg_color` background (or, in
/// `overlay` mode, on top of the first/last selected segment's footage).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct TextCard {
    /// Total time the card is on screen (inclusive of fades).
    #[serde(with = "humantime_serde")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub duration: Duration,
    #[serde(with = "humantime_serde", default = "default_card_fade")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub fade_in: Duration,
    #[serde(with = "humantime_serde", default = "default_card_fade")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub fade_out: Duration,
    /// "card" (default): solid `bg_color` background. "overlay": layers are
    /// drawn directly on top of the first (intro) or last (outro) selected
    /// segment — no separate clip is generated.
    #[serde(default)]
    pub mode: CardMode,
    /// Background color for `card` mode. Ignored in `overlay` mode.
    #[serde(default = "default_card_bg")]
    pub bg_color: String,
    /// Stacked layers — text and/or images. Drawn in declaration order (later
    /// layers paint on top of earlier ones).
    #[serde(default, rename = "layers")]
    pub layers: Vec<CardLayer>,
}

/// One layer of an intro/outro card. Tagged enum: `kind = "text" | "image"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export, tag = "kind", rename_all = "snake_case"))]
pub enum CardLayer {
    Text(TextLayer),
    Image(ImageLayer),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct TextLayer {
    #[serde(alias = "text")]
    pub content: String,
    #[serde(default)]
    pub font: Option<String>,
    #[serde(default = "default_card_font_size")]
    pub size: u32,
    #[serde(default = "default_color")]
    pub color: String,
    /// Anchor: `center`, `top-left`, `top-right`, `top-center`, `bottom-left`,
    /// `bottom-right`, `bottom-center`. Combine with `y_offset` for stacking.
    #[serde(default = "default_position")]
    pub position: String,
    /// Pixel offset applied AFTER the position anchor. Negative pushes up,
    /// positive pushes down. Use to vertically stack multiple text layers.
    #[serde(default)]
    pub y_offset: i32,
    /// Pixel offset applied to x after the position anchor. Default 0.
    #[serde(default)]
    pub x_offset: i32,
    /// Per-layer fade-in. When unset, the layer inherits its parent
    /// `[intro|outro].fade_in`.
    #[serde(default, with = "humantime_serde::option")]
    #[cfg_attr(feature = "ts-export", ts(type = "string | null"))]
    pub fade_in: Option<Duration>,
    /// Per-layer fade-out. When unset, inherits `[intro|outro].fade_out`.
    #[serde(default, with = "humantime_serde::option")]
    #[cfg_attr(feature = "ts-export", ts(type = "string | null"))]
    pub fade_out: Option<Duration>,
    /// Time relative to the card's start at which this layer becomes
    /// visible. Default 0 (visible from card start). Combine with
    /// `disappear_at` to stage multiple text lines (e.g. title fades out at
    /// 2 s, subtitle fades in at 2.2 s).
    #[serde(default, with = "humantime_serde::option")]
    #[cfg_attr(feature = "ts-export", ts(type = "string | null"))]
    pub appear_at: Option<Duration>,
    /// Time relative to the card's start at which this layer disappears.
    /// Default = card's `duration` (visible until card ends).
    #[serde(default, with = "humantime_serde::option")]
    #[cfg_attr(feature = "ts-export", ts(type = "string | null"))]
    pub disappear_at: Option<Duration>,
    /// Optional opaque or semi-transparent box behind the text.
    #[serde(default)]
    pub background: Option<TextBackground>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct ImageLayer {
    /// Path to a PNG or other image (transparency supported).
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub path: Utf8PathBuf,
    /// Same anchor names as text layers.
    #[serde(default = "default_position")]
    pub position: String,
    /// Target height in pixels; width auto-scales to preserve aspect ratio.
    /// Default 200 px.
    #[serde(default = "default_image_height")]
    pub height: u32,
    #[serde(default)]
    pub y_offset: i32,
    #[serde(default)]
    pub x_offset: i32,
    /// Opacity 0..1. Default 1.0.
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    /// Per-layer fade-in. When unset, the layer inherits its parent
    /// `[intro|outro].fade_in`.
    #[serde(default, with = "humantime_serde::option")]
    #[cfg_attr(feature = "ts-export", ts(type = "string | null"))]
    pub fade_in: Option<Duration>,
    /// Per-layer fade-out. When unset, inherits `[intro|outro].fade_out`.
    #[serde(default, with = "humantime_serde::option")]
    #[cfg_attr(feature = "ts-export", ts(type = "string | null"))]
    pub fade_out: Option<Duration>,
    /// Time relative to card start at which the image becomes visible.
    /// Default 0. Same semantics as `TextLayer::appear_at`.
    #[serde(default, with = "humantime_serde::option")]
    #[cfg_attr(feature = "ts-export", ts(type = "string | null"))]
    pub appear_at: Option<Duration>,
    /// Time relative to card start at which the image disappears.
    /// Default = card's `duration`.
    #[serde(default, with = "humantime_serde::option")]
    #[cfg_attr(feature = "ts-export", ts(type = "string | null"))]
    pub disappear_at: Option<Duration>,
}

fn default_image_height() -> u32 {
    200
}
fn default_opacity() -> f32 {
    1.0
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export, rename_all = "snake_case"))]
pub enum CardMode {
    #[default]
    Card,
    Overlay,
}

fn default_card_bg() -> String {
    "#000000".into()
}

fn default_card_font_size() -> u32 {
    96
}
fn default_card_fade() -> Duration {
    Duration::from_millis(400)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct TextBackground {
    pub color: String,
    #[serde(default)]
    pub padding: Option<String>,
}

/// Multi-camera angle planner. When `enabled = true`, the picker treats
/// overlapping recording windows from different cameras as one logical scene
/// and may cut between angles on the same beat.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct MultiAngle {
    #[serde(default)]
    pub enabled: bool,
    /// How to compute the per-member offset (member's local file time = master scene-time + offset):
    /// - `auto` — `audio` if both members have audio, else `creation_time`.
    /// - `creation_time` — delta of MP4 `creation_time` ISO-8601 stamps. Cheap; assumes synced clocks.
    /// - `audio` — refines the `creation_time` guess with a 30 s `cross_correlate` window near the middle of the overlap. Falls back to `creation_time` if confidence < 0.5.
    /// - `manual` — read offsets from each `[[multi_angle.groups]].offsets` table.
    #[serde(default)]
    pub sync: SyncMethod,
    /// Minimum time on one angle before the picker may switch to a different
    /// member of the same group. Avoids epileptic angle cuts.
    #[serde(with = "humantime_serde", default = "default_min_hold")]
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub min_hold: Duration,
    /// Optional explicit groups. When empty and `enabled = true`, groups are
    /// auto-discovered from overlapping recording windows.
    #[serde(default, rename = "groups")]
    pub groups: Vec<AngleGroupCfg>,
}

/// One explicit angle group.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct AngleGroupCfg {
    /// Source id of the master camera. Member offsets are expressed in this
    /// camera's local-time origin.
    pub master: String,
    /// Other source ids in the group (the master is implicit; do not list it here).
    #[serde(default)]
    pub members: Vec<String>,
    /// Manual offsets keyed by source id, only consulted when `sync = "manual"`.
    /// Value is a humantime duration string with optional sign: `"+2.4s"`, `"-800ms"`.
    /// Convention: `member_file_time = master_scene_time + offset`.
    #[serde(default)]
    pub offsets: std::collections::BTreeMap<String, String>,
}

impl Default for MultiAngle {
    fn default() -> Self {
        Self {
            enabled: false,
            sync: SyncMethod::default(),
            min_hold: default_min_hold(),
            groups: Vec::new(),
        }
    }
}

fn default_min_hold() -> Duration {
    Duration::from_millis(1500)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export, rename_all = "snake_case"))]
pub enum SyncMethod {
    #[default]
    Auto,
    CreationTime,
    Audio,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct Grading {
    #[cfg_attr(feature = "ts-export", ts(type = "string | null"))]
    pub lut: Option<Utf8PathBuf>,
    pub stabilize: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export, rename_all = "snake_case"))]
pub enum Encoder {
    #[default]
    Auto,
    Libx264,
    Libx265,
    Nvenc,
    Amf,
    Qsv,
}

// ---------- loading ----------

impl Config {
    pub fn load(path: &camino::Utf8Path) -> Result<Self, ConfigError> {
        let bytes = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        let cfg: Config = toml::from_str(&bytes)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(ConfigError::UnsupportedSchemaVersion {
                found: self.schema_version,
                supported: CURRENT_SCHEMA_VERSION,
            });
        }

        if self.outputs.is_empty() {
            return Err(ConfigError::Validation(
                "at least one [[outputs]] entry is required".into(),
            ));
        }
        for (i, o) in self.outputs.iter().enumerate() {
            if let Err(e) = o.resolve() {
                // Key errors read "outputs[0].resolution must …".
                let sep = if e.starts_with("needs") { " " } else { "." };
                return Err(ConfigError::Validation(format!("outputs[{i}]{sep}{e}")));
            }
            if o.path.as_str().is_empty() {
                return Err(ConfigError::Validation(format!(
                    "outputs[{i}].path is empty"
                )));
            }
        }
        if self.project.fps == 0 {
            return Err(ConfigError::Validation(
                "project.fps must be > 0".into(),
            ));
        }
        if self.project.target_duration.is_zero() {
            return Err(ConfigError::Validation(
                "project.target_duration must be > 0".into(),
            ));
        }

        if self.selection.min_clip > self.selection.max_clip {
            return Err(ConfigError::Validation(format!(
                "selection.min_clip ({:?}) must be <= selection.max_clip ({:?})",
                self.selection.min_clip, self.selection.max_clip
            )));
        }

        if let Some(s) = &self.soundtrack {
            if !(0.0..=1.0).contains(&s.pacing_strength) {
                return Err(ConfigError::Validation(format!(
                    "soundtrack.pacing_strength ({}) must be between 0 and 1",
                    s.pacing_strength
                )));
            }
        }

        for r in &self.input.ranges {
            if r.start >= r.end {
                return Err(ConfigError::Validation(format!(
                    "input.ranges entry {:?}: start must be < end",
                    r.file
                )));
            }
        }

        if matches!(self.selection.order, Order::StrictChronological) {
            for (i, img) in self.input.images.iter().enumerate() {
                if img.timestamp.as_deref().map(str::trim).unwrap_or("").is_empty() {
                    return Err(ConfigError::Validation(format!(
                        "input.images[{i}] ({}) is missing `timestamp`, which is required \
                         when [selection].order = \"strict_chronological\"",
                        img.path
                    )));
                }
            }
        }

        Ok(())
    }

    /// Pick the body's render resolution: the largest master-shaped output
    /// (a vertical or square platform preset never sets it), or a 16:9
    /// master when every output is one. Each output is then transcoded
    /// from this body. See [`crate::output_format::body_resolution`].
    pub fn body_resolution(&self) -> (u32, u32) {
        crate::output_format::body_resolution(&self.resolved_outputs())
    }

    /// Every output with its preset applied. Outputs that don't resolve
    /// are skipped — [`Config::validate`] rejects those.
    pub fn resolved_outputs(&self) -> Vec<crate::output_format::ResolvedOutput> {
        self.outputs.iter().filter_map(|o| o.resolve().ok()).collect()
    }

    /// First output's path — used for manifest naming, cache directory anchor,
    /// and the "primary" output reference in logs.
    pub fn primary_output_path(&self) -> &camino::Utf8Path {
        &self.outputs[0].path
    }

    /// Resolve the workspace (intermediate-file) directory.
    /// Falls back to `<primary_output_dir>/.vibecut_<project_name>` if
    /// `project.temp_dir` is unset.
    pub fn workspace_dir(&self) -> Utf8PathBuf {
        if let Some(d) = &self.project.temp_dir {
            return d.clone();
        }
        let out = self.primary_output_path();
        let parent = out.parent().unwrap_or(camino::Utf8Path::new("."));
        let stem = sanitize_for_path(&self.project.name);
        parent.join(format!(".vibecut_{stem}"))
    }
}

fn sanitize_for_path(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Inlined rather than loaded from an examples/ file — this crate ships
    // with no example projects, so the schema round-trip test carries its
    // own fixture.
    const SAMPLE: &str = r##"
schema_version = 1

[project]
name = "Summer Surf"
target_duration = "60s"
fps = 30
seed = 42

[[outputs]]
path          = "out/summer_surf.mp4"
resolution    = "1920x1080"
encoder       = "auto"
crf           = 20
audio_bitrate = "192k"

[input]
folder = "footage/"
include = ["**/*.MP4", "**/*.mp4"]

[soundtrack]
file = "music/track.wav"
beat_align = true
energy_drives_pacing = true
ducking_db = -18.0

[scoring]
hilight_tags          = 1.0
accel_peaks           = 0.6
gps_speed              = 0.4
faces                 = 0.3
motion_peaks          = 0.3
sharpness_floor       = 0.2
exposure_floor        = 0.15
diversity_per_source  = 0.5

[selection]
min_clip = "1.5s"
max_clip = "6s"
order    = "energy_arc"

[transition]
default  = "fade"
duration = "400ms"
palette  = ["fade", "dissolve", "circleopen"]

[intro]
duration = "2.5s"
fade_in  = "400ms"
fade_out = "400ms"
mode     = "card"
bg_color = "#000000"

[[intro.layers]]
kind     = "text"
content  = "Costa Rica · 2026"
font     = "Inter-Bold.ttf"
size     = 96
color    = "#FFFFFF"
position = "center"

[outro]
duration = "2s"
mode     = "card"

[[outro.layers]]
kind     = "text"
content  = "Example Project"
position = "bottom-right"

[multi_angle]
enabled  = false
sync     = "auto"
min_hold = "1s"

[grading]
stabilize = false
"##;

    #[test]
    fn round_trip_basic_example() {
        let cfg: Config = toml::from_str(SAMPLE).expect("parse");
        cfg.validate().expect("validate");
        // re-serialize and re-parse to make sure the schema is round-trippable
        let s = toml::to_string(&cfg).expect("serialize");
        let cfg2: Config = toml::from_str(&s).expect("re-parse");
        cfg2.validate().expect("re-validate");
        assert_eq!(cfg.project.name, cfg2.project.name);
        assert_eq!(cfg.project.target_duration, cfg2.project.target_duration);
    }

    #[test]
    fn hard_cut_mode_is_cut_none_or_a_zero_duration() {
        let t = |default: &str, ms: u64| Transition {
            default: default.into(),
            duration: Duration::from_millis(ms),
            ..Transition::default()
        };
        assert!(!t("fade", 400).hard_cut());
        assert!(!t("dissolve", 1).hard_cut());
        assert!(t("cut", 400).hard_cut());
        assert!(t("none", 400).hard_cut());
        assert!(t("fade", 0).hard_cut());
    }

    #[test]
    fn unknown_field_rejected() {
        let bad = r#"
            schema_version = 1
            [project]
            name = "x"
            target_duration = "10s"
            fps = 30
            bogus = true
            [input]
            folder = "."
            [[outputs]]
            path = "out.mp4"
            resolution = "1920x1080"
        "#;
        let err = toml::from_str::<Config>(bad).unwrap_err();
        assert!(err.to_string().contains("bogus"), "{err}");
    }

    #[test]
    fn schema_version_mismatch_is_loud() {
        let bad = r#"
            schema_version = 999
            [project]
            name = "x"
            target_duration = "10s"
            fps = 30
            [input]
            folder = "."
            [[outputs]]
            path = "out.mp4"
            resolution = "1920x1080"
        "#;
        let cfg: Config = toml::from_str(bad).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, ConfigError::UnsupportedSchemaVersion { .. }));
    }

    #[test]
    fn soundtrack_file_or_files_both_work() {
        let single = r#"
            schema_version = 1
            [project]
            name = "x"
            target_duration = "10s"
            fps = 30
            [input]
            folder = "."
            [[outputs]]
            path = "out.mp4"
            resolution = "1920x1080"
            [soundtrack]
            file = "a.mp3"
        "#;
        let cfg: Config = toml::from_str(single).unwrap();
        assert_eq!(cfg.soundtrack.as_ref().unwrap().files.len(), 1);

        let many = r#"
            schema_version = 1
            [project]
            name = "x"
            target_duration = "10s"
            fps = 30
            [input]
            folder = "."
            [[outputs]]
            path = "out.mp4"
            resolution = "1920x1080"
            [soundtrack]
            files = ["a.mp3", "b.mp3", "c.mp3"]
        "#;
        let cfg: Config = toml::from_str(many).unwrap();
        assert_eq!(cfg.soundtrack.as_ref().unwrap().files.len(), 3);
    }

    #[test]
    fn music_led_pacing_is_on_by_default_and_its_strength_is_checked() {
        let base = "schema_version = 1\n[project]\nname = \"x\"\ntarget_duration = \"10s\"\nfps = 30\n\
                    [input]\nfolder = \".\"\n[[outputs]]\npath = \"out.mp4\"\nresolution = \"1920x1080\"\n\
                    [soundtrack]\nfile = \"a.mp3\"\n";
        let cfg: Config = toml::from_str(base).unwrap();
        cfg.validate().unwrap();
        let s = cfg.soundtrack.as_ref().unwrap();
        assert!(s.energy_drives_pacing && s.musical_ending && s.hero_on_drop);
        assert!((s.pacing_strength - 0.6).abs() < 1e-6);
        let off: Config = toml::from_str(&format!(
            "{base}energy_drives_pacing = false\nmusical_ending = false\nhero_on_drop = false\npacing_strength = 1.0\n"
        ))
        .unwrap();
        off.validate().unwrap();
        let bad: Config = toml::from_str(&format!("{base}pacing_strength = 1.5\n")).unwrap();
        let e = bad.validate().unwrap_err().to_string();
        assert!(e.contains("pacing_strength"), "{e}");
    }

    /// A minimal config whose outputs are `outputs` (`[[outputs]]` tables), validated.
    fn with_outputs(outputs: &str) -> Result<Config, ConfigError> {
        let cfg: Config = toml::from_str(&format!(
            "schema_version = 1\n\
             [project]\nname = \"x\"\ntarget_duration = \"10s\"\nfps = 30\n\
             [input]\nfolder = \".\"\n\
             {outputs}"
        ))?;
        cfg.validate()?;
        Ok(cfg)
    }

    #[test]
    fn outputs_take_platform_presets() {
        let cfg = with_outputs(
            "[[outputs]]\npath = \"out/a.mp4\"\nresolution = \"1920x1080\"\n\
             [[outputs]]\npath = \"out/a.reels.mp4\"\npreset = \"reels\"\n",
        )
        .unwrap();
        // The vertical preset needs no resolution and never makes the body vertical.
        assert_eq!(cfg.body_resolution(), (1920, 1080));
        let outs = cfg.resolved_outputs();
        assert_eq!((outs[1].width, outs[1].height), (1080, 1920));
        assert_eq!(outs[0].crf, 20);
        // Unset keys stay unset when written back, so the preset keeps deciding them.
        let s = toml::to_string(&cfg).unwrap();
        assert!(!s.contains("crf") && !s.contains("fit"), "{s}");
        // Only a vertical preset: the body is a 16:9 master.
        let cfg = with_outputs("[[outputs]]\npath = \"out/a.mp4\"\npreset = \"reels\"\n").unwrap();
        assert_eq!(cfg.body_resolution(), (1920, 1080));
    }

    #[test]
    fn configs_from_before_platform_presets_still_load() {
        // `preset` used to hold the encoder preset.
        let cfg = with_outputs(
            "[[outputs]]\npath = \"out/a.mp4\"\nresolution = \"1920x1080\"\nencoder = \"nvenc\"\n\
             crf = 22\npreset = \"p5\"\naudio_bitrate = \"160k\"\n",
        )
        .unwrap();
        let o = &cfg.resolved_outputs()[0];
        assert_eq!((o.width, o.height, o.crf), (1920, 1080, 22));
        assert_eq!((o.preset, o.encoder_preset.as_deref()), (None, Some("p5")));
        assert_eq!(o.audio_bitrate, "160k");
    }

    #[test]
    fn output_errors_name_the_output() {
        let e = with_outputs(
            "[[outputs]]\npath = \"a.mp4\"\nresolution = \"1920x1080\"\n\
             [[outputs]]\npath = \"b.mp4\"\nresolution = \"1080\"\n",
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("outputs[1].resolution must look like"), "{e}");
        let e = with_outputs("[[outputs]]\npath = \"a.mp4\"\n").unwrap_err().to_string();
        assert!(e.contains("outputs[0] needs a `resolution`"), "{e}");
        let e = with_outputs("[[outputs]]\npath = \"a.mp4\"\npreset = \"vine\"\n").unwrap_err().to_string();
        assert!(e.contains("outputs[0].preset \"vine\""), "{e}");
        let e = with_outputs("[[outputs]]\npath = \"a.mp4\"\npreset = \"reels\"\nfit = \"zoom\"\n")
            .unwrap_err()
            .to_string();
        assert!(e.contains("zoom"), "{e}");
    }
}
