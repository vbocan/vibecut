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
//! Everything happens in *output* time. Song time only equals output time
//! until the soundtrack first loops, so the energy curve is mapped through
//! the mux's soundtrack layout (every loop copy) and the burst starts at the
//! loudest moment that actually plays inside the placement window, snapped
//! to the next beat in output time. Where each pick sits comes from the
//! concat stage's own layout arithmetic (intro card, crossfades, beat
//! lock). Speed-ramp near-cuts are decided after the burst; the beat lock
//! absorbs them, but with `beat_align = false` each ramped seam ahead of
//! the burst moves it later by the crossfade it no longer has. The pick
//! the drop falls in is cut short so the burst starts
//! on it, and the burst replaces the picks it covers so the total runtime
//! stays close to `target_duration`.
//!
//! # Source clips
//!
//! Burst sub-clips come from the *same* score curves the main picker drew
//! from, but explicitly avoid moments already chosen by the main picker.
//! NMS-spaced (≥ 1 s, and ≥ one cut, apart in source time) so no two burst
//! cuts show the same instant. Short of distinct moments, the burst uses
//! fewer, longer cuts, or none — it never repeats a shot.

use camino::Utf8PathBuf;

use crate::audio_analysis::AnalyzedAudio;
use crate::beat_sync::beats_in_output;
use crate::project_config::{BeatBurstMode, Soundtrack};
use crate::selection_planner::{Segment, Source, snap_to_chapter};
use crate::selection_scoring::ScoreCurve;
use crate::video_renderer::SoundtrackSpan;

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
/// the same source, enforced by non-maximum suppression during pool building
/// (raised to the cut length for longer cuts, so windows never overlap).
const BURST_NMS_SPACING_SECS: f64 = 1.0;

/// Shortest stretch of the segment the drop falls in worth keeping ahead of
/// the burst; with less, the burst starts at that segment's own cut.
const BURST_MIN_LEAD_IN_SECS: f64 = 1.0;

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
    /// How many existing segments are removed at `replace_at` (may be 0:
    /// the burst is then inserted between two picks).
    pub replace_count: usize,
    /// Seconds to cut from the end of `segments[replace_at - 1]` so the
    /// burst starts on the drop instead of at the next existing cut.
    pub trim_before_secs: Option<f64>,
    /// New burst sub-clips to insert. Never two of the same moment.
    pub sub_clips: Vec<BurstSubClip>,
    /// For logs / explain output.
    pub verdict: SuitabilityVerdict,
    /// Output time (in the rendered timeline) where the burst starts.
    pub output_start_secs: f32,
    /// Length of each burst cut (`beats_per_cut` beats).
    pub beat_secs: f32,
    /// Beats per burst cut: 1 normally, 2 for fast tempos or when there
    /// aren't enough distinct moments for one-beat cuts.
    pub beats_per_cut: u32,
}

/// Where the planned cut and the soundtrack sit in the output, for
/// [`plan_burst`].
#[derive(Debug, Clone, Copy)]
pub struct BurstContext<'a> {
    /// Visible span `(cut_in, cut_out)` of each segment in output seconds,
    /// parallel to `segments` — predicted with the concat stage's own
    /// arithmetic (`output_timeline::predict_layout`).
    pub clip_spans: &'a [(f64, f64)],
    /// Where each copy of the soundtrack plays in the output (the mux
    /// layout, `SoundtrackLoopPlan::spans`): song time is not output time
    /// once the soundtrack loops.
    pub soundtrack: &'a [SoundtrackSpan],
    /// Output time the footage starts (after an intro card) and ends.
    pub body_start: f64,
    pub body_end: f64,
    /// Length of the whole output.
    pub total: f64,
}

/// Plan a single beat-burst at the soundtrack's energy peak *as it plays in
/// the output*: the loudest moment of the song inside the placement window,
/// looking through every loop copy, snapped to the next beat in output
/// time. The segment that moment falls in is cut short so the burst starts
/// right there (or, when that would leave a stub, the burst starts at the
/// segment's own cut), and the burst replaces the picks it covers.
///
/// Returns `None` if the soundtrack isn't suitable, the burst won't fit, or
/// there aren't enough distinct moments to fill it: when one-beat cuts would
/// need more moments than the footage has, the burst uses fewer, two-beat
/// cuts, and skips altogether rather than repeat a shot.
pub fn plan_burst(
    audio: &AnalyzedAudio,
    snd: &Soundtrack,
    segments: &[Segment],
    ctx: &BurstContext<'_>,
    sources: &[Source],
    curves: &[ScoreCurve],
) -> Option<BurstPlan> {
    if matches!(snd.beat_burst, BeatBurstMode::Off) {
        return None;
    }
    let verdict = check_suitability(audio);
    let force_on = matches!(snd.beat_burst, BeatBurstMode::On);
    if !verdict.is_suitable() && !force_on {
        return None;
    }
    if segments.is_empty() || ctx.clip_spans.len() != segments.len() || ctx.soundtrack.is_empty() {
        return None;
    }
    // Loud region of the song (song time). Forced on an unsuitable track:
    // the whole song counts.
    let (loud_start, loud_end) = match verdict {
        SuitabilityVerdict::Suitable { loud_start, loud_end, .. } => (loud_start, loud_end),
        _ => (0.0, audio.duration_secs),
    };
    let beat_interval = audio.beat_interval_secs() as f64;
    if beat_interval <= 0.0 || audio.beats.is_empty() {
        return None;
    }

    // Placement window, in output time. [25 %, 70 %] keeps the burst away
    // from the very start (the viewer hasn't settled in) and from the
    // closing tail, and never before the footage starts.
    let max_t = ctx.total.max(1.0) as f32;
    let placement_lo = (max_t * BURST_PLACEMENT_LO_FRACTION).max(ctx.body_start as f32);
    let placement_hi = (max_t * BURST_PLACEMENT_HI_FRACTION).max(placement_lo + 1.0);
    let loud = Some((loud_start, loud_end));
    let peak = output_energy_peak(audio, ctx.soundtrack, placement_lo, placement_hi, loud)
        .or_else(|| output_energy_peak(audio, ctx.soundtrack, placement_lo, placement_hi, None))?;
    let beats = beats_in_output(&audio.beats, ctx.soundtrack);
    let peak_beat = *beats.get(beats.partition_point(|&b| b < peak - 1e-4))? as f64;

    // Start the burst on the drop: keep the segment it falls in, cut short,
    // unless that leaves a stub — then the burst starts at that segment's
    // own (beat-locked) cut instead.
    let i0 = ctx.clip_spans.iter().position(|&(a, b)| peak_beat >= a && peak_beat < b)?;
    let (cut_in, cut_out) = ctx.clip_spans[i0];
    let (replace_at, trim_before_secs, start) = if peak_beat - cut_in >= BURST_MIN_LEAD_IN_SECS {
        let trim = cut_out - peak_beat;
        (i0 + 1, (trim > 1e-3).then_some(trim), peak_beat)
    } else {
        (i0, None, cut_in)
    };

    // How many beats fit: while the music stays loud (song time inside the
    // loud region, whichever copy is playing) and the footage lasts.
    let loud_at = |o: f64| {
        song_time_at(ctx.soundtrack, o as f32).is_some_and(|t| t >= loud_start && t <= loud_end)
    };
    let room = ((ctx.body_end - start) / beat_interval + 1e-6).floor().max(0.0) as u32;
    let mut n_beats = 0u32;
    while n_beats < room.min(snd.beat_burst_max_beats)
        && loud_at(start + n_beats as f64 * beat_interval)
    {
        n_beats += 1;
    }
    let min_beats = snd.beat_burst_min_beats.max(2);
    if n_beats < min_beats {
        if !force_on || room < min_beats {
            return None;
        }
        n_beats = min_beats;
    }

    // Burst sub-clips: the highest-scoring moments NOT already used by main
    // picks, never the same moment twice. One-beat cuts (two above 140 BPM,
    // so the cut rate stays under ~3 Hz); with too few distinct moments for
    // those, cuts twice as long — and no burst at all if even that falls
    // short of `beat_burst_min_beats`.
    let used: Vec<(String, f64, f64)> = segments
        .iter()
        .map(|s| (s.source_id.clone(), s.virtual_start_secs, s.virtual_start_secs + s.duration_secs))
        .collect();
    let base: u32 = if audio.tempo_bpm > BURST_EVERY_OTHER_BEAT_THRESHOLD_BPM { 2 } else { 1 };
    let (beats_per_cut, sub_clips) = [base, base * 2].into_iter().find_map(|per| {
        let wanted = (n_beats / per) as usize;
        if wanted < 2 {
            return None;
        }
        let cut_secs = beat_interval * per as f64;
        let spacing = BURST_NMS_SPACING_SECS.max(cut_secs);
        let candidates = rank_burst_candidates(sources, curves, &used, cut_secs, spacing, wanted * 4);
        let clips = distinct_sub_clips(&candidates, sources, &used, cut_secs, wanted);
        (clips.len() >= 2 && clips.len() as u32 * per >= min_beats).then_some((per, clips))
    })?;
    let beat_secs = (beat_interval * beats_per_cut as f64) as f32;

    // Replace the picks the burst covers: those whose middle falls before
    // the burst ends, so the runtime stays close to the plan.
    let burst_end = start + sub_clips.len() as f64 * beat_secs as f64;
    let mut replace_end = replace_at;
    while replace_end < segments.len() {
        let (a, b) = ctx.clip_spans[replace_end];
        if (a + b) * 0.5 >= burst_end {
            break;
        }
        replace_end += 1;
    }

    Some(BurstPlan {
        replace_at,
        replace_count: replace_end - replace_at,
        trim_before_secs,
        sub_clips,
        verdict,
        output_start_secs: start as f32,
        beat_secs,
        beats_per_cut,
    })
}

/// Song time playing at output time `o`, if the soundtrack plays there.
fn song_time_at(spans: &[SoundtrackSpan], o: f32) -> Option<f32> {
    spans
        .iter()
        .find(|s| o >= s.output_start && o < s.output_end)
        .map(|s| s.source_start + (o - s.output_start))
}

/// Output time of the soundtrack's loudest moment within `[lo, hi]`,
/// looking at every copy the mux lays down. `loud` restricts it to song
/// times inside the loud region. The earliest of equally loud moments wins.
fn output_energy_peak(
    audio: &AnalyzedAudio,
    spans: &[SoundtrackSpan],
    lo: f32,
    hi: f32,
    loud: Option<(f32, f32)>,
) -> Option<f32> {
    let dt = 1.0 / audio.energy_rate.max(1.0);
    let mut best: Option<(f32, f32)> = None; // (energy, output time)
    for span in spans {
        for (i, &e) in audio.energy_curve.iter().enumerate() {
            let t = i as f32 * dt;
            if loud.is_some_and(|(a, b)| t < a || t > b) {
                continue;
            }
            let o = span.output_start + (t - span.source_start);
            if o < span.output_start || o >= span.output_end || o < lo || o > hi {
                continue;
            }
            if best.map_or(true, |(be, bo)| e > be || (e == be && o < bo)) {
                best = Some((e, o));
            }
        }
    }
    best.map(|(_, o)| o)
}

/// Snap ranked candidates to clips of `cut_secs`, keeping only windows that
/// overlap neither each other nor a main pick (in virtual source time): a
/// window slid back to fit its chapter can land on a moment already shown.
fn distinct_sub_clips(
    candidates: &[BurstCandidate],
    sources: &[Source],
    used: &[(String, f64, f64)],
    cut_secs: f64,
    want: usize,
) -> Vec<BurstSubClip> {
    let mut out: Vec<BurstSubClip> = Vec::with_capacity(want);
    for cand in candidates {
        if out.len() >= want {
            break;
        }
        let Some(source) = sources.iter().find(|s| s.id == cand.source_id) else {
            continue;
        };
        let v_start = (cand.center_secs - cut_secs * 0.5).max(0.0);
        let Some(snapped) = snap_to_chapter(source, v_start, cut_secs) else {
            continue;
        };
        let (a, b) = (snapped.virtual_start_secs, snapped.virtual_start_secs + cut_secs);
        let overlaps_burst = out.iter().any(|c| {
            c.source_id == snapped.source_id
                && a < c.virtual_start_secs + c.duration_secs
                && c.virtual_start_secs < b
        });
        let overlaps_pick =
            used.iter().any(|(id, us, ue)| *id == snapped.source_id && a < *ue && *us < b);
        if overlaps_burst || overlaps_pick {
            continue;
        }
        out.push(BurstSubClip {
            source_id: snapped.source_id,
            chapter_path: snapped.chapter_path,
            chapter_index: snapped.chapter_index,
            in_chapter_start_secs: snapped.in_chapter_start_secs,
            duration_secs: cut_secs,
            virtual_start_secs: snapped.virtual_start_secs,
            score: cand.score,
        });
    }
    out
}

#[derive(Debug, Clone)]
struct BurstCandidate {
    source_id: String,
    center_secs: f64,
    score: f32,
}

/// Rank candidate moments across all sources for the burst pool. Excludes
/// moments overlapping the main picker's segments and keeps candidates from
/// the same source at least `spacing` apart, so no two burst cuts show the
/// same instant.
fn rank_burst_candidates(
    sources: &[Source],
    curves: &[ScoreCurve],
    used: &[(String, f64, f64)],
    beat_secs: f64,
    spacing: f64,
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
                && (k.center_secs - c.center_secs).abs() < spacing
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
                color: None,
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

    /// Peaks every other sample (every 0.5 s at 4 Hz) — plenty of
    /// distinct burst candidates.
    fn busy_curve(dur: f32) -> ScoreCurve {
        let mut c = ScoreCurve::zeros(dur, 4.0);
        for i in 0..c.samples.len() {
            c.samples[i] = if i % 2 == 0 { 0.5 + (i as f32 * 0.01).sin().abs() } else { 0.0 };
        }
        c
    }

    /// A curve with peaks only at `peaks` (seconds).
    fn sparse_curve(dur: f32, peaks: &[f32]) -> ScoreCurve {
        let mut c = ScoreCurve::zeros(dur, 4.0);
        for &p in peaks {
            let i = (p * 4.0) as usize;
            c.samples[i] = 1.0;
        }
        c
    }

    /// 120 BPM song with quiet edges and a loud stretch `[peak, peak + 2)`.
    fn song_with_peak(dur: f32, peak: f32) -> AnalyzedAudio {
        let n = (dur * 4.0) as usize;
        let mut energy = vec![0.5_f32; n];
        for (i, e) in energy.iter_mut().enumerate() {
            let t = i as f32 / 4.0;
            if t >= peak && t < peak + 2.0 {
                *e = 1.0;
            }
        }
        for i in 0..6 {
            energy[i] = 0.05;
            energy[n - 1 - i] = 0.05;
        }
        make_audio(120.0, 3.0, dur, energy)
    }

    /// `n` main picks of `len` seconds back to back from `source`.
    fn picks(n: usize, len: f64, source: &str) -> (Vec<Segment>, Vec<(f64, f64)>) {
        let segs = (0..n)
            .map(|k| Segment {
                source_id: source.into(),
                chapter_path: format!("{source}.MP4").into(),
                chapter_index: 1,
                in_chapter_start_secs: k as f64 * len,
                duration_secs: len,
                virtual_start_secs: k as f64 * len,
                reason: "test".into(),
                score: None,
                signals: Vec::new(),
            })
            .collect();
        let spans = (0..n).map(|k| (k as f64 * len, (k + 1) as f64 * len)).collect();
        (segs, spans)
    }

    fn whole_song(len: f32) -> Vec<SoundtrackSpan> {
        vec![SoundtrackSpan { output_start: 0.0, output_end: len, source_start: 0.0 }]
    }

    fn ctx<'a>(spans: &'a [(f64, f64)], soundtrack: &'a [SoundtrackSpan]) -> BurstContext<'a> {
        let end = spans.last().map_or(0.0, |s| s.1);
        BurstContext { clip_spans: spans, soundtrack, body_start: 0.0, body_end: end, total: end }
    }

    fn assert_distinct(plan: &BurstPlan) {
        for (i, a) in plan.sub_clips.iter().enumerate() {
            for b in &plan.sub_clips[i + 1..] {
                let overlap = a.source_id == b.source_id
                    && a.virtual_start_secs < b.virtual_start_secs + b.duration_secs
                    && b.virtual_start_secs < a.virtual_start_secs + a.duration_secs;
                assert!(!overlap, "repeated shot: {a:?} / {b:?}");
            }
        }
    }

    #[test]
    fn plan_returns_burst_when_suitable() {
        let sources = vec![make_source("A", 30.0), make_source("B", 30.0), make_source("C", 30.0)];
        let curves = vec![busy_curve(30.0), busy_curve(30.0), busy_curve(30.0)];
        // 5 main picks at output 0, 3, 6, 9, 12 (3 s each).
        let (segments, spans) = picks(5, 3.0, "A");
        // Loudest 9–11 s; the song plays once, from output 0.
        let audio = song_with_peak(20.0, 9.0);
        let snd = default_soundtrack();
        let st = whole_song(20.0);
        let c = BurstContext { total: 30.0, ..ctx(&spans, &st) };
        let plan = plan_burst(&audio, &snd, &segments, &c, &sources, &curves)
            .expect("expected a burst plan");
        assert_eq!(plan.beats_per_cut, 1);
        assert!(plan.sub_clips.len() >= snd.beat_burst_min_beats as usize);
        // 9.0 s is on a cut: the burst starts there, replacing what it covers.
        assert!((plan.output_start_secs - 9.0).abs() < 1e-4, "{}", plan.output_start_secs);
        assert_eq!((plan.replace_at, plan.trim_before_secs), (3, None));
        assert_distinct(&plan);
    }

    #[test]
    fn burst_cuts_short_the_pick_the_drop_lands_in() {
        let sources = vec![make_source("A", 60.0), make_source("B", 60.0)];
        let curves = vec![busy_curve(60.0), busy_curve(60.0)];
        let (segments, spans) = picks(10, 3.0, "A");
        // Drop at 10 s: 1 s into the pick spanning 9–12 s.
        let audio = song_with_peak(40.0, 10.0);
        let snd = default_soundtrack();
        let st = whole_song(40.0);
        let plan = plan_burst(&audio, &snd, &segments, &ctx(&spans, &st), &sources, &curves).unwrap();
        assert!((plan.output_start_secs - 10.0).abs() < 1e-4, "{}", plan.output_start_secs);
        // That pick stays, 2 s shorter, so the burst starts on the drop.
        assert_eq!(plan.replace_at, 4);
        assert!((plan.trim_before_secs.unwrap() - 2.0).abs() < 1e-6);
        // 12 one-beat cuts end at 16 s: the picks whose middle comes first
        // go (12–15 s); the 15–18 s pick stays and follows the burst.
        assert_eq!(plan.replace_count, 1);
    }

    #[test]
    fn burst_finds_the_drop_in_a_loop_copy() {
        // A 20 s song under a 60 s cut: the natural intro plays to 18 s, then
        // the loud region [4, 18) loops. The drop (song 12 s) first plays at
        // output 12 s — before the placement window (15 s) — and next in loop
        // copy 1, which starts at 16 s and plays song 12 s at output 24 s.
        // Treating song time as output time would put the burst at 15 s.
        let plan_loop = crate::video_renderer::SoundtrackLoopPlan {
            first_play_end_secs: 18.0,
            loop_start_secs: 4.0,
            loop_secs: 14.0,
            beat_interval_secs: 0.5,
        };
        let st = plan_loop.spans(60.0, 20.0);
        let audio = song_with_peak(20.0, 12.0);
        let sources = vec![make_source("A", 90.0), make_source("B", 60.0)];
        let curves = vec![busy_curve(90.0), busy_curve(60.0)];
        let (segments, spans) = picks(20, 3.0, "A");
        let snd = default_soundtrack();
        let plan = plan_burst(&audio, &snd, &segments, &ctx(&spans, &st), &sources, &curves).unwrap();
        assert!((plan.output_start_secs - 24.0).abs() < 1e-3, "{}", plan.output_start_secs);
        assert_eq!(plan.replace_at, 8);
    }

    #[test]
    fn burst_starts_after_the_intro_card() {
        // Footage starts at 8 s (intro card); the song is loudest at 6 s,
        // which plays over the card.
        let sources = vec![make_source("A", 60.0), make_source("B", 60.0)];
        let curves = vec![busy_curve(60.0), busy_curve(60.0)];
        let (segments, mut spans) = picks(8, 3.0, "A");
        for s in &mut spans {
            *s = (s.0 + 8.0, s.1 + 8.0);
        }
        let audio = song_with_peak(40.0, 6.0);
        let snd = default_soundtrack();
        let st = whole_song(40.0);
        let c = BurstContext { body_start: 8.0, total: 32.0, ..ctx(&spans, &st) };
        let plan = plan_burst(&audio, &snd, &segments, &c, &sources, &curves).unwrap();
        assert!(plan.output_start_secs >= 8.0, "{}", plan.output_start_secs);
    }

    #[test]
    fn short_of_moments_the_burst_uses_longer_cuts_not_repeats() {
        // Only five distinct moments exist outside the main picks: twelve
        // one-beat cuts would repeat shots, so the burst takes five two-beat
        // cuts (ten beats, above the eight-beat minimum).
        let sources = vec![make_source("A", 60.0), make_source("D", 60.0)];
        let curves = vec![
            ScoreCurve::zeros(60.0, 4.0),
            sparse_curve(60.0, &[5.0, 15.0, 25.0, 35.0, 45.0]),
        ];
        let (segments, spans) = picks(10, 3.0, "A");
        let audio = song_with_peak(40.0, 10.0);
        let snd = default_soundtrack();
        let st = whole_song(40.0);
        let plan = plan_burst(&audio, &snd, &segments, &ctx(&spans, &st), &sources, &curves).unwrap();
        assert_eq!(plan.beats_per_cut, 2);
        assert_eq!(plan.sub_clips.len(), 5);
        assert!((plan.beat_secs - 1.0).abs() < 1e-6);
        assert_distinct(&plan);
    }

    #[test]
    fn too_few_moments_skip_the_burst() {
        // Three moments: even two-beat cuts make only six beats.
        let sources = vec![make_source("A", 60.0), make_source("D", 60.0)];
        let curves = vec![ScoreCurve::zeros(60.0, 4.0), sparse_curve(60.0, &[5.0, 25.0, 45.0])];
        let (segments, spans) = picks(10, 3.0, "A");
        let audio = song_with_peak(40.0, 10.0);
        let st = whole_song(40.0);
        for mode in [BeatBurstMode::Auto, BeatBurstMode::On] {
            let mut snd = default_soundtrack();
            snd.beat_burst = mode;
            let plan = plan_burst(&audio, &snd, &segments, &ctx(&spans, &st), &sources, &curves);
            assert!(plan.is_none(), "{mode:?}: {plan:?}");
        }
    }

    #[test]
    fn plan_returns_none_when_off() {
        let mut snd = default_soundtrack();
        snd.beat_burst = BeatBurstMode::Off;
        let audio = make_audio(120.0, 3.0, 20.0, vec![1.0; 80]);
        let st = whole_song(20.0);
        let plan = plan_burst(&audio, &snd, &[], &ctx(&[], &st), &[], &[]);
        assert!(plan.is_none());
    }
}
