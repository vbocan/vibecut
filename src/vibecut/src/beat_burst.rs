//! Beat-burst effect.
//!
//! Inserts a short rapid-cut sequence into the output where every clip lasts
//! exactly one beat (or every-other beat for very fast songs). The burst is
//! placed at the soundtrack's energy peak (the "drop" / chorus) so the visual
//! cut rate snaps to the music's loudest moment.
//!
//! # Suitability gate
//!
//! Auto mode enables the burst only when:
//! - `tempo_bpm ∈ [80, 180]` — slow ballads and frantic drum-and-bass don't
//!   read well as beat-cuts.
//! - `pulse_strength ≥ 1.8` — the autocorrelation peak-to-mean ratio. Ambient
//!   / arrhythmic music falls below this and gets skipped.
//! - `loud_region_length ≥ 6 s` — we need enough room for at least
//!   `beat_burst_min_beats` cuts inside the song's loud region.
//!
//! # Placement
//!
//! Burst start = `energy_peak_time` (the argmax of the soundtrack's energy
//! curve), snapped to the next beat. The burst replaces a contiguous slice
//! of normally-paced segments around that output time so the total runtime
//! stays close to `target_duration`.
//!
//! # Source clips
//!
//! Burst sub-clips come from the *same* score curves the main picker drew
//! from, but explicitly avoid moments already chosen by the main picker.
//! NMS-spaced (≥ 1 s apart in source time) to avoid two consecutive burst
//! cuts being from the same instant of the same source.

use camino::Utf8PathBuf;

use crate::audio_analysis::AnalyzedAudio;
use crate::project_config::{BeatBurstMode, Soundtrack};
use crate::selection_planner::{Segment, Source, snap_to_chapter};
use crate::selection_scoring::ScoreCurve;

// ---- Suitability gate thresholds ----
/// Minimum tempo for beat-burst suitability: slow ballads don't cut well.
const BURST_MIN_TEMPO_BPM: f32 = 80.0;
/// Maximum tempo for beat-burst suitability: frantic D&B produces sub-frame cuts.
const BURST_MAX_TEMPO_BPM: f32 = 180.0;
/// Minimum autocorrelation pulse-strength to confirm rhythmic regularity.
const BURST_MIN_PULSE_STRENGTH: f32 = 1.8;
/// Minimum loud-region length needed to fit `beat_burst_min_beats` cuts inside.
const BURST_MIN_LOUD_REGION_SECS: f32 = 6.0;

// ---- Output-timeline placement bounds ----
/// Burst start is clamped no earlier than this fraction of total output duration.
const BURST_PLACEMENT_LO_FRACTION: f32 = 0.25;
/// Burst start is clamped no later than this fraction of total output duration.
const BURST_PLACEMENT_HI_FRACTION: f32 = 0.70;

// ---- Beat-cut rate limiting ----
/// Above this BPM the burst skips every other beat so the cut rate stays ≤ ~3 Hz.
const BURST_EVERY_OTHER_BEAT_THRESHOLD_BPM: f32 = 140.0;

// ---- NMS spacing inside the burst candidate pool ----
/// Minimum source-time gap (seconds) between two burst sub-clip candidates from
/// the same source, enforced by non-maximum suppression during pool building.
const BURST_NMS_SPACING_SECS: f64 = 1.0;

/// Was a burst attempted? Why or why not?
#[derive(Debug, Clone)]
pub enum SuitabilityVerdict {
    Suitable {
        peak_secs: f32,
        loud_start: f32,
        loud_end: f32,
    },
    SkippedTempo {
        bpm: f32,
    },
    SkippedPulseStrength {
        pulse_strength: f32,
    },
    SkippedLoudTooShort {
        loud_secs: f32,
    },
}

impl SuitabilityVerdict {
    pub fn is_suitable(&self) -> bool {
        matches!(self, SuitabilityVerdict::Suitable { .. })
    }
}

/// Evaluate whether the soundtrack supports a beat-burst effect.
pub fn check_suitability(audio: &AnalyzedAudio) -> SuitabilityVerdict {
    if audio.tempo_bpm < BURST_MIN_TEMPO_BPM || audio.tempo_bpm > BURST_MAX_TEMPO_BPM {
        return SuitabilityVerdict::SkippedTempo {
            bpm: audio.tempo_bpm,
        };
    }
    if audio.pulse_strength < BURST_MIN_PULSE_STRENGTH {
        return SuitabilityVerdict::SkippedPulseStrength {
            pulse_strength: audio.pulse_strength,
        };
    }
    let (loud_start, loud_end) = audio.detect_loud_region(0.6);
    let loud_secs = (loud_end - loud_start).max(0.0);
    if loud_secs < BURST_MIN_LOUD_REGION_SECS {
        return SuitabilityVerdict::SkippedLoudTooShort { loud_secs };
    }
    // Energy peak: argmax of the energy curve, but only within the loud region.
    let dt = 1.0 / audio.energy_rate.max(1.0);
    let lo_idx = ((loud_start / dt).floor() as usize).min(audio.energy_curve.len());
    let hi_idx = ((loud_end / dt).ceil() as usize).min(audio.energy_curve.len());
    let mut peak_idx = lo_idx;
    let mut peak_val = -1.0_f32;
    for i in lo_idx..hi_idx {
        let v = audio.energy_curve[i];
        if v > peak_val {
            peak_val = v;
            peak_idx = i;
        }
    }
    let peak_secs = (peak_idx as f32) * dt;
    SuitabilityVerdict::Suitable {
        peak_secs,
        loud_start,
        loud_end,
    }
}

/// One sub-clip inside a burst.
#[derive(Debug, Clone)]
pub struct BurstSubClip {
    pub source_id: String,
    pub chapter_path: Utf8PathBuf,
    pub chapter_index: u32,
    pub in_chapter_start_secs: f64,
    pub duration_secs: f64,
    pub virtual_start_secs: f64,
    pub score: f32,
}

/// Result of `plan_burst`.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `verdict` is kept for future explain-output / manifest fields
pub struct BurstPlan {
    /// Index into `segments[]` where the burst replaces existing picks.
    pub replace_at: usize,
    /// How many existing segments are removed at `replace_at`.
    pub replace_count: usize,
    /// New burst sub-clips to insert.
    pub sub_clips: Vec<BurstSubClip>,
    /// For logs / explain output.
    pub verdict: SuitabilityVerdict,
    /// Output time (in the rendered timeline) where the burst starts.
    pub output_start_secs: f32,
    /// Per-clip beat duration after applying the every-other-beat rule.
    pub beat_secs: f32,
    /// Number of beats per burst cut: 1 (normal) or 2 (every-other for fast tempos).
    pub beats_per_cut: u32,
}

/// Plan a single beat-burst at the soundtrack's energy peak.
///
/// `output_starts[i]` and `output_durs[i]` are the existing main-picker
/// segments' offsets in the OUTPUT timeline, accounting for intro and xfade
/// overlaps. Both are parallel to `segments`.
///
/// Returns `None` if the soundtrack isn't suitable, the burst won't fit, or
/// no burst clips can be drawn from the score curves.
pub fn plan_burst(
    audio: &AnalyzedAudio,
    snd: &Soundtrack,
    segments: &[Segment],
    output_starts: &[f32],
    output_durs: &[f32],
    sources: &[Source],
    curves: &[ScoreCurve],
    intro_dur_secs: f32,
    total_output_secs: f32,
) -> Option<BurstPlan> {
    if matches!(snd.beat_burst, BeatBurstMode::Off) {
        return None;
    }
    let verdict = check_suitability(audio);
    let force_on = matches!(snd.beat_burst, BeatBurstMode::On);
    if !verdict.is_suitable() && !force_on {
        return None;
    }

    let (peak_secs, loud_start, loud_end) = match verdict {
        SuitabilityVerdict::Suitable {
            peak_secs,
            loud_start,
            loud_end,
        } => (peak_secs, loud_start, loud_end),
        _ => {
            // force_on path: fall back to the middle of the track so the burst still lands somewhere musical.
            (
                audio.duration_secs * 0.5,
                0.0,
                audio.duration_secs.max(6.0),
            )
        }
    };

    // The soundtrack plays under the OUTPUT timeline starting at output_t = 0.
    // We additionally clamp the burst to the middle ~half of the output so it
    // never lands too close to the start (visually disorienting before the
    // viewer settles in) or too close to the end (collides with the outro and
    // truncates the burst's effect). [25%, 70%] is a good compromise: well
    // past the intro, well before the closing tail, and biased slightly past
    // the midpoint so the burst feels like a "build to climax" moment.
    let max_t = total_output_secs.max(1.0);
    let placement_lo = (max_t * BURST_PLACEMENT_LO_FRACTION).max(intro_dur_secs);
    let placement_hi = (max_t * BURST_PLACEMENT_HI_FRACTION).max(placement_lo + 1.0);
    let loud_end = loud_end.min(placement_hi);
    let loud_start = loud_start.max(placement_lo);
    let mut peak_secs = peak_secs.clamp(placement_lo, placement_hi);
    if peak_secs < loud_start {
        // Loud region got clipped past the peak; aim near the middle of what's audible.
        peak_secs = ((loud_start + loud_end) * 0.5).max(placement_lo);
    }

    let beat_interval = audio.beat_interval_secs();
    if beat_interval <= 0.0 || audio.beats.is_empty() {
        return None;
    }

    // Every-other-beat for fast tempos so the cut rate stays under ~3 Hz.
    let beats_per_cut: u32 = if audio.tempo_bpm > BURST_EVERY_OTHER_BEAT_THRESHOLD_BPM { 2 } else { 1 };
    let beat_secs = beat_interval * beats_per_cut as f32;

    // How many beats fit in the loud region?
    let loud_len = (loud_end - loud_start).max(0.0);
    let max_beats_by_loud = (loud_len / beat_secs).floor() as u32;
    let mut n_beats = max_beats_by_loud.min(snd.beat_burst_max_beats);
    if n_beats < snd.beat_burst_min_beats {
        if force_on {
            n_beats = snd.beat_burst_min_beats;
        } else {
            return None;
        }
    }

    // The soundtrack starts at output_t = 0 (it plays under the entire output
    // including the intro card). So peak_secs in the soundtrack === peak_secs
    // in the output timeline. Snap to the next beat ≥ peak_secs.
    let mut burst_output_start = audio.beat_at_or_after(peak_secs);
    // Ensure the burst doesn't fall outside the output. If we don't know total
    // output duration here, we just ensure it doesn't start before the intro
    // ends (so it lands on real footage, not on the intro card).
    if burst_output_start < intro_dur_secs {
        burst_output_start = audio.beat_at_or_after(intro_dur_secs);
    }
    let burst_secs = beat_secs * n_beats as f32;
    let burst_output_end = burst_output_start + burst_secs;

    // Find the segment slice that ACTUALLY overlaps the burst output window.
    // A segment [s, s+d] overlaps the burst window [bs, be] iff s+d > bs && s < be.
    if segments.is_empty() {
        return None;
    }
    let mut first_overlap: Option<usize> = None;
    let mut last_overlap: Option<usize> = None;
    for i in 0..segments.len() {
        let s = output_starts[i];
        let e = s + output_durs[i];
        if e > burst_output_start && s < burst_output_end {
            if first_overlap.is_none() {
                first_overlap = Some(i);
            }
            last_overlap = Some(i);
        }
    }
    let (replace_at, replace_end) = match (first_overlap, last_overlap) {
        (Some(a), Some(b)) => (a, b + 1),
        _ => {
            // Burst falls past the end of the existing picks (e.g. peak near
            // the end of a long song that exceeds output duration). Snap the
            // burst to a fitting earlier window: the LAST few segments of the
            // output, replacing them. We keep `replace_count` proportional to
            // the burst's total duration so the output length stays close to
            // target.
            let target_replace = ((burst_secs / (output_durs.first().copied().unwrap_or(1.0))).round() as usize).max(1);
            let count = target_replace.min(segments.len());
            (segments.len() - count, segments.len())
        }
    };
    let replace_count = replace_end.saturating_sub(replace_at).max(1);

    // Pick burst sub-clips: the highest-scoring moments NOT already used by
    // main picks, NMS-spaced ≥ 1 s apart in their source's virtual timeline.
    let used: Vec<(String, f64, f64)> = segments
        .iter()
        .map(|s| {
            (
                s.source_id.clone(),
                s.virtual_start_secs,
                s.virtual_start_secs + s.duration_secs,
            )
        })
        .collect();
    let candidates =
        rank_burst_candidates(sources, curves, &used, beat_secs as f64, n_beats as usize * 4);
    if candidates.is_empty() {
        return None;
    }

    let mut sub_clips = Vec::with_capacity(n_beats as usize);
    for k in 0..(n_beats as usize) {
        let cand = &candidates[k % candidates.len()];
        let source = match sources.iter().find(|s| s.id == cand.source_id) {
            Some(s) => s,
            None => continue,
        };
        // Center the beat-clip on the candidate moment.
        let v_start = (cand.center_secs - beat_secs as f64 * 0.5).max(0.0);
        let snapped = match snap_to_chapter(source, v_start, beat_secs as f64) {
            Some(seg) => seg,
            None => continue,
        };
        sub_clips.push(BurstSubClip {
            source_id: snapped.source_id,
            chapter_path: snapped.chapter_path,
            chapter_index: snapped.chapter_index,
            in_chapter_start_secs: snapped.in_chapter_start_secs,
            duration_secs: beat_secs as f64,
            virtual_start_secs: snapped.virtual_start_secs,
            score: cand.score,
        });
    }
    if sub_clips.is_empty() {
        return None;
    }

    Some(BurstPlan {
        replace_at,
        replace_count,
        sub_clips,
        verdict,
        output_start_secs: burst_output_start,
        beat_secs,
        beats_per_cut,
    })
}

#[derive(Debug, Clone)]
struct BurstCandidate {
    source_id: String,
    center_secs: f64,
    score: f32,
}

/// Rank candidate moments across all sources for the burst pool. Excludes
/// moments overlapping the main picker's segments and applies a minimum
/// 1 s spacing per source so we don't draw two adjacent burst cuts from the
/// same instant.
fn rank_burst_candidates(
    sources: &[Source],
    curves: &[ScoreCurve],
    used: &[(String, f64, f64)],
    beat_secs: f64,
    want: usize,
) -> Vec<BurstCandidate> {
    let mut cands: Vec<BurstCandidate> = Vec::new();
    for (src, curve) in sources.iter().zip(curves.iter()) {
        if curve.samples.is_empty() {
            continue;
        }
        let dt = 1.0 / curve.sample_rate as f64;
        // Find local maxima in the curve.
        for i in 1..curve.samples.len() - 1 {
            let v = curve.samples[i];
            if v <= 0.0 {
                continue;
            }
            if v < curve.samples[i - 1] || v < curve.samples[i + 1] {
                continue;
            }
            let t = (i as f64) * dt;
            // Skip if overlapping an already-picked segment in this source.
            let overlaps = used.iter().any(|(uid, us, ue)| {
                uid == &src.id && (t + beat_secs * 0.5) > *us && (t - beat_secs * 0.5) < *ue
            });
            if overlaps {
                continue;
            }
            cands.push(BurstCandidate {
                source_id: src.id.clone(),
                center_secs: t,
                score: v,
            });
        }
    }
    // Sort by score descending.
    cands.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // NMS within each source: enforce minimum spacing.
    let mut kept: Vec<BurstCandidate> = Vec::new();
    for c in cands {
        let collide = kept.iter().any(|k| {
            k.source_id == c.source_id
                && (k.center_secs - c.center_secs).abs() < BURST_NMS_SPACING_SECS
        });
        if !collide {
            kept.push(c);
            if kept.len() >= want {
                break;
            }
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_config::Soundtrack;
    use crate::selection_planner::Chapter;

    fn make_audio(bpm: f32, pulse: f32, dur: f32, energy: Vec<f32>) -> AnalyzedAudio {
        let beat_interval = if bpm > 0.0 { 60.0 / bpm } else { 0.5 };
        let mut beats = Vec::new();
        let mut t = 0.0;
        while t < dur {
            beats.push(t);
            t += beat_interval;
        }
        AnalyzedAudio {
            duration_secs: dur,
            sample_rate: 22050,
            tempo_bpm: bpm,
            pulse_strength: pulse,
            beats,
            energy_curve: energy,
            energy_rate: 4.0,
            beat_grid: None,
        }
    }

    #[test]
    fn suitability_skips_slow_tempo() {
        let energy = vec![1.0; 80]; // 20 s
        let v = check_suitability(&make_audio(60.0, 3.0, 20.0, energy));
        assert!(matches!(v, SuitabilityVerdict::SkippedTempo { .. }));
    }

    #[test]
    fn suitability_skips_weak_pulse() {
        let energy = vec![1.0; 80];
        let v = check_suitability(&make_audio(120.0, 1.0, 20.0, energy));
        assert!(matches!(v, SuitabilityVerdict::SkippedPulseStrength { .. }));
    }

    #[test]
    fn suitability_skips_short_loud_region() {
        // 5s total → loud region < 6s no matter what
        let energy = vec![1.0; 20];
        let v = check_suitability(&make_audio(120.0, 3.0, 5.0, energy));
        assert!(matches!(v, SuitabilityVerdict::SkippedLoudTooShort { .. }));
    }

    #[test]
    fn suitability_passes_for_typical_pop() {
        // 30 s of mostly-loud music with a clear peak around the 60% mark.
        let n = 120;
        let mut energy: Vec<f32> = (0..n)
            .map(|i| {
                let x = i as f32 / n as f32;
                0.6 + 0.4 * (1.0 - (x - 0.6).abs() * 2.0).max(0.0)
            })
            .collect();
        // ramp up the very edges so detect_loud_region trims them
        energy[0] = 0.05;
        energy[1] = 0.08;
        energy[n - 1] = 0.05;
        energy[n - 2] = 0.08;
        let audio = make_audio(120.0, 3.0, 30.0, energy);
        let v = check_suitability(&audio);
        assert!(v.is_suitable(), "expected Suitable, got {:?}", v);
    }

    fn make_source(id: &str, dur: f64) -> Source {
        Source {
            id: id.into(),
            chapters: vec![Chapter {
                path: format!("{id}.MP4").into(),
                chapter_index: 1,
                duration_secs: dur,
                creation_time: None,
                blake3: None,
            }],
            total_duration_secs: dur,
        }
    }

    fn default_soundtrack() -> Soundtrack {
        // Build via toml round-trip to apply defaults.
        let s = r#"
            file = "x"
        "#;
        toml::from_str(s).unwrap()
    }

    #[test]
    fn plan_returns_burst_when_suitable() {
        // 3 sources of 30s each; main picker took 5 segments.
        let sources = vec![
            make_source("A", 30.0),
            make_source("B", 30.0),
            make_source("C", 30.0),
        ];
        // Curves: each source has alternating peaks + valleys → many local maxima.
        let mut curves = Vec::new();
        for _ in 0..3 {
            let mut c = ScoreCurve::zeros(30.0, 4.0);
            for i in 0..c.samples.len() {
                c.samples[i] = if i % 2 == 0 { 0.5 + (i as f32 * 0.01).sin().abs() } else { 0.0 };
            }
            curves.push(c);
        }
        // 5 main segments at output_t 0, 3, 6, 9, 12 (3s each).
        let mut segments = Vec::new();
        for k in 0..5 {
            segments.push(Segment {
                source_id: format!("{}", ['A', 'B', 'C'][k % 3]),
                chapter_path: format!("{}.MP4", ['A', 'B', 'C'][k % 3]).into(),
                chapter_index: 1,
                in_chapter_start_secs: (k * 5) as f64,
                duration_secs: 3.0,
                virtual_start_secs: (k * 5) as f64,
                reason: "test".into(),
                score: None,
            });
        }
        let output_starts: Vec<f32> = (0..5).map(|k| k as f32 * 3.0).collect();
        let output_durs: Vec<f32> = vec![3.0; 5];

        // Audio with peak at t=10s.
        let mut energy = vec![0.5_f32; 80];
        for i in 36..44 {
            energy[i] = 1.0;
        }
        // Trim edges so loud-region detection picks the middle.
        for i in 0..6 {
            energy[i] = 0.05;
            energy[80 - 1 - i] = 0.05;
        }
        let audio = make_audio(120.0, 3.0, 20.0, energy);
        let snd = default_soundtrack();

        let plan = plan_burst(
            &audio, &snd, &segments, &output_starts, &output_durs, &sources, &curves, 0.0, 30.0,
        );
        let plan = plan.expect("expected a burst plan");
        assert_eq!(plan.beats_per_cut, 1);
        assert!(plan.sub_clips.len() >= snd.beat_burst_min_beats as usize);
        assert!(plan.output_start_secs >= 6.0 && plan.output_start_secs <= 16.0);
    }

    #[test]
    fn plan_returns_none_when_off() {
        let mut snd = default_soundtrack();
        snd.beat_burst = BeatBurstMode::Off;
        let audio = make_audio(120.0, 3.0, 20.0, vec![1.0; 80]);
        let plan = plan_burst(
            &audio, &snd, &[], &[], &[], &[], &[], 0.0, 20.0,
        );
        assert!(plan.is_none());
    }
}
