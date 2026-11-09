//! Audio signal analysis — pure Rust, no aubio C-binding, no FFT.
//!
//! All functions operate on PCM that's decoded by shelling out to ffmpeg
//! (the system binary), keeping the build native on every platform.
//!
//! # Surfaces
//!
//! - **PCM I/O** ([`decode_pcm_mono`], [`decode_pcm_range`]) —
//!   produce mono `Vec<f32>` from any audio/video file ffmpeg can read.
//!
//! - **Soundtrack analysis** ([`analyze_file`], [`analyze_pcm`] →
//!   [`AnalyzedAudio`]) — energy curve, tempo (BPM via autocorrelation with
//!   fundamental-vs-harmonic disambiguation), beat times, loud-region
//!   detection for seamless looping. Used to drive beat-aligned cuts and the
//!   smart-loop soundtrack mux.
//!
//! - **Audio interestingness** ([`audio_interestingness`]) — peak-to-baseline
//!   RMS ratio that surfaces sustained events above a constant background
//!   (laughter, splashes over engine noise). Drives "duck and reveal"
//!   duck-window selection.
//!
//! - **Multi-camera sync** ([`cross_correlate`], [`sync_two_files`] →
//!   [`XCorrResult`]) — Pearson-normalized lag-domain xcorr with parabolic
//!   sub-sample interpolation. Recovers the time offset between two cameras
//!   recording the same scene.
//!
//! # Beat-detection math
//!
//! 1. [`decode_pcm_mono`] runs ffmpeg → 22.05 kHz mono PCM.
//! 2. [`analyze_pcm`] computes per-frame energy (1024-sample frames, 512-sample
//!    hop), then an onset envelope (energy − short moving average), then
//!    autocorrelation in the 60–200 BPM range.
//! 3. Tempo: pick the SMALLEST local maximum within 75 % of the global max —
//!    avoids the classic 1/2-tempo trap when the true period is not an integer
//!    number of onset frames.
//! 4. Beat grid ([`fit_beat_grid`]). The autocorrelation lag is an integer
//!    number of onset frames (~23 ms each), so its tempo can be off by up to
//!    ~4 % — enough for an evenly-spaced grid to drift a whole beat within a
//!    minute. The grid is refined in three passes:
//!    - **fold search**: fold the onset envelope modulo candidate periods
//!      within ±4.5 % of the raw lag (coarse, then fine); the sharpest
//!      folded peak gives the exact period and phase;
//!    - **octave check**: when the in-between beats carry almost no onset
//!      energy the grid is double-time and gets halved; when the half-beat
//!      positions are as strong as the beats it gets doubled;
//!    - **least squares**: snap each grid beat to its nearest real onset
//!      peak (sub-frame, parabolic), drop outliers, and fit `t = t0 + k·T`.
//!    Residual statistics land in [`BeatGridFit`]. Falls back to the plain
//!    fixed-period placement when too few onsets match.
//! 5. A coarse (4 Hz) energy envelope is also produced; consumed by
//!    [`AnalyzedAudio::detect_loud_region`] for smart-loop seam selection.

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use serde::Serialize;
use std::process::Command;

use crate::cancel::CancellableCommand as _;

/// Concatenate multiple audio files into a single AAC-in-M4A track at
/// `output`. Used to fuse a `[soundtrack].files = [...]` list into one
/// virtual track before analysis. ffmpeg's `concat` filter handles
/// heterogeneous codecs/sample-rates by re-encoding through a normalized
/// 48 kHz stereo path.
///
/// Returns `output` so callers can chain. If `inputs.len() == 1`, the file is
/// returned as-is — no copy / no re-encode.
pub fn concat_soundtrack_files(
    inputs: &[Utf8PathBuf],
    output: &Utf8Path,
) -> Result<Utf8PathBuf> {
    anyhow::ensure!(!inputs.is_empty(), "no soundtrack files provided");
    if inputs.len() == 1 {
        return Ok(inputs[0].clone());
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating dir {parent}"))?;
    }
    let mut cmd: Vec<String> = vec![
        "-y".into(), "-hide_banner".into(),
        "-loglevel".into(), "error".into(),
    ];
    for f in inputs {
        cmd.push("-i".into());
        cmd.push(f.to_string());
    }
    // Build concat filter: [0:a][1:a]...concat=n=N:v=0:a=1[a]
    let mut filt = String::new();
    for k in 0..inputs.len() {
        filt.push_str(&format!("[{k}:a]"));
    }
    filt.push_str(&format!("concat=n={}:v=0:a=1[a]", inputs.len()));
    cmd.extend([
        "-filter_complex".into(), filt,
        "-map".into(), "[a]".into(),
        "-c:a".into(), "aac".into(),
        "-b:a".into(), "192k".into(),
        "-ar".into(), "48000".into(),
        "-ac".into(), "2".into(),
        output.to_string(),
    ]);
    let refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
    let status = Command::new("ffmpeg")
        .args(&refs)
        .cancellable_status()
        .with_context(|| "spawning ffmpeg for soundtrack concat")?;
    anyhow::ensure!(
        status.success(),
        "ffmpeg failed concatenating soundtrack files (exit {})",
        status.code().unwrap_or(-1)
    );
    Ok(output.to_path_buf())
}

#[derive(Debug, Clone, Serialize)]
pub struct AnalyzedAudio {
    pub duration_secs: f32,
    pub sample_rate: u32,
    pub tempo_bpm: f32,
    /// How clearly the beat pulses. 0..1+; values ≥ 0.5 mean a strong
    /// repeating pulse (typical pop/rock/EDM); values near 0 mean ambient
    /// or arrhythmic music where rapid beat-aligned cuts won't read well.
    /// Computed as the chosen-lag autocorrelation peak divided by the mean
    /// autocorrelation across the search range.
    pub pulse_strength: f32,
    /// Beat times in seconds since the start of the audio.
    pub beats: Vec<f32>,
    /// Coarse energy envelope, sampled at `energy_rate` Hz.
    pub energy_curve: Vec<f32>,
    pub energy_rate: f32,
    /// How the beat grid was fitted and how well it matches the music.
    /// `None` when no tempo was found or the grid fit fell back to plain
    /// fixed-period placement.
    pub beat_grid: Option<BeatGridFit>,
}

/// Diagnostics for the fitted beat grid (see the module docs, step 4).
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct BeatGridFit {
    /// Tempo the autocorrelation picked, before refinement.
    pub raw_bpm: f32,
    /// Whether the octave check changed the autocorrelation's tempo.
    pub octave: GridOctave,
    /// Exact beat interval of the fitted grid, in seconds.
    pub interval_secs: f64,
    /// Fraction of grid beats (between the first and last matched one) that
    /// have a real onset peak within ±20 % of a beat.
    pub matched_fraction: f32,
    /// Mean |onset − grid| over the matched beats, in milliseconds.
    pub mean_abs_residual_ms: f32,
    /// Worst |onset − grid| over the matched beats, in milliseconds.
    pub max_abs_residual_ms: f32,
}

impl BeatGridFit {
    /// A grid is "tight" when most beats have a real onset and those onsets
    /// sit within half a frame at 30 fps of the grid (machine-timed music).
    /// Loose grids (live drummers, tempo changes) still drive cuts, but the
    /// numbers say how much to trust them.
    pub fn is_tight(&self) -> bool {
        self.matched_fraction >= 0.8 && self.mean_abs_residual_ms <= 15.0
    }
}

/// What the octave check did to the autocorrelation tempo.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GridOctave {
    /// The autocorrelation tempo was kept.
    Kept,
    /// Every other beat had almost no onset energy: the grid was double-time.
    Halved,
    /// The half-beat positions were as strong as the beats: the grid was half-time.
    Doubled,
}

impl AnalyzedAudio {
    pub fn beat_interval_secs(&self) -> f32 {
        if self.tempo_bpm > 0.0 {
            60.0 / self.tempo_bpm
        } else {
            0.5
        }
    }

    /// Find the high-energy region of the track for loop selection. Returns
    /// `(start_secs, end_secs)` clipped to the track's natural duration.
    ///
    /// Algorithm: trim the energy curve from both ends as long as the trimmed
    /// edges have mean energy below `ratio × inner_mean_energy`, where
    /// `inner_mean_energy` is recomputed each iteration. This is more robust
    /// than a fixed threshold against tracks where the loud middle is itself
    /// dynamic — we shave off only the parts that are clearly quieter than
    /// the rest, not just the bottom X% of the energy range.
    ///
    /// `ratio` controls how aggressively we trim:
    ///   - 0.40 (loose): keeps build-ups
    ///   - 0.60 (default): musically dense middle
    ///   - 0.80 (tight): only the loudest sustained section
    pub fn detect_loud_region(&self, ratio: f32) -> (f32, f32) {
        if self.energy_curve.is_empty() {
            return (0.0, self.duration_secs);
        }
        let n = self.energy_curve.len();
        if n < 4 {
            return (0.0, self.duration_secs);
        }
        let dt = 1.0 / self.energy_rate;

        let mut lo = 0usize;
        let mut hi = n - 1;
        // Iteratively shave: at each step compute mean of remaining region;
        // if the leftmost or rightmost bin is below ratio × that mean, drop
        // it and recurse. Stops when both edges are loud enough.
        loop {
            if hi <= lo + 4 {
                break;
            }
            let mean: f32 =
                self.energy_curve[lo..=hi].iter().sum::<f32>() / (hi - lo + 1) as f32;
            let threshold = ratio * mean;
            let drop_lo = self.energy_curve[lo] < threshold;
            let drop_hi = self.energy_curve[hi] < threshold;
            if !drop_lo && !drop_hi {
                break;
            }
            if drop_lo {
                lo += 1;
            }
            if drop_hi {
                hi -= 1;
            }
        }
        let start = lo as f32 * dt;
        let end = ((hi + 1) as f32 * dt).min(self.duration_secs);
        (start, end)
    }

    /// Snap `t` to the nearest beat **≥** t (round up). Used to align loop
    /// start to a downbeat without leaking pre-beat audio into the loop.
    pub fn beat_at_or_after(&self, t: f32) -> f32 {
        match self.beats.iter().find(|&&b| b >= t) {
            Some(&b) => b,
            None => t,
        }
    }

    /// Snap `t` to the nearest beat **≤** t (round down).
    pub fn beat_at_or_before(&self, t: f32) -> f32 {
        match self.beats.iter().rev().find(|&&b| b <= t) {
            Some(&b) => b,
            None => t,
        }
    }
}

const ANALYSIS_SAMPLE_RATE: u32 = 22_050;
const FRAME_SIZE: usize = 1024;
const HOP_SIZE: usize = 512;
const ENERGY_TARGET_RATE_HZ: f32 = 4.0;

/// Decode any audio file ffmpeg can read into mono f32 samples at `sample_rate`.
pub fn decode_pcm_mono(path: &Utf8Path, sample_rate: u32) -> Result<Vec<f32>> {
    decode_pcm_range(path, sample_rate, None, None)
}

/// Like `decode_pcm_mono` but optionally trims to `[start_secs, start_secs + duration_secs)`.
/// `-ss` is placed before `-i` for fast keyframe seek (good enough for RMS analysis).
pub fn decode_pcm_range(
    path: &Utf8Path,
    sample_rate: u32,
    start_secs: Option<f32>,
    duration_secs: Option<f32>,
) -> Result<Vec<f32>> {
    let mut args: Vec<String> = vec!["-v".into(), "error".into()];
    if let Some(ss) = start_secs {
        args.push("-ss".into());
        args.push(format!("{ss:.3}"));
    }
    args.push("-i".into());
    args.push(path.to_string());
    if let Some(t) = duration_secs {
        args.push("-t".into());
        args.push(format!("{t:.3}"));
    }
    args.push("-f".into());
    args.push("s16le".into());
    args.push("-ar".into());
    args.push(sample_rate.to_string());
    args.push("-ac".into());
    args.push("1".into());
    args.push("-".into());

    let output = Command::new("ffmpeg")
        .args(&args)
        .cancellable_output()
        .with_context(|| "spawning ffmpeg for audio decode")?;
    if !output.status.success() {
        anyhow::bail!(
            "ffmpeg decode failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut pcm = Vec::with_capacity(output.stdout.len() / 2);
    for chunk in output.stdout.chunks_exact(2) {
        let s = i16::from_le_bytes([chunk[0], chunk[1]]);
        pcm.push(s as f32 / 32768.0);
    }
    Ok(pcm)
}

/// Score the "interestingness" of a chunk of audio. Higher = more peaks above
/// baseline (laughter, exclamations, splashes, impacts). For sources with a
/// constant background (engine, wind), this surfaces excursions above that
/// background — same idea as the relative-spike accelerometer scoring.
///
/// Definition: `mean(top 10% RMS frames) / mean(bottom 50% RMS frames)`.
/// - Pure silence → returns 0.
/// - Constant background (no events) → ~1.0.
/// - Sustained event over background → ≫1.0.
/// - Single click/spike → modest score (top-10% mean dilutes single peaks).
pub fn audio_interestingness(pcm: &[f32], sample_rate: u32) -> f32 {
    if pcm.len() < (sample_rate / 10) as usize {
        return 0.0;
    }
    // 50ms frames, 25ms hop.
    let frame = (sample_rate / 20).max(64) as usize;
    let hop = (frame / 2).max(32);
    let mut rms: Vec<f32> = Vec::new();
    let mut start = 0usize;
    while start + frame <= pcm.len() {
        let window = &pcm[start..start + frame];
        let mean_sq: f32 =
            window.iter().map(|x| x * x).sum::<f32>() / frame as f32;
        rms.push(mean_sq.sqrt());
        start += hop;
    }
    if rms.is_empty() {
        return 0.0;
    }
    let mut sorted = rms.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    let top_n = (n / 10).max(1);
    let bot_n = (n / 2).max(1);
    let top_mean: f32 =
        sorted[n - top_n..].iter().sum::<f32>() / top_n as f32;
    let bot_mean: f32 = sorted[..bot_n].iter().sum::<f32>() / bot_n as f32;
    if bot_mean < 1e-5 {
        // Effectively silent baseline. If anything happened at all, score it
        // moderately — but we don't want a divide-by-zero blowup.
        return if top_mean > 1e-3 { 5.0 } else { 0.0 };
    }
    top_mean / bot_mean
}

/// Convenience wrapper: decode at our analysis rate and analyze.
pub fn analyze_file(path: &Utf8Path) -> Result<AnalyzedAudio> {
    let pcm = decode_pcm_mono(path, ANALYSIS_SAMPLE_RATE)?;
    if pcm.is_empty() {
        anyhow::bail!("decoded audio is empty");
    }
    Ok(analyze_pcm(&pcm, ANALYSIS_SAMPLE_RATE))
}

/// Analyze a mono f32 PCM buffer at `sample_rate` (any reasonable rate works;
/// the analyzer's parameters are tuned for ~22 kHz).
pub fn analyze_pcm(pcm: &[f32], sample_rate: u32) -> AnalyzedAudio {
    let duration_secs = pcm.len() as f32 / sample_rate as f32;
    if pcm.len() < FRAME_SIZE * 4 {
        return AnalyzedAudio {
            duration_secs,
            sample_rate,
            tempo_bpm: 0.0,
            pulse_strength: 0.0,
            beats: Vec::new(),
            energy_curve: Vec::new(),
            energy_rate: ENERGY_TARGET_RATE_HZ,
            beat_grid: None,
        };
    }

    // (1) Per-frame energy.
    let n_frames = (pcm.len().saturating_sub(FRAME_SIZE)) / HOP_SIZE + 1;
    let mut energy = Vec::with_capacity(n_frames);
    for i in 0..n_frames {
        let start = i * HOP_SIZE;
        let end = (start + FRAME_SIZE).min(pcm.len());
        let frame = &pcm[start..end];
        let e: f32 =
            frame.iter().map(|x| x * x).sum::<f32>() / frame.len().max(1) as f32;
        energy.push(e);
    }

    let onset_rate = sample_rate as f32 / HOP_SIZE as f32; // ~43 Hz at 22 kHz

    // (2) Onset envelope: positive deviation from a short moving average.
    let smooth_n = 8usize;
    let mut onset = vec![0.0_f32; n_frames];
    let mut running = 0.0_f32;
    for i in 0..n_frames {
        running += energy[i];
        if i >= smooth_n {
            running -= energy[i - smooth_n];
        }
        let denom = (i + 1).min(smooth_n) as f32;
        let baseline = running / denom;
        onset[i] = (energy[i] - baseline).max(0.0);
    }

    // Normalize onset to [0, 1] for stable downstream comparisons.
    let max_onset = onset.iter().cloned().fold(0.0_f32, f32::max);
    if max_onset > 0.0 {
        for v in &mut onset {
            *v /= max_onset;
        }
    }

    // (3) Tempo via autocorrelation in a musically plausible range.
    let (raw_bpm, pulse_strength) = estimate_tempo_with_strength(&onset, onset_rate);

    // (4) Beats: fit an exact grid to the onsets; fall back to the raw
    //     fixed-period placement when too few onsets match.
    let energy_prefix = energy_prefix_sums(pcm);
    let (bpm, beats, beat_grid) = match fit_beat_grid(
        &onset,
        onset_rate,
        &energy_prefix,
        sample_rate,
        raw_bpm,
        duration_secs,
    ) {
        Some((grid_beats, fit)) => ((60.0 / fit.interval_secs) as f32, grid_beats, Some(fit)),
        None => (raw_bpm, track_beats(&onset, onset_rate, raw_bpm), None),
    };

    // (5) Coarse energy envelope at ENERGY_TARGET_RATE_HZ.
    let bin = ((onset_rate / ENERGY_TARGET_RATE_HZ).round() as usize).max(1);
    let mut energy_curve: Vec<f32> = energy
        .chunks(bin)
        .map(|c| c.iter().sum::<f32>() / c.len().max(1) as f32)
        .collect();
    let max_e = energy_curve.iter().cloned().fold(0.0_f32, f32::max);
    if max_e > 0.0 {
        for v in &mut energy_curve {
            *v /= max_e;
        }
    }

    AnalyzedAudio {
        duration_secs,
        sample_rate,
        tempo_bpm: bpm,
        pulse_strength,
        beats,
        energy_curve,
        energy_rate: ENERGY_TARGET_RATE_HZ,
        beat_grid,
    }
}

/// Returns `(bpm, pulse_strength)`. `pulse_strength = peak_corr / mean_corr`
/// across the BPM search range — bigger means a clearer repeating pulse, with
/// 1.0 = "no preferred period" and ≥ 3.0 = "very strong beat".
fn estimate_tempo_with_strength(onset: &[f32], onset_rate: f32) -> (f32, f32) {
    let bpm_min = 60.0_f32;
    let bpm_max = 200.0_f32;
    let lag_min = (60.0 * onset_rate / bpm_max).max(2.0) as usize;
    let lag_max = (60.0 * onset_rate / bpm_min) as usize;
    let n = onset.len();
    if lag_max >= n {
        return (0.0, 0.0);
    }

    // Compute autocorrelation across the candidate lag range.
    let mut corrs = Vec::with_capacity(lag_max - lag_min + 1);
    for lag in lag_min..=lag_max {
        let mut c = 0.0_f32;
        let limit = n - lag;
        for i in 0..limit {
            c += onset[i] * onset[i + lag];
        }
        corrs.push((lag, c));
    }
    if corrs.is_empty() {
        return (0.0, 0.0);
    }
    let global_max = corrs.iter().map(|x| x.1).fold(0.0_f32, f32::max);
    if global_max <= 0.0 {
        return (0.0, 0.0);
    }
    let mean_corr: f32 = corrs.iter().map(|x| x.1).sum::<f32>() / corrs.len() as f32;
    let pulse_strength = if mean_corr > 0.0 { global_max / mean_corr } else { 0.0 };

    // Pick the *smallest* local maximum within 75% of the global max. This
    // resolves the classic 1/2-tempo harmonic ambiguity: when 2P aligns at
    // an integer lag and P does not, both peaks are tall — the smaller is
    // the true fundamental.
    let threshold = global_max * 0.75;
    let m = corrs.len();
    for i in 1..m - 1 {
        let (lag, c) = corrs[i];
        if c < threshold {
            continue;
        }
        let prev = corrs[i - 1].1;
        let next = corrs[i + 1].1;
        if c >= prev && c >= next {
            return (60.0 * onset_rate / lag as f32, pulse_strength);
        }
    }

    // Fallback: argmax (rare — only if no local maxima passed threshold).
    let (best_lag, _) = corrs
        .iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap();
    (60.0 * onset_rate / *best_lag as f32, pulse_strength)
}

fn track_beats(onset: &[f32], onset_rate: f32, bpm: f32) -> Vec<f32> {
    if bpm <= 0.0 || onset.is_empty() {
        return Vec::new();
    }
    let period_frames = (60.0 * onset_rate / bpm).round() as usize;
    if period_frames == 0 {
        return Vec::new();
    }

    // Anchor: search the first half-period for the strongest onset above 30%
    // of the global max. Falls back to frame 0 if nothing crosses threshold.
    let max_onset = onset.iter().cloned().fold(0.0_f32, f32::max);
    let threshold = max_onset * 0.3;
    let search_end = period_frames.min(onset.len());
    let mut anchor = 0usize;
    let mut best = 0.0_f32;
    for i in 0..search_end {
        if onset[i] > threshold && onset[i] > best {
            best = onset[i];
            anchor = i;
        }
    }

    let mut beats = Vec::new();
    let mut t = anchor;
    while t < onset.len() {
        beats.push(t as f32 / onset_rate);
        t += period_frames;
    }
    beats
}

// ---------- Beat-grid fit ----------

/// Fold-search span around the raw autocorrelation period. Integer lags in
/// the 60–200 BPM range quantize the period by at most 0.5 / 12.9 ≈ 3.9 %.
const GRID_SEARCH_SPAN: f64 = 0.045;
const GRID_COARSE_STEP: f64 = 0.001;
const GRID_FINE_SPAN: f64 = 0.002;
const GRID_FINE_STEP: f64 = 0.000_1;
/// Snap window around each grid beat, as a fraction of the beat interval.
const GRID_SNAP_WINDOW: f64 = 0.2;
/// Weakest onset peak (envelope is normalized to the track's loudest onset)
/// that counts as a real hit when snapping grid beats.
const GRID_MIN_PEAK: f32 = 0.05;
/// Fewest matched onsets for the least-squares fit to be trusted.
const GRID_MIN_MATCHES: usize = 4;
/// Attack refinement: sliding energy window (samples) and its step. A
/// 256-sample window (~12 ms at 22 kHz) is long enough to smooth over a
/// 70 Hz kick's waveform, short enough to locate the attack to ~1 ms.
const ATTACK_WINDOW: usize = 256;
const ATTACK_STEP: usize = 16;
/// Fewest grid beats the octave check needs before it will judge.
const OCTAVE_MIN_BEATS: usize = 8;
/// Halve when the weaker half of the beats (odd or even) carries less than
/// this fraction of the stronger half's onset energy. Kept low on purpose:
/// with an energy envelope a snare on 2 and 4 is often only 30–60 % of the
/// kick on 1 and 3, and that must NOT read as double-time.
const OCTAVE_HALVE_RATIO: f32 = 0.25;
/// Double when the half-beat positions carry at least this fraction of the
/// beats' onset energy.
const OCTAVE_DOUBLE_RATIO: f32 = 0.85;
const GRID_BPM_MIN: f64 = 60.0;
const GRID_BPM_MAX: f64 = 200.0;
/// Outlier floor for the least-squares fit: residuals under this always
/// count as on-grid.
const GRID_OUTLIER_FLOOR_SECS: f64 = 0.005;

/// Beat grid in onset-frame units: beat `k` sits at `phase + k * period`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Grid {
    period: f64,
    phase: f64,
}

impl Grid {
    fn at(&self, k: i64) -> f64 {
        self.phase + k as f64 * self.period
    }

    /// Inclusive range of beat indices that fall inside `[0, n_frames - 1]`.
    fn index_range(&self, n_frames: usize) -> Option<(i64, i64)> {
        if n_frames == 0 || self.period <= 0.0 {
            return None;
        }
        let k_min = (-self.phase / self.period).ceil() as i64;
        let k_max = (((n_frames - 1) as f64 - self.phase) / self.period).floor() as i64;
        (k_max >= k_min).then_some((k_min, k_max))
    }
}

/// A grid beat matched to a real hit: `(beat index k, attack time in
/// seconds, fit weight)`.
type BeatPoint = (i64, f64, f64);

/// Fit an exact beat grid to the music, starting from the raw
/// autocorrelation tempo. `onset` is the analysis-rate onset envelope;
/// `energy_prefix` is the running sum of squared samples of the PCM at
/// `sample_rate` (see [`energy_prefix_sums`]) used to time each attack.
/// Returns beat times (seconds, spanning the whole track) plus fit
/// diagnostics, or `None` to fall back to [`track_beats`].
fn fit_beat_grid(
    onset: &[f32],
    onset_rate: f32,
    energy_prefix: &[f64],
    sample_rate: u32,
    raw_bpm: f32,
    duration_secs: f32,
) -> Option<(Vec<f32>, BeatGridFit)> {
    if raw_bpm <= 0.0 || onset.len() < 16 {
        return None;
    }
    let rate = onset_rate as f64;
    let env = smooth_binomial5(onset);

    let p0 = 60.0 * rate / raw_bpm as f64;
    let coarse = search_period(&env, p0, GRID_SEARCH_SPAN, GRID_COARSE_STEP)?;
    let folded = search_period(&env, coarse.period, GRID_FINE_SPAN, GRID_FINE_STEP)?;
    let (grid, octave) = octave_check(onset, folded, rate);

    // From here on, everything is in seconds.
    let points = snap_to_attacks(onset, grid, energy_prefix, sample_rate);
    if points.len() < GRID_MIN_MATCHES {
        return None;
    }
    let fold_secs = Grid { period: grid.period / rate, phase: grid.phase / rate };
    let (fitted, kept) = robust_line_fit(&points, fold_secs)?;

    let first_k = kept.iter().map(|p| p.0).min()?;
    let last_k = kept.iter().map(|p| p.0).max()?;
    let span = (last_k - first_k + 1).max(1) as f32;
    let residuals_ms: Vec<f64> = kept
        .iter()
        .map(|&(k, t, _)| (t - fitted.at(k)).abs() * 1000.0)
        .collect();
    let mean_abs = residuals_ms.iter().sum::<f64>() / residuals_ms.len() as f64;
    let max_abs = residuals_ms.iter().cloned().fold(0.0_f64, f64::max);

    // The grid spans the whole track — music keeps its tempo through a soft
    // intro or outro even where no onset is strong enough to match.
    let mut beats = Vec::new();
    let mut k = (-fitted.phase / fitted.period).floor() as i64;
    loop {
        let t = fitted.at(k);
        if t >= duration_secs as f64 {
            break;
        }
        if t >= 0.0 {
            beats.push(t as f32);
        }
        k += 1;
    }
    if beats.is_empty() {
        return None;
    }

    Some((
        beats,
        BeatGridFit {
            raw_bpm,
            octave,
            interval_secs: fitted.period,
            matched_fraction: (kept.len() as f32 / span).min(1.0),
            mean_abs_residual_ms: mean_abs as f32,
            max_abs_residual_ms: max_abs as f32,
        },
    ))
}

/// Running sum of squared samples: `out[i] = Σ pcm[..i]²`, so the energy of
/// `pcm[a..b]` is `out[b] − out[a]`.
fn energy_prefix_sums(pcm: &[f32]) -> Vec<f64> {
    let mut out = Vec::with_capacity(pcm.len() + 1);
    let mut acc = 0.0_f64;
    out.push(0.0);
    for &x in pcm {
        acc += x as f64 * x as f64;
        out.push(acc);
    }
    out
}

/// Time (seconds) of the attack near an onset-envelope peak at analysis
/// frame `frame`. Slides an `ATTACK_WINDOW`-sample energy window from one
/// hop before the frame to the frame's end, finds the energy peak, and
/// returns where the window's centre first crosses halfway between the
/// floor before the peak and the peak itself. For a step attack that is
/// the attack sample, whatever the hit's decay — unlike the onset frame,
/// which reads 0–17 ms early depending on how long the hit rings.
fn refine_attack(energy_prefix: &[f64], sample_rate: u32, frame: usize) -> Option<f64> {
    let n_samples = energy_prefix.len().saturating_sub(1);
    if n_samples < ATTACK_WINDOW {
        return None;
    }
    let first_centre = (frame * HOP_SIZE).saturating_sub(HOP_SIZE).max(ATTACK_WINDOW / 2);
    let last_centre = (frame * HOP_SIZE + FRAME_SIZE).min(n_samples - ATTACK_WINDOW / 2);
    if last_centre <= first_centre {
        return None;
    }
    let energy_at = |centre: usize| {
        let a = centre - ATTACK_WINDOW / 2;
        energy_prefix[a + ATTACK_WINDOW] - energy_prefix[a]
    };
    let centres: Vec<usize> = (first_centre..=last_centre).step_by(ATTACK_STEP).collect();
    let energies: Vec<f64> = centres.iter().map(|&c| energy_at(c)).collect();
    let mut peak = 0usize;
    for (j, &e) in energies.iter().enumerate() {
        if e > energies[peak] {
            peak = j;
        }
    }
    let floor = energies[..=peak].iter().cloned().fold(f64::INFINITY, f64::min);
    if energies[peak] <= floor {
        return None;
    }
    let half = floor + 0.5 * (energies[peak] - floor);
    let j = energies[..=peak].iter().position(|&e| e >= half)?;
    let centre = if j == 0 {
        centres[0] as f64
    } else {
        let (e0, e1) = (energies[j - 1], energies[j]);
        let frac = if e1 > e0 { (half - e0) / (e1 - e0) } else { 0.0 };
        centres[j - 1] as f64 + frac * ATTACK_STEP as f64
    };
    Some(centre / sample_rate as f64)
}

/// 5-tap binomial smoothing ([1 4 6 4 1] / 16, σ ≈ 1 frame). Makes the fold
/// search tolerant of the ±1-frame jitter every real onset has.
fn smooth_binomial5(x: &[f32]) -> Vec<f32> {
    const K: [f32; 5] = [1.0, 4.0, 6.0, 4.0, 1.0];
    let n = x.len() as i64;
    (0..n)
        .map(|i| {
            let mut acc = 0.0_f32;
            let mut wsum = 0.0_f32;
            for (j, w) in K.iter().enumerate() {
                let idx = i + j as i64 - 2;
                if (0..n).contains(&idx) {
                    acc += w * x[idx as usize];
                    wsum += w;
                }
            }
            acc / wsum
        })
        .collect()
}

/// Sweep periods `center × (1 ± span)` in relative `step`s; return the grid
/// whose folded onset peak is highest. Ties keep the shorter period, so the
/// result is deterministic.
fn search_period(env: &[f32], center: f64, span: f64, step: f64) -> Option<Grid> {
    let n_steps = (span / step).round() as i64;
    let mut best: Option<(f64, Grid)> = None;
    for s in -n_steps..=n_steps {
        let period = center * (1.0 + s as f64 * step);
        if period < 2.0 {
            continue;
        }
        let (phase, score) = fold_peak(env, period);
        if best.map_or(true, |(b, _)| score > b) {
            best = Some((score, Grid { period, phase }));
        }
    }
    best.map(|(_, g)| g)
}

/// Fold the envelope modulo `period` into quarter-frame bins and return the
/// phase (frames, in `[0, period)`) and height of the tallest folded peak.
/// At the true period every beat's onset lands in the same bin; a slightly
/// wrong period smears them across the circle and the peak drops.
fn fold_peak(env: &[f32], period: f64) -> (f64, f64) {
    let bins = ((period * 4.0).round() as usize).max(8);
    let scale = bins as f64 / period;
    let mut hist = vec![0.0_f64; bins];
    for (i, &v) in env.iter().enumerate() {
        if v <= 0.0 {
            continue;
        }
        let pos = (i as f64 % period) * scale;
        let lo = pos.floor();
        let frac = pos - lo;
        let lo = lo as usize % bins;
        hist[lo] += v as f64 * (1.0 - frac);
        hist[(lo + 1) % bins] += v as f64 * frac;
    }
    let smoothed: Vec<f64> = (0..bins)
        .map(|j| {
            0.25 * hist[(j + bins - 1) % bins] + 0.5 * hist[j] + 0.25 * hist[(j + 1) % bins]
        })
        .collect();
    let mut j = 0usize;
    for (idx, &v) in smoothed.iter().enumerate() {
        if v > smoothed[j] {
            j = idx;
        }
    }
    let delta = parabolic_vertex(
        smoothed[(j + bins - 1) % bins],
        smoothed[j],
        smoothed[(j + 1) % bins],
    );
    let phase = ((j as f64 + delta) / scale).rem_euclid(period);
    (phase, smoothed[j])
}

/// Sub-sample offset (in `[-0.5, 0.5]`) of the vertex of the parabola
/// through three equally spaced samples centred on a local maximum.
fn parabolic_vertex(y0: f64, y1: f64, y2: f64) -> f64 {
    let denom = y0 - 2.0 * y1 + y2;
    if denom.abs() < 1e-12 {
        0.0
    } else {
        (0.5 * (y0 - y2) / denom).clamp(-0.5, 0.5)
    }
}

/// Strongest raw onset within ±1.5 frames of `center`.
fn onset_strength_near(onset: &[f32], center: f64) -> f32 {
    let n = onset.len();
    if n == 0 || center < -1.5 || center > n as f64 + 0.5 {
        return 0.0;
    }
    let lo = (center - 1.5).ceil().max(0.0) as usize;
    let hi = ((center + 1.5).floor().max(0.0) as usize).min(n - 1);
    onset[lo.min(hi)..=hi].iter().cloned().fold(0.0_f32, f32::max)
}

/// Resolve double-time / half-time grids (see `OCTAVE_*` constants).
fn octave_check(onset: &[f32], grid: Grid, rate: f64) -> (Grid, GridOctave) {
    let Some((k_min, k_max)) = grid.index_range(onset.len()) else {
        return (grid, GridOctave::Kept);
    };
    if ((k_max - k_min + 1) as usize) < OCTAVE_MIN_BEATS {
        return (grid, GridOctave::Kept);
    }
    let last = (onset.len() - 1) as f64;
    let mut on_sum = [0.0_f64; 2];
    let mut on_n = [0usize; 2];
    let mut mid_sum = 0.0_f64;
    let mut mid_n = 0usize;
    for k in k_min..=k_max {
        let c = grid.at(k);
        let parity = k.rem_euclid(2) as usize;
        on_sum[parity] += onset_strength_near(onset, c) as f64;
        on_n[parity] += 1;
        let m = c + 0.5 * grid.period;
        if m <= last {
            mid_sum += onset_strength_near(onset, m) as f64;
            mid_n += 1;
        }
    }
    let mean = |s: f64, n: usize| if n > 0 { s / n as f64 } else { 0.0 };
    let even = mean(on_sum[0], on_n[0]);
    let odd = mean(on_sum[1], on_n[1]);
    let all = mean(on_sum[0] + on_sum[1], on_n[0] + on_n[1]);
    let mid = mean(mid_sum, mid_n);
    let bpm = 60.0 * rate / grid.period;

    let (strong, weak, strong_parity) = if even >= odd { (even, odd, 0) } else { (odd, even, 1) };
    if strong > 0.0 && weak < OCTAVE_HALVE_RATIO as f64 * strong && bpm / 2.0 >= GRID_BPM_MIN {
        let period = grid.period * 2.0;
        let phase = (grid.phase + strong_parity as f64 * grid.period).rem_euclid(period);
        return (Grid { period, phase }, GridOctave::Halved);
    }
    if all > 0.0 && mid >= OCTAVE_DOUBLE_RATIO as f64 * all && bpm * 2.0 <= GRID_BPM_MAX {
        let period = grid.period / 2.0;
        return (
            Grid { period, phase: grid.phase.rem_euclid(period) },
            GridOctave::Doubled,
        );
    }
    (grid, GridOctave::Kept)
}

/// Snap every grid beat (onset-frame units) to the strongest onset peak
/// within ±`GRID_SNAP_WINDOW` beats, then time that hit's attack in the
/// raw audio. Returns `(k, attack_secs, weight)`.
fn snap_to_attacks(
    onset: &[f32],
    grid: Grid,
    energy_prefix: &[f64],
    sample_rate: u32,
) -> Vec<BeatPoint> {
    let Some((k_min, k_max)) = grid.index_range(onset.len()) else {
        return Vec::new();
    };
    let n = onset.len();
    let half = GRID_SNAP_WINDOW * grid.period;
    let mut points = Vec::new();
    for k in k_min..=k_max {
        let c = grid.at(k);
        let lo = (c - half).ceil().max(0.0) as usize;
        let hi = ((c + half).floor().max(0.0) as usize).min(n - 1);
        if hi < lo {
            continue;
        }
        let mut best_i = lo;
        for i in lo..=hi {
            if onset[i] > onset[best_i] {
                best_i = i;
            }
        }
        let v = onset[best_i];
        if v < GRID_MIN_PEAK {
            continue;
        }
        // A maximum on the window edge that keeps rising outside it is the
        // flank of a neighbouring onset, not a hit on this beat.
        let rising_left = best_i == lo && lo > 0 && onset[lo - 1] > v;
        let rising_right = best_i == hi && hi + 1 < n && onset[hi + 1] > v;
        if rising_left || rising_right {
            continue;
        }
        if let Some(t) = refine_attack(energy_prefix, sample_rate, best_i) {
            points.push((k, t, (v as f64).sqrt()));
        }
    }
    points
}

/// Weighted least-squares line `tau = a + b·k`. Returns `(a, b)`.
fn weighted_line_fit(points: &[BeatPoint]) -> Option<(f64, f64)> {
    let (mut s, mut sk, mut skk, mut st, mut skt) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for &(k, tau, w) in points {
        let k = k as f64;
        s += w;
        sk += w * k;
        skk += w * k * k;
        st += w * tau;
        skt += w * k * tau;
    }
    let den = s * skk - sk * sk;
    if s <= 0.0 || den.abs() < 1e-9 {
        return None;
    }
    let b = (s * skt - sk * st) / den;
    let a = (st - b * sk) / s;
    Some((a, b))
}

/// Fit, drop outliers (> 3 robust σ, never under
/// `GRID_OUTLIER_FLOOR_SECS`), refit. Points and `grid` are in seconds.
/// Returns the fitted grid and the points it kept. If the refit period
/// strays more than 5 % from the fold-search period something is badly off
/// (sparse, syncopated hits); keep the fold grid in that case.
fn robust_line_fit(
    points: &[BeatPoint],
    grid: Grid,
) -> Option<(Grid, Vec<BeatPoint>)> {
    let (a, b) = weighted_line_fit(points)?;
    let mut abs_res: Vec<f64> = points.iter().map(|&(k, t, _)| (t - (a + b * k as f64)).abs()).collect();
    let mut sorted = abs_res.clone();
    sorted.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2];
    let threshold = (3.0 * 1.4826 * median).max(GRID_OUTLIER_FLOOR_SECS);
    let kept: Vec<BeatPoint> = points
        .iter()
        .zip(abs_res.drain(..))
        .filter(|(_, r)| *r <= threshold)
        .map(|(p, _)| *p)
        .collect();
    if kept.len() < GRID_MIN_MATCHES {
        return None;
    }
    let (a, b) = weighted_line_fit(&kept)?;
    if (b / grid.period - 1.0).abs() > 0.05 {
        return Some((grid, kept));
    }
    Some((Grid { period: b, phase: a }, kept))
}

// ---------- Snap helpers (used by the planner) ----------

/// Decode a window of audio from two video files and cross-correlate to find
/// the time offset between them. Useful for syncing multiple cameras of the
/// same scene by their captured ambient audio. `window_secs` controls how
/// much audio is used from each file (longer = more confidence, slower).
pub fn sync_two_files(
    path_a: &Utf8Path,
    path_b: &Utf8Path,
    start_a_secs: f32,
    start_b_secs: f32,
    window_secs: f32,
    max_lag_secs: f32,
) -> anyhow::Result<XCorrResult> {
    let sr = 8_000u32; // 8 kHz is plenty for ambient-audio sync.
    let a = decode_pcm_range(path_a, sr, Some(start_a_secs), Some(window_secs))?;
    let b = decode_pcm_range(path_b, sr, Some(start_b_secs), Some(window_secs))?;
    Ok(cross_correlate(&a, &b, sr, max_lag_secs))
}

// ---------- Multi-angle audio sync ----------

/// Result of cross-correlating two audio signals to recover their relative
/// time offset (e.g. for syncing multiple cameras of the same scene).
#[derive(Debug, Clone)]
pub struct XCorrResult {
    /// Offset in seconds to align signal `b` to signal `a`. Positive = `b`
    /// starts later (must be shifted earlier by this amount), negative = `b`
    /// starts earlier than `a`. Sub-sample precision via parabolic interpolation.
    pub offset_secs: f32,
    /// Peak normalized correlation in [0, 1]. <0.3 ≈ no clear match (cameras
    /// in different acoustic environments or non-overlapping windows).
    pub confidence: f32,
}

/// Cross-correlate two equal-rate mono signals to find the offset that best
/// aligns `b` to `a`. Searches up to `±max_lag_secs` around zero.
///
/// Implementation: brute-force O(N·M) lag-domain search at the analysis sample
/// rate. For the typical use case — a few seconds of overlapping audio at
/// 8 kHz — N·M is about 10⁸ multiplies, runs in well under a second on any
/// modern CPU. An FFT-based O((N+M)·log(N+M)) version would be faster for
/// long signals; deferred until a profiler says it matters.
///
/// Both signals should be roughly DC-removed and amplitude-normalized for the
/// correlation to be comparable across content; this function does that
/// internally to keep callers simple.
pub fn cross_correlate(
    a: &[f32],
    b: &[f32],
    sample_rate: u32,
    max_lag_secs: f32,
) -> XCorrResult {
    if a.is_empty() || b.is_empty() {
        return XCorrResult {
            offset_secs: 0.0,
            confidence: 0.0,
        };
    }
    let max_lag = ((max_lag_secs * sample_rate as f32).round() as i32)
        .max(1)
        .min(a.len() as i32 + b.len() as i32);

    // Zero-mean (don't unit-normalize globally — we Pearson-normalize each
    // overlap window so the result is in [-1, 1] regardless of signal length).
    let an = zero_mean(a);
    let bn = zero_mean(b);

    let na = an.len() as i32;
    let nb = bn.len() as i32;

    // Heuristic: require overlaps of at least min(N/4, 0.5s) so very short
    // edges don't get arbitrarily high Pearson scores from one sample's noise.
    let min_overlap = ((sample_rate as i32 / 2).min(na / 4)).max(64);

    let mut best_lag = 0i32;
    let mut best_corr = -f32::INFINITY;
    for lag in -max_lag..=max_lag {
        let lo = lag.max(0);
        let hi = (lag + nb).min(na);
        if hi - lo < min_overlap {
            continue;
        }
        let count = (hi - lo) as usize;
        let mut sum = 0.0_f64;
        let mut sa = 0.0_f64;
        let mut sb = 0.0_f64;
        let mut a_idx = lo as usize;
        let mut b_idx = (lo - lag) as usize;
        for _ in 0..count {
            let av = an[a_idx] as f64;
            let bv = bn[b_idx] as f64;
            sum += av * bv;
            sa += av * av;
            sb += bv * bv;
            a_idx += 1;
            b_idx += 1;
        }
        let denom = (sa * sb).sqrt();
        if denom < 1e-12 {
            continue;
        }
        let c = (sum / denom) as f32;
        if c > best_corr {
            best_corr = c;
            best_lag = lag;
        }
    }

    // Parabolic interpolation around the integer-lag peak for sub-sample precision.
    let frac = parabolic_peak_offset(&an, &bn, best_lag, na, nb);
    // Convention: positive `offset_secs` = `b` started LATER than `a` in
    // source-content time (equivalently: b's index 0 corresponds to a's
    // content `offset_secs` later). The xcorr peak lag has the same sign
    // under this convention — no flip.
    let offset = (best_lag as f32 + frac) / sample_rate as f32;
    XCorrResult {
        offset_secs: offset,
        confidence: best_corr.clamp(0.0, 1.0),
    }
}

fn zero_mean(x: &[f32]) -> Vec<f32> {
    let n = x.len() as f32;
    if n <= 1.0 {
        return x.to_vec();
    }
    let mean: f32 = x.iter().sum::<f32>() / n;
    x.iter().map(|v| v - mean).collect()
}

fn corr_at_lag(a: &[f32], b: &[f32], lag: i32, na: i32, nb: i32) -> f32 {
    let lo = lag.max(0);
    let hi = (lag + nb).min(na);
    if hi <= lo {
        return 0.0;
    }
    let mut sum = 0.0_f64;
    let mut sa = 0.0_f64;
    let mut sb = 0.0_f64;
    let mut a_idx = lo as usize;
    let mut b_idx = (lo - lag) as usize;
    let count = (hi - lo) as usize;
    for _ in 0..count {
        let av = a[a_idx] as f64;
        let bv = b[b_idx] as f64;
        sum += av * bv;
        sa += av * av;
        sb += bv * bv;
        a_idx += 1;
        b_idx += 1;
    }
    let denom = (sa * sb).sqrt();
    if denom < 1e-12 {
        0.0
    } else {
        (sum / denom) as f32
    }
}

/// Parabolic interpolation around an integer correlation peak, returning a
/// sub-sample offset in `[-0.5, 0.5]`. Uses three samples (lag-1, lag, lag+1).
fn parabolic_peak_offset(a: &[f32], b: &[f32], lag: i32, na: i32, nb: i32) -> f32 {
    let c0 = corr_at_lag(a, b, lag - 1, na, nb);
    let c1 = corr_at_lag(a, b, lag, na, nb);
    let c2 = corr_at_lag(a, b, lag + 1, na, nb);
    let denom = c0 - 2.0 * c1 + c2;
    if denom.abs() < 1e-9 {
        return 0.0;
    }
    (0.5 * (c0 - c2) / denom).clamp(-1.0, 1.0)
}

/// Round `target_clip_len` to the nearest multiple of `beat_interval`, but
/// clamp to `[min, max]` (also rounded to beat multiples). At least one beat.
pub fn snap_clip_length_to_beats(
    target_clip_len: f32,
    min_clip: f32,
    max_clip: f32,
    beat_interval: f32,
) -> f32 {
    if beat_interval <= 0.0 {
        return target_clip_len;
    }
    let n = (target_clip_len / beat_interval).round().max(1.0);
    let min_n = (min_clip / beat_interval).ceil().max(1.0);
    let max_n = (max_clip / beat_interval).floor().max(min_n);
    n.clamp(min_n, max_n) * beat_interval
}

/// Given a beat-aligned `clip_len`, choose the number of clips so the total
/// is the closest beat-multiple to `target_duration`. Returns `(k, total)`.
pub fn choose_clip_count(target_duration: f32, clip_len: f32) -> (usize, f32) {
    if clip_len <= 0.0 {
        return (0, 0.0);
    }
    let k = (target_duration / clip_len).round().max(1.0) as usize;
    (k, k as f32 * clip_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthesize a kick-drum-like signal at a given BPM and analyze it.
    fn synth_kick(bpm: f32, secs: f32, sample_rate: u32) -> Vec<f32> {
        let n = (secs * sample_rate as f32) as usize;
        let mut out = Vec::with_capacity(n);
        let period = 60.0 / bpm;
        for i in 0..n {
            let t = i as f32 / sample_rate as f32;
            let phase = (t / period).fract();
            // Damped 70 Hz sine inside each beat period.
            let env = (-8.0 * phase * period).exp();
            let s = 0.6 * (2.0 * std::f32::consts::PI * 70.0 * t).sin() * env;
            out.push(s);
        }
        out
    }

    #[test]
    fn audio_interestingness_distinguishes_event_from_background() {
        let sr = 8000u32;
        // 2 seconds of constant low-amplitude noise.
        let mut bg = Vec::with_capacity(2 * sr as usize);
        for i in 0..2 * sr as usize {
            bg.push(0.1 * ((i as f32 * 0.7).sin()));
        }
        // Same duration, but with a half-second loud event in the middle.
        let mut evt = bg.clone();
        let evt_start = (0.75 * sr as f32) as usize;
        let evt_end = (1.25 * sr as f32) as usize;
        for i in evt_start..evt_end {
            evt[i] += 0.7 * (i as f32 * 0.5).sin();
        }
        let s_bg = audio_interestingness(&bg, sr);
        let s_evt = audio_interestingness(&evt, sr);
        assert!(
            s_evt > s_bg * 1.5,
            "event score {s_evt} should be >> background {s_bg}"
        );
    }

    #[test]
    fn estimates_120_bpm() {
        let pcm = synth_kick(120.0, 8.0, 22_050);
        let a = analyze_pcm(&pcm, 22_050);
        assert!(
            (a.tempo_bpm - 120.0).abs() < 5.0,
            "tempo was {} BPM",
            a.tempo_bpm
        );
        assert!(!a.beats.is_empty());
    }

    #[test]
    fn estimates_90_bpm() {
        let pcm = synth_kick(90.0, 12.0, 22_050);
        let a = analyze_pcm(&pcm, 22_050);
        assert!(
            (a.tempo_bpm - 90.0).abs() < 5.0,
            "tempo was {} BPM",
            a.tempo_bpm
        );
    }

    #[test]
    fn beat_count_matches_period() {
        let pcm = synth_kick(120.0, 10.0, 22_050);
        let a = analyze_pcm(&pcm, 22_050);
        // 10 seconds at 120 BPM ≈ 20 beats (give or take edge effects).
        assert!(a.beats.len() >= 18 && a.beats.len() <= 22, "got {} beats", a.beats.len());
    }

    /// Kicks with a sharp attack and an exponential decay (`decay_secs`
    /// time constant), the first one at `first_secs`, then every beat.
    fn synth_kicks(bpm: f64, secs: f64, sample_rate: u32, decay_secs: f64, first_secs: f64) -> Vec<f32> {
        let n = (secs * sample_rate as f64) as usize;
        let period = 60.0 / bpm;
        (0..n)
            .map(|i| {
                let t = i as f64 / sample_rate as f64;
                if t < first_secs {
                    return 0.0;
                }
                let since = (t - first_secs) % period;
                let env = (-since / decay_secs).exp();
                (0.6 * (2.0 * std::f64::consts::PI * 70.0 * since).sin() * env) as f32
            })
            .collect()
    }

    /// Signed error (s) from each detected beat to the nearest true kick.
    fn beat_errors(beats: &[f32], bpm: f64, first_secs: f64) -> Vec<f64> {
        let period = 60.0 / bpm;
        beats
            .iter()
            .filter(|&&b| b as f64 >= first_secs - period * 0.5)
            .map(|&b| {
                let k = ((b as f64 - first_secs) / period).round();
                b as f64 - (first_secs + k * period)
            })
            .collect()
    }

    #[test]
    fn grid_tracks_kicks_over_two_minutes() {
        // 123 BPM is a non-integer lag at the ~43 Hz onset rate (21.01
        // frames): the raw fixed-period placement would drift ~1 beat
        // within the first minute.
        let bpm = 123.0;
        let pcm = synth_kicks(bpm, 120.0, 22_050, 0.06, 0.0);
        let a = analyze_pcm(&pcm, 22_050);
        assert!((a.tempo_bpm as f64 - bpm).abs() < 0.05, "tempo {} BPM", a.tempo_bpm);
        let errs = beat_errors(&a.beats, bpm, 0.0);
        let mean = errs.iter().sum::<f64>() / errs.len() as f64;
        let worst = errs.iter().map(|e| e.abs()).fold(0.0, f64::max);
        assert!(worst < 0.005, "worst beat error {:.1} ms", worst * 1000.0);
        assert!(mean.abs() < 0.003, "mean beat bias {:.1} ms", mean * 1000.0);
        let expected = (120.0 / (60.0 / bpm)).floor() as usize;
        assert!(
            a.beats.len().abs_diff(expected) <= 1,
            "{} beats, expected ≈ {expected}",
            a.beats.len()
        );
        let fit = a.beat_grid.expect("grid fit");
        assert_eq!(fit.octave, GridOctave::Kept);
        assert!(fit.is_tight(), "{fit:?}");
    }

    #[test]
    fn grid_attack_timing_does_not_depend_on_decay() {
        // The onset envelope alone reads 0–17 ms early depending on how long
        // a hit rings; attack refinement must hold every shape to ±3 ms.
        for &decay in &[0.02, 0.06, 0.2] {
            for &bpm in &[84.0, 171.0] {
                let pcm = synth_kicks(bpm, 40.0, 22_050, decay, 0.37);
                let a = analyze_pcm(&pcm, 22_050);
                let errs = beat_errors(&a.beats, bpm, 0.37);
                let mean = errs.iter().sum::<f64>() / errs.len() as f64;
                assert!(
                    mean.abs() < 0.003,
                    "decay {decay}s @ {bpm} BPM: bias {:.2} ms",
                    mean * 1000.0
                );
            }
        }
    }

    /// Deterministic white noise in [-1, 1].
    fn noise(seed: &mut u64) -> f64 {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((*seed >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }

    /// A small dance mix: kick on every beat, snare on 2 and 4, closed
    /// hi-hats on the off-beats, a sustained bass note and a noise floor.
    fn synth_mix(bpm: f64, secs: f64, sample_rate: u32) -> Vec<f32> {
        let n = (secs * sample_rate as f64) as usize;
        let beat = 60.0 / bpm;
        let mut seed = 0x5eed_u64;
        (0..n)
            .map(|i| {
                let t = i as f64 / sample_rate as f64;
                let k = (t / beat).floor();
                let since = t - k * beat;
                let tau = 2.0 * std::f64::consts::PI;
                let kick = 0.55 * (tau * 60.0 * since).sin() * (-since / 0.05).exp();
                let snare = if (k as i64).rem_euclid(2) == 1 {
                    0.25 * noise(&mut seed) * (-since / 0.08).exp()
                } else {
                    0.0
                };
                let off = (t - (k + 0.5) * beat).rem_euclid(beat);
                let hat = 0.06 * noise(&mut seed) * (-off / 0.015).exp();
                let bass = 0.08 * (tau * 55.0 * t).sin();
                let floor = 0.01 * noise(&mut seed);
                (kick + snare + hat + bass + floor) as f32
            })
            .collect()
    }

    #[test]
    fn grid_reads_a_slow_track_with_soft_eighths_at_its_real_tempo() {
        // 70 BPM kicks with weaker eighth-note ticks between them: neither
        // the tempo picker nor the octave check may promote the eighth-note
        // pulse to the beat (140 BPM), and the grid must still sit on the
        // kicks, not on the ticks.
        let bpm = 70.0;
        let sr = 22_050u32;
        let beat = 60.0 / bpm;
        let mut pcm = synth_kicks(bpm, 60.0, sr, 0.06, 0.0);
        for (i, s) in pcm.iter_mut().enumerate() {
            let t = i as f64 / sr as f64;
            let off = (t - 0.5 * beat).rem_euclid(beat);
            *s += (0.22 * (2.0 * std::f64::consts::PI * 70.0 * off).sin() * (-off / 0.03).exp()) as f32;
        }
        let a = analyze_pcm(&pcm, sr);
        let fit = a.beat_grid.expect("grid fit");
        assert!((a.tempo_bpm as f64 - bpm).abs() < 0.05, "tempo {} BPM ({fit:?})", a.tempo_bpm);
        let worst = beat_errors(&a.beats, bpm, 0.0)
            .iter()
            .map(|e| e.abs())
            .fold(0.0, f64::max);
        assert!(worst < 0.005, "worst beat error {:.1} ms", worst * 1000.0);
    }

    #[test]
    fn grid_locks_onto_kicks_in_a_full_mix() {
        let bpm = 128.0;
        let a = analyze_pcm(&synth_mix(bpm, 60.0, 22_050), 22_050);
        assert!((a.tempo_bpm as f64 - bpm).abs() < 0.05, "tempo {} BPM", a.tempo_bpm);
        let worst = beat_errors(&a.beats, bpm, 0.0)
            .iter()
            .map(|e| e.abs())
            .fold(0.0, f64::max);
        assert!(worst < 0.010, "worst beat error {:.1} ms", worst * 1000.0);
        let fit = a.beat_grid.expect("grid fit");
        assert_eq!(fit.octave, GridOctave::Kept);
        assert!(fit.is_tight(), "{fit:?}");
    }

    #[test]
    fn grid_tracks_kicks_after_silent_intro() {
        let bpm = 97.0;
        let first = 5.0;
        let pcm = synth_kicks(bpm, 60.0, 22_050, 0.08, first);
        let a = analyze_pcm(&pcm, 22_050);
        assert!((a.tempo_bpm as f64 - bpm).abs() < 0.05, "tempo {} BPM", a.tempo_bpm);
        let worst = beat_errors(&a.beats, bpm, first)
            .iter()
            .map(|e| e.abs())
            .fold(0.0, f64::max);
        assert!(worst < 0.005, "worst beat error {:.1} ms", worst * 1000.0);
        // The grid is extended back through the silent intro.
        assert!(a.beats[0] < 60.0 / bpm as f32, "first beat at {}", a.beats[0]);
    }

    fn spikes(n: usize, every: usize, offset: usize, value: f32, into: &mut [f32]) {
        let mut i = offset;
        while i < n {
            into[i] = into[i].max(value);
            i += every;
        }
    }

    #[test]
    fn octave_check_halves_double_time_grid() {
        let rate = 22_050.0 / HOP_SIZE as f64; // ≈ 43 Hz
        let n = 2000;
        // Strong hits every 20 frames on odd grid beats, near-silence between.
        let mut onset = vec![0.0_f32; n];
        spikes(n, 20, 10, 1.0, &mut onset);
        spikes(n, 20, 0, 0.1, &mut onset);
        let (g, oct) = octave_check(&onset, Grid { period: 10.0, phase: 0.0 }, rate);
        assert_eq!(oct, GridOctave::Halved);
        assert!((g.period - 20.0).abs() < 1e-9);
        assert!((g.phase - 10.0).abs() < 1e-9, "phase {}", g.phase);
    }

    #[test]
    fn octave_check_doubles_half_time_grid() {
        let rate = 22_050.0 / HOP_SIZE as f64;
        let n = 4000;
        let mut onset = vec![0.0_f32; n];
        spikes(n, 20, 0, 1.0, &mut onset);
        // A 40-frame grid (≈ 65 BPM) sits on every other hit.
        let (g, oct) = octave_check(&onset, Grid { period: 40.0, phase: 0.0 }, rate);
        assert_eq!(oct, GridOctave::Doubled);
        assert!((g.period - 20.0).abs() < 1e-9);
    }

    #[test]
    fn octave_check_keeps_rock_backbeat() {
        // Kick on 1 and 3, a snare at 40 % of the kick's onset energy on 2
        // and 4, nothing between: a normal backbeat, not double-time.
        let rate = 22_050.0 / HOP_SIZE as f64;
        let n = 2000;
        let mut onset = vec![0.0_f32; n];
        spikes(n, 40, 0, 1.0, &mut onset);
        spikes(n, 40, 20, 0.4, &mut onset);
        let grid = Grid { period: 20.0, phase: 0.0 };
        let (g, oct) = octave_check(&onset, grid, rate);
        assert_eq!(oct, GridOctave::Kept);
        assert_eq!(g, grid);
    }

    #[test]
    fn xcorr_recovers_synthetic_offset() {
        // Multi-camera analogy: A and B record the same scene; B started
        // 0.4 s LATER than A. So at index 0, B's data corresponds to A's
        // data 0.4 s in. To recover that we slice `a` at lag_samples.
        let sr = 8000u32;
        let n = 4 * sr as usize;
        let mut a = Vec::with_capacity(n);
        for i in 0..n {
            let t = i as f32 / sr as f32;
            a.push(0.6 * (2.0 * std::f32::consts::PI * 220.0 * t).sin()
                + 0.4 * (2.0 * std::f32::consts::PI * 880.0 * t * t.sin()).sin());
        }
        let lag_samples = (0.4 * sr as f32) as usize;
        let b: Vec<f32> = a[lag_samples..].to_vec();

        let r = cross_correlate(&a, &b, sr, 1.0);
        assert!(
            (r.offset_secs - 0.4).abs() < 0.01,
            "got offset {} (expected +0.4)",
            r.offset_secs
        );
        assert!(r.confidence > 0.9, "low confidence: {}", r.confidence);
    }

    #[test]
    fn xcorr_low_confidence_for_dissimilar_signals() {
        let sr = 8000u32;
        let n = sr as usize; // 1 second
        let mut a = Vec::with_capacity(n);
        let mut b = Vec::with_capacity(n);
        for i in 0..n {
            let t = i as f32 / sr as f32;
            a.push((2.0 * std::f32::consts::PI * 220.0 * t).sin());
            // Pure noise-like for b.
            b.push(((i as f32 * 7919.0).sin() * (i as f32 * 31.0).cos()).clamp(-1.0, 1.0));
        }
        let r = cross_correlate(&a, &b, sr, 0.5);
        assert!(r.confidence < 0.3, "confidence {} unexpectedly high", r.confidence);
    }

    #[test]
    fn snap_clip_length_clamps_and_quantizes() {
        // beat = 0.5s, target 1.4s → 3 beats = 1.5s
        assert!((snap_clip_length_to_beats(1.4, 0.5, 4.0, 0.5) - 1.5).abs() < 1e-6);
        // target 0.2s but min is 1.0 → 2 beats = 1.0s
        assert!((snap_clip_length_to_beats(0.2, 1.0, 4.0, 0.5) - 1.0).abs() < 1e-6);
        // target 5.0 but max is 3.0 → 6 beats = 3.0s
        assert!((snap_clip_length_to_beats(5.0, 0.5, 3.0, 0.5) - 3.0).abs() < 1e-6);
    }

    #[test]
    fn choose_clip_count_rounds_target() {
        assert_eq!(choose_clip_count(15.0, 1.5), (10, 15.0));
        assert_eq!(choose_clip_count(15.0, 2.0), (8, 16.0)); // 7.5 rounds to 8
        assert_eq!(choose_clip_count(0.0, 1.0), (1, 1.0)); // floor of 1
    }
}
