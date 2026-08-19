//! Score curves: a 1-D function of time that ranks how interesting each
//! moment is for inclusion in the final cut. Built from HiLight tags and
//! accelerometer, with audio onsets, visual signals, and ML scores
//! layered in on top.
//!
//! # Design
//!
//! - Curves are fixed-rate samples (default 4 Hz) over a known total duration.
//! - Curves are additively combinable with weights; non-negative.
//! - Selection runs non-maximum suppression over the combined curve to pick
//!   `k` top-scoring windows of length `L` with a minimum spacing constraint.

use serde::Serialize;

// ---- Signal-processing constants ----
/// Moving-average window for the "baseline" subtracted from raw accelerometer
/// magnitudes. Long enough to average out sustained vibration (ATV rides,
/// engine hum) without smoothing away the sharp impact spikes we want to keep.
const ACCEL_BASELINE_WINDOW_SECS: f32 = 10.0;
/// Short smooth applied to the baseline-subtracted signal before normalisation.
/// Widens the spike into a ~0.5 s window so the picker can center a clip on it.
const ACCEL_SMOOTH_WINDOW_SECS: f32 = 0.5;

/// A score curve: `samples[i]` is the score at time `i / sample_rate`.
#[derive(Debug, Clone, Serialize)]
pub struct ScoreCurve {
    pub samples: Vec<f32>,
    pub sample_rate: f32,
}

impl ScoreCurve {
    pub fn zeros(duration_secs: f32, sample_rate: f32) -> Self {
        let n = (duration_secs * sample_rate).ceil() as usize;
        Self {
            samples: vec![0.0; n.max(1)],
            sample_rate,
        }
    }

    /// Constant-1.0 curve — used as the uniform even-spacing fallback when all
    /// scoring weights are zero and no telemetry is available.
    pub fn constant(duration_secs: f32, sample_rate: f32) -> Self {
        let n = (duration_secs * sample_rate).ceil() as usize;
        Self {
            samples: vec![1.0; n.max(1)],
            sample_rate,
        }
    }

    pub fn duration_secs(&self) -> f32 {
        self.samples.len() as f32 / self.sample_rate
    }

    pub fn time_to_index(&self, t_secs: f32) -> usize {
        let idx = (t_secs * self.sample_rate).round() as i64;
        idx.clamp(0, self.samples.len() as i64 - 1) as usize
    }

    pub fn index_to_time(&self, i: usize) -> f32 {
        i as f32 / self.sample_rate
    }

    /// Add another curve into this one with a weight, after time-resampling
    /// to this curve's sample rate. Both curves are assumed to start at t=0.
    pub fn add_weighted(&mut self, other: &ScoreCurve, weight: f32) {
        if weight == 0.0 {
            return;
        }
        for i in 0..self.samples.len() {
            let t = self.index_to_time(i);
            let j = other.time_to_index(t);
            self.samples[i] += weight * other.samples[j];
        }
    }

    /// Apply a 1-D box-blur with a `window_secs` window. In-place.
    pub fn smooth(&mut self, window_secs: f32) {
        let half = ((window_secs * 0.5 * self.sample_rate).round() as usize).max(1);
        let n = self.samples.len();
        if n == 0 {
            return;
        }
        let mut prefix = vec![0.0_f64; n + 1];
        for i in 0..n {
            prefix[i + 1] = prefix[i] + self.samples[i] as f64;
        }
        for i in 0..n {
            let lo = i.saturating_sub(half);
            let hi = (i + half + 1).min(n);
            let len = (hi - lo) as f64;
            self.samples[i] = ((prefix[hi] - prefix[lo]) / len) as f32;
        }
    }

    /// Add a Dirac (`delta`) at `t_secs` with the given amplitude.
    pub fn add_dirac(&mut self, t_secs: f32, amplitude: f32) {
        if t_secs < 0.0 {
            return;
        }
        let i = self.time_to_index(t_secs);
        self.samples[i] += amplitude;
    }

    /// Normalize in-place so the peak sample == 1.0. No-op on an all-zero curve.
    /// Used after signal processing to give every curve the same amplitude
    /// range before they are additively combined with scoring weights.
    pub fn normalize_peak(&mut self) {
        let max = self.samples.iter().cloned().fold(0.0_f32, f32::max);
        if max > 0.0 {
            for s in &mut self.samples {
                *s /= max;
            }
        }
    }

    /// Multiply every sample by `factor`. Used by the
    /// picker to honour `[[selection.source_weights]]` entries before
    /// the multi-source pick stage.
    pub fn scale(&mut self, factor: f32) {
        if (factor - 1.0).abs() < f32::EPSILON {
            return;
        }
        for s in &mut self.samples {
            *s *= factor;
        }
    }

    /// Zero out the score across `[start_secs, end_secs)`.
    /// Used by `[[selection.exclude]]` to make the picker ignore
    /// ranges the user (or their feedback) marked as off-limits.
    pub fn zero_range(&mut self, start_secs: f32, end_secs: f32) {
        if !(end_secs > start_secs) || self.samples.is_empty() {
            return;
        }
        let i0 = self.time_to_index(start_secs.max(0.0));
        let i1 = self.time_to_index(end_secs).min(self.samples.len());
        if i1 > i0 {
            for s in &mut self.samples[i0..i1] {
                *s = 0.0;
            }
        }
    }
}

// ---------- Builders for individual signals ----------

/// Build a HiLight curve: a Dirac at every tag time, smoothed to a 1-second
/// gaussian-ish bump so neighbouring frames also score positively.
pub fn hilight_curve(
    hilight_times_ms: &[u32],
    duration_secs: f32,
    sample_rate: f32,
) -> ScoreCurve {
    let mut c = ScoreCurve::zeros(duration_secs, sample_rate);
    for &t_ms in hilight_times_ms {
        c.add_dirac(t_ms as f32 / 1000.0, 1.0);
    }
    c.smooth(1.0);
    // After smoothing the peaks shrink — renormalize so peak == 1.0 if any.
    c.normalize_peak();
    c
}

/// Build an accelerometer curve from per-sample magnitudes.
///
/// `magnitudes[i]` is `||a||` (m/s²) at sample i; `time_of(i)` returns the
/// wall-clock time of that sample. We:
///   1. Bin magnitudes into curve bins (max within bin).
///   2. Subtract a long-window moving average (the "baseline"), keeping only
///      excess magnitude — this surfaces *spikes* relative to constant
///      vibration (an ATV ride has high steady-state vibration).
///   3. Clip to [0, ∞) and normalize peak to 1.
pub fn accel_curve(
    magnitudes: &[f32],
    time_of: impl Fn(usize) -> f32,
    duration_secs: f32,
    sample_rate: f32,
) -> ScoreCurve {
    let mut c = ScoreCurve::zeros(duration_secs, sample_rate);
    if magnitudes.is_empty() {
        return c;
    }
    // (1) max-bin into the curve.
    let mut counts = vec![0u32; c.samples.len()];
    for (i, &m) in magnitudes.iter().enumerate() {
        let t = time_of(i);
        if t < 0.0 || t >= duration_secs {
            continue;
        }
        let j = c.time_to_index(t);
        if m > c.samples[j] {
            c.samples[j] = m;
        }
        counts[j] += 1;
    }
    // (2) compute baseline (long moving average — averages out steady vibration).
    let mut baseline = c.clone();
    baseline.smooth(ACCEL_BASELINE_WINDOW_SECS);
    // (3) subtract.
    for i in 0..c.samples.len() {
        c.samples[i] = (c.samples[i] - baseline.samples[i]).max(0.0);
    }
    // (4) light smooth to widen spikes, then normalize.
    c.smooth(ACCEL_SMOOTH_WINDOW_SECS);
    c.normalize_peak();
    c
}

// ---------- Selection: NMS-based picker ----------

/// One picked clip: midpoint time and the score it received.
#[derive(Debug, Clone)]
pub struct PickedClip {
    pub center_secs: f32,
    pub score: f32,
}

/// Non-maximum suppression: pick `k` clip centers from `curve` such that
/// each pick is at least `min_spacing_secs` from the others. Greedy on score.
///
/// Boundary handling: picks must be at least `clip_length_secs / 2` from the
/// start and end of the curve so the full clip fits.
///
/// `variety` ∈ [0, 1+] adds deterministic per-candidate jitter to scores
/// (`score * (1 + r * variety)`, `r ∈ [-1, 1]` keyed by `rng_seed` and the
/// candidate index). 0 = strict top-K. 0.15 = "two close-scoring candidates
/// might swap". 0.30 = "noticeable shake-up". `rng_seed` should incorporate
/// the source id so different sources produce uncorrelated jitter patterns.
pub fn pick_top_k_with_spacing(
    curve: &ScoreCurve,
    k: usize,
    clip_length_secs: f32,
    min_spacing_secs: f32,
    variety: f32,
    rng_seed: u64,
) -> Vec<PickedClip> {
    if k == 0 || curve.samples.is_empty() {
        return Vec::new();
    }
    let half = clip_length_secs * 0.5;
    let total = curve.duration_secs();
    if total < clip_length_secs {
        return Vec::new();
    }

    // Pre-rank candidate indices by score, restricted to the valid window.
    // Multiplicative score jitter when `variety > 0`: zero-score candidates
    // stay at zero (filler-pick correctness preserved); positive-score ones
    // get shifted by ±variety×100%. The jitter is deterministic per
    // (rng_seed, candidate index) pair so changing the seed changes the cut.
    let lo_i = curve.time_to_index(half);
    let hi_i = curve.time_to_index(total - half);
    let mut candidates: Vec<(usize, f32)> = (lo_i..=hi_i)
        .map(|i| {
            let raw = curve.samples[i];
            let score = if variety > 0.0 {
                let r = jitter_minus1_to_1(rng_seed, i);
                raw * (1.0 + r * variety)
            } else {
                raw
            };
            (i, score)
        })
        .collect();
    // Sort by score descending; stable on index for determinism.
    candidates.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });

    let mut picks: Vec<PickedClip> = Vec::with_capacity(k);
    // Pass 1: pick only positive-scoring candidates (the "interesting" moments).
    for (idx, score) in &candidates {
        if *score <= 0.0 {
            break;
        }
        let t = curve.index_to_time(*idx);
        let too_close = picks.iter().any(|p| (p.center_secs - t).abs() < min_spacing_secs);
        if !too_close {
            picks.push(PickedClip {
                center_secs: t,
                score: *score,
            });
            if picks.len() >= k {
                break;
            }
        }
    }
    // Pass 2 (filler): if we still need more picks and positive-score
    // candidates are exhausted, fill from zero-score regions respecting the
    // same spacing. These get score = 0.0 so callers can tell they were filler.
    if picks.len() < k {
        for (idx, _) in candidates.iter().filter(|(_, s)| *s <= 0.0) {
            let t = curve.index_to_time(*idx);
            let too_close = picks.iter().any(|p| (p.center_secs - t).abs() < min_spacing_secs);
            if !too_close {
                picks.push(PickedClip {
                    center_secs: t,
                    score: 0.0,
                });
                if picks.len() >= k {
                    break;
                }
            }
        }
    }
    // Return in chronological order (caller wants timeline order).
    picks.sort_by(|a, b| a.center_secs.partial_cmp(&b.center_secs).unwrap());
    picks
}

/// Build a relative-spike curve from any sequence of values + their times.
/// Conceptually identical to `accel_curve` but generic over any signal —
/// use this for visual motion (frame-difference MSD).
pub fn spike_curve(
    values: &[f32],
    times_secs: &[f32],
    duration_secs: f32,
    sample_rate: f32,
    baseline_window_secs: f32,
    smooth_secs: f32,
) -> ScoreCurve {
    let mut c = ScoreCurve::zeros(duration_secs, sample_rate);
    if values.is_empty() {
        return c;
    }
    debug_assert_eq!(values.len(), times_secs.len());
    for (v, t) in values.iter().zip(times_secs) {
        if *t < 0.0 || *t >= duration_secs {
            continue;
        }
        let i = c.time_to_index(*t);
        if *v > c.samples[i] {
            c.samples[i] = *v;
        }
    }
    let mut baseline = c.clone();
    baseline.smooth(baseline_window_secs);
    for i in 0..c.samples.len() {
        c.samples[i] = (c.samples[i] - baseline.samples[i]).max(0.0);
    }
    if smooth_secs > 0.0 {
        c.smooth(smooth_secs);
    }
    c.normalize_peak();
    c
}

/// Zero out every bin in `curve` whose corresponding sample in `frame_metric`
/// (sampled at `frame_fps_hz`) is in the bottom `floor_percentile` fraction.
/// Use for sharpness/exposure floors.
pub fn apply_percentile_floor(
    curve: &mut ScoreCurve,
    frame_metric: &[f32],
    frame_fps_hz: f32,
    floor_percentile: f32,
) {
    if floor_percentile <= 0.0 || frame_metric.is_empty() || frame_fps_hz <= 0.0 {
        return;
    }
    let mut sorted: Vec<f32> = frame_metric.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((sorted.len() as f32) * floor_percentile.clamp(0.0, 1.0)) as usize;
    let threshold = sorted[idx.min(sorted.len() - 1)];
    for i in 0..curve.samples.len() {
        let t = curve.index_to_time(i);
        let fi = (t * frame_fps_hz) as usize;
        if fi < frame_metric.len() && frame_metric[fi] < threshold {
            curve.samples[i] = 0.0;
        }
    }
}

// ---------- Multi-source diverse picker ----------

/// One picked clip with its source attribution.
#[derive(Debug, Clone)]
pub struct DiversePick {
    pub source_id: String,
    pub center_secs: f32,
    pub score: f32,
}

/// Pick at most `k` clips across multiple sources, with two diversity
/// constraints:
///
/// 1. **Per-source cap.** Each source contributes at most
///    `cap_override` (when given) or `ceil(k / n_sources_with_signal)`
///    (when `None`) clips. With 3 sources and `k=6`, the auto cap is 2.
///
/// 2. **Within-source spread.** Within each source the per-source picker
///    enforces a minimum spacing of `source_duration / (cap × 2)` so picks
///    are spread across the source's timeline, not clustered around a
///    single hot spot.
///
/// Sources with no positive score are skipped. Returned picks are sorted
/// first by `source_id`, then chronologically within each source.
pub fn pick_diverse(
    curves: &[(String, &ScoreCurve)],
    k: usize,
    clip_length_secs: f32,
    floor_min_spacing_secs: f32,
    cap_override: Option<usize>,
    variety: f32,
    seed: u64,
) -> Vec<DiversePick> {
    if k == 0 || curves.is_empty() {
        return Vec::new();
    }

    let n_with_signal = curves
        .iter()
        .filter(|(_, c)| c.samples.iter().any(|&s| s > 0.0))
        .count();
    if n_with_signal == 0 {
        // All sources have flat scores — no telemetry / all weights zero.
        // Fall back to uniform even-spacing: synthesise a constant-1.0 curve
        // for every source so picks are distributed proportionally across their
        // durations (longer sources get proportionally more picks). The recursion
        // terminates because the synthetic curves have n_with_signal > 0.
        let uniform: Vec<ScoreCurve> = curves
            .iter()
            .map(|(_, c)| ScoreCurve::constant(c.duration_secs(), c.sample_rate))
            .collect();
        let uniform_refs: Vec<(String, &ScoreCurve)> = curves
            .iter()
            .zip(uniform.iter())
            .map(|((id, _), uc)| (id.clone(), uc))
            .collect();
        return pick_diverse(
            &uniform_refs,
            k,
            clip_length_secs,
            floor_min_spacing_secs,
            cap_override,
            variety,
            seed,
        );
    }
    let cap = cap_override
        .unwrap_or_else(|| k.div_ceil(n_with_signal))
        .max(1);

    let mut all = Vec::with_capacity(cap * curves.len());
    for (id, curve) in curves {
        let dur = curve.duration_secs();
        // Within-source spacing forces spread across the source. Floor by
        // `floor_min_spacing_secs` so very short sources still get reasonable
        // separation between adjacent picks.
        let per_source_spacing =
            (dur / (cap as f32 * 2.0)).max(floor_min_spacing_secs);
        // Per-source RNG seed = global seed mixed with a hash of the source id
        // so different sources get uncorrelated jitter patterns.
        let per_source_seed = seed.wrapping_mul(0x9e3779b97f4a7c15).wrapping_add(hash_str(id));
        let picks = pick_top_k_with_spacing(
            curve,
            cap,
            clip_length_secs,
            per_source_spacing,
            variety,
            per_source_seed,
        );
        for p in picks {
            all.push(DiversePick {
                source_id: id.clone(),
                center_secs: p.center_secs,
                score: p.score,
            });
        }
    }

    // Keep only the global top-k by score.
    let n_positive = all.iter().filter(|p| p.score > 0.0).count();
    let n_filler = all.len() - n_positive;
    tracing::debug!(
        per_source_pool = all.len(),
        positive = n_positive,
        filler = n_filler,
        target_k = k,
        cap = cap,
        "pick_diverse pool"
    );
    all.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all.truncate(k);

    // Final order: by source id, then by time within source. Stable across
    // runs so manifests diff cleanly when scoring weights tweak.
    all.sort_by(|a, b| {
        a.source_id
            .cmp(&b.source_id)
            .then(a.center_secs.partial_cmp(&b.center_secs).unwrap())
    });
    all
}

/// Fast deterministic 64-bit hash of a string. Used to derive per-source RNG
/// seeds in `pick_diverse` so each source gets an uncorrelated jitter
/// pattern. Quality requirement is "different inputs → different outputs",
/// not cryptographic — FxHash-style mix is sufficient.
fn hash_str(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325; // FNV-1a 64-bit offset basis
    for &b in s.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3); // FNV-1a prime
    }
    h
}

/// Deterministic jitter in `[-1, 1)` from `(seed, idx)`. Thin wrapper around
/// `crate::rng::jitter_f32` — kept as a private helper so call sites inside
/// this module stay readable.
#[inline]
fn jitter_minus1_to_1(seed: u64, idx: usize) -> f32 {
    crate::rng::jitter_f32(seed, idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirac_lands_in_right_bin() {
        let mut c = ScoreCurve::zeros(10.0, 4.0);
        c.add_dirac(2.5, 1.0);
        let i = (2.5 * 4.0) as usize;
        assert!(c.samples[i] > 0.0);
    }

    #[test]
    fn hilight_curve_has_peaks_at_tag_times() {
        let c = hilight_curve(&[1000, 5000], 10.0, 4.0);
        let i1 = c.time_to_index(1.0);
        let i5 = c.time_to_index(5.0);
        let i3 = c.time_to_index(3.0);
        assert!(c.samples[i1] > c.samples[i3]);
        assert!(c.samples[i5] > c.samples[i3]);
    }

    #[test]
    fn accel_curve_promotes_relative_spikes() {
        // 100s of constant-magnitude background with a sharp spike at t=50.
        let mut mags = vec![10.0_f32; 1000]; // 10 samples per second
        mags[500] = 50.0;
        mags[501] = 50.0;
        let c = accel_curve(
            &mags,
            |i| i as f32 / 10.0,
            100.0,
            4.0,
        );
        let i_spike = c.time_to_index(50.0);
        let i_calm = c.time_to_index(10.0);
        assert!(c.samples[i_spike] > c.samples[i_calm]);
    }

    #[test]
    fn nms_respects_spacing() {
        let mut c = ScoreCurve::zeros(60.0, 4.0);
        // Two adjacent peaks 5s apart at t=10 and t=15.
        c.add_dirac(10.0, 1.0);
        c.add_dirac(15.0, 0.9);
        c.smooth(0.5);
        // With 10s spacing requirement: t=15 is too close to t=10 → rejected.
        // The picker may then fill from zero-score regions to fill k=2 — but
        // the fillers must also respect the spacing constraint. So picks[0]
        // should be the positive peak; picks[1..] should all be ≥10s away.
        let picks = pick_top_k_with_spacing(&c, 2, 4.0, 10.0, 0.0, 0);
        assert!(!picks.is_empty(), "expected at least one pick");
        let positive = picks.iter().find(|p| p.score > 0.0).expect("a positive pick");
        assert!((positive.center_secs - 10.0).abs() < 1.0);
        // All pairs of picks must be at least min_spacing apart.
        for (i, a) in picks.iter().enumerate() {
            for b in picks.iter().skip(i + 1) {
                assert!(
                    (a.center_secs - b.center_secs).abs() >= 10.0 - 1e-3,
                    "spacing violated: {} vs {}",
                    a.center_secs,
                    b.center_secs
                );
            }
        }
    }

    #[test]
    fn diverse_picker_caps_per_source() {
        // Two sources, each with two strong peaks. k=2 → cap = 1 per source,
        // so we should get exactly one pick from each.
        let mut a = ScoreCurve::zeros(60.0, 4.0);
        a.add_dirac(10.0, 1.0);
        a.add_dirac(50.0, 0.95);
        a.smooth(0.3);
        let mut b = ScoreCurve::zeros(60.0, 4.0);
        b.add_dirac(20.0, 0.8);
        b.add_dirac(40.0, 0.75);
        b.smooth(0.3);

        let picks = pick_diverse(
            &[("A".into(), &a), ("B".into(), &b)],
            2,
            4.0,
            5.0,
            None,
            0.0,
            0,
        );
        assert_eq!(picks.len(), 2);
        let from_a: usize = picks.iter().filter(|p| p.source_id == "A").count();
        let from_b: usize = picks.iter().filter(|p| p.source_id == "B").count();
        assert_eq!(from_a, 1);
        assert_eq!(from_b, 1);
    }

    #[test]
    fn diverse_picker_spreads_within_source() {
        // One long source with two equal-strength peaks far apart.
        // k=2 → cap=2 → both picks come from this source, but the within-source
        // spacing should keep them at the two peaks (not cluster on one).
        let mut s = ScoreCurve::zeros(1000.0, 4.0);
        s.add_dirac(100.0, 1.0);
        s.add_dirac(900.0, 0.95);
        s.smooth(0.5);

        let picks = pick_diverse(&[("S".into(), &s)], 2, 4.0, 5.0, None, 0.0, 0);
        assert_eq!(picks.len(), 2);
        let dt = (picks[1].center_secs - picks[0].center_secs).abs();
        assert!(dt > 100.0, "picks too close: dt={dt}");
    }

    #[test]
    fn diverse_picker_fills_with_many_sources_egypt_scenario() {
        // 80 sources, each with ~1 positive peak. K=108, cap=5. Want 108 picks.
        let mut curves: Vec<(String, ScoreCurve)> = Vec::new();
        for i in 0..80 {
            let mut c = ScoreCurve::zeros(120.0, 4.0); // 2 minutes
            c.add_dirac(60.0 + (i as f32 % 30.0), 0.5);
            c.smooth(0.3);
            curves.push((format!("src{i:03}"), c));
        }
        let refs: Vec<(String, &ScoreCurve)> =
            curves.iter().map(|(id, c)| (id.clone(), c)).collect();
        let picks = pick_diverse(&refs, 108, 2.787, 3.34, Some(5), 0.0, 0);
        // Should produce close to 108 picks total — maybe slightly less if
        // spacing constraints clip some sources.
        assert!(picks.len() >= 100, "expected ~108 picks, got {}", picks.len());
        let pos = picks.iter().filter(|p| p.score > 0.0).count();
        let filler = picks.iter().filter(|p| p.score == 0.0).count();
        eprintln!("80-source K=108 test: {} positive + {} filler = {} total",
            pos, filler, picks.len());
    }

    #[test]
    fn diverse_picker_fills_with_zero_score_picks_when_pool_short() {
        // Two sources, each with ONE positive peak. Without filler, we'd
        // get 2 picks total. With cap_override=5 and k=8, we want
        // pick_diverse to produce 8 picks total: 2 positive + 6 fillers.
        let mut a = ScoreCurve::zeros(60.0, 4.0);
        a.add_dirac(15.0, 1.0);
        a.smooth(0.3);
        let mut b = ScoreCurve::zeros(60.0, 4.0);
        b.add_dirac(40.0, 0.95);
        b.smooth(0.3);
        let picks = pick_diverse(
            &[("A".into(), &a), ("B".into(), &b)],
            8,
            4.0,
            5.0,
            Some(5),
            0.0,
            0,
        );
        assert_eq!(picks.len(), 8, "expected 8 picks (with fillers)");
        let positive = picks.iter().filter(|p| p.score > 0.0).count();
        let filler = picks.iter().filter(|p| p.score == 0.0).count();
        assert_eq!(positive, 2);
        assert_eq!(filler, 6);
    }

    #[test]
    fn diverse_picker_skips_dead_sources() {
        let mut a = ScoreCurve::zeros(60.0, 4.0);
        a.add_dirac(30.0, 1.0);
        a.smooth(0.3);
        let dead = ScoreCurve::zeros(60.0, 4.0);

        let picks = pick_diverse(
            &[("A".into(), &a), ("DEAD".into(), &dead)],
            3,
            4.0,
            5.0,
            None,
            0.0,
            0,
        );
        // Only A has signal; cap = ceil(3/1) = 3 — but A only has one positive peak.
        assert!(!picks.is_empty());
        for p in &picks {
            assert_eq!(p.source_id, "A");
        }
    }

    #[test]
    fn variety_zero_is_deterministic() {
        // Two close-scoring peaks. With variety = 0, the picker is deterministic
        // and the same call always produces the same picks regardless of seed.
        let mut c = ScoreCurve::zeros(60.0, 4.0);
        c.add_dirac(15.0, 1.00);
        c.add_dirac(45.0, 0.97);
        c.smooth(0.3);
        let p1 = pick_top_k_with_spacing(&c, 1, 4.0, 5.0, 0.0, 1);
        let p2 = pick_top_k_with_spacing(&c, 1, 4.0, 5.0, 0.0, 999_999);
        assert_eq!(p1.len(), 1);
        assert_eq!(p2.len(), 1);
        assert!((p1[0].center_secs - p2[0].center_secs).abs() < 0.5,
            "different seeds should not change picks at variety=0");
    }

    #[test]
    fn variety_swaps_close_scoring_picks_with_different_seeds() {
        // Two peaks within ~5% score of each other. With moderate variety
        // and different seeds, the picker can swap which one wins. We don't
        // assert WHICH seed picks which (the seed→outcome mapping is opaque)
        // — only that there EXISTS a seed under which the order differs from
        // the deterministic one.
        let mut c = ScoreCurve::zeros(60.0, 4.0);
        c.add_dirac(15.0, 1.00);
        c.add_dirac(45.0, 0.97);
        c.smooth(0.3);
        let baseline = pick_top_k_with_spacing(&c, 1, 4.0, 5.0, 0.0, 0)[0].center_secs;
        let mut differs = false;
        for seed in 1..200 {
            let p = pick_top_k_with_spacing(&c, 1, 4.0, 5.0, 0.30, seed);
            if (p[0].center_secs - baseline).abs() > 5.0 {
                differs = true;
                break;
            }
        }
        assert!(differs, "variety=0.30 should pick the secondary peak under at least one of 200 seeds");
    }

    #[test]
    fn nms_returns_chronological_order() {
        let mut c = ScoreCurve::zeros(60.0, 4.0);
        c.add_dirac(45.0, 1.0);
        c.add_dirac(10.0, 0.9);
        c.add_dirac(25.0, 0.8);
        c.smooth(0.3);
        let picks = pick_top_k_with_spacing(&c, 3, 2.0, 5.0, 0.0, 0);
        assert_eq!(picks.len(), 3);
        for w in picks.windows(2) {
            assert!(w[0].center_secs <= w[1].center_secs);
        }
    }
}
