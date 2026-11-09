//! The rendered video's timeline, exactly as the concat stage builds it.
//!
//! One computation answers "what plays when" for every stage:
//!
//! - the concat stage feeds ffmpeg the xfade offsets from [`ChainLayout`];
//! - ducking centres its windows on the [`OutputTimeline`] clip spans;
//! - the beat-sync check scores [`OutputTimeline::cuts`];
//! - the manifest records the [`OutputTimeline`] for the UI and for the
//!   feedback actions ("drop the moment at 0:12");
//! - before anything is rendered, the beat-burst planner predicts the same
//!   layout with [`predict_layout`].
//!
//! # How a concat chain lays out
//!
//! Input `k` of a crossfade chain starts at the running output length minus
//! its incoming crossfade (the xfade `offset`), and the running length grows
//! by `d_k − x_k`. ffmpeg starts a transition on the first frame at or after
//! its offset, so an offset that sits off a frame boundary — or one that
//! three-decimal formatting nudged a hair past it (`17/30 s` written as
//! `0.567`) — moves the clip a whole frame. [`ChainLayout`] rounds every
//! offset to the nearest frame and the renderer passes exactly that frame's
//! time, so the layout and the render agree frame for frame. The running
//! length stays in seconds, so per-seam rounding never accumulates. A hard
//! concat is the same chain with zero-length seams (frames simply add up).

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::beat_sync::{CutKind, PlannedCut};
use crate::video_renderer::SpeedRamp;

/// Where every input of a concat chain plays, in whole output frames.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainLayout {
    pub fps: u32,
    /// First output frame of each input.
    pub start_frames: Vec<u32>,
    /// Frames of each input.
    pub len_frames: Vec<u32>,
    /// Crossfade into each input, seconds (0 for the first input and for
    /// hard cuts).
    pub seam_in_secs: Vec<f64>,
}

impl ChainLayout {
    /// Lay out a chain. `input_durs[k]` is input `k`'s on-disk duration;
    /// `seam_durs[k]` is the crossfade between inputs `k` and `k + 1` as the
    /// renderer applies it (0 = hard cut).
    pub fn new(input_durs: &[f64], seam_durs: &[f64], fps: u32) -> Self {
        let fps = fps.max(1);
        let f = fps as f64;
        let n = input_durs.len();
        let mut start_frames = Vec::with_capacity(n);
        let mut len_frames = Vec::with_capacity(n);
        let mut seam_in_secs = Vec::with_capacity(n);
        // Running output length in seconds, as the xfade offsets are built.
        let mut cum = 0.0_f64;
        // End of the chain so far, in frames.
        let mut chain_end = 0u32;
        for (k, &d) in input_durs.iter().enumerate() {
            let d = d.max(0.0);
            let len = ((d * f).round() as u32).max(1);
            // To the microsecond: seams arrive as f32 seconds (0.3 →
            // 0.30000001192…), which would otherwise leak into the manifest.
            let x = if k == 0 {
                0.0
            } else {
                (seam_durs.get(k - 1).copied().unwrap_or(0.0).max(0.0) * 1e6).round() / 1e6
            };
            let start = if k == 0 {
                0
            } else if x <= 0.0 {
                // Hard concat: inputs play back to back.
                chain_end
            } else {
                let want = ((cum - x) * f).round().max(0.0) as u32;
                // The transition must start on a frame the chain so far still
                // has, or xfade stops emitting video (see `concat_with_xfade`).
                // A crossfade that runs past the end of the left side is fine.
                want.min(chain_end.saturating_sub(1))
            };
            cum = if k == 0 { d } else { cum - x + d };
            chain_end = start + len;
            start_frames.push(start);
            len_frames.push(len);
            seam_in_secs.push(x);
        }
        ChainLayout { fps, start_frames, len_frames, seam_in_secs }
    }

    pub fn len(&self) -> usize {
        self.start_frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.start_frames.is_empty()
    }

    /// Frames of the whole chain: where the last input ends.
    pub fn total_frames(&self) -> u32 {
        match (self.start_frames.last(), self.len_frames.last()) {
            (Some(s), Some(l)) => s + l,
            _ => 0,
        }
    }

    /// Output seconds of a frame boundary.
    pub fn secs(&self, frame: u32) -> f64 {
        frame as f64 / self.fps.max(1) as f64
    }

    pub fn duration_secs(&self) -> f64 {
        self.secs(self.total_frames())
    }

    pub fn start_secs(&self, k: usize) -> f64 {
        self.secs(self.start_frames[k])
    }

    pub fn end_secs(&self, k: usize) -> f64 {
        self.secs(self.start_frames[k] + self.len_frames[k])
    }

    /// The visible cut into input `k` (k ≥ 1): the middle of its incoming
    /// crossfade, or its first frame after a hard cut. 0 for input 0.
    pub fn cut_secs(&self, k: usize) -> f64 {
        if k == 0 {
            return 0.0;
        }
        self.start_secs(k) + self.seam_in_secs[k] * 0.5
    }
}

/// Cuts of a chain, in output order. `burst` is `(input_index,
/// sub_clip_durations)` for a folded beat-burst input, whose sub-clips play
/// back to back with hard cuts.
pub fn chain_cuts(
    layout: &ChainLayout,
    has_intro: bool,
    has_outro: bool,
    burst: Option<(usize, &[f64])>,
) -> Vec<PlannedCut> {
    let n = layout.len();
    let mut cuts = Vec::new();
    for k in 1..n {
        let kind = if has_intro && k == 1 {
            CutKind::IntroToBody
        } else if has_outro && k == n - 1 {
            CutKind::BodyToOutro
        } else {
            CutKind::Body
        };
        cuts.push(PlannedCut { output_secs: layout.cut_secs(k) as f32, kind });
    }
    if let Some((idx, subs)) = burst {
        if idx < n {
            let mut frame = layout.start_frames[idx];
            for &d in subs.iter().take(subs.len().saturating_sub(1)) {
                frame += frames_of(d, layout.fps);
                cuts.push(PlannedCut {
                    output_secs: layout.secs(frame) as f32,
                    kind: CutKind::BurstInternal,
                });
            }
        }
    }
    cuts.sort_by(|a, b| a.output_secs.total_cmp(&b.output_secs));
    cuts
}

/// Frames a clip of `secs` occupies (the normalize stage writes exactly
/// this many; at least one).
fn frames_of(secs: f64, fps: u32) -> u32 {
    ((secs.max(0.0) * fps.max(1) as f64).round() as u32).max(1)
}

// ---------- The manifest record ----------

/// The rendered video's exact timeline. Written to the manifest as
/// `timeline`; every time is a frame boundary except the cut points, which
/// sit mid-crossfade.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct OutputTimeline {
    pub fps: u32,
    /// Frames in the rendered video.
    pub frames: u32,
    /// `frames / fps`: the rendered video's length.
    pub duration_secs: f64,
    /// The intro card, when one was rendered as its own clip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub intro: Option<TimelineCard>,
    /// The outro card, when one was rendered as its own clip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub outro: Option<TimelineCard>,
    /// Every clip in output order. A folded beat-burst contributes one entry
    /// per burst cut, all with the burst's `segment_index`.
    pub clips: Vec<TimelineClip>,
}

/// An intro or outro card's place in the output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct TimelineCard {
    pub start_frame: u32,
    /// Exclusive.
    pub end_frame: u32,
    pub output_start_secs: f64,
    pub output_end_secs: f64,
    /// The visible cut between the card and the footage (mid-crossfade).
    pub cut_secs: f64,
    /// Crossfade between the card and the footage.
    pub transition_secs: f64,
}

/// One clip's place in the output and what it shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct TimelineClip {
    /// Index into the manifest's `segments`.
    pub segment_index: usize,
    /// Position inside a folded beat-burst (0-based), else absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub burst_part: Option<u32>,
    pub source_id: String,
    /// The footage file the clip reads.
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub source_path: Utf8PathBuf,
    /// Window read from the source (chapter-local seconds), including a
    /// speed-ramp tail.
    pub source_in_secs: f64,
    pub source_out_secs: f64,
    /// Playback speed of the clip body (a ramp speeds up only the tail).
    pub speed: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub ramp: Option<TimelineRamp>,
    /// Frames on screen, including the crossfades in and out.
    pub start_frame: u32,
    /// Exclusive.
    pub end_frame: u32,
    pub output_start_secs: f64,
    pub output_end_secs: f64,
    /// Visible cuts into and out of the clip (mid-crossfade). Consecutive
    /// clips share them, so `[cut_in_secs, cut_out_secs)` spans partition
    /// the footage.
    pub cut_in_secs: f64,
    pub cut_out_secs: f64,
    pub transition_in_secs: f64,
    pub transition_out_secs: f64,
}

/// A speed-ramped tail: from `output_start_secs` to the clip's end, the
/// source plays `factor`× fast.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct TimelineRamp {
    pub output_start_secs: f64,
    pub factor: f32,
}

/// What a clip reads, for [`OutputTimeline::build`].
#[derive(Debug, Clone)]
pub struct ClipSource {
    pub segment_index: usize,
    pub source_id: String,
    pub source_path: Utf8PathBuf,
    pub source_in_secs: f64,
    /// The clip's planned length (seconds of output it fills).
    pub duration_secs: f64,
    pub ramp: Option<SpeedRamp>,
}

/// What one concat input is, for [`OutputTimeline::build`].
#[derive(Debug, Clone)]
pub enum ChainItem {
    IntroCard,
    OutroCard,
    Clip(ClipSource),
    /// A folded beat-burst: `(part, on-disk seconds)` back to back.
    Burst(Vec<(ClipSource, f64)>),
}

impl OutputTimeline {
    /// Build the timeline of a chain. `items[k]` describes `layout`'s input
    /// `k`.
    pub fn build(layout: &ChainLayout, items: &[ChainItem]) -> Self {
        debug_assert_eq!(layout.len(), items.len(), "one chain item per concat input");
        let n = layout.len().min(items.len());
        let fps = layout.fps;
        let seam_out = |k: usize| -> f64 {
            if k + 1 < layout.len() { layout.seam_in_secs[k + 1] } else { 0.0 }
        };
        let cut_out = |k: usize| -> f64 {
            if k + 1 < layout.len() { layout.cut_secs(k + 1) } else { layout.end_secs(k) }
        };
        let mut timeline = OutputTimeline {
            fps,
            frames: layout.total_frames(),
            duration_secs: layout.duration_secs(),
            intro: None,
            outro: None,
            clips: Vec::new(),
        };
        let card = |k: usize, cut_secs: f64, transition_secs: f64| TimelineCard {
            start_frame: layout.start_frames[k],
            end_frame: layout.start_frames[k] + layout.len_frames[k],
            output_start_secs: layout.start_secs(k),
            output_end_secs: layout.end_secs(k),
            cut_secs,
            transition_secs,
        };
        for k in 0..n {
            let start = layout.start_frames[k];
            let end = start + layout.len_frames[k];
            match &items[k] {
                ChainItem::IntroCard => timeline.intro = Some(card(k, cut_out(k), seam_out(k))),
                ChainItem::OutroCard => {
                    timeline.outro = Some(card(k, layout.cut_secs(k), layout.seam_in_secs[k]))
                }
                ChainItem::Clip(src) => timeline.clips.push(clip_entry(
                    src,
                    None,
                    (start, end),
                    (layout.cut_secs(k), cut_out(k)),
                    (layout.seam_in_secs[k], seam_out(k)),
                    fps,
                )),
                ChainItem::Burst(parts) => {
                    let mut frame = start;
                    let last = parts.len().saturating_sub(1);
                    for (p, (src, secs)) in parts.iter().enumerate() {
                        let part_start = frame;
                        let part_end = if p == last { end } else { frame + frames_of(*secs, fps) };
                        frame = part_end;
                        let cut_in =
                            if p == 0 { layout.cut_secs(k) } else { layout.secs(part_start) };
                        let cut_out = if p == last { cut_out(k) } else { layout.secs(part_end) };
                        let trans_in = if p == 0 { layout.seam_in_secs[k] } else { 0.0 };
                        let trans_out = if p == last { seam_out(k) } else { 0.0 };
                        timeline.clips.push(clip_entry(
                            src,
                            Some(p as u32),
                            (part_start, part_end),
                            (cut_in, cut_out),
                            (trans_in, trans_out),
                            fps,
                        ));
                    }
                }
            }
        }
        timeline
    }

    /// Every cut in output order, as the beat-sync check scores them.
    pub fn cuts(&self) -> Vec<PlannedCut> {
        let mut cuts = Vec::new();
        if let (Some(intro), Some(_)) = (&self.intro, self.clips.first()) {
            cuts.push(PlannedCut { output_secs: intro.cut_secs as f32, kind: CutKind::IntroToBody });
        }
        for w in self.clips.windows(2) {
            let kind = if w[1].burst_part.is_some_and(|p| p > 0) {
                CutKind::BurstInternal
            } else {
                CutKind::Body
            };
            cuts.push(PlannedCut { output_secs: w[1].cut_in_secs as f32, kind });
        }
        if let (Some(outro), Some(_)) = (&self.outro, self.clips.last()) {
            cuts.push(PlannedCut { output_secs: outro.cut_secs as f32, kind: CutKind::BodyToOutro });
        }
        cuts
    }

    /// Where each of `n_segments` segments sits on screen, crossfades
    /// included: `(output_start, duration)` per segment index, parallel to
    /// the manifest's `segments` (what duck-window selection takes). A
    /// segment missing from the timeline gets `(0, 0)`.
    pub fn segment_windows(&self, n_segments: usize) -> (Vec<f32>, Vec<f32>) {
        let mut span: Vec<Option<(f64, f64)>> = vec![None; n_segments];
        for c in &self.clips {
            if let Some(s) = span.get_mut(c.segment_index) {
                *s = Some(match *s {
                    Some((a, b)) => (a.min(c.output_start_secs), b.max(c.output_end_secs)),
                    None => (c.output_start_secs, c.output_end_secs),
                });
            }
        }
        span.into_iter()
            .map(|s| s.map_or((0.0, 0.0), |(a, b)| (a as f32, (b - a) as f32)))
            .unzip()
    }

    /// The clip on screen at output time `t`, by visible cut span.
    pub fn clip_at(&self, t: f64) -> Option<&TimelineClip> {
        self.clips.iter().find(|c| t >= c.cut_in_secs && t < c.cut_out_secs)
    }
}

fn clip_entry(
    src: &ClipSource,
    burst_part: Option<u32>,
    (start_frame, end_frame): (u32, u32),
    (cut_in_secs, cut_out_secs): (f64, f64),
    (transition_in_secs, transition_out_secs): (f64, f64),
    fps: u32,
) -> TimelineClip {
    let f = fps.max(1) as f64;
    let output_start_secs = start_frame as f64 / f;
    let output_end_secs = end_frame as f64 / f;
    // The ramp reads `duration × (1 − 1/factor)` extra seconds of source and
    // plays its last `duration` seconds of source in `duration / factor`.
    let (ramp, extra) = match src.ramp {
        Some(r) if r.factor > 1.0 && r.duration_secs > 0.0 => {
            let factor = r.factor;
            let out_len = r.duration_secs as f64 / factor as f64;
            let extra = r.duration_secs as f64 * (1.0 - 1.0 / factor as f64);
            (
                Some(TimelineRamp { output_start_secs: (output_end_secs - out_len).max(output_start_secs), factor }),
                extra,
            )
        }
        _ => (None, 0.0),
    };
    TimelineClip {
        segment_index: src.segment_index,
        burst_part,
        source_id: src.source_id.clone(),
        source_path: src.source_path.clone(),
        source_in_secs: src.source_in_secs,
        source_out_secs: src.source_in_secs + src.duration_secs + extra,
        speed: 1.0,
        ramp,
        start_frame,
        end_frame,
        output_start_secs,
        output_end_secs,
        cut_in_secs,
        cut_out_secs,
        transition_in_secs,
        transition_out_secs,
    }
}

// ---------- Prediction (before rendering) ----------

/// The layout a planned cut will get, predicted before rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct PredictedLayout {
    /// Visible span `(cut_in, cut_out)` of each planned clip.
    pub clip_spans: Vec<(f64, f64)>,
    /// Where the footage starts (the cut out of the intro card, or 0).
    pub body_start: f64,
    /// Where the footage ends (the cut into the outro card, or the end).
    pub body_end: f64,
    /// Length of the whole output.
    pub total: f64,
}

/// Predict where planned clips will play, with the same chain arithmetic
/// the concat stage uses. `clip_secs` are the planned lengths; `seam_secs`
/// the crossfade every seam gets (0 for hard cuts); cards are only those
/// rendered as their own clips.
///
/// With `beat_locked`, the beat-lock pass later stretches every clip by
/// half of each crossfade it has, so the span between its cuts is its full
/// planned length — the prediction applies the same stretch.
pub fn predict_layout(
    intro_card_secs: Option<f64>,
    clip_secs: &[f64],
    outro_card_secs: Option<f64>,
    seam_secs: f64,
    beat_locked: bool,
    fps: u32,
) -> PredictedLayout {
    let n = clip_secs.len();
    let has_intro = intro_card_secs.is_some();
    let has_outro = outro_card_secs.is_some();
    let mut durs: Vec<f64> = Vec::with_capacity(n + 2);
    if let Some(d) = intro_card_secs {
        durs.push(d);
    }
    for (i, &d) in clip_secs.iter().enumerate() {
        let w_in = if i > 0 || has_intro { seam_secs } else { 0.0 };
        let w_out = if i + 1 < n || has_outro { seam_secs } else { 0.0 };
        // Beat lock leaves the clip that ends the video at its planned length.
        let stretch = if beat_locked {
            if i + 1 < n || has_outro { (w_in + w_out) * 0.5 } else { 0.0 }
        } else {
            0.0
        };
        durs.push(d + stretch);
    }
    if let Some(d) = outro_card_secs {
        durs.push(d);
    }
    let seams = vec![seam_secs; durs.len().saturating_sub(1)];
    let layout = ChainLayout::new(&durs, &seams, fps);
    let first = usize::from(has_intro);
    let clip_spans: Vec<(f64, f64)> = (first..first + n)
        .map(|k| {
            let cut_out = if k + 1 < layout.len() { layout.cut_secs(k + 1) } else { layout.end_secs(k) };
            (layout.cut_secs(k), cut_out)
        })
        .collect();
    let total = layout.duration_secs();
    PredictedLayout {
        body_start: clip_spans.first().map_or(0.0, |s| s.0),
        body_end: clip_spans.last().map_or(total, |s| s.1),
        clip_spans,
        total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    fn src(i: usize, d: f64) -> ClipSource {
        ClipSource {
            segment_index: i,
            source_id: format!("s{i}"),
            source_path: format!("/f/GX01000{i}.MP4").into(),
            source_in_secs: 10.0,
            duration_secs: d,
            ramp: None,
        }
    }

    #[test]
    fn offsets_land_on_whole_frames() {
        // 17/30 s written to three decimals is 0.567 s, past frame 17's
        // boundary: ffmpeg would start the clip on frame 18. The layout
        // rounds to the nearest frame instead.
        let a = 1.0;
        let x = 1.0 - 17.0 / 30.0;
        let l = ChainLayout::new(&[a, 1.0], &[x], 30);
        assert_eq!(l.start_frames, vec![0, 17]);
        assert_eq!(l.total_frames(), 47);
    }

    #[test]
    fn seam_rounding_does_not_accumulate() {
        // 0.35 s seams are 10.5 frames at 30 fps. Ten of them must still
        // put the last clip where the float arithmetic does (± half a frame).
        let durs = vec![2.0; 11];
        let seams = vec![0.35; 10];
        let l = ChainLayout::new(&durs, &seams, 30);
        let float_start = 10.0 * (2.0 - 0.35);
        assert!((l.start_secs(10) - float_start).abs() <= 0.5 / 30.0 + 1e-9, "{}", l.start_secs(10));
    }

    #[test]
    fn hard_concat_adds_frames() {
        let l = ChainLayout::new(&[1.5, 2.0, 2.5], &[0.0, 0.0], 30);
        assert_eq!(l.start_frames, vec![0, 45, 105]);
        assert!(approx(l.duration_secs(), 6.0));
    }

    #[test]
    fn per_seam_crossfades_place_each_clip() {
        // Intro 3 s, then 3 clips of 2 s; the second seam is a 0.1 s
        // near-cut after a speed ramp, the others 0.4 s.
        let l = ChainLayout::new(&[3.0, 2.0, 2.0, 2.0], &[0.4, 0.1, 0.4], 30);
        assert!(approx(l.start_secs(1), 2.6));
        assert!(approx(l.start_secs(2), 4.5));
        assert!(approx(l.start_secs(3), 6.1));
        assert!(approx(l.cut_secs(2), 4.55));
        assert!(approx(l.duration_secs(), 8.1));
    }

    #[test]
    fn timeline_records_cards_clips_and_burst_parts() {
        let layout = ChainLayout::new(&[3.0, 2.0, 1.5, 2.0, 2.0], &[0.4, 0.4, 0.4, 0.4], 30);
        let mut ramped = src(0, 2.0);
        ramped.ramp = Some(SpeedRamp { duration_secs: 0.6, factor: 3.0, flash: false });
        let burst = vec![(src(1, 0.5), 0.5), (src(1, 0.5), 0.5), (src(1, 0.5), 0.5)];
        let items = vec![
            ChainItem::IntroCard,
            ChainItem::Clip(ramped),
            ChainItem::Burst(burst),
            ChainItem::Clip(src(2, 2.0)),
            ChainItem::OutroCard,
        ];
        let t = OutputTimeline::build(&layout, &items);
        assert_eq!(t.frames, layout.total_frames());
        let intro = t.intro.as_ref().unwrap();
        assert!(approx(intro.cut_secs, 2.8) && approx(intro.output_end_secs, 3.0));
        assert_eq!(t.clips.len(), 5);
        // Clip 0: 2.6 → 4.6 on screen, cuts at 2.8 and 4.4.
        let c0 = &t.clips[0];
        assert!(approx(c0.output_start_secs, 2.6) && approx(c0.output_end_secs, 4.6));
        assert!(approx(c0.cut_in_secs, 2.8) && approx(c0.cut_out_secs, 4.4));
        assert!(approx(c0.source_out_secs, 10.0 + 2.0 + 0.4), "{}", c0.source_out_secs);
        assert!(approx(c0.ramp.unwrap().output_start_secs, 4.4));
        // Burst input starts at 4.2: parts at 4.2, 4.7, 5.2; hard cuts inside.
        let parts: Vec<&TimelineClip> = t.clips.iter().filter(|c| c.segment_index == 1).collect();
        assert_eq!(parts.len(), 3);
        assert!(approx(parts[0].cut_in_secs, 4.4));
        assert!(approx(parts[1].cut_in_secs, 4.7) && approx(parts[1].transition_in_secs, 0.0));
        assert!(approx(parts[2].output_end_secs, 5.7));
        // Spans partition the footage.
        for w in t.clips.windows(2) {
            assert!(approx(w[0].cut_out_secs, w[1].cut_in_secs));
        }
        // The cut list matches the chain cuts.
        let a: Vec<f32> = t.cuts().iter().map(|c| c.output_secs).collect();
        let subs = [0.5, 0.5, 0.5];
        let b: Vec<f32> =
            chain_cuts(&layout, true, true, Some((2, &subs))).iter().map(|c| c.output_secs).collect();
        assert_eq!(a.len(), b.len());
        assert!(a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-4), "{a:?} vs {b:?}");
        let kinds: Vec<CutKind> = t.cuts().iter().map(|c| c.kind).collect();
        assert_eq!(
            kinds,
            vec![
                CutKind::IntroToBody,
                CutKind::Body,
                CutKind::BurstInternal,
                CutKind::BurstInternal,
                CutKind::Body,
                CutKind::BodyToOutro
            ]
        );
        // Segment windows cover the burst as one span.
        let (starts, durs) = t.segment_windows(3);
        assert!((starts[1] - 4.2).abs() < 1e-4 && (durs[1] - 1.5).abs() < 1e-4);
        assert!(t.clip_at(4.8).is_some_and(|c| c.burst_part == Some(1)));
        assert!(t.clip_at(1.0).is_none());
    }

    #[test]
    fn ramped_near_cuts_move_every_later_clip() {
        // Intro card and six 3 s clips with 0.4 s crossfades, except that
        // clips 0–3 speed-ramp out through a 0.1 s near-cut. Assuming every
        // seam overlaps by the full crossfade (what duck placement used to
        // do) puts clip 5 at 3 + 5·3 − 6·0.4 = 15.6 s; each near-cut overlaps
        // 0.3 s less, so it really starts 1.2 s later — and so must its duck.
        let durs = [3.0; 7];
        let seams = [0.4, 0.1, 0.1, 0.1, 0.1, 0.4];
        let layout = ChainLayout::new(&durs, &seams, 30);
        let ramp = SpeedRamp { duration_secs: 0.9, factor: 3.0, flash: true };
        let mut items = vec![ChainItem::IntroCard];
        for i in 0..6 {
            let mut s = src(i, 3.0);
            s.ramp = (i < 4).then_some(ramp);
            items.push(ChainItem::Clip(s));
        }
        let t = OutputTimeline::build(&layout, &items);
        let (starts, lens) = t.segment_windows(6);
        assert!((starts[5] - 16.8).abs() < 1e-4, "{starts:?}");
        assert!((lens[5] - 3.0).abs() < 1e-4);
        assert!((starts[1] - 5.5).abs() < 1e-4, "{starts:?}");
        assert!(approx(t.duration_secs, 19.8));
        // The ramped clips' sped-up tails end where their near-cut begins.
        let c0 = &t.clips[0];
        assert!(approx(c0.ramp.unwrap().output_start_secs, c0.output_end_secs - 0.3));
        assert!(approx(c0.transition_out_secs, 0.1) && approx(t.clips[4].transition_in_secs, 0.1));
    }

    #[test]
    fn prediction_matches_the_render_layout() {
        // Without beat lock, the prediction is the chain itself.
        let p = predict_layout(Some(3.0), &[2.0, 2.0, 2.0], None, 0.4, false, 30);
        let l = ChainLayout::new(&[3.0, 2.0, 2.0, 2.0], &[0.4; 3], 30);
        assert!(approx(p.clip_spans[1].0, l.cut_secs(2)));
        assert!(approx(p.body_start, 2.8) && approx(p.total, l.duration_secs()));
        // With beat lock, the cut-to-cut span is the planned length.
        let p = predict_layout(Some(3.0), &[2.0, 2.0, 2.0], None, 0.4, true, 30);
        assert!(approx(p.clip_spans[0].0, 2.8));
        assert!(approx(p.clip_spans[1].0, 4.8) && approx(p.clip_spans[2].0, 6.8));
        // No intro card (overlay mode): the footage starts at 0.
        let p = predict_layout(None, &[2.0, 2.0], None, 0.4, true, 30);
        assert!(approx(p.body_start, 0.0) && approx(p.clip_spans[1].0, 2.0));
    }
}
