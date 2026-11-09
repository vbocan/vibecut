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

// ---------- Optional per-camera telemetry signals ----------
//
// Each of these is built only when the camera recorded the stream behind
// it (GPS, gyro, accelerometer), so footage without telemetry scores
// exactly as before. Unlike `accel_curve` they keep physical meaning
// rather than being peak-normalised: 0 means "nothing happening", 1
// "as eventful as this signal gets", so a calm take doesn't get a
// manufactured peak.

/// Below this top speed (18 km/h) a take never counts as fast: the speed
/// level is measured against `max(top speed, this)`.
pub const GPS_MIN_FAST_MPS: f32 = 5.0;
/// Window of the "cruising speed" a speed burst stands out from.
const GPS_BASELINE_SECS: f32 = 20.0;
/// Smoothing of the binned speed track (GPS noise, 10–18 Hz samples).
const GPS_SMOOTH_SECS: f32 = 1.0;
/// Gaps without a good fix shorter than this are bridged; longer ones
/// score nothing (and never fake a sudden stop at their edges).
const GPS_GAP_SECS: f32 = 3.0;
/// Deceleration (m/s²) that starts to count as braking, and where the
/// braking score saturates (~0.5 g: a hard stop on a bike or skis).
pub const GPS_DECEL_FLOOR: f32 = 1.5;
pub const GPS_DECEL_FULL: f32 = 5.0;

/// Ground speed per curve bin (m/s): the mean of the GPS samples in each
/// bin, bridged across short gaps, smoothed over ~1 s. `None` where there
/// was no good fix nearby.
pub fn speed_track(
    times_secs: &[f32],
    speeds_mps: &[f32],
    duration_secs: f32,
    sample_rate: f32,
) -> Vec<Option<f32>> {
    let n = ScoreCurve::zeros(duration_secs, sample_rate).samples.len();
    let mut sum = vec![0.0_f64; n];
    let mut count = vec![0u32; n];
    for (&t, &v) in times_secs.iter().zip(speeds_mps) {
        if !(0.0..duration_secs).contains(&t) || !v.is_finite() {
            continue;
        }
        let i = ((t * sample_rate).round() as usize).min(n - 1);
        sum[i] += v as f64;
        count[i] += 1;
    }
    let mut bins: Vec<Option<f32>> = (0..n)
        .map(|i| (count[i] > 0).then(|| (sum[i] / count[i] as f64) as f32))
        .collect();
    // Bridge short gaps linearly.
    let max_gap = (GPS_GAP_SECS * sample_rate).round() as usize;
    let mut prev: Option<usize> = None;
    for i in 0..n {
        if bins[i].is_none() {
            continue;
        }
        if let Some(p) = prev {
            if i - p > 1 && i - p <= max_gap {
                let (a, b) = (bins[p].unwrap_or(0.0), bins[i].unwrap_or(0.0));
                for k in p + 1..i {
                    let f = (k - p) as f32 / (i - p) as f32;
                    bins[k] = Some(a + (b - a) * f);
                }
            }
        }
        prev = Some(i);
    }
    let half = ((GPS_SMOOTH_SECS * 0.5 * sample_rate).round() as usize).max(1);
    masked_mean(&bins, half)
}

/// Box mean over `±half` bins of the known values only; unknown stays
/// unknown.
fn masked_mean(values: &[Option<f32>], half: usize) -> Vec<Option<f32>> {
    let n = values.len();
    let mut sum = vec![0.0_f64; n + 1];
    let mut cnt = vec![0u32; n + 1];
    for i in 0..n {
        sum[i + 1] = sum[i] + values[i].unwrap_or(0.0) as f64;
        cnt[i + 1] = cnt[i] + values[i].is_some() as u32;
    }
    (0..n)
        .map(|i| {
            values[i]?;
            let (lo, hi) = (i.saturating_sub(half), (i + half + 1).min(n));
            let c = cnt[hi] - cnt[lo];
            Some(((sum[hi] - sum[lo]) / c.max(1) as f64) as f32)
        })
        .collect()
}

/// Deceleration (m/s², positive = slowing down) at each bin of a speed
/// track, measured over one second; `None` where the track is unknown on
/// either side.
pub fn deceleration_track(track: &[Option<f32>], sample_rate: f32) -> Vec<Option<f32>> {
    let h = ((0.5 * sample_rate).round() as usize).max(1);
    let span_secs = (2 * h) as f32 / sample_rate;
    (0..track.len())
        .map(|i| {
            let before = track.get(i.checked_sub(h)?)?.as_ref()?;
            let after = track.get(i + h)?.as_ref()?;
            Some((before - after) / span_secs)
        })
        .collect()
}

/// GPS speed score from a [`speed_track`]: half the speed level (against
/// the take's top speed, never below [`GPS_MIN_FAST_MPS`]), half a speed
/// burst above the ~20 s cruising speed — or a sudden deceleration
/// (braking, a stop at the bottom of a run, a crash), whichever is
/// higher. In `[0, 1]`; 0 where there was no good fix.
pub fn gps_speed_curve_from_track(
    track: &[Option<f32>],
    duration_secs: f32,
    sample_rate: f32,
) -> ScoreCurve {
    let mut c = ScoreCurve::zeros(duration_secs, sample_rate);
    let top = track.iter().flatten().copied().fold(0.0_f32, f32::max);
    if top <= 0.0 {
        return c;
    }
    let norm = top.max(GPS_MIN_FAST_MPS);
    let base_half = ((GPS_BASELINE_SECS * 0.5 * sample_rate).round() as usize).max(1);
    let baseline = masked_mean(track, base_half);
    let decel = deceleration_track(track, sample_rate);
    for (i, s) in c.samples.iter_mut().enumerate() {
        let Some(v) = track.get(i).copied().flatten() else { continue };
        let level = (v / norm).clamp(0.0, 1.0);
        let burst = baseline[i].map_or(0.0, |b| ((v - b) / norm).clamp(0.0, 1.0));
        let braking = decel[i].map_or(0.0, |d| {
            ((d - GPS_DECEL_FLOOR) / (GPS_DECEL_FULL - GPS_DECEL_FLOOR)).clamp(0.0, 1.0)
        });
        *s = (0.5 * level + 0.5 * burst).max(braking);
    }
    c
}

/// [`gps_speed_curve_from_track`] straight from GPS samples.
pub fn gps_speed_curve(
    times_secs: &[f32],
    speeds_mps: &[f32],
    duration_secs: f32,
    sample_rate: f32,
) -> ScoreCurve {
    let track = speed_track(times_secs, speeds_mps, duration_secs, sample_rate);
    gps_speed_curve_from_track(&track, duration_secs, sample_rate)
}

/// Rotation slower than this (~29°/s: head turns, a drifting hand) scores
/// nothing.
pub const GYRO_FLOOR_RAD_S: f32 = 0.5;
/// Rotation at or above this (~230°/s: a quick spin, a flip) scores 1.
pub const GYRO_FULL_RAD_S: f32 = 4.0;

/// Net rotation rate (rad/s) per curve bin: the length of the *mean*
/// angular-velocity vector over the bin. Averaging the vector before
/// taking its length cancels vibration (handlebar buzz oscillates around
/// zero) while a spin, flip or carve — rotation that keeps going one way
/// — survives.
pub fn rotation_track(
    times_secs: &[f32],
    rad_s: &[[f32; 3]],
    duration_secs: f32,
    sample_rate: f32,
) -> Vec<f32> {
    let n = ScoreCurve::zeros(duration_secs, sample_rate).samples.len();
    let mut sum = vec![[0.0_f64; 3]; n];
    let mut count = vec![0u32; n];
    for (&t, w) in times_secs.iter().zip(rad_s) {
        if !(0.0..duration_secs).contains(&t) {
            continue;
        }
        let i = ((t * sample_rate).round() as usize).min(n - 1);
        for a in 0..3 {
            sum[i][a] += w[a] as f64;
        }
        count[i] += 1;
    }
    (0..n)
        .map(|i| {
            if count[i] == 0 {
                return 0.0;
            }
            let c = count[i] as f64;
            let (x, y, z) = (sum[i][0] / c, sum[i][1] / c, sum[i][2] / c);
            (x * x + y * y + z * z).sqrt() as f32
        })
        .collect()
}

/// Rotation-energy score from a [`rotation_track`]: 0 below
/// [`GYRO_FLOOR_RAD_S`], rising linearly to 1 at [`GYRO_FULL_RAD_S`],
/// lightly smoothed so a half-second flip reads as one bump.
pub fn gyro_curve_from_track(rotation: &[f32], duration_secs: f32, sample_rate: f32) -> ScoreCurve {
    let mut c = ScoreCurve::zeros(duration_secs, sample_rate);
    for (s, &r) in c.samples.iter_mut().zip(rotation) {
        *s = ((r - GYRO_FLOOR_RAD_S) / (GYRO_FULL_RAD_S - GYRO_FLOOR_RAD_S)).clamp(0.0, 1.0);
    }
    c.smooth(0.5);
    c
}

/// [`gyro_curve_from_track`] straight from gyro samples.
pub fn gyro_curve(
    times_secs: &[f32],
    rad_s: &[[f32; 3]],
    duration_secs: f32,
    sample_rate: f32,
) -> ScoreCurve {
    let track = rotation_track(times_secs, rad_s, duration_secs, sample_rate);
    gyro_curve_from_track(&track, duration_secs, sample_rate)
}

/// Standard gravity, m/s².
pub const G: f32 = 9.80665;
/// Below this the camera is in free fall: an accelerometer reads ~0 g in
/// the air, ~1 g riding along.
pub const AIR_MAX_G: f32 = 0.45;
/// Shortest and longest free fall that counts as a jump.
pub const AIR_MIN_SECS: f32 = 0.2;
pub const AIR_MAX_SECS: f32 = 3.0;
/// The landing: a spike of at least this within [`IMPACT_WINDOW_SECS`]
/// of the free fall ending.
pub const IMPACT_MIN_G: f32 = 1.8;
pub const IMPACT_WINDOW_SECS: f32 = 0.4;
/// Accelerometers below this rate can't time a jump.
const LANDING_MIN_RATE_HZ: f32 = 20.0;

/// A jump the accelerometer felt: free fall followed by a landing spike.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Landing {
    pub takeoff_secs: f32,
    pub touchdown_secs: f32,
    pub airtime_secs: f32,
    /// Peak of the landing, in g.
    pub impact_g: f32,
}

/// Find jumps in an accelerometer magnitude series (`mags` in m/s² at
/// `times`): a run of at least [`AIR_MIN_SECS`] (at most
/// [`AIR_MAX_SECS`]) under [`AIR_MAX_G`] — smoothed over ~50 ms so
/// vibration doesn't break it up — followed within
/// [`IMPACT_WINDOW_SECS`] by a spike of [`IMPACT_MIN_G`] or more.
pub fn detect_landings(mags: &[f32], times: &[f32]) -> Vec<Landing> {
    let n = mags.len().min(times.len());
    if n < 3 {
        return Vec::new();
    }
    let span = times[n - 1] - times[0];
    let rate = if span > 0.0 { (n - 1) as f32 / span } else { 0.0 };
    if rate < LANDING_MIN_RATE_HZ {
        return Vec::new();
    }
    let half = ((0.025 * rate).round() as usize).max(1);
    let mut prefix = vec![0.0_f64; n + 1];
    for i in 0..n {
        prefix[i + 1] = prefix[i] + mags[i] as f64;
    }
    let smooth = |i: usize| {
        let (lo, hi) = (i.saturating_sub(half), (i + half + 1).min(n));
        ((prefix[hi] - prefix[lo]) / (hi - lo) as f64) as f32
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if smooth(i) >= AIR_MAX_G * G {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && smooth(i) < AIR_MAX_G * G {
            i += 1;
        }
        let end = i - 1;
        let airtime = times[end] - times[start];
        if !(AIR_MIN_SECS..=AIR_MAX_SECS).contains(&airtime) {
            continue;
        }
        let mut peak = 0.0_f32;
        let mut j = i;
        while j < n && times[j] - times[end] <= IMPACT_WINDOW_SECS {
            peak = peak.max(mags[j]);
            j += 1;
        }
        if peak >= IMPACT_MIN_G * G {
            out.push(Landing {
                takeoff_secs: times[start],
                touchdown_secs: times[end],
                airtime_secs: airtime,
                impact_g: peak / G,
            });
        }
    }
    out
}

/// A bump per jump, peaking mid-flight so the picked clip holds the
/// take-off, the flight and the landing. Height grows with airtime (1 at
/// 0.8 s or more, never below 0.35); the bump spans the flight plus
/// ~0.75 s either side.
pub fn landing_curve(landings: &[Landing], duration_secs: f32, sample_rate: f32) -> ScoreCurve {
    let mut c = ScoreCurve::zeros(duration_secs, sample_rate);
    for l in landings {
        let mid = 0.5 * (l.takeoff_secs + l.touchdown_secs);
        let width = 0.5 * l.airtime_secs + 0.75;
        let height = (l.airtime_secs / 0.8).clamp(0.35, 1.0);
        for (i, s) in c.samples.iter_mut().enumerate() {
            let t = i as f32 / sample_rate;
            let v = height * (1.0 - (t - mid).abs() / width);
            if v > *s {
                *s = v;
            }
        }
    }
    c
}

/// The height a camera's *continuous* signals (accelerometer, GPS speed,
/// gyro, visual motion) are blended to: the visual-motion weight — the
/// one signal any camera has — or 1 when it is off. See
/// [`blend_available`].
pub fn reference_weight(motion_weight: f32) -> f32 {
    if motion_weight > 0.0 {
        motion_weight
    } else {
        1.0
    }
}

/// Blend the continuous signals one camera provides: their weighted mean
/// over the signals in `available` (`(weight, curve)`, weights ≤ 0
/// skipped), scaled so the camera's most eventful moment scores
/// `reference_weight`.
///
/// A signal the camera didn't record is simply absent: the others share
/// its weight, so the mix *within* a take follows the configured weights
/// over what it has. Scaling every camera's best moment to the same
/// height keeps cameras comparable however many sensors they have —
/// signals rarely all fire at once, so a plain mean would mark a GoPro's
/// top-speed moment (GPS high, gyro quiet) below a phone's busiest frame
/// (its peak-normalised motion), and a plain sum would let any GoPro
/// outrank any phone. Telemetry decides *which* moments of a take are
/// its best; HiLight tags and landings (added on top) decide what beats
/// a take's best. With visual motion alone (peak 1) the scale is exactly
/// 1: footage without telemetry scores as it always did.
///
/// Returns the blended curve and the factor every weight was multiplied
/// by (0 when nothing was available or everything is zero).
pub fn blend_available(
    available: &[(f32, &ScoreCurve)],
    reference_weight: f32,
    duration_secs: f32,
    sample_rate: f32,
) -> (ScoreCurve, f32) {
    let mut c = ScoreCurve::zeros(duration_secs, sample_rate);
    for (w, curve) in available {
        if *w > 0.0 {
            c.add_weighted(curve, *w);
        }
    }
    // Σ wᵢcᵢ / Σ wᵢ, peak-normalised: the Σ wᵢ cancels.
    let peak = c.samples.iter().copied().fold(0.0_f32, f32::max);
    if !(peak > 0.0) {
        return (c, 0.0);
    }
    let k = reference_weight / peak;
    c.scale(k);
    (c, k)
}

// ---------- Scene cuts ----------

/// How far before a clip a scene cut may be for the clip to be pulled
/// back to start right on it.
pub const SCENE_SNAP_SECS: f32 = 1.0;

/// Where a clip lands after [`place_clear_of_cuts`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CutPlacement {
    pub start: f32,
    /// The clip now starts right after a scene cut.
    pub on_cut: bool,
    /// The clip doesn't straddle a cut (false only when the scene around
    /// its middle is shorter than the clip).
    pub clear: bool,
}

/// Whether the window `[start, start + len]` straddles one of `cuts`
/// (`[before, after]`: the cut happened between those two analysed
/// frames).
pub fn straddles_cut(start: f32, len: f32, cuts: &[[f32; 2]]) -> bool {
    cuts.iter().any(|c| c[1] > start && c[0] < start + len)
}

/// Move a clip `[start, start + len]` (inside a take `total` long) so it
/// doesn't straddle a hard scene change, and so it starts on one that
/// lies just before it:
///
/// 1. A clip that straddles a cut moves the least it can to sit inside the
///    scene its middle is in — between the cut before (starting no
///    earlier than its `after` frame) and the cut after. A scene shorter
///    than the clip leaves it where it was.
/// 2. A clip that starts within [`SCENE_SNAP_SECS`] after a cut is pulled
///    back to start on it, so the edit's cut lines up with the footage's.
pub fn place_clear_of_cuts(start: f32, len: f32, total: f32, cuts: &[[f32; 2]]) -> CutPlacement {
    let mut s = start;
    if straddles_cut(s, len, cuts) {
        let mid = start + 0.5 * len;
        let lo = cuts
            .iter()
            .filter(|c| 0.5 * (c[0] + c[1]) <= mid)
            .map(|c| c[1])
            .fold(0.0_f32, f32::max);
        let hi = cuts
            .iter()
            .filter(|c| 0.5 * (c[0] + c[1]) > mid)
            .map(|c| c[0])
            .fold(total, f32::min);
        if hi - lo < len {
            return CutPlacement { start, on_cut: false, clear: false };
        }
        s = s.clamp(lo, (hi - len).max(lo));
    }
    let just_before = cuts
        .iter()
        .map(|c| c[1])
        .filter(|&after| after <= s && s - after <= SCENE_SNAP_SECS)
        .fold(None, |acc: Option<f32>, a| Some(acc.map_or(a, |b| b.max(a))));
    if let Some(after) = just_before {
        s = after;
    }
    CutPlacement {
        start: s,
        on_cut: cuts.iter().any(|c| c[1] == s),
        clear: !straddles_cut(s, len, cuts),
    }
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

    // ---------- telemetry signals ----------

    /// `f(t)` sampled at `hz` over `[0, secs)`.
    fn series(secs: f32, hz: f32, f: impl Fn(f32) -> f32) -> (Vec<f32>, Vec<f32>) {
        let n = (secs * hz) as usize;
        let t: Vec<f32> = (0..n).map(|i| i as f32 / hz).collect();
        let v = t.iter().map(|&t| f(t)).collect();
        (t, v)
    }

    fn at(c: &ScoreCurve, t: f32) -> f32 {
        c.samples[c.time_to_index(t)]
    }

    #[test]
    fn gps_speed_scores_fast_stretches_bursts_and_hard_stops() {
        // Cruise at 5 m/s, burst to 15 m/s over 40–45 s, back to cruise;
        // from 70 s speed up to 12 m/s, then brake to 0 within 2 s at 80 s.
        let speed = |t: f32| match t {
            t if (40.0..45.0).contains(&t) => 15.0,
            t if (70.0..80.0).contains(&t) => 12.0,
            t if (80.0..82.0).contains(&t) => 12.0 - 6.0 * (t - 80.0),
            t if t >= 82.0 => 0.0,
            _ => 5.0,
        };
        let (t, v) = series(100.0, 10.0, speed);
        let c = gps_speed_curve(&t, &v, 100.0, 4.0);
        assert!(c.samples.iter().all(|s| (0.0..=1.0).contains(s)));
        let cruise = at(&c, 20.0);
        let burst = at(&c, 42.5);
        // Top speed (level 1) and well above the 20 s cruising average
        // (7.5 m/s): 0.5 + 0.5 × 0.5.
        assert!((burst - 0.75).abs() < 0.02, "the take's top speed, well above cruising: {burst}");
        assert!(cruise < 0.25, "cruising at a third of top speed: {cruise}");
        let braking = at(&c, 81.0);
        assert!(braking > 0.6, "6 m/s² of braking: {braking}");
        assert!(at(&c, 90.0) < 0.05, "standing still");
    }

    #[test]
    fn gps_gaps_are_bridged_when_short_and_score_nothing_when_long() {
        // 10 m/s throughout, but no fix for 1 s at 20 s and for 30 s from 40 s.
        let (t, v): (Vec<f32>, Vec<f32>) = series(100.0, 10.0, |_| 10.0)
            .0
            .into_iter()
            .filter(|t| !(20.0..21.0).contains(t) && !(40.0..70.0).contains(t))
            .map(|t| (t, 10.0))
            .unzip();
        let track = speed_track(&t, &v, 100.0, 4.0);
        assert_eq!(track[(20.5 * 4.0) as usize], Some(10.0), "1 s gap bridged");
        assert_eq!(track[(55.0 * 4.0) as usize], None, "30 s without a fix stays unknown");
        let c = gps_speed_curve_from_track(&track, 100.0, 4.0);
        assert_eq!(at(&c, 55.0), 0.0);
        // The gap's edges are not a sudden stop.
        assert!(at(&c, 40.0) <= at(&c, 30.0) + 1e-6 && at(&c, 70.5) <= at(&c, 30.0) + 1e-6);
        // A slow walk never reads as fast: 1.4 m/s against the 5 m/s floor.
        let (t, v) = series(60.0, 10.0, |_| 1.4);
        assert!(gps_speed_curve(&t, &v, 60.0, 4.0).samples.iter().all(|&s| s < 0.2));
    }

    #[test]
    fn gyro_vibration_cancels_but_a_spin_scores() {
        // 200 Hz: ±3 rad/s handlebar buzz throughout, plus a 1 s spin at
        // 6 rad/s about z from 10 s.
        let (t, _) = series(20.0, 200.0, |_| 0.0);
        let w: Vec<[f32; 3]> = t
            .iter()
            .enumerate()
            .map(|(i, &t)| {
                let buzz = if i % 2 == 0 { 3.0 } else { -3.0 };
                let spin = if (10.0..11.0).contains(&t) { 6.0 } else { 0.0 };
                [buzz, -buzz, spin]
            })
            .collect();
        let rot = rotation_track(&t, &w, 20.0, 4.0);
        assert!(rot[(5.0 * 4.0) as usize] < 0.1, "buzz averages out: {}", rot[20]);
        assert!((rot[(10.5 * 4.0) as usize] - 6.0).abs() < 0.1, "the spin survives: {}", rot[42]);
        let c = gyro_curve(&t, &w, 20.0, 4.0);
        assert!(at(&c, 10.5) > 0.9);
        assert!(at(&c, 5.0) == 0.0);
    }

    /// 200 Hz accelerometer magnitude: 1 g riding with a little vibration,
    /// free fall for `air` seconds from `takeoff`, then a landing of
    /// `impact_g` (none when 0).
    fn jump(takeoff: f32, air: f32, impact_g: f32) -> (Vec<f32>, Vec<f32>) {
        let (t, _) = series(20.0, 200.0, |_| 0.0);
        let m = t
            .iter()
            .enumerate()
            .map(|(i, &t)| {
                let wobble = if i % 2 == 0 { 0.08 } else { -0.08 };
                let g = if (takeoff..takeoff + air).contains(&t) {
                    0.05
                } else if impact_g > 0.0 && (takeoff + air..takeoff + air + 0.08).contains(&t) {
                    impact_g
                } else {
                    1.0 + wobble
                };
                g * G
            })
            .collect();
        (m, t)
    }

    #[test]
    fn landings_are_free_fall_then_an_impact() {
        let (m, t) = jump(8.0, 0.6, 3.2);
        let l = detect_landings(&m, &t);
        assert_eq!(l.len(), 1, "{l:?}");
        assert!((l[0].airtime_secs - 0.6).abs() < 0.05, "{:?}", l[0]);
        assert!((l[0].takeoff_secs - 8.0).abs() < 0.05);
        assert!(l[0].impact_g >= 3.0);
        // Free fall with no landing (the camera dropped into snow) and a
        // 0.1 s blip are not jumps.
        let (m, t) = jump(8.0, 0.6, 0.0);
        assert!(detect_landings(&m, &t).is_empty());
        let (m, t) = jump(8.0, 0.1, 3.2);
        assert!(detect_landings(&m, &t).is_empty());
        // A 4 Hz series can't time one.
        let (m, t) = jump(8.0, 0.6, 3.2);
        let (m4, t4): (Vec<f32>, Vec<f32>) = m.iter().zip(&t).step_by(50).map(|(a, b)| (*a, *b)).unzip();
        assert!(detect_landings(&m4, &t4).is_empty());
    }

    #[test]
    fn landing_curve_peaks_mid_flight_and_grows_with_airtime() {
        let short = Landing { takeoff_secs: 10.0, touchdown_secs: 10.3, airtime_secs: 0.3, impact_g: 2.0 };
        let long = Landing { takeoff_secs: 30.0, touchdown_secs: 31.0, airtime_secs: 1.0, impact_g: 3.0 };
        let c = landing_curve(&[short, long], 60.0, 4.0);
        assert!((at(&c, 30.5) - 1.0).abs() < 1e-6);
        assert!(at(&c, 10.15) > 0.3 && at(&c, 10.15) < 0.5);
        assert_eq!(at(&c, 20.0), 0.0);
        assert!(at(&c, 31.0) > 0.5, "the landing itself is in the bump");
    }

    #[test]
    fn missing_signals_renormalise_and_motion_alone_is_unchanged() {
        let flat = |v: f32| ScoreCurve { samples: vec![v; 40], sample_rate: 4.0 };
        // Peak-normalised, as `spike_curve` makes every motion curve.
        let mut motion = ScoreCurve { samples: (0..40).map(|i| (i as f32 * 0.37).sin().abs()).collect(), sample_rate: 4.0 };
        motion.normalize_peak();
        // Visual motion alone (no telemetry): bit-for-bit the old `w × motion`.
        let mut legacy = ScoreCurve::zeros(10.0, 4.0);
        legacy.add_weighted(&motion, 0.3);
        let (alone, k) = blend_available(&[(0.3, &motion)], reference_weight(0.3), 10.0, 4.0);
        assert_eq!(k, 1.0);
        assert_eq!(alone.samples, legacy.samples);
        // Two cameras' most eventful moments score the same however many
        // sensors they have...
        let one = flat(1.0);
        let half = |from: usize| ScoreCurve { samples: (0..40).map(|i| if i >= from { 1.0 } else { 0.0 }).collect(), sample_rate: 4.0 };
        let (phone, _) = blend_available(&[(0.3, &one)], 0.3, 10.0, 4.0);
        let (gopro, k) = blend_available(&[(0.3, &one), (0.4, &half(20)), (0.4, &half(30))], 0.3, 10.0, 4.0);
        assert!((k - 0.3 / 1.1).abs() < 1e-6, "{k}");
        assert!((gopro.samples[35] - phone.samples[35]).abs() < 1e-6);
        // ...even when its signals never all fire together: the GoPro's
        // speed moment (motion + GPS, gyro quiet) is its best, at 0.3.
        let (no_spin, _) = blend_available(&[(0.3, &one), (0.4, &half(20)), (0.4, &flat(0.0))], 0.3, 10.0, 4.0);
        assert!((no_spin.samples[25] - 0.3).abs() < 1e-6, "{}", no_spin.samples[25]);
        // Within a take the weights hold over the signals it has: where
        // only motion fires it scores 0.3 / (0.3 + 0.4) of where GPS joins in.
        assert!((no_spin.samples[5] - 0.3 * 0.3 / 0.7).abs() < 1e-6, "{}", no_spin.samples[5]);
        // A camera without GPS isn't marked down for it.
        let (no_gps, _) = blend_available(&[(0.3, &one)], 0.3, 10.0, 4.0);
        assert!((no_gps.samples[5] - 0.3).abs() < 1e-6);
        // Zero weights are off; nothing at all is a zero curve.
        let zero = flat(0.0);
        let (off, k) = blend_available(&[(0.0, &one), (0.3, &zero)], 0.3, 10.0, 4.0);
        assert!(off.samples.iter().all(|&s| s == 0.0));
        assert_eq!(k, 0.0);
        let (none, k) = blend_available(&[], 0.3, 10.0, 4.0);
        assert_eq!(k, 0.0);
        assert!(none.samples.iter().all(|&s| s == 0.0));
        assert_eq!(reference_weight(0.0), 1.0);
    }

    #[test]
    fn picks_move_off_scene_cuts_and_onto_ones_just_before() {
        let cut = |t: f32| [t - 0.5, t];
        // A 3 s clip at 9–12 straddles the cut at 10.5; its middle (10.5)
        // is past the cut's middle (10.25), so it moves into the later scene.
        let p = place_clear_of_cuts(9.0, 3.0, 60.0, &[cut(10.5)]);
        assert_eq!(p, CutPlacement { start: 10.5, on_cut: true, clear: true });
        // Middle before the cut: it ends before the cut instead.
        let p = place_clear_of_cuts(9.0, 3.0, 60.0, &[cut(11.5)]);
        assert_eq!(p.start, 8.0);
        assert!(p.clear && !p.on_cut);
        // Starting 0.8 s after a cut: pulled back to start on it.
        let p = place_clear_of_cuts(20.8, 3.0, 60.0, &[cut(20.0)]);
        assert_eq!(p, CutPlacement { start: 20.0, on_cut: true, clear: true });
        // 1.5 s after: left alone.
        assert_eq!(place_clear_of_cuts(21.5, 3.0, 60.0, &[cut(20.0)]).start, 21.5);
        // A scene shorter than the clip can't hold it: unchanged, not clear.
        let p = place_clear_of_cuts(30.0, 3.0, 60.0, &[cut(30.5), cut(32.0)]);
        assert_eq!(p, CutPlacement { start: 30.0, on_cut: false, clear: false });
        // No cuts: untouched.
        assert_eq!(place_clear_of_cuts(5.0, 3.0, 60.0, &[]), CutPlacement { start: 5.0, on_cut: false, clear: true });
        // Pulled back to end before a cut it ran into.
        let p = place_clear_of_cuts(55.5, 3.0, 60.0, &[cut(58.5)]);
        assert_eq!(p, CutPlacement { start: 55.0, on_cut: false, clear: true });
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
