//! Beat-locked cut placement: choose each clip's exact length so the cut
//! after it lands on a beat of the soundtrack *as it plays in the output*.
//!
//! `[soundtrack].beat_align` makes every planned clip a whole number of
//! beats long, but that alone doesn't put cuts on beats: each crossfade
//! overlaps two clips, the intro card shifts the start, and frame rounding
//! adds up. [`lock_cuts`] walks the timeline left to right with the same
//! arithmetic as the concat stage (see `beat_sync::planned_cuts`):
//!
//! - clip `j` starts at `s_j` (output time) and its incoming cut sits in
//!   the middle of the incoming crossfade, `s_j + w_in / 2`;
//! - the outgoing cut is aimed at the beat nearest `incoming cut + planned
//!   length`, so the visible span stays the planned number of beats;
//! - the clip's length becomes `beat − s_j + w_out / 2` (it has to run on
//!   through half of the outgoing crossfade), rounded to whole frames.
//!
//! Every cut targets an absolute beat, so rounding never accumulates: each
//! cut is within half a frame of its beat. When a longer clip would read
//! past the end of its source chapter, the clip's window slides earlier;
//! failing that, a speed-ramped clip gives up its ramp (which needs extra
//! footage), and failing that, a nearer beat is chosen. A clip that fits
//! nowhere keeps its planned length.

/// One clip on the timeline, as planned.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LockClip {
    /// Planned length: the visible span the planner intended between cuts.
    pub planned_secs: f64,
    /// Where the clip starts reading in its source chapter.
    pub in_start_secs: f64,
    /// End of the footage the clip may read to. `None` = unlimited (stills).
    pub source_end_secs: Option<f64>,
    /// Footage read past the clip's own length (a speed-ramp tail).
    pub tail_extra_secs: f64,
    /// Crossfade into the next clip (0 = hard cut). `None` when nothing
    /// follows: the clip ends the video (at `target_secs` when given, else
    /// its planned length).
    pub seam_out_secs: Option<f64>,
    /// For a speed-ramped clip: the seam it would have without its ramp.
    pub seam_out_without_ramp: Option<f64>,
    /// Output time the outgoing cut should land on — a beat the music plan
    /// chose (see `pacing`) — or, for the clip that ends the video, where
    /// the video ends. `None`: the beat nearest `incoming cut + planned`.
    /// An absolute target keeps the plan's bars and phrases even when
    /// something ahead of the clip (a beat-burst, a nearer beat taken for
    /// lack of footage) moved its start.
    pub target_secs: Option<f64>,
}

/// What [`lock_cuts`] decided for one clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LockedClip {
    pub duration_secs: f64,
    pub in_start_secs: f64,
    /// The clip's speed-ramp was dropped so the locked length fits.
    pub drop_ramp: bool,
    /// The outgoing cut was placed on a beat.
    pub on_beat: bool,
}

/// Where the first clip starts: output time and incoming crossfade
/// (an intro card ahead of it), or `(0, 0)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LockStart {
    pub output_secs: f64,
    pub seam_in_secs: f64,
}

/// Visible span allowed per clip, relative to its planned length.
const MIN_SPAN_FRACTION: f64 = 0.5;
const MAX_SPAN_FRACTION: f64 = 1.5;
/// Frames a clip must outlast its two crossfades by (xfade needs a clip to
/// be longer than both overlaps).
const MIN_CORE_FRAMES: f64 = 2.0;

/// Lock every clip's outgoing cut to the nearest suitable beat. `beats` are
/// output-time beats (sorted), `fps` the project frame rate.
pub fn lock_cuts(clips: &[LockClip], start: LockStart, beats: &[f32], fps: u32) -> Vec<LockedClip> {
    let fps = fps.max(1) as f64;
    let frames = |d: f64| ((d * fps).round().max(1.0)) / fps;
    let mut s = start.output_secs;
    let mut w_in = start.seam_in_secs;
    let mut out = Vec::with_capacity(clips.len());

    for c in clips {
        let planned = frames(c.planned_secs);
        let min_core = MIN_CORE_FRAMES / fps;
        let Some(seam) = c.seam_out_secs else {
            // The clip that ends the video: at its target when it has one
            // and the footage allows, else its planned length.
            let ending = c.target_secs.and_then(|t| {
                let d = frames(t - s);
                if d < w_in + min_core {
                    return None;
                }
                Some((d, fit_window(c.in_start_secs, d + c.tail_extra_secs, c.source_end_secs)?))
            });
            let (d, in_start) = ending.unwrap_or((planned, c.in_start_secs));
            out.push(LockedClip {
                duration_secs: d,
                in_start_secs: in_start,
                drop_ramp: false,
                on_beat: ending.is_some(),
            });
            s += d;
            w_in = 0.0;
            continue;
        };
        let incoming_cut = s + w_in * 0.5;
        let desired = c.target_secs.unwrap_or(incoming_cut + c.planned_secs);
        let mut lo = incoming_cut + MIN_SPAN_FRACTION * c.planned_secs;
        let mut hi = incoming_cut + MAX_SPAN_FRACTION * c.planned_secs;
        if let Some(t) = c.target_secs {
            lo = lo.min(t);
            hi = hi.max(t);
        }
        let mut candidates: Vec<f64> = beats
            .iter()
            .map(|&b| b as f64)
            .filter(|&b| b >= lo && b <= hi)
            .collect();
        candidates.sort_by(|a, b| {
            (a - desired).abs().total_cmp(&(b - desired).abs()).then(a.total_cmp(b))
        });

        // Nearest beat first; for each beat, keep the speed-ramp if the
        // footage allows, else drop it. Holding the planned number of beats
        // matters more to the rhythm than one decorative ramp.
        let mut attempts = vec![(seam, c.tail_extra_secs, false)];
        if let Some(alt) = c.seam_out_without_ramp {
            attempts.push((alt, 0.0, true));
        }
        let placed = candidates.iter().find_map(|&b| {
            attempts.iter().find_map(|&(w_out, tail, drop_ramp)| {
                let d = frames(b - s + w_out * 0.5);
                if d < w_in + w_out + min_core {
                    return None;
                }
                let in_start = fit_window(c.in_start_secs, d + tail, c.source_end_secs)?;
                Some((d, in_start, w_out, drop_ramp))
            })
        });
        match placed {
            Some((d, in_start, w_out, drop_ramp)) => {
                out.push(LockedClip { duration_secs: d, in_start_secs: in_start, drop_ramp, on_beat: true });
                s += d - w_out;
                w_in = w_out;
            }
            None => {
                out.push(LockedClip {
                    duration_secs: planned,
                    in_start_secs: c.in_start_secs,
                    drop_ramp: false,
                    on_beat: false,
                });
                s += planned - seam;
                w_in = seam;
            }
        }
    }
    out
}

/// Length for an intro card so the cut out of it (the middle of its
/// outgoing crossfade, `seam`) lands on the beat nearest where it would
/// fall anyway. Moves the card by at most half a beat (`beat_interval / 2`),
/// keeps it frame-exact, and never shortens it below the crossfade plus two
/// frames. Returns the card's own length when no beat qualifies.
pub fn lock_card_end(card_secs: f64, seam: f64, beats: &[f32], beat_interval: f64, fps: u32) -> f64 {
    let fps = fps.max(1) as f64;
    let frames = |d: f64| ((d * fps).round().max(1.0)) / fps;
    let cut = card_secs - seam * 0.5;
    let half = beat_interval * 0.5 + 1e-9;
    beats
        .iter()
        .map(|&b| b as f64)
        .filter(|b| (b - cut).abs() <= half)
        .min_by(|a, b| (a - cut).abs().total_cmp(&(b - cut).abs()).then(a.total_cmp(b)))
        .map(|b| frames(b + seam * 0.5))
        .filter(|&len| len >= seam + MIN_CORE_FRAMES / fps)
        .unwrap_or_else(|| frames(card_secs))
}

/// Start of a `len`-second window that begins at `in_start` if the footage
/// allows, otherwise as late as the footage end allows. `None` if even a
/// window starting at 0 runs past the end.
fn fit_window(in_start: f64, len: f64, source_end: Option<f64>) -> Option<f64> {
    let Some(end) = source_end else {
        return Some(in_start);
    };
    let overflow = in_start + len - end;
    if overflow <= 1e-6 {
        return Some(in_start);
    }
    let shifted = in_start - overflow;
    (shifted >= 0.0).then_some(shifted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beat_sync::{planned_cuts, CutKind};

    const FPS: u32 = 30;
    const HALF_FRAME: f32 = 0.5 / FPS as f32 + 1e-4;

    fn beats(first: f32, interval: f32, n: usize) -> Vec<f32> {
        (0..n).map(|k| first + k as f32 * interval).collect()
    }

    fn clip(planned: f64, seam: Option<f64>) -> LockClip {
        LockClip {
            planned_secs: planned,
            in_start_secs: 10.0,
            source_end_secs: Some(60.0),
            tail_extra_secs: 0.0,
            seam_out_secs: seam,
            seam_out_without_ramp: None,
            target_secs: None,
        }
    }

    /// Distance from each cut to its nearest beat.
    fn cut_errors(cuts: &[f32], grid: &[f32]) -> Vec<f32> {
        cuts.iter()
            .map(|&c| grid.iter().map(|&b| (c - b).abs()).fold(f32::INFINITY, f32::min))
            .collect()
    }

    #[test]
    fn crossfaded_clips_after_an_intro_cut_on_the_beat() {
        // 120 BPM, first beat at 0.25 s; intro card 3 s; 0.4 s crossfades;
        // eight clips planned at 4 beats; an outro card after the last.
        let grid = beats(0.25, 0.5, 120);
        let intro = 3.0;
        let w = 0.4;
        let clips: Vec<LockClip> = (0..8).map(|_| clip(2.0, Some(w))).collect();
        let locked = lock_cuts(&clips, LockStart { output_secs: intro - w, seam_in_secs: w }, &grid, FPS);
        assert!(locked.iter().all(|l| l.on_beat && !l.drop_ramp));

        let mut durs = vec![intro as f32];
        durs.extend(locked.iter().map(|l| l.duration_secs as f32));
        durs.push(2.0); // outro
        let seams = vec![w as f32; durs.len() - 1];
        let cuts: Vec<f32> = planned_cuts(&durs, &seams, true, true, None, FPS)
            .into_iter()
            .filter(|c| c.kind != CutKind::IntroToBody)
            .map(|c| c.output_secs)
            .collect();
        let errs = cut_errors(&cuts, &grid);
        assert!(errs.iter().all(|&e| e <= HALF_FRAME), "{errs:?}");
        // After the first clip, every span between cuts is exactly 4 beats.
        for pair in cuts.windows(2) {
            assert!((pair[1] - pair[0] - 2.0).abs() <= 2.0 * HALF_FRAME, "{cuts:?}");
        }
        // Each clip is frame-exact.
        for l in &locked {
            let f = l.duration_secs * FPS as f64;
            assert!((f - f.round()).abs() < 1e-9);
        }
    }

    #[test]
    fn hard_cuts_land_on_off_frame_beats_without_drift() {
        // 128 BPM: a beat is 14.06 frames, so per-clip rounding would drift
        // if lengths were rounded independently.
        let interval = 60.0 / 128.0;
        let grid = beats(0.1, interval, 400);
        let clips: Vec<LockClip> = (0..40).map(|_| clip(interval as f64 * 2.0, Some(0.0))).collect();
        let locked = lock_cuts(&clips, LockStart { output_secs: 0.0, seam_in_secs: 0.0 }, &grid, FPS);
        let durs: Vec<f32> = locked.iter().map(|l| l.duration_secs as f32).collect();
        let cuts: Vec<f32> = planned_cuts(&durs, &vec![0.0; durs.len() - 1], false, false, None, FPS)
            .into_iter()
            .map(|c| c.output_secs)
            .collect();
        let errs = cut_errors(&cuts, &grid);
        assert!(errs.iter().all(|&e| e <= HALF_FRAME), "worst {:?}", errs.iter().cloned().fold(0.0, f32::max));
    }

    #[test]
    fn a_clip_at_the_end_of_its_chapter_slides_earlier() {
        let grid = beats(0.0, 0.5, 40);
        let mut c = clip(2.0, Some(0.4));
        c.in_start_secs = 8.0;
        c.source_end_secs = Some(10.0); // planned window ends exactly at the end
        let locked = lock_cuts(&[c], LockStart { output_secs: 0.0, seam_in_secs: 0.0 }, &grid, FPS);
        let l = locked[0];
        assert!(l.on_beat);
        assert!((l.duration_secs - 2.2).abs() < 1e-9, "{l:?}");
        assert!((l.in_start_secs - 7.8).abs() < 1e-9, "{l:?}");
    }

    #[test]
    fn a_short_chapter_takes_an_earlier_beat() {
        let grid = beats(0.0, 0.5, 40);
        let mut c = clip(2.0, Some(0.4));
        c.in_start_secs = 0.0;
        c.source_end_secs = Some(2.0); // can't read 2.2 s, can't slide
        let locked = lock_cuts(&[c], LockStart { output_secs: 0.0, seam_in_secs: 0.0 }, &grid, FPS);
        let l = locked[0];
        assert!(l.on_beat);
        // Cut at 1.5 s → length 1.5 + 0.2.
        assert!((l.duration_secs - 1.7).abs() < 1e-9, "{l:?}");
    }

    #[test]
    fn a_ramped_clip_without_spare_footage_drops_its_ramp() {
        let grid = beats(0.0, 0.5, 40);
        let mut c = clip(2.0, Some(0.1));
        c.in_start_secs = 0.0;
        c.source_end_secs = Some(2.25);
        c.tail_extra_secs = 0.5;
        c.seam_out_without_ramp = Some(0.4);
        let locked = lock_cuts(&[c], LockStart { output_secs: 0.0, seam_in_secs: 0.0 }, &grid, FPS);
        let l = locked[0];
        assert!(l.on_beat && l.drop_ramp, "{l:?}");
        // Cut on the planned beat (2.0 s): with the ramp that needs 2.07 s
        // + 0.5 s of tail; without it (0.4 s seam), 2.2 s, which fits.
        assert!((l.duration_secs - 2.2).abs() < 1e-9, "{l:?}");
    }

    #[test]
    fn the_last_clip_keeps_its_planned_length() {
        let grid = beats(0.0, 0.5, 40);
        let locked = lock_cuts(
            &[clip(2.0, Some(0.4)), clip(2.33, None)],
            LockStart { output_secs: 0.0, seam_in_secs: 0.0 },
            &grid,
            FPS,
        );
        assert!(locked[0].on_beat);
        assert!(!locked[1].on_beat);
        assert!((locked[1].duration_secs - 70.0 / 30.0).abs() < 1e-9, "{:?}", locked[1]);
    }

    #[test]
    fn intro_card_moves_by_at_most_half_a_beat() {
        let grid = beats(0.25, 0.5, 40);
        // 3 s card, 0.4 s seam: cut at 2.8 → beat 2.75 → card 2.95 s, which
        // rounds to 89 frames (2.967 s, cut at 2.767).
        let len = lock_card_end(3.0, 0.4, &grid, 0.5, FPS);
        assert!((len - 89.0 / 30.0).abs() < 1e-9, "{len}");
        assert!((len - 0.2 - 2.75).abs() <= HALF_FRAME as f64);
        // No beat within half a beat → unchanged.
        assert!((lock_card_end(3.0, 0.4, &[10.0], 0.5, FPS) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn stills_have_no_footage_limit() {
        let grid = beats(0.0, 0.5, 40);
        let mut c = clip(3.0, Some(0.4));
        c.source_end_secs = None;
        c.in_start_secs = 0.0;
        let locked = lock_cuts(&[c], LockStart { output_secs: 0.0, seam_in_secs: 0.0 }, &grid, FPS);
        assert!(locked[0].on_beat);
        assert!((locked[0].duration_secs - 3.2).abs() < 1e-9);
    }

    #[test]
    fn targets_bring_the_cuts_back_to_the_plan() {
        // Planned: cuts at 2.0, 4.0, 6.0 s. The first clip's chapter is
        // too short, so its cut comes a beat early (1.5 s); relative
        // planning would carry that slip to every later cut, the plan's
        // targets don't.
        let grid = beats(0.0, 0.5, 40);
        let mut first = clip(2.0, Some(0.4));
        first.in_start_secs = 0.0;
        first.source_end_secs = Some(2.0);
        first.target_secs = Some(2.0);
        let mut second = clip(2.0, Some(0.4));
        second.target_secs = Some(4.0);
        let mut third = clip(2.0, Some(0.4));
        third.target_secs = Some(6.0);
        let start = LockStart { output_secs: 0.0, seam_in_secs: 0.0 };
        let locked = lock_cuts(&[first, second, third], start, &grid, FPS);
        let durs: Vec<f32> = locked.iter().map(|l| l.duration_secs as f32).collect();
        let cuts: Vec<f32> = planned_cuts(&durs, &[0.4, 0.4], false, false, None, FPS)
            .into_iter()
            .map(|c| c.output_secs)
            .collect();
        assert!((cuts[0] - 1.5).abs() <= HALF_FRAME, "{cuts:?}");
        assert!((cuts[1] - 4.0).abs() <= HALF_FRAME, "{cuts:?}");
        assert!(locked.iter().all(|l| l.on_beat), "{locked:?}");

        // Without targets the slip carries on: the second cut at 3.5 s.
        let untargeted: Vec<LockClip> =
            [first, second].iter().map(|c| LockClip { target_secs: None, ..*c }).collect();
        let locked = lock_cuts(&untargeted, start, &grid, FPS);
        let d: Vec<f32> = locked.iter().map(|l| l.duration_secs as f32).collect();
        let cuts = planned_cuts(&d, &[0.4], false, false, None, FPS);
        assert!((cuts[0].output_secs - 1.5).abs() <= HALF_FRAME);
        // Starts at 1.3 s; 2.4 s long puts its cut (mid-crossfade) at 3.5 s.
        assert!((locked[1].duration_secs - 2.4).abs() < 1e-9, "{locked:?}");
    }

    #[test]
    fn the_last_clip_ends_the_video_on_its_target() {
        // Video end planned on the downbeat at 6.0 s.
        let grid = beats(0.0, 0.5, 40);
        let mut last = clip(2.33, None);
        last.target_secs = Some(6.0);
        let locked = lock_cuts(
            &[clip(2.0, Some(0.4)), clip(2.0, Some(0.4)), last],
            LockStart { output_secs: 0.0, seam_in_secs: 0.0 },
            &grid,
            FPS,
        );
        assert!(locked[2].on_beat);
        // Clips start at 0, 1.8, 3.8 (each overlaps the next by 0.4 s).
        let end = locked[0].duration_secs - 0.4 + locked[1].duration_secs - 0.4 + locked[2].duration_secs;
        assert!((end - 6.0).abs() <= 0.5 / FPS as f64 + 1e-9, "video ends at {end}");
        // A target the footage can't reach falls back to the planned length.
        let mut short = clip(2.0, None);
        short.in_start_secs = 0.0;
        short.source_end_secs = Some(2.1);
        short.target_secs = Some(3.0);
        let l = lock_cuts(&[short], LockStart { output_secs: 0.0, seam_in_secs: 0.0 }, &grid, FPS);
        assert!(!l[0].on_beat);
        assert!((l[0].duration_secs - 2.0).abs() < 1e-9);
    }
}
