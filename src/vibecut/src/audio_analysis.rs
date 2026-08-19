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
//! 4. Beats placed at the fixed inferred period from the first strong onset.
//! 5. A coarse (4 Hz) energy envelope is also produced; consumed by
//!    [`AnalyzedAudio::detect_loud_region`] for smart-loop seam selection.

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use serde::Serialize;
use std::process::Command;

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
        .status()
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
        .output()
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
    let (bpm, pulse_strength) = estimate_tempo_with_strength(&onset, onset_rate);

    // (4) Beats: anchor at first strong onset, then place at fixed period.
    let beats = track_beats(&onset, onset_rate, bpm);

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
