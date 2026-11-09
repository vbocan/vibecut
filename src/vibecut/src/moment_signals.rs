//! Why a moment was picked — and keeping picks off scene cuts.
//!
//! [`build_source_signals`] turns one source's [`SourceTelemetry`] into
//! the score curve the picker ranks moments on, keeping every signal
//! apart ([`SignalPart`]) along the way. Once the picks are made,
//! [`explain_segments`] uses those parts to record, per pick, which
//! signals carried it and what they measured — "38 km/h", "airtime
//! 0.6 s", "fast spin (540°/s)", "HiLight tag" — in the manifest
//! (`segments[].signals`), where the web UI's moment inspector shows them.
//!
//! # Signals
//!
//! Every telemetry signal is optional per camera: it is used only when
//! that camera recorded the stream behind it.
//!
//! - **Continuous** — accelerometer spikes, GPS speed, gyro rotation and
//!   on-screen motion — are blended as a weighted mean over the ones the
//!   camera has, scaled so the take's most eventful moment scores the
//!   motion weight ([`crate::selection_scoring::blend_available`]): a
//!   missing stream's weight is shared by the rest, a GoPro with GPS and
//!   gyro neither outranks a phone clip just for having more sensors nor
//!   loses to it because its sensors seldom all fire at once, and footage
//!   with no telemetry scores exactly as it did before these signals
//!   existed.
//! - **Events** — HiLight tags and jump landings — are added on top at
//!   their own weight: they mark a moment rather than describe the take.
//!
//! Faces (`[scoring].faces`) are not a signal yet: they need a face
//! detection model that isn't bundled, so that weight is still unused.
//!
//! # Scene cuts
//!
//! [`avoid_scene_cuts`] moves each pick so it doesn't straddle a hard
//! scene change found by the frame analysis, and pulls a pick that starts
//! just after one back onto it.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::project_config::Scoring;
use crate::selection_planner::{snap_to_chapter, Segment, Source, SourceTelemetry};
use crate::selection_scoring::{self as scoring, Landing, ScoreCurve};

/// A signal that can explain a pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export, rename_all = "snake_case"))]
pub enum SignalKind {
    /// A HiLight tag (the camera's button or voice command).
    Hilight,
    /// An accelerometer jolt above the take's steady vibration.
    Accel,
    /// GPS: a fast stretch, a burst of speed or a hard stop.
    GpsSpeed,
    /// Gyroscope: a spin, flip or carve.
    Gyro,
    /// A jump: free fall, then a landing.
    Landing,
    /// Movement on screen (frame difference).
    Motion,
    /// The clip starts right after a scene cut — a placement note, not a
    /// score.
    SceneStart,
}

/// One reason a moment was picked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct PickSignal {
    pub kind: SignalKind,
    /// This signal's share of the moment's score, 0–1 (rounded to 0.01;
    /// 0 for placement notes such as `scene_start`).
    pub share: f32,
    /// What it measured, in plain words: "38 km/h", "airtime 0.6 s",
    /// "fast spin (540°/s)", "HiLight tag".
    pub label: String,
}

/// One signal as it went into a source's combined curve: `weight` is the
/// weight it actually carried there (after the continuous blend's
/// rescaling).
#[derive(Debug, Clone)]
pub struct SignalPart {
    pub kind: SignalKind,
    pub weight: f32,
    pub curve: ScoreCurve,
}

/// A source's signals, combined and apart, plus the physical tracks the
/// explanations quote.
#[derive(Debug, Clone)]
pub struct SourceSignals {
    /// The curve the picker ranks moments on.
    pub combined: ScoreCurve,
    pub parts: Vec<SignalPart>,
    pub sample_rate: f32,
    /// Smoothed GPS ground speed per curve bin, m/s (`None`: no fix).
    /// Empty without GPS.
    pub speed: Vec<Option<f32>>,
    /// Net rotation rate per curve bin, rad/s. Empty without a gyro.
    pub rotation: Vec<f32>,
    pub landings: Vec<Landing>,
}

/// Build a source's score curve from its telemetry with the configured
/// weights (see the module docs for how the signals combine), keeping
/// the parts for [`explain_window`]. The sharpness / exposure floors
/// apply to the combined curve last.
pub fn build_source_signals(tel: &SourceTelemetry, w: &Scoring, sample_rate: f32) -> SourceSignals {
    let dur = tel.total_duration_secs;
    let mut parts: Vec<SignalPart> = Vec::new();

    // Continuous signals, each only when this camera recorded it.
    if w.accel_peaks > 0.0 && !tel.accel_mag.is_empty() {
        let times = &tel.accel_time_secs;
        let curve = scoring::accel_curve(
            &tel.accel_mag,
            |i| times.get(i).copied().unwrap_or(0.0),
            dur,
            sample_rate,
        );
        parts.push(SignalPart { kind: SignalKind::Accel, weight: w.accel_peaks, curve });
    }
    let speed = if tel.gps_speed_mps.is_empty() {
        Vec::new()
    } else {
        scoring::speed_track(&tel.gps_time_secs, &tel.gps_speed_mps, dur, sample_rate)
    };
    if w.gps_speed > 0.0 && speed.iter().any(Option::is_some) {
        let curve = scoring::gps_speed_curve_from_track(&speed, dur, sample_rate);
        parts.push(SignalPart { kind: SignalKind::GpsSpeed, weight: w.gps_speed, curve });
    }
    let rotation = if tel.gyro_rad_s.is_empty() {
        Vec::new()
    } else {
        scoring::rotation_track(&tel.gyro_time_secs, &tel.gyro_rad_s, dur, sample_rate)
    };
    if w.gyro > 0.0 && !rotation.is_empty() {
        let curve = scoring::gyro_curve_from_track(&rotation, dur, sample_rate);
        parts.push(SignalPart { kind: SignalKind::Gyro, weight: w.gyro, curve });
    }
    // Visual motion: the same relative-spike treatment as the
    // accelerometer, on frame-difference MSD.
    if w.motion_peaks > 0.0 && !tel.visual_motion.is_empty() {
        let curve = scoring::spike_curve(
            &tel.visual_motion,
            &tel.visual_time_secs,
            dur,
            sample_rate,
            10.0, // baseline window seconds
            0.5,  // smooth window seconds
        );
        parts.push(SignalPart { kind: SignalKind::Motion, weight: w.motion_peaks, curve });
    }
    let available: Vec<(f32, &ScoreCurve)> = parts.iter().map(|p| (p.weight, &p.curve)).collect();
    let (mut combined, k) =
        scoring::blend_available(&available, scoring::reference_weight(w.motion_peaks), dur, sample_rate);
    for p in &mut parts {
        p.weight *= k;
    }

    // Events, on top at their own weight.
    if w.hilight_tags > 0.0 && !tel.hilights_secs.is_empty() {
        let ms: Vec<u32> = tel.hilights_secs.iter().map(|t| (t * 1000.0) as u32).collect();
        let curve = scoring::hilight_curve(&ms, dur, sample_rate);
        combined.add_weighted(&curve, w.hilight_tags);
        parts.push(SignalPart { kind: SignalKind::Hilight, weight: w.hilight_tags, curve });
    }
    let landings = if tel.accel_mag.is_empty() {
        Vec::new()
    } else {
        scoring::detect_landings(&tel.accel_mag, &tel.accel_time_secs)
    };
    if w.landings > 0.0 && !landings.is_empty() {
        let curve = scoring::landing_curve(&landings, dur, sample_rate);
        combined.add_weighted(&curve, w.landings);
        parts.push(SignalPart { kind: SignalKind::Landing, weight: w.landings, curve });
    }

    // Floors: zero out moments where the picture is too blurry or too
    // clipped, after everything else so a great spike on a blurry frame
    // is still suppressed.
    if !tel.visual_sharpness.is_empty() {
        scoring::apply_percentile_floor(&mut combined, &tel.visual_sharpness, tel.visual_fps_hz, w.sharpness_floor);
    }
    if !tel.visual_exposure.is_empty() {
        scoring::apply_percentile_floor(&mut combined, &tel.visual_exposure, tel.visual_fps_hz, w.exposure_floor);
    }

    SourceSignals { combined, parts, sample_rate, speed, rotation, landings }
}

/// At most this many scored signals explain a pick...
const MAX_SIGNALS: usize = 3;
/// ...and beyond the first, only those carrying at least this share.
const MIN_SHARE: f32 = 0.12;
/// A deceleration (m/s²) this hard reads as "hard braking"...
const BRAKING_LABEL_MPS2: f32 = 3.0;
/// ...and as "a hard stop" when it ends at or below this (~7 km/h).
const STOPPED_MPS: f32 = 2.0;

/// The signals behind the window `[start, end]` (source time) of a
/// source: each part's contribution is its weight × its peak in the
/// window; the strongest (up to [`MAX_SIGNALS`], later ones only from
/// [`MIN_SHARE`]) are returned with their share and a plain-words value,
/// then a `scene_start` note when the window starts on a scene cut.
pub fn explain_window(sig: &SourceSignals, tel: &SourceTelemetry, start: f32, end: f32) -> Vec<PickSignal> {
    let mut scored: Vec<(SignalKind, f32, f32)> = sig
        .parts
        .iter()
        .filter_map(|p| {
            let peak = window_max(&p.curve, start, end);
            let c = p.weight * peak;
            (c > 0.0).then_some((p.kind, c, peak))
        })
        .collect();
    let total: f32 = scored.iter().map(|s| s.1).sum();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = Vec::new();
    for (kind, c, peak) in scored {
        let share = c / total;
        if out.len() >= MAX_SIGNALS || (share < MIN_SHARE && !out.is_empty()) {
            break;
        }
        if let Some(label) = label_for(kind, peak, sig, tel, start, end) {
            out.push(PickSignal { kind, share: (share * 100.0).round() / 100.0, label });
        }
    }
    if tel.scene_cuts.iter().any(|c| (c[1] - start).abs() < 0.01) {
        out.push(PickSignal {
            kind: SignalKind::SceneStart,
            share: 0.0,
            label: "opens on a scene change".into(),
        });
    }
    out
}

/// Fill in `signals` for every scored pick in `segments` (source time
/// windows; picks with no score or a zero, filler, score are left alone).
/// `telemetries` is parallel to `sources`. Each source's signals are
/// rebuilt once from its telemetry with `weights` — the same curves the
/// picker ranked. A multi-camera angle group's pick is explained by its
/// master camera's signals.
pub fn explain_segments(
    segments: &mut [Segment],
    sources: &[Source],
    telemetries: &[SourceTelemetry],
    weights: &Scoring,
    sample_rate: f32,
) {
    let mut built: HashMap<usize, SourceSignals> = HashMap::new();
    for seg in segments.iter_mut() {
        if !seg.score.is_some_and(|s| s > 0.0) {
            continue;
        }
        let Some(i) = sources.iter().position(|s| s.id == seg.source_id) else { continue };
        let Some(tel) = telemetries.get(i) else { continue };
        let sig = built.entry(i).or_insert_with(|| build_source_signals(tel, weights, sample_rate));
        let start = seg.virtual_start_secs as f32;
        seg.signals = explain_window(sig, tel, start, start + seg.duration_secs as f32);
    }
}

/// Move picks off hard scene changes ([`scoring::place_clear_of_cuts`]):
/// a pick that straddles a cut moves into the scene its middle is in, and
/// one that starts just after a cut starts on it. A move is kept only when
/// the pick still fits its chapter, ends up clear of cuts (or no worse)
/// and doesn't overlap another pick of the same source. `telemetries` is
/// parallel to `sources`. Returns how many picks moved.
pub fn avoid_scene_cuts(segments: &mut [Segment], sources: &[Source], telemetries: &[SourceTelemetry]) -> usize {
    let mut moved = 0;
    for i in 0..segments.len() {
        let seg = &segments[i];
        let Some(si) = sources.iter().position(|s| s.id == seg.source_id) else { continue };
        let Some(tel) = telemetries.get(si) else { continue };
        if tel.scene_cuts.is_empty() {
            continue;
        }
        let source = &sources[si];
        let (start, len) = (seg.virtual_start_secs as f32, seg.duration_secs as f32);
        let placed =
            scoring::place_clear_of_cuts(start, len, source.total_duration_secs as f32, &tel.scene_cuts);
        if placed.start == start {
            continue;
        }
        let Some(mut next) = snap_to_chapter(source, placed.start as f64, seg.duration_secs) else { continue };
        let was_clear = !scoring::straddles_cut(start, len, &tel.scene_cuts);
        let now_clear = !scoring::straddles_cut(next.virtual_start_secs as f32, len, &tel.scene_cuts);
        if was_clear && !now_clear {
            continue;
        }
        let (a, b) = (next.virtual_start_secs, next.virtual_start_secs + next.duration_secs);
        let overlaps = segments.iter().enumerate().any(|(j, o)| {
            j != i && o.source_id == seg.source_id && o.virtual_start_secs < b && a < o.virtual_start_secs + o.duration_secs
        });
        if overlaps {
            continue;
        }
        next.reason = seg.reason.clone();
        next.score = seg.score;
        next.signals = seg.signals.clone();
        segments[i] = next;
        moved += 1;
    }
    moved
}

/// The highest sample of `curve` in `[start, end]`.
fn window_max(curve: &ScoreCurve, start: f32, end: f32) -> f32 {
    if curve.samples.is_empty() {
        return 0.0;
    }
    let (i0, i1) = (curve.time_to_index(start.max(0.0)), curve.time_to_index(end.max(start)));
    curve.samples[i0..=i1.max(i0)].iter().copied().fold(0.0, f32::max)
}

/// The bins of a per-bin track that fall in `[start, end]`.
fn bins(len: usize, sample_rate: f32, start: f32, end: f32) -> std::ops::Range<usize> {
    if len == 0 {
        return 0..0;
    }
    let i0 = ((start.max(0.0) * sample_rate).round() as usize).min(len - 1);
    let i1 = ((end.max(start) * sample_rate).round() as usize).min(len - 1);
    i0..i1 + 1
}

fn label_for(
    kind: SignalKind,
    peak: f32,
    sig: &SourceSignals,
    tel: &SourceTelemetry,
    start: f32,
    end: f32,
) -> Option<String> {
    match kind {
        SignalKind::Hilight => {
            let n = tel.hilights_secs.iter().filter(|&&t| t >= start - 0.5 && t <= end + 0.5).count();
            Some(if n > 1 { format!("{n} HiLight tags") } else { "HiLight tag".into() })
        }
        SignalKind::Accel => {
            let g = tel
                .accel_time_secs
                .iter()
                .zip(&tel.accel_mag)
                .filter(|(t, _)| (start..=end).contains(*t))
                .map(|(_, m)| *m)
                .fold(0.0_f32, f32::max)
                / scoring::G;
            Some(if g > 0.0 { format!("{g:.1} g jolt") } else { "a jolt".into() })
        }
        SignalKind::GpsSpeed => Some(speed_label(sig, start, end)),
        SignalKind::Gyro => Some(rotation_label(sig, start, end)),
        SignalKind::Landing => {
            let air = sig
                .landings
                .iter()
                .filter(|l| l.touchdown_secs >= start && l.takeoff_secs <= end)
                .map(|l| l.airtime_secs)
                .fold(0.0_f32, f32::max);
            Some(if air > 0.0 { format!("airtime {air:.1} s") } else { "a jump".into() })
        }
        SignalKind::Motion => Some(
            if peak >= 0.6 { "lots of movement on screen" } else { "movement on screen" }.into(),
        ),
        SignalKind::SceneStart => None,
    }
}

/// "38 km/h", "52 km/h, top speed" or — when the GPS score peaks in the
/// window on braking rather than on speed — "hard braking from 40 km/h"
/// / "hard stop from 31 km/h".
pub fn speed_label(sig: &SourceSignals, start: f32, end: f32) -> String {
    let kmh = |v: f32| (v * 3.6).round();
    let range = bins(sig.speed.len(), sig.sample_rate, start, end);
    let fastest = sig.speed[range.clone()].iter().flatten().copied().fold(0.0_f32, f32::max);
    let decel = scoring::deceleration_track(&sig.speed, sig.sample_rate);
    // The bin where the GPS score peaks in the window, and whether braking
    // is what put it there (the score is max(speed, braking)).
    let braking_peak = sig.parts.iter().find(|p| p.kind == SignalKind::GpsSpeed).and_then(|p| {
        let peak = range
            .clone()
            .filter(|&i| i < p.curve.samples.len())
            .max_by(|&a, &b| p.curve.samples[a].partial_cmp(&p.curve.samples[b]).unwrap_or(std::cmp::Ordering::Equal))?;
        let d = decel.get(peak).copied().flatten()?;
        let braking = ((d - scoring::GPS_DECEL_FLOOR) / (scoring::GPS_DECEL_FULL - scoring::GPS_DECEL_FLOOR)).clamp(0.0, 1.0);
        (d >= BRAKING_LABEL_MPS2 && braking >= p.curve.samples[peak] - 1e-6).then_some(())
    });
    if braking_peak.is_some() {
        // The speed it braked from (the fastest from a second before), and
        // whether it came to a stop (the slowest till a second after).
        let before = bins(sig.speed.len(), sig.sample_rate, start - 1.0, end);
        let from = sig.speed[before].iter().flatten().copied().fold(0.0_f32, f32::max);
        let after = bins(sig.speed.len(), sig.sample_rate, start, end + 1.0);
        let to = sig.speed[after].iter().flatten().copied().fold(f32::INFINITY, f32::min);
        let what = if to <= STOPPED_MPS { "hard stop" } else { "hard braking" };
        return format!("{what} from {:.0} km/h", kmh(from));
    }
    let top = sig.speed.iter().flatten().copied().fold(0.0_f32, f32::max);
    if fastest >= 0.97 * top && top >= scoring::GPS_MIN_FAST_MPS {
        format!("{:.0} km/h, top speed", kmh(fastest))
    } else {
        format!("{:.0} km/h", kmh(fastest))
    }
}

/// "fast spin (540°/s)", "spin", "sharp turn" or "turning", by the peak
/// net rotation rate in the window.
pub fn rotation_label(sig: &SourceSignals, start: f32, end: f32) -> String {
    let range = bins(sig.rotation.len(), sig.sample_rate, start, end);
    let deg = sig.rotation[range].iter().copied().fold(0.0_f32, f32::max).to_degrees();
    let what = if deg >= 360.0 {
        "fast spin"
    } else if deg >= 180.0 {
        "spin"
    } else if deg >= 90.0 {
        "sharp turn"
    } else {
        "turning"
    };
    format!("{what} ({:.0}°/s)", deg.round())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection_planner::Chapter;

    const RATE: f32 = 4.0;

    /// 2 fps visual analysis of a `secs`-long take: wobbly motion with a
    /// burst at 12 s, sharp and well exposed except a soft patch.
    fn visual(secs: f32) -> SourceTelemetry {
        let n = (secs * 2.0) as usize;
        let t: Vec<f32> = (0..n).map(|i| i as f32 / 2.0).collect();
        SourceTelemetry {
            source_id: "0001".into(),
            total_duration_secs: secs,
            visual_fps_hz: 2.0,
            visual_motion: t
                .iter()
                .map(|&t| if (12.0..13.0).contains(&t) { 900.0 } else { 200.0 + 40.0 * (t * 1.7).sin() })
                .collect(),
            visual_sharpness: t.iter().map(|&t| if (40.0..44.0).contains(&t) { 30.0 } else { 400.0 + t }).collect(),
            visual_exposure: vec![0.97; n],
            visual_time_secs: t,
            ..Default::default()
        }
    }

    /// The pre-telemetry `build_score_curve`, verbatim: HiLight, then
    /// accelerometer, then motion, each added at its weight; then floors.
    fn legacy_curve(tel: &SourceTelemetry, w: &Scoring) -> ScoreCurve {
        let mut c = ScoreCurve::zeros(tel.total_duration_secs, RATE);
        if w.hilight_tags > 0.0 && !tel.hilights_secs.is_empty() {
            let ms: Vec<u32> = tel.hilights_secs.iter().map(|t| (t * 1000.0) as u32).collect();
            c.add_weighted(&scoring::hilight_curve(&ms, tel.total_duration_secs, RATE), w.hilight_tags);
        }
        if w.motion_peaks > 0.0 && !tel.visual_motion.is_empty() {
            let mc = scoring::spike_curve(
                &tel.visual_motion,
                &tel.visual_time_secs,
                tel.total_duration_secs,
                RATE,
                10.0,
                0.5,
            );
            c.add_weighted(&mc, w.motion_peaks);
        }
        scoring::apply_percentile_floor(&mut c, &tel.visual_sharpness, tel.visual_fps_hz, w.sharpness_floor);
        scoring::apply_percentile_floor(&mut c, &tel.visual_exposure, tel.visual_fps_hz, w.exposure_floor);
        c
    }

    #[test]
    fn footage_without_telemetry_scores_exactly_as_before() {
        let w = Scoring::default();
        let tel = visual(60.0);
        let sig = build_source_signals(&tel, &w, RATE);
        assert_eq!(sig.combined.samples, legacy_curve(&tel, &w).samples);
        assert_eq!(sig.parts.iter().map(|p| p.kind).collect::<Vec<_>>(), vec![SignalKind::Motion]);
        assert_eq!(sig.parts[0].weight, w.motion_peaks, "motion alone keeps its weight");
        // HiLight tags (an older camera's HMMT atom) on top: still the same.
        let tagged = SourceTelemetry { hilights_secs: vec![20.0, 33.5], ..visual(60.0) };
        assert_eq!(build_source_signals(&tagged, &w, RATE).combined.samples, legacy_curve(&tagged, &w).samples);
        // And the faces weight changes nothing (no face model yet).
        let no_faces = Scoring { faces: 0.0, ..Scoring::default() };
        assert_eq!(build_source_signals(&tel, &no_faces, RATE).combined.samples, sig.combined.samples);
    }

    /// `secs` from a camera with GPS (cruise 15 km/h, 38 km/h over
    /// 20–24 s), a 200 Hz accelerometer (a 0.6 s jump from 40 s) and a
    /// 200 Hz gyro (an 8 rad/s spin over 50–51 s); visual analysis too.
    fn gopro(secs: f32) -> SourceTelemetry {
        let mut tel = visual(secs);
        for i in 0..(secs * 10.0) as usize {
            let t = i as f32 / 10.0;
            tel.gps_time_secs.push(t);
            tel.gps_speed_mps.push(if (20.0..24.0).contains(&t) { 38.0 / 3.6 } else { 15.0 / 3.6 });
        }
        for i in 0..(secs * 200.0) as usize {
            let t = i as f32 / 200.0;
            let wobble = if i % 2 == 0 { 0.06 } else { -0.06 };
            let g = if (40.0..40.6).contains(&t) {
                0.05
            } else if (40.6..40.68).contains(&t) {
                3.0
            } else {
                1.0 + wobble
            };
            tel.accel_time_secs.push(t);
            tel.accel_mag.push(g * scoring::G);
            tel.gyro_time_secs.push(t);
            tel.gyro_rad_s.push([wobble * 10.0, 0.0, if (50.0..51.0).contains(&t) { 8.0 } else { 0.0 }]);
        }
        tel
    }

    #[test]
    fn each_signal_is_used_only_when_the_camera_recorded_it() {
        use SignalKind::*;
        let w = Scoring::default();
        let kinds = |tel: &SourceTelemetry, w: &Scoring| {
            build_source_signals(tel, w, RATE).parts.iter().map(|p| p.kind).collect::<Vec<_>>()
        };
        assert_eq!(kinds(&gopro(60.0), &w), vec![Accel, GpsSpeed, Gyro, Motion, Landing]);
        let no_gps = SourceTelemetry { gps_time_secs: vec![], gps_speed_mps: vec![], ..gopro(60.0) };
        assert_eq!(kinds(&no_gps, &w), vec![Accel, Gyro, Motion, Landing]);
        // A weight of 0 turns a signal off.
        let off = Scoring { gyro: 0.0, landings: 0.0, ..w.clone() };
        assert_eq!(kinds(&gopro(60.0), &off), vec![Accel, GpsSpeed, Motion]);
        // The continuous signals keep their relative weights and the take's
        // best continuous moment scores the motion weight; events keep theirs.
        let sig = build_source_signals(&gopro(60.0), &w, RATE);
        let cont: Vec<&SignalPart> = sig.parts.iter().filter(|p| p.kind != Landing).collect();
        assert!((cont[0].weight / cont[1].weight - w.accel_peaks / w.gps_speed).abs() < 1e-5);
        let best = (0..sig.combined.samples.len())
            .map(|i| cont.iter().map(|p| p.weight * p.curve.samples[i]).sum::<f32>())
            .fold(0.0_f32, f32::max);
        assert!((best - w.motion_peaks).abs() < 1e-5, "{best}");
        assert_eq!(sig.parts.last().unwrap().weight, w.landings);
    }

    #[test]
    fn a_pick_is_explained_by_its_strongest_signals_in_plain_words() {
        let w = Scoring::default();
        let tel = SourceTelemetry { hilights_secs: vec![70.0], scene_cuts: vec![[29.5, 30.0]], ..gopro(90.0) };
        let sig = build_source_signals(&tel, &w, RATE);
        let why = |a: f32, b: f32| explain_window(&sig, &tel, a, b);

        let fast = why(20.5, 23.5);
        assert_eq!(fast[0].kind, SignalKind::GpsSpeed, "{fast:?}");
        assert_eq!(fast[0].label, "38 km/h, top speed");
        let jump = why(39.0, 42.0);
        assert_eq!(jump[0].kind, SignalKind::Landing, "{jump:?}");
        assert!(jump[0].label.starts_with("airtime 0."), "{}", jump[0].label);
        let spin = why(49.5, 52.0);
        assert_eq!(spin[0].kind, SignalKind::Gyro, "{spin:?}");
        assert_eq!(spin[0].label, "fast spin (458°/s)");
        let tag = why(68.5, 71.5);
        assert_eq!((tag[0].kind, tag[0].label.as_str()), (SignalKind::Hilight, "HiLight tag"));

        for s in [&fast, &jump, &spin, &tag] {
            let scored: Vec<_> = s.iter().filter(|x| x.kind != SignalKind::SceneStart).collect();
            assert!(!scored.is_empty() && scored.len() <= MAX_SIGNALS, "{s:?}");
            assert!(scored.windows(2).all(|p| p[0].share >= p[1].share), "{s:?}");
            // Shares are rounded to 0.01 each.
            assert!(scored.iter().map(|x| x.share).sum::<f32>() <= 1.0 + 0.02, "{s:?}");
        }
        // A pick that starts on a scene cut says so, last.
        let opens = why(30.0, 33.0);
        assert_eq!(opens.last().map(|s| (s.kind, s.share)), Some((SignalKind::SceneStart, 0.0)), "{opens:?}");
        // Accelerometer jolts are quoted in g.
        let jolt = SourceSignals {
            parts: vec![SignalPart { kind: SignalKind::Accel, weight: 1.0, curve: sig.parts[0].curve.clone() }],
            ..sig.clone()
        };
        let label = explain_window(&jolt, &tel, 39.0, 42.0)[0].label.clone();
        assert_eq!(label, "3.0 g jolt");
        // A hard stop reads as one.
        let mut stop = gopro(60.0);
        for (t, v) in stop.gps_time_secs.iter().zip(stop.gps_speed_mps.iter_mut()) {
            *v = if *t < 30.0 { 10.0 } else if *t < 31.5 { 10.0 - 6.0 * (t - 30.0) } else { 1.0 };
        }
        let sig = build_source_signals(&stop, &w, RATE);
        assert_eq!(speed_label(&sig, 30.0, 32.0), "hard stop from 36 km/h");
        // Braking that doesn't end in a stop is just braking.
        let mut slow = gopro(60.0);
        for (t, v) in slow.gps_time_secs.iter().zip(slow.gps_speed_mps.iter_mut()) {
            *v = if *t < 30.0 { 10.0 } else if *t < 30.8 { 10.0 - 6.25 * (t - 30.0) } else { 5.0 };
        }
        let sig = build_source_signals(&slow, &w, RATE);
        assert_eq!(speed_label(&sig, 30.0, 32.0), "hard braking from 36 km/h");
        // Steady riding is just a speed.
        assert_eq!(speed_label(&sig, 10.0, 13.0), "36 km/h, top speed");
        assert_eq!(speed_label(&sig, 40.0, 43.0), "18 km/h");
    }

    fn source(secs: f64) -> Source {
        Source {
            id: "0001".into(),
            chapters: vec![Chapter {
                path: "GX010001.MP4".into(),
                chapter_index: 1,
                duration_secs: secs,
                creation_time: None,
                blake3: None,
                color: None,
            }],
            total_duration_secs: secs,
        }
    }

    fn pick(start: f64, score: f32) -> Segment {
        Segment {
            source_id: "0001".into(),
            chapter_path: "GX010001.MP4".into(),
            chapter_index: 1,
            in_chapter_start_secs: start,
            duration_secs: 3.0,
            virtual_start_secs: start,
            reason: format!("score={score:.3} (multi-source)"),
            score: Some(score),
            signals: Vec::new(),
        }
    }

    #[test]
    fn picks_are_moved_clear_of_scene_cuts() {
        let sources = [source(60.0)];
        let tels = [SourceTelemetry { scene_cuts: vec![[29.5, 30.0]], ..visual(60.0) }];
        // 28.5–31.5 straddles the cut and its middle is past it: it now
        // starts on the cut.
        let mut segs = vec![pick(28.5, 0.4), pick(45.0, 0.3)];
        assert_eq!(avoid_scene_cuts(&mut segs, &sources, &tels), 1);
        assert_eq!(segs[0].virtual_start_secs, 30.0);
        assert_eq!(segs[0].in_chapter_start_secs, 30.0);
        assert_eq!((segs[0].score, segs[0].reason.as_str()), (Some(0.4), "score=0.400 (multi-source)"));
        assert_eq!(segs[1].virtual_start_secs, 45.0);
        // ...unless that would run into another pick of the same source.
        let mut crowded = vec![pick(28.5, 0.4), pick(32.0, 0.3)];
        assert_eq!(avoid_scene_cuts(&mut crowded, &sources, &tels), 0);
        assert_eq!(crowded[0].virtual_start_secs, 28.5);
        // No cuts, nothing moves.
        let mut plain = vec![pick(28.5, 0.4)];
        assert_eq!(avoid_scene_cuts(&mut plain, &sources, &[visual(60.0)]), 0);
    }

    #[test]
    fn explanations_go_on_scored_picks_only() {
        let sources = [source(60.0)];
        let tels = [visual(60.0)];
        let mut segs = vec![pick(11.0, 0.3), pick(40.0, 0.0), Segment { score: None, ..pick(50.0, 0.1) }];
        explain_segments(&mut segs, &sources, &tels, &Scoring::default(), RATE);
        assert_eq!(segs[0].signals.len(), 1, "{:?}", segs[0].signals);
        assert_eq!(segs[0].signals[0].kind, SignalKind::Motion);
        assert_eq!(segs[0].signals[0].share, 1.0);
        assert_eq!(segs[0].signals[0].label, "lots of movement on screen");
        assert!(segs[1].signals.is_empty(), "filler");
        assert!(segs[2].signals.is_empty(), "unscored");
        // They round-trip through the manifest's JSON, and manifests
        // without them still read.
        let json = serde_json::to_value(&segs[0]).unwrap();
        assert_eq!(json["signals"][0]["kind"], "motion");
        let back: Segment = serde_json::from_value(json).unwrap();
        assert_eq!(back.signals, segs[0].signals);
        let old = serde_json::to_value(&segs[1]).unwrap();
        assert!(old.get("signals").is_none(), "empty is left out");
        assert!(serde_json::from_value::<Segment>(old).unwrap().signals.is_empty());
    }
}
