//! Post-render beat-sync check: did the cuts land on the music's beats?
//!
//! Three things decide whether a cut in the rendered file hits a beat, and
//! none of them is visible from the planner's clip lengths alone:
//!
//! 1. **Where the cut really is.** Each crossfade overlaps its two clips,
//!    ramped seams collapse to near-cuts, the beat-burst is folded into one
//!    input with its own internal hard cuts, and an intro card shifts
//!    everything after it. [`planned_cuts`] lays the chain out with the
//!    concat stage's own frame-exact arithmetic (`output_timeline`).
//! 2. **Where each beat really plays.** The smart loop chains copies of the
//!    soundtrack with ~2 s crossfades, which shifts the beat grid at every
//!    loop seam. [`beats_in_output`] maps the soundtrack's beats through
//!    [`SoundtrackLoopPlan::spans`], the same layout the mux stage builds.
//! 3. **Whether the encoded audio is offset from the picture.** Encoders and
//!    muxers can shift the whole audio track (AAC encoder priming is ~1,000–
//!    2,100 samples, about 1.3 frames at 30 fps, when the container doesn't
//!    cancel it). [`measure_audio_offset`] cross-correlates a stretch of the
//!    rendered audio against the soundtrack to measure that directly.
//!
//! [`score_cuts`] combines the three into a [`BeatSyncReport`]: per-cut
//! error to the nearest beat, in milliseconds and frames, against a
//! ±3-frame tolerance (about where an offset starts to read as "off the
//! beat"). The report also carries the error that random cut placement
//! would average (a quarter beat), so "not beat-locked at all" is easy to
//! tell apart from "slightly late".

use anyhow::Result;
use camino::Utf8Path;
use serde::{Deserialize, Serialize};

use crate::audio_analysis::{cross_correlate, decode_pcm_range, XCorrResult};
use crate::video_renderer::SoundtrackSpan;

/// Cuts within this many frames of a beat count as on the beat.
pub const SYNC_TOLERANCE_FRAMES: f32 = 3.0;

/// Sample rate for the offset measurement. 8 kHz keeps an 8 s window cheap
/// and, with sub-sample interpolation, resolves well under a millisecond.
const OFFSET_SAMPLE_RATE: u32 = 8_000;
/// Longest window the offset measurement uses.
const OFFSET_WINDOW_SECS: f32 = 8.0;
/// Shortest window worth measuring.
const OFFSET_MIN_WINDOW_SECS: f32 = 2.0;
/// Largest offset searched for. Real encoder/muxer offsets are tens of ms.
const OFFSET_MAX_LAG_SECS: f32 = 0.25;
/// Offsets measured with less confidence are reported but not applied.
pub const OFFSET_MIN_CONFIDENCE: f32 = 0.5;

/// What sits on each side of a cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub enum CutKind {
    /// Intro card → first clip.
    IntroToBody,
    /// Clip → clip.
    Body,
    /// Inside the beat-burst (hard cuts between burst sub-clips).
    BurstInternal,
    /// Last clip → outro card.
    BodyToOutro,
}

/// A cut in the rendered output, as the concat stage builds it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlannedCut {
    /// Output time of the cut. For a crossfade, its midpoint.
    pub output_secs: f32,
    pub kind: CutKind,
}

/// Cuts of a concat chain. `input_durs[k]` is the on-disk duration of
/// concat input `k`; `seam_durs[k]` is the crossfade between inputs `k` and
/// `k + 1` as the renderer applies it (0 for hard concat). `burst` is
/// `(input_index, sub_clip_durations)` for a folded beat-burst input.
///
/// Laid out by [`ChainLayout`](crate::output_timeline::ChainLayout), the
/// same frame-exact arithmetic the concat stage takes its xfade offsets
/// from: input `k` starts at the running output length minus its incoming
/// crossfade, and the visible cut sits in the middle of that crossfade.
pub fn planned_cuts(
    input_durs: &[f32],
    seam_durs: &[f32],
    has_intro: bool,
    has_outro: bool,
    burst: Option<(usize, &[f32])>,
    fps: u32,
) -> Vec<PlannedCut> {
    use crate::output_timeline::{chain_cuts, ChainLayout};
    let durs: Vec<f64> = input_durs.iter().map(|&d| d as f64).collect();
    let seams: Vec<f64> = seam_durs.iter().map(|&d| d as f64).collect();
    let subs: Option<(usize, Vec<f64>)> =
        burst.map(|(i, d)| (i, d.iter().map(|&x| x as f64).collect()));
    let layout = ChainLayout::new(&durs, &seams, fps);
    chain_cuts(&layout, has_intro, has_outro, subs.as_ref().map(|(i, d)| (*i, d.as_slice())))
}

/// Map soundtrack beat times into output time through the mux layout.
/// Beats a span doesn't play (outside its stretch of the soundtrack) are
/// dropped; the result is sorted.
pub fn beats_in_output(beats: &[f32], spans: &[SoundtrackSpan]) -> Vec<f32> {
    let mut out = Vec::new();
    for span in spans {
        let len = span.output_end - span.output_start;
        for &b in beats {
            let rel = b - span.source_start;
            if (0.0..len).contains(&rel) {
                out.push(span.output_start + rel);
            }
        }
    }
    out.sort_by(f32::total_cmp);
    out
}

/// A stretch of output where only the soundtrack plays, for measuring the
/// audio offset: `(output_start, source_start, len)`. Prefers the earliest
/// window inside the first soundtrack span that overlaps no duck window
/// (where source audio is mixed in); otherwise the least-overlapping one.
/// `duck_windows` are `(start, end, ramp)`; `usable_end` excludes the
/// soundtrack fade-out at the end.
pub fn pick_reference_window(
    spans: &[SoundtrackSpan],
    duck_windows: &[(f32, f32, f32)],
    usable_end: f32,
) -> Option<(f32, f32, f32)> {
    let span = spans.first()?;
    let lo = span.output_start + 0.5;
    let hi = span.output_end.min(usable_end) - 0.5;
    let len = (hi - lo).min(OFFSET_WINDOW_SECS);
    if len < OFFSET_MIN_WINDOW_SECS {
        return None;
    }
    let overlap = |s: f32| -> f32 {
        let e = s + len;
        duck_windows
            .iter()
            .map(|&(ws, we, r)| ((e.min(we + r)) - s.max(ws - r)).max(0.0))
            .sum()
    };
    let mut best: Option<(f32, f32)> = None; // (start, overlap)
    let mut s = lo;
    while s + len <= hi + 1e-3 {
        let o = overlap(s);
        if o <= 0.0 {
            best = Some((s, 0.0));
            break;
        }
        if best.map_or(true, |(_, bo)| o < bo) {
            best = Some((s, o));
        }
        s += 0.5;
    }
    let (start, _) = best?;
    Some((start, span.source_start + (start - span.output_start), len))
}

/// Measure how far the soundtrack in `rendered` sits from where the mux
/// put it: cross-correlates `len` seconds of the rendered audio at
/// `output_start` against the soundtrack at `source_start`. A positive
/// `offset_secs` means the music plays late relative to the picture.
pub fn measure_audio_offset(
    rendered: &Utf8Path,
    soundtrack: &Utf8Path,
    output_start: f32,
    source_start: f32,
    len: f32,
) -> Result<XCorrResult> {
    let sr = OFFSET_SAMPLE_RATE;
    let a = decode_pcm_range(rendered, sr, Some(output_start), Some(len))?;
    let b = decode_pcm_range(soundtrack, sr, Some(source_start), Some(len))?;
    anyhow::ensure!(!a.is_empty() && !b.is_empty(), "decoded audio window is empty");
    Ok(cross_correlate(&a, &b, sr, OFFSET_MAX_LAG_SECS))
}

/// How one cut sits against the nearest beat.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct CutSync {
    /// Output time of the cut (crossfade midpoint).
    pub output_secs: f32,
    pub kind: CutKind,
    /// Output time of the nearest beat, including the measured audio offset.
    pub beat_secs: f32,
    /// `output_secs − beat_secs`: positive = the cut comes after the beat.
    pub error_ms: f32,
    pub error_frames: f32,
}

/// Result of the post-render beat-sync check. Written to the manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct BeatSyncReport {
    pub tempo_bpm: f32,
    pub beat_interval_secs: f32,
    pub fps: u32,
    pub tolerance_frames: f32,
    /// Measured delay of the soundtrack in the rendered file (seconds,
    /// positive = late). `None` when it couldn't be measured.
    pub audio_offset_secs: Option<f32>,
    /// Normalized correlation of that measurement, 0..1.
    pub audio_offset_confidence: Option<f32>,
    pub n_cuts: u32,
    /// Cuts within ±`tolerance_frames` of a beat.
    pub within_tolerance: u32,
    pub mean_abs_error_ms: f32,
    pub max_abs_error_ms: f32,
    /// Mean signed error. Far from zero while the spread is small means a
    /// constant shift (audio offset), not a timing pattern problem.
    pub mean_signed_error_ms: f32,
    /// Mean |error| if cuts fell at random relative to the beat: a quarter
    /// beat interval. Beat-locked cuts sit far below it.
    pub chance_mean_abs_error_ms: f32,
    pub cuts: Vec<CutSync>,
}

impl BeatSyncReport {
    /// One-phrase reading of the numbers, for logs.
    pub fn verdict(&self) -> &'static str {
        if self.n_cuts == 0 {
            return "no cuts to check";
        }
        // Random placement already lands within ±3 frames of a beat 40 % of
        // the time at 120 BPM (±100 ms of a 500 ms beat), so the share alone
        // can't tell "mostly locked" from "not locked": check the mean
        // error against chance first.
        let on = self.within_tolerance as f32 / self.n_cuts as f32;
        if on >= 0.9 {
            "on the beat"
        } else if self.mean_abs_error_ms >= 0.75 * self.chance_mean_abs_error_ms {
            "not beat-locked (no better than random cut placement)"
        } else if on >= 0.6 {
            "mostly on the beat"
        } else {
            "loosely beat-locked"
        }
    }
}

/// Score every cut against the nearest beat. `beats` are output-time beats
/// from [`beats_in_output`]; `audio_offset_secs` (measured) is added to
/// them. Returns `None` when there are no beats to compare against.
pub fn score_cuts(
    cuts: &[PlannedCut],
    beats: &[f32],
    audio_offset_secs: f32,
    beat_interval_secs: f32,
    fps: u32,
) -> Option<BeatSyncReport> {
    if beats.is_empty() || beat_interval_secs <= 0.0 {
        return None;
    }
    let frame_ms = 1000.0 / fps.max(1) as f32;
    let tolerance_ms = SYNC_TOLERANCE_FRAMES * frame_ms;
    let scored: Vec<CutSync> = cuts
        .iter()
        .map(|c| {
            let beat = nearest(beats, c.output_secs - audio_offset_secs) + audio_offset_secs;
            let error_ms = (c.output_secs - beat) * 1000.0;
            CutSync {
                output_secs: c.output_secs,
                kind: c.kind,
                beat_secs: beat,
                error_ms,
                error_frames: error_ms / frame_ms,
            }
        })
        .collect();
    let n = scored.len();
    let (mut sum_abs, mut sum, mut max_abs, mut within) = (0.0_f32, 0.0_f32, 0.0_f32, 0u32);
    for c in &scored {
        sum_abs += c.error_ms.abs();
        sum += c.error_ms;
        max_abs = max_abs.max(c.error_ms.abs());
        if c.error_ms.abs() <= tolerance_ms {
            within += 1;
        }
    }
    let mean = |x: f32| if n > 0 { x / n as f32 } else { 0.0 };
    Some(BeatSyncReport {
        tempo_bpm: 60.0 / beat_interval_secs,
        beat_interval_secs,
        fps,
        tolerance_frames: SYNC_TOLERANCE_FRAMES,
        audio_offset_secs: None,
        audio_offset_confidence: None,
        n_cuts: n as u32,
        within_tolerance: within,
        mean_abs_error_ms: mean(sum_abs),
        max_abs_error_ms: max_abs,
        mean_signed_error_ms: mean(sum),
        chance_mean_abs_error_ms: beat_interval_secs * 1000.0 / 4.0,
        cuts: scored,
    })
}

/// Nearest value in a sorted, non-empty slice.
fn nearest(sorted: &[f32], t: f32) -> f32 {
    let i = sorted.partition_point(|&b| b < t);
    match (i.checked_sub(1).map(|j| sorted[j]), sorted.get(i).copied()) {
        (Some(a), Some(b)) => {
            if t - a <= b - t {
                a
            } else {
                b
            }
        }
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => t,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video_renderer::SoundtrackLoopPlan;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn crossfade_cuts_sit_mid_seam_and_shrink_the_spacing() {
        // Intro 3 s, three 2 s clips, outro 2 s, 0.4 s crossfades.
        let durs = [3.0, 2.0, 2.0, 2.0, 2.0];
        let seams = [0.4; 4];
        let cuts = planned_cuts(&durs, &seams, true, true, None, 30);
        let times: Vec<f32> = cuts.iter().map(|c| c.output_secs).collect();
        // Input k starts at Σd − k·0.4; cut at start + 0.2.
        let want = [2.8, 4.4, 6.0, 7.6];
        assert!(times.iter().zip(want).all(|(&a, b)| approx(a, b)), "{times:?}");
        assert_eq!(cuts[0].kind, CutKind::IntroToBody);
        assert_eq!(cuts[1].kind, CutKind::Body);
        assert_eq!(cuts[3].kind, CutKind::BodyToOutro);
        // 2 s clips (4 beats at 120 BPM) end up 1.6 s apart — 3.2 beats.
        assert!(approx(times[2] - times[1], 1.6));
    }

    #[test]
    fn hard_concat_cuts_are_cumulative() {
        let cuts = planned_cuts(&[1.5, 2.0, 2.5], &[0.0, 0.0], false, false, None, 30);
        let times: Vec<f32> = cuts.iter().map(|c| c.output_secs).collect();
        assert!(approx(times[0], 1.5) && approx(times[1], 3.5), "{times:?}");
        assert!(cuts.iter().all(|c| c.kind == CutKind::Body));
    }

    #[test]
    fn burst_internal_cuts_follow_the_burst_input_start() {
        // Inputs: clip 2 s, burst (4 × 0.5 s) 2 s, clip 2 s; 0.4 s seams.
        let subs = [0.5, 0.5, 0.5, 0.5];
        let cuts = planned_cuts(&[2.0, 2.0, 2.0], &[0.4, 0.4], false, false, Some((1, &subs)), 30);
        let internal: Vec<f32> = cuts
            .iter()
            .filter(|c| c.kind == CutKind::BurstInternal)
            .map(|c| c.output_secs)
            .collect();
        // Burst input starts at 2.0 − 0.4 = 1.6.
        assert_eq!(internal.len(), 3);
        assert!(approx(internal[0], 2.1) && approx(internal[2], 3.1), "{internal:?}");
    }

    #[test]
    fn smart_loop_spans_shift_beats_at_each_seam() {
        // Natural intro to 20 s, then loop [8, 20) (L = 12 s, X = 2 s).
        // No beat interval given: the crossfade stays at 2 s.
        let plan = SoundtrackLoopPlan {
            first_play_end_secs: 20.0,
            loop_start_secs: 8.0,
            loop_secs: 12.0,
            beat_interval_secs: 0.0,
        };
        let spans = plan.spans(40.0, 30.0);
        assert!(approx(plan.crossfade_secs(), 2.0));
        // Copy 1 starts at 20 − 2 = 18; handover at the crossfade midpoint.
        assert!(approx(spans[0].output_end, 19.0), "{spans:?}");
        assert!(approx(spans[1].output_start, 19.0));
        assert!(approx(spans[1].source_start, 9.0));
        // Copy 2 starts at 20 + 12 − 4 = 28.
        assert!(approx(spans[1].output_end, 29.0));
        assert!(approx(spans.last().unwrap().output_end, 40.0));

        // Beats every 0.3 s from 0.15. Copy 1 plays source 8 s at output
        // 18 s, so after the handover at 19 s every beat is shifted by
        // 10 s: the last copy-0 beat is source 18.75 (output 18.75), the
        // first copy-1 beat is source 9.15 (output 19.15) — 0.4 s apart
        // instead of 0.3.
        let beats: Vec<f32> = (0..100).map(|k| 0.15 + k as f32 * 0.3).collect();
        let out = beats_in_output(&beats, &spans);
        assert!(out.windows(2).all(|w| w[1] > w[0]));
        let i = out.partition_point(|&b| b < 19.0);
        assert!(approx(out[i - 1], 18.75) && approx(out[i], 19.15), "{:?}", &out[i - 2..i + 2]);
    }

    #[test]
    fn whole_beat_loop_crossfade_keeps_the_grid_unbroken() {
        // Beats every 0.3 s from 0.15; the loop runs beat to beat
        // [7.95, 18.75) (36 beats). The ~2 s crossfade rounds to 7 beats
        // (2.1 s), so every copy's beats land on the previous copy's grid.
        let beat = 0.3;
        let plan = SoundtrackLoopPlan {
            first_play_end_secs: 18.75,
            loop_start_secs: 7.95,
            loop_secs: 10.8,
            beat_interval_secs: beat,
        };
        assert!(approx(plan.crossfade_secs(), 2.1), "{}", plan.crossfade_secs());
        let beats: Vec<f32> = (0..100).map(|k| 0.15 + k as f32 * beat).collect();
        let out = beats_in_output(&beats, &plan.spans(45.0, 30.0));
        assert!(out.last().copied().unwrap_or(0.0) > 40.0);
        for w in out.windows(2) {
            assert!((w[1] - w[0] - beat).abs() < 2e-3, "grid breaks at {w:?}");
        }
    }

    #[test]
    fn simple_loop_restarts_every_loop_length() {
        let plan = SoundtrackLoopPlan {
            first_play_end_secs: 0.0,
            loop_start_secs: 0.0,
            loop_secs: 10.0,
            beat_interval_secs: 0.0,
        };
        let spans = plan.spans(25.0, 10.3);
        assert_eq!(spans.len(), 3);
        assert!(approx(spans[2].output_start, 20.0) && approx(spans[2].output_end, 25.0));
        let out = beats_in_output(&[0.25, 9.75], &spans);
        let want = [0.25, 9.75, 10.25, 19.75, 20.25];
        assert!(out.iter().zip(want).all(|(&a, b)| approx(a, b)), "{out:?}");
    }

    #[test]
    fn reference_window_avoids_duck_windows() {
        let spans = [SoundtrackSpan { output_start: 0.0, output_end: 30.0, source_start: 0.0 }];
        // Duck 1–5 s (ramp 0.3): the first clean 8 s window starts at 5.5.
        let (out, src, len) = pick_reference_window(&spans, &[(1.0, 5.0, 0.3)], 30.0).unwrap();
        assert!(approx(out, 5.5) && approx(src, 5.5) && approx(len, 8.0), "{out} {src} {len}");
        // No room at all → the least-overlapping window.
        let ducks = [(0.0, 30.0, 0.3)];
        assert!(pick_reference_window(&spans, &ducks, 30.0).is_some());
        // Too short to measure.
        assert!(pick_reference_window(&spans, &[], 2.0).is_none());
    }

    #[test]
    fn beat_locked_cuts_score_on_the_beat() {
        let beats: Vec<f32> = (0..40).map(|k| 0.1 + k as f32 * 0.5).collect();
        let cuts: Vec<PlannedCut> = (1..10)
            .map(|k| PlannedCut { output_secs: 0.1 + k as f32 * 2.0 + 0.01, kind: CutKind::Body })
            .collect();
        let r = score_cuts(&cuts, &beats, 0.0, 0.5, 30).unwrap();
        assert_eq!(r.within_tolerance, 9);
        assert!(approx(r.mean_signed_error_ms, 10.0));
        assert!(approx(r.chance_mean_abs_error_ms, 125.0));
        assert_eq!(r.verdict(), "on the beat");
    }

    #[test]
    fn audio_offset_moves_the_beats() {
        // Cuts exactly on the planned beats, but the music plays 40 ms late:
        // every cut comes 40 ms before its beat.
        let beats: Vec<f32> = (0..40).map(|k| k as f32 * 0.5).collect();
        let cuts = [PlannedCut { output_secs: 4.0, kind: CutKind::Body }];
        let r = score_cuts(&cuts, &beats, 0.040, 0.5, 30).unwrap();
        assert!(approx(r.cuts[0].error_ms, -40.0), "{:?}", r.cuts[0]);
        assert!(approx(r.cuts[0].beat_secs, 4.04));
    }

    #[test]
    fn drifting_cuts_read_as_not_beat_locked() {
        // 0.4 s crossfades on 2 s clips at 120 BPM: cuts every 3.2 beats.
        let beats: Vec<f32> = (0..200).map(|k| k as f32 * 0.5).collect();
        let cuts: Vec<PlannedCut> = (1..30)
            .map(|k| PlannedCut { output_secs: k as f32 * 1.6 + 0.2, kind: CutKind::Body })
            .collect();
        let r = score_cuts(&cuts, &beats, 0.0, 0.5, 30).unwrap();
        // Errors cycle −200, −100, 0, +100, +200 ms: three in five are
        // "within 3 frames", but the mean error is chance level.
        assert!(r.mean_abs_error_ms >= 0.75 * r.chance_mean_abs_error_ms, "{r:?}");
        assert!(r.verdict().starts_with("not beat-locked"), "{}", r.verdict());
    }
}
