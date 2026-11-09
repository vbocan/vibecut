//! Smart reframe: follow the action into a narrower (or wider) shape.
//!
//! `fit = "smart"` crops a 16:9 master into 9:16, 1:1 or 4:5 the way
//! `crop` does, but the crop window slides to keep the action in shot
//! instead of sitting in the middle. No model: everything is measured on
//! small grey frames of the finished video.
//!
//! 1. **Analysis** ([`analyse_video`]): the video is decoded at
//!    [`ANALYSIS_HZ`] frames a second, [`ANALYSIS_LONG_SIDE`] pixels on its
//!    long side. For each pair of frames ([`analyse_pair`]) the camera's
//!    own movement is estimated — a global shift, matched on the frames'
//!    column and row profiles — and taken out; whatever still differs,
//!    above the frame's own motion floor (the median block difference plus
//!    compression noise), is the action. Summed per column (and per row),
//!    with each column's detail (Laplacian) as a tie-breaker, it gives the
//!    best place for the crop window in that frame and how sure that is:
//!    how much of the motion the window holds, over what an even spread
//!    would put there ([`roi`]). Hard cuts the manifest doesn't know about
//!    show up as spikes of the raw frame difference
//!    ([`MotionTrack::detected_cuts`]).
//! 2. **Camera path** ([`plan_path`]): the video is split into shots at
//!    the cuts (from the manifest's `timeline`, plus detected ones). A shot
//!    without a clear subject in at least [`PathParams::min_follow_share`]
//!    of its frames keeps the centre crop. Otherwise the targets are
//!    median-filtered, low-motion holes hold the last target, and the
//!    result is low-passed without lag (the whole shot is known). A shot
//!    whose subject never leaves a dead zone around one place is locked
//!    off. Otherwise a virtual camera holds until the subject leaves the
//!    dead zone, follows it on a critically damped spring capped at a
//!    maximum pan speed, looking a little ahead (which offsets the
//!    spring's lag), holds again once it settles — and snaps at every cut
//!    instead of panning across it.
//! 3. **Rendering** ([`offset_expr`]): the path is simplified to a few
//!    linear pieces (constant pan rates) and becomes the crop filter's
//!    per-frame `x` (or `y`) expression: a sum of time-gated linear terms
//!    in `t`. The piece count is capped at [`MAX_PIECES`] — the
//!    simplification coarsens until the path fits — so the filter graph
//!    stays bounded however long the video is
//!    (`crate::output_format::reframe_graph_with_path`).

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use serde::Serialize;

use crate::output_timeline::OutputTimeline;

/// Analysis frames per second.
pub const ANALYSIS_HZ: f64 = 10.0;
/// Long side of the grey analysis frames, in pixels.
pub const ANALYSIS_LONG_SIDE: u32 = 192;
/// Grey levels of difference above the frame's motion floor that still
/// count as noise (compression, sensor).
const NOISE_LEVELS: f32 = 4.0;
/// Block size the motion floor is measured on.
const FLOOR_BLOCK: usize = 8;
/// Largest camera shift searched between two analysis frames, as a share
/// of the frame along that axis.
const MAX_SHIFT_SHARE: f64 = 0.08;
/// Cost per pixel of shift, in grey levels: a still camera wins near-ties.
const SHIFT_PENALTY: f32 = 0.05;
/// Mean residual motion per pixel (grey levels) below which a frame has
/// no clear action.
pub const MIN_ENERGY: f64 = 0.04;
/// Weight of detail against motion in the window score (a tie-breaker:
/// motion sums to 1, detail to this).
const DETAIL_WEIGHT: f64 = 0.1;
/// A scene cut: a frame pair whose raw mean difference is at least this
/// many times the median of its neighbours'...
const CUT_RATIO: f32 = 3.0;
/// ...and at least this many grey levels.
const CUT_MIN_DIFF: f32 = 12.0;
/// Neighbours either side a detected cut is measured against.
const CUT_CONTEXT: usize = 5;
/// Cuts closer than this are one cut; shots shorter than this don't count.
const MIN_SHOT_SECS: f64 = 0.2;
/// A detected cut this close to a manifest cut is that cut.
const CUT_MATCH_SECS: f64 = 0.25;
/// Most linear pieces a camera path is rendered with: bounds the crop
/// expression (about 60 characters a piece) and so the filter graph.
pub const MAX_PIECES: usize = 200;
/// Simplification tolerance to start from, as a share of the frame (about
/// 1.7 px of a 9:16 1080×1920 output).
const BASE_TOLERANCE: f64 = 0.0005;
/// Coarsest simplification before shots are flattened to constants. Only
/// a long, busy video gets anywhere near it (a 30 s reel needs a few dozen
/// pieces at [`BASE_TOLERANCE`]).
const MAX_TOLERANCE: f64 = 0.25;

/// The axis a crop window slides along.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PanAxis {
    /// A narrower shape (9:16 from 16:9): the window slides left/right.
    X,
    /// A wider shape (16:9 from 9:16): the window slides up/down.
    Y,
}

/// How a `src` frame is cropped to `target`'s shape (after scaling to
/// cover it): the axis the window slides along and the window's share of
/// the frame on that axis. `None` when the aspects match.
pub fn crop_window(src: (u32, u32), target: (u32, u32)) -> Option<(PanAxis, f64)> {
    let sa = src.0 as f64 / src.1.max(1) as f64;
    let ta = target.0 as f64 / target.1.max(1) as f64;
    if !(sa > 0.0 && ta > 0.0) {
        return None;
    }
    let r = ta / sa;
    if r < 0.99 {
        Some((PanAxis::X, r))
    } else if r > 1.01 {
        Some((PanAxis::Y, 1.0 / r))
    } else {
        None
    }
}

/// Size of the analysis frames for a `src`-shaped video: the long side
/// [`ANALYSIS_LONG_SIDE`], the short side in proportion (even).
pub fn analysis_size(src: (u32, u32)) -> (u32, u32) {
    let (sw, sh) = (src.0.max(1) as f64, src.1.max(1) as f64);
    let long = ANALYSIS_LONG_SIDE as f64;
    let even = |v: f64| (((v / 2.0).round() as u32) * 2).max(16);
    if sw >= sh {
        (ANALYSIS_LONG_SIDE, even(long * sh / sw))
    } else {
        (even(long * sw / sh), ANALYSIS_LONG_SIDE)
    }
}

// ---------- per-frame analysis ----------

/// One analysed pair of frames.
#[derive(Debug, Clone, PartialEq)]
pub struct MotionSample {
    /// Seconds into the video: the middle of the pair.
    pub t: f64,
    /// Residual motion (camera movement and the frame's motion floor taken
    /// out), summed per column / per row.
    pub motion_cols: Vec<f32>,
    pub motion_rows: Vec<f32>,
    /// Detail (|Laplacian|) of the later frame, summed per column / row.
    pub detail_cols: Vec<f32>,
    pub detail_rows: Vec<f32>,
    /// Raw mean absolute difference of the pair; spikes at a cut.
    pub mean_diff: f32,
}

/// Mean grey level per column (`X`) or per row (`Y`), mean-centred.
fn profile(frame: &[u8], w: usize, h: usize, axis: PanAxis) -> Vec<f32> {
    let mut p = match axis {
        PanAxis::X => vec![0f32; w],
        PanAxis::Y => vec![0f32; h],
    };
    for y in 0..h {
        for x in 0..w {
            let v = frame[y * w + x] as f32;
            match axis {
                PanAxis::X => p[x] += v,
                PanAxis::Y => p[y] += v,
            }
        }
    }
    let across = match axis {
        PanAxis::X => h,
        PanAxis::Y => w,
    } as f32;
    p.iter_mut().for_each(|v| *v /= across.max(1.0));
    let mean = p.iter().sum::<f32>() / p.len().max(1) as f32;
    p.iter_mut().for_each(|v| *v -= mean);
    p
}

/// The shift `s` that best lines `cur` up with `prev` (`cur[i] ≈
/// prev[i - s]`), within ±`max`. A small per-pixel penalty and the search
/// order (0, −1, 1, −2, …) make a still camera win near-ties.
fn best_shift(prev: &[f32], cur: &[f32], max: usize) -> i32 {
    let n = prev.len().min(cur.len()) as i64;
    let mut best = (f32::INFINITY, 0i32);
    for k in 0..=(2 * max as i64) {
        let s = if k % 2 == 0 { k / 2 } else { -(k + 1) / 2 };
        let (lo, hi) = (s.max(0), n + s.min(0));
        if hi - lo < n / 2 || hi <= lo {
            continue;
        }
        let mut sum = 0f32;
        for i in lo..hi {
            sum += (cur[i as usize] - prev[(i - s) as usize]).abs();
        }
        let cost = sum / (hi - lo) as f32 + SHIFT_PENALTY * s.unsigned_abs() as f32;
        if cost < best.0 {
            best = (cost, s as i32);
        }
    }
    best.1
}

fn median(v: &mut [f32]) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    let mid = v.len() / 2;
    v.select_nth_unstable_by(mid, f32::total_cmp);
    Some(v[mid])
}

/// Analyse one pair of `w × h` grey frames, `t` seconds into the video.
pub fn analyse_pair(prev: &[u8], cur: &[u8], w: usize, h: usize, t: f64) -> MotionSample {
    let n = w * h;
    let mut sample = MotionSample {
        t,
        motion_cols: vec![0.0; w],
        motion_rows: vec![0.0; h],
        detail_cols: vec![0.0; w],
        detail_rows: vec![0.0; h],
        mean_diff: 0.0,
    };
    if w < 3 || h < 3 || prev.len() < n || cur.len() < n {
        return sample;
    }
    // The camera's own movement, from the column and row profiles.
    let max_dx = ((w as f64 * MAX_SHIFT_SHARE).round() as usize).max(1);
    let max_dy = ((h as f64 * MAX_SHIFT_SHARE).round() as usize).max(1);
    let dx = best_shift(&profile(prev, w, h, PanAxis::X), &profile(cur, w, h, PanAxis::X), max_dx);
    let dy = best_shift(&profile(prev, w, h, PanAxis::Y), &profile(cur, w, h, PanAxis::Y), max_dy);
    // Where both frames see the same scene after that shift.
    let x0 = dx.max(0) as usize;
    let x1 = (w as i32 + dx.min(0)).max(0) as usize;
    let y0 = dy.max(0) as usize;
    let y1 = (h as i32 + dy.min(0)).max(0) as usize;
    let mut diff = vec![0f32; n];
    for y in y0..y1 {
        let py = (y as i32 - dy) as usize;
        for x in x0..x1 {
            let px = (x as i32 - dx) as usize;
            diff[y * w + x] = (cur[y * w + x] as f32 - prev[py * w + px] as f32).abs();
        }
    }
    // The frame's motion floor: what a typical block differs by (camera
    // shake, parallax the shift couldn't take out, noise).
    let mut blocks = Vec::new();
    let mut by = y0;
    while by + FLOOR_BLOCK <= y1 {
        let mut bx = x0;
        while bx + FLOOR_BLOCK <= x1 {
            let mut s = 0f32;
            for y in by..by + FLOOR_BLOCK {
                for x in bx..bx + FLOOR_BLOCK {
                    s += diff[y * w + x];
                }
            }
            blocks.push(s / (FLOOR_BLOCK * FLOOR_BLOCK) as f32);
            bx += FLOOR_BLOCK;
        }
        by += FLOOR_BLOCK;
    }
    let floor = median(&mut blocks).unwrap_or(0.0) + NOISE_LEVELS;
    for y in y0..y1 {
        for x in x0..x1 {
            let r = diff[y * w + x] - floor;
            if r > 0.0 {
                sample.motion_cols[x] += r;
                sample.motion_rows[y] += r;
            }
        }
    }
    // Detail of the later frame: 4-neighbour |Laplacian|.
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let c = cur[y * w + x] as i32;
            let lap = (4 * c
                - cur[(y - 1) * w + x] as i32
                - cur[(y + 1) * w + x] as i32
                - cur[y * w + x - 1] as i32
                - cur[y * w + x + 1] as i32)
                .unsigned_abs() as f32;
            sample.detail_cols[x] += lap;
            sample.detail_rows[y] += lap;
        }
    }
    let raw: f64 = cur[..n].iter().zip(&prev[..n]).map(|(a, b)| (*a as f64 - *b as f64).abs()).sum();
    sample.mean_diff = (raw / n as f64) as f32;
    sample
}

/// Where the crop window should sit in one frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Roi {
    /// Window centre, as a share of the frame along the axis.
    pub centre: f64,
    /// 0 (the motion is spread evenly) to 1 (all of it inside the window).
    pub confidence: f64,
}

/// The best place for a crop window `window` wide (a share of the frame
/// along `axis`) in `sample`: the window holding the most motion (detail
/// breaks ties), centred on that motion. `None` when the frame has no
/// clear motion ([`MIN_ENERGY`]).
pub fn roi(sample: &MotionSample, axis: PanAxis, window: f64) -> Option<Roi> {
    let (m, d, across) = match axis {
        PanAxis::X => (&sample.motion_cols, &sample.detail_cols, sample.motion_rows.len()),
        PanAxis::Y => (&sample.motion_rows, &sample.detail_rows, sample.motion_cols.len()),
    };
    let n = m.len();
    if n == 0 || across == 0 {
        return None;
    }
    let total_m: f64 = m.iter().map(|&v| v as f64).sum();
    // A frame without motion (or a NaN from a broken one) has no target.
    let energy = total_m / (n * across) as f64;
    if energy.is_nan() || energy < MIN_ENERGY {
        return None;
    }
    let total_d: f64 = d.iter().map(|&v| v as f64).sum();
    let score: Vec<f64> = (0..n)
        .map(|i| {
            let detail = if total_d > 0.0 { DETAIL_WEIGHT * d[i] as f64 / total_d } else { 0.0 };
            m[i] as f64 / total_m + detail
        })
        .collect();
    let win = ((window.clamp(0.0, 1.0) * n as f64).round() as usize).clamp(1, n);
    let mut prefix = vec![0.0; n + 1];
    for i in 0..n {
        prefix[i + 1] = prefix[i] + score[i];
    }
    let (mut best, mut best_sum) = (0usize, f64::MIN);
    for s in 0..=(n - win) {
        let v = prefix[s + win] - prefix[s];
        if v > best_sum + 1e-12 {
            best_sum = v;
            best = s;
        }
    }
    let inside = m[best..best + win].iter().map(|&v| v as f64).sum::<f64>() / total_m;
    let even = win as f64 / n as f64;
    let confidence = if even >= 1.0 { 0.0 } else { ((inside - even) / (1.0 - even)).clamp(0.0, 1.0) };
    // Centre on the motion inside the window, not the window's own middle:
    // with a small subject many windows hold it all.
    let (mut num, mut den) = (0.0, 0.0);
    for (i, s) in score.iter().enumerate().skip(best).take(win) {
        num += (i as f64 + 0.5) * s;
        den += s;
    }
    let half = win as f64 / 2.0;
    let centre_bins = if den > 0.0 { num / den } else { best as f64 + half };
    Some(Roi { centre: centre_bins.clamp(half, n as f64 - half) / n as f64, confidence })
}

/// Every analysed frame pair of a video.
#[derive(Debug, Clone, PartialEq)]
pub struct MotionTrack {
    pub sample_hz: f64,
    pub width: usize,
    pub height: usize,
    /// Frames decoded (one more than `samples`).
    pub frames: usize,
    pub samples: Vec<MotionSample>,
}

impl MotionTrack {
    /// Seconds analysed.
    pub fn duration_secs(&self) -> f64 {
        self.frames as f64 / self.sample_hz
    }

    /// Per sample: its time and, when its frame has a subject clear enough
    /// (`min_confidence`), the window centre on it.
    pub fn targets(&self, axis: PanAxis, window: f64, min_confidence: f64) -> Vec<(f64, Option<f64>)> {
        self.samples
            .iter()
            .map(|s| {
                let c = roi(s, axis, window).filter(|r| r.confidence >= min_confidence).map(|r| r.centre);
                (s.t, c)
            })
            .collect()
    }

    /// Hard cuts seen in the pictures: a spike of the raw frame difference
    /// against its neighbours. Seconds, the middle of the pair.
    pub fn detected_cuts(&self) -> Vec<f64> {
        let d: Vec<f32> = self.samples.iter().map(|s| s.mean_diff).collect();
        let mut cuts = Vec::new();
        for i in 0..d.len() {
            if d[i] < CUT_MIN_DIFF {
                continue;
            }
            let lo = i.saturating_sub(CUT_CONTEXT);
            let hi = (i + CUT_CONTEXT + 1).min(d.len());
            let mut around: Vec<f32> = (lo..hi).filter(|&j| j != i).map(|j| d[j]).collect();
            let typical = median(&mut around).unwrap_or(0.0).max(1.0);
            // The larger of two neighbouring spikes (a cut that straddles
            // two analysis frames) is the cut.
            let peak = (i == 0 || d[i] >= d[i - 1]) && (i + 1 >= d.len() || d[i] > d[i + 1]);
            if d[i] >= CUT_RATIO * typical && peak {
                cuts.push(self.samples[i].t);
            }
        }
        cuts
    }
}

/// Builds a [`MotionTrack`] one decoded frame at a time.
pub struct TrackBuilder {
    track: MotionTrack,
    prev: Option<Vec<u8>>,
}

impl TrackBuilder {
    pub fn new(width: usize, height: usize, sample_hz: f64) -> Self {
        TrackBuilder {
            track: MotionTrack { sample_hz, width, height, frames: 0, samples: Vec::new() },
            prev: None,
        }
    }

    pub fn push(&mut self, frame: &[u8]) {
        let (w, h) = (self.track.width, self.track.height);
        let i = self.track.frames;
        if let Some(prev) = &self.prev {
            let t = (i as f64 - 0.5) / self.track.sample_hz;
            self.track.samples.push(analyse_pair(prev, frame, w, h, t));
        }
        if let Some(p) = self.prev.as_mut() {
            p.copy_from_slice(&frame[..w * h]);
        } else {
            self.prev = Some(frame[..w * h].to_vec());
        }
        self.track.frames += 1;
    }

    pub fn finish(self) -> MotionTrack {
        self.track
    }
}

// ---------- the camera path ----------

/// How the virtual camera follows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathParams {
    /// The camera holds still while the subject stays within this distance
    /// (a share of the crop window) of where it holds; a shot whose subject
    /// never strays further from one place is locked off.
    pub dead_zone: f64,
    /// Fastest pan, in crop windows a second.
    pub max_speed: f64,
    /// Time constant of the critically damped follow, seconds; the targets
    /// are also low-passed over about this long (without lag: the whole
    /// shot is known).
    pub smooth_secs: f64,
    /// How far ahead the camera looks: it starts moving before the subject
    /// gets away, offsetting the follow's lag.
    pub lead_secs: f64,
    /// A shot is followed only when at least this share of its frames has
    /// a clear subject; otherwise it keeps the centre crop.
    pub min_follow_share: f64,
    /// [`Roi::confidence`] a frame's subject needs to count.
    pub min_confidence: f64,
}

impl Default for PathParams {
    fn default() -> Self {
        PathParams {
            dead_zone: 0.15,
            max_speed: 0.6,
            smooth_secs: 0.45,
            lead_secs: 0.4,
            min_follow_share: 0.3,
            min_confidence: 0.3,
        }
    }
}

/// One stretch of the path at a constant pan rate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PathPiece {
    pub t0: f64,
    pub t1: f64,
    /// Window centre at `t0` and at `t1`, as a share of the frame along the
    /// axis (0.5 = the centre crop).
    pub c0: f64,
    pub c1: f64,
}

/// Where the crop window is over the whole video: linear pieces covering
/// `[0, duration_secs)` in time order, jumping only at cuts.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CameraPath {
    pub axis: PanAxis,
    /// The window's share of the frame along `axis`.
    pub window: f64,
    pub duration_secs: f64,
    pub pieces: Vec<PathPiece>,
    pub shots: usize,
    /// Shots with a clear subject, followed; the rest keep the centre.
    pub followed_shots: usize,
}

impl CameraPath {
    /// Window centre at `t`.
    pub fn at(&self, t: f64) -> f64 {
        let Some(last) = self.pieces.last() else { return 0.5 };
        let p = self
            .pieces
            .iter()
            .find(|p| t < p.t1)
            .unwrap_or(last);
        if p.t1 > p.t0 {
            let f = ((t - p.t0) / (p.t1 - p.t0)).clamp(0.0, 1.0);
            p.c0 + (p.c1 - p.c0) * f
        } else {
            p.c0
        }
    }

    /// The crop offset at `t` as a share of its free range (0 = the left
    /// or top edge, 1 = the right or bottom one).
    pub fn offset_at(&self, t: f64) -> f64 {
        offset_of(self.at(t), self.window)
    }

    /// Whether the window stays at the centre over `[from, to)`.
    pub fn is_centred(&self, from: f64, to: Option<f64>) -> bool {
        let to = to.unwrap_or(f64::INFINITY);
        self.pieces
            .iter()
            .filter(|p| p.t1 > from && p.t0 < to)
            .all(|p| (p.c0 - 0.5).abs() < 1e-6 && (p.c1 - 0.5).abs() < 1e-6)
    }

    /// How steady the path is, frame by frame at `fps`: the cuts it jumps
    /// at, the largest move between two frames of the same shot (a share
    /// of the frame), and how often a pan turns back on itself.
    pub fn smoothness(&self, fps: f64) -> Smoothness {
        let jumps: Vec<f64> = self
            .pieces
            .windows(2)
            .filter(|w| (w[0].c1 - w[1].c0).abs() > 1e-9)
            .map(|w| w[1].t0)
            .collect();
        let frames = (self.duration_secs * fps).floor() as usize;
        let (mut max_step, mut reversals, mut last_dir) = (0.0f64, 0usize, 0i8);
        for k in 1..frames {
            let (t0, t1) = ((k - 1) as f64 / fps, k as f64 / fps);
            if jumps.iter().any(|&j| j > t0 && j <= t1) {
                last_dir = 0;
                continue;
            }
            let step = self.at(t1) - self.at(t0);
            max_step = max_step.max(step.abs());
            let dir = if step > 1e-6 { 1 } else if step < -1e-6 { -1 } else { 0 };
            if dir != 0 {
                if last_dir != 0 && dir != last_dir {
                    reversals += 1;
                }
                last_dir = dir;
            }
        }
        Smoothness { jumps, max_step, reversals }
    }

    /// One line for logs: how much of the video it followed.
    pub fn describe(&self) -> String {
        if self.followed_shots == 0 {
            return format!("no clear action in {} shot{}: centre crop", self.shots, plural(self.shots));
        }
        format!(
            "followed the action in {} of {} shot{} ({} pan piece{})",
            self.followed_shots,
            self.shots,
            plural(self.shots),
            self.pieces.len(),
            plural(self.pieces.len())
        )
    }

    /// A compact JSON record: axis, window, shot counts and the pieces as
    /// `[t0, t1, c0, c1]`, rounded.
    pub fn to_json(&self) -> serde_json::Value {
        let r = |v: f64, d: i32| {
            let k = 10f64.powi(d);
            (v * k).round() / k
        };
        serde_json::json!({
            "axis": self.axis,
            "window": r(self.window, 4),
            "shots": self.shots,
            "followed_shots": self.followed_shots,
            "pieces": self.pieces.iter().map(|p| [r(p.t0, 3), r(p.t1, 3), r(p.c0, 4), r(p.c1, 4)]).collect::<Vec<_>>(),
        })
    }
}

/// See [`CameraPath::smoothness`].
#[derive(Debug, Clone, PartialEq)]
pub struct Smoothness {
    /// Where the window jumps (cuts), seconds.
    pub jumps: Vec<f64>,
    /// Largest frame-to-frame move within a shot, a share of the frame.
    pub max_step: f64,
    /// Times a pan turned back within a shot.
    pub reversals: usize,
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// A window centre `c` as a share of the crop's free range.
fn offset_of(c: f64, window: f64) -> f64 {
    if window >= 1.0 {
        0.5
    } else {
        ((c - window / 2.0) / (1.0 - window)).clamp(0.0, 1.0)
    }
}

/// Shot boundaries for `cuts` over `[0, duration)`: sorted, merged when
/// closer than [`MIN_SHOT_SECS`], clear of both ends.
pub fn shot_bounds(cuts: &[f64], duration: f64) -> Vec<(f64, f64)> {
    let mut c: Vec<f64> = cuts
        .iter()
        .copied()
        .filter(|t| t.is_finite() && *t >= MIN_SHOT_SECS && *t <= duration - MIN_SHOT_SECS)
        .collect();
    c.sort_by(f64::total_cmp);
    let mut bounds = vec![0.0];
    for t in c {
        if t - bounds.last().copied().unwrap_or(0.0) >= MIN_SHOT_SECS {
            bounds.push(t);
        }
    }
    bounds.push(duration.max(0.0));
    bounds.windows(2).map(|w| (w[0], w[1])).collect()
}

/// The manifest's cuts and the ones seen in the pictures: a detected cut
/// near a manifest one is that cut (the manifest's time is exact).
pub fn merge_cuts(manifest: &[f64], detected: &[f64]) -> Vec<f64> {
    let mut all: Vec<f64> = manifest.to_vec();
    all.extend(detected.iter().copied().filter(|d| manifest.iter().all(|m| (m - d).abs() > CUT_MATCH_SECS)));
    all.sort_by(f64::total_cmp);
    all
}

/// Every cut in `timeline`, output seconds (mid-crossfade), in order.
pub fn timeline_cuts(timeline: &OutputTimeline) -> Vec<f64> {
    let mut cuts = Vec::new();
    if let (Some(intro), Some(_)) = (&timeline.intro, timeline.clips.first()) {
        cuts.push(intro.cut_secs);
    }
    for w in timeline.clips.windows(2) {
        cuts.push(w[1].cut_in_secs);
    }
    if let (Some(outro), Some(_)) = (&timeline.outro, timeline.clips.last()) {
        cuts.push(outro.cut_secs);
    }
    cuts
}

/// One step of the critically damped follow towards `goal` (the
/// "smooth damp" spring, stable for any `dt`), never overshooting and
/// never faster than `vmax`. Returns the new position; updates `v`.
fn follow_step(c: f64, goal: f64, v: &mut f64, dt: f64, smooth: f64, vmax: f64) -> f64 {
    if dt <= 0.0 {
        return c;
    }
    let omega = 2.0 / smooth.max(1e-3);
    let x = omega * dt;
    let decay = 1.0 / (1.0 + x + 0.48 * x * x + 0.235 * x * x * x);
    let change = c - goal;
    let temp = (*v + omega * change) * dt;
    let mut nv = (*v - omega * temp) * decay;
    let mut next = goal + (change + temp) * decay;
    if (goal - c > 0.0) == (next > goal) {
        next = goal;
        nv = 0.0;
    }
    let step = (next - c).clamp(-vmax * dt, vmax * dt);
    if step != next - c {
        nv = step / dt;
    }
    *v = nv.clamp(-vmax, vmax);
    c + step
}

/// Median of each present value with its present neighbours (±2).
fn median_filter(v: &[Option<f64>]) -> Vec<Option<f64>> {
    (0..v.len())
        .map(|i| {
            v[i]?;
            let mut around: Vec<f64> = v[i.saturating_sub(2)..(i + 3).min(v.len())].iter().flatten().copied().collect();
            let mid = around.len() / 2;
            around.select_nth_unstable_by(mid, f64::total_cmp);
            Some(around[mid])
        })
        .collect()
}

/// The camera over one shot `[a, b)`: `None` when it has no clear
/// subject (centre crop), else `(t, centre)` points from `a` to `b`.
/// `cut_in` / `cut_out`: the shot starts / ends at a cut (not at an end of
/// the video). `targets` are in time order.
fn follow_shot(
    targets: &[(f64, Option<f64>)],
    (a, b): (f64, f64),
    (cut_in, cut_out): (bool, bool),
    window: f64,
    hz: f64,
    p: &PathParams,
) -> Option<Vec<(f64, f64)>> {
    let half_pair = 0.5 / hz;
    const EPS: f64 = 1e-6;
    let first = targets.partition_point(|(t, _)| *t < a);
    let end = targets.partition_point(|(t, _)| *t < b);
    // A pair (frames at t ± half_pair) that straddles a cut measures the
    // cut, not the action: one whose later frame is the cut's first.
    let straddles = |t: f64| (cut_in && t - a < half_pair - EPS) || (cut_out && b - t <= half_pair + EPS);
    let shot: Vec<(f64, Option<f64>)> = targets[first..end.max(first)]
        .iter()
        .map(|&(t, c)| (t, c.filter(|_| !straddles(t))))
        .collect();
    if shot.is_empty() {
        return None;
    }
    let raw: Vec<Option<f64>> = shot.iter().map(|(_, c)| *c).collect();
    let present = raw.iter().flatten().count();
    if (present as f64) < p.min_follow_share * shot.len() as f64 || present == 0 {
        return None;
    }
    // Hold through low motion; before the first clear frame, aim where it is.
    let filtered = median_filter(&raw);
    let first = filtered.iter().flatten().next().copied()?;
    let mut held = Vec::with_capacity(filtered.len());
    let mut last = first;
    for c in &filtered {
        if let Some(c) = c {
            last = *c;
        }
        held.push(last);
    }
    // Zero-phase low-pass: the whole shot is known, so smoothing costs no lag.
    let smooth = gaussian_smooth(&held, p.smooth_secs * hz / 2.0);
    let (lo, hi) = if window < 1.0 { (window / 2.0, 1.0 - window / 2.0) } else { (0.5, 0.5) };
    let lead = (p.lead_secs * hz).round().max(0.0) as usize;
    let n = smooth.len();
    let ahead = |i: usize| smooth[(i + lead).min(n - 1)].clamp(lo, hi);
    let dead = p.dead_zone * window;
    let vmax = p.max_speed * window;
    let stop = STOP_SPEED * window;
    // A subject that never strays further than the dead zone from one
    // place: a locked-off shot, no movement at all.
    let (min, max) = (0..n).map(ahead).fold((f64::MAX, f64::MIN), |(lo, hi), c| (lo.min(c), hi.max(c)));
    if max - min <= 2.0 * dead {
        let c = (min + max) / 2.0;
        return Some(vec![(a, c), (b, c)]);
    }
    // Snap at the cut: the shot opens already aimed, at rest. The camera
    // holds while the subject stays within the dead zone of where it holds;
    // once it leaves, the camera follows it (on the spring, capped at the
    // pan speed) until it settles again, and holds there.
    let mut hold = Some(ahead(0));
    let mut c = ahead(0);
    let mut v = 0.0;
    let mut points = vec![(a, c)];
    let mut at = a;
    const SUBSTEPS: usize = 4;
    for (i, (t, _)) in shot.iter().enumerate() {
        let want = ahead(i);
        let speed = (ahead((i + 1).min(n - 1)) - ahead(i.saturating_sub(1))).abs() * hz / 2.0;
        match hold {
            Some(h) if (want - h).abs() > dead => hold = None,
            None if speed < stop && (want - c).abs() < dead / 2.0 => hold = Some(want),
            _ => {}
        }
        let goal = hold.unwrap_or(want);
        let dt = (t - at) / SUBSTEPS as f64;
        for _ in 0..SUBSTEPS {
            c = follow_step(c, goal, &mut v, dt, p.smooth_secs, vmax).clamp(lo, hi);
        }
        at = *t;
        points.push((*t, c));
    }
    // To the end of the shot.
    let goal = hold.unwrap_or_else(|| ahead(n - 1));
    let dt = (b - at) / SUBSTEPS as f64;
    for _ in 0..SUBSTEPS {
        c = follow_step(c, goal, &mut v, dt, p.smooth_secs, vmax).clamp(lo, hi);
    }
    points.push((b, c));
    Some(points)
}

/// Below this speed (crop windows a second) a followed subject has
/// settled, and the camera holds again.
const STOP_SPEED: f64 = 0.1;

/// Gaussian low-pass of `v` (sigma in samples), weights renormalised at
/// the ends so a shot's edges aren't pulled towards zero.
fn gaussian_smooth(v: &[f64], sigma: f64) -> Vec<f64> {
    if sigma < 0.5 || v.len() < 2 {
        return v.to_vec();
    }
    let r = (3.0 * sigma).ceil() as usize;
    let k: Vec<f64> = (0..=r).map(|d| (-(d as f64).powi(2) / (2.0 * sigma * sigma)).exp()).collect();
    (0..v.len())
        .map(|i| {
            let (lo, hi) = (i.saturating_sub(r), (i + r).min(v.len() - 1));
            let (mut s, mut w) = (0.0, 0.0);
            for (j, x) in v.iter().enumerate().take(hi + 1).skip(lo) {
                let kw = k[i.abs_diff(j)];
                s += kw * x;
                w += kw;
            }
            s / w
        })
        .collect()
}

/// Ramer–Douglas–Peucker on `(t, c)` points, by the error in `c` at each
/// point's time: no kept piece strays more than `tol` from the points it
/// replaces.
fn simplify(points: &[(f64, f64)], tol: f64) -> Vec<(f64, f64)> {
    let n = points.len();
    if n <= 2 {
        return points.to_vec();
    }
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;
    let mut stack = vec![(0usize, n - 1)];
    while let Some((a, b)) = stack.pop() {
        let (ta, ca) = points[a];
        let (tb, cb) = points[b];
        let mut worst = (0.0f64, 0usize);
        for (i, &(t, c)) in points.iter().enumerate().take(b).skip(a + 1) {
            let line = if tb > ta { ca + (cb - ca) * (t - ta) / (tb - ta) } else { ca };
            let e = (c - line).abs();
            if e > worst.0 {
                worst = (e, i);
            }
        }
        if worst.0 > tol {
            keep[worst.1] = true;
            stack.push((a, worst.1));
            stack.push((worst.1, b));
        }
    }
    points.iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| *p).collect()
}

/// Pieces of one shot's polyline (or one centre piece).
fn shot_pieces(shot: (f64, f64), points: Option<&Vec<(f64, f64)>>, tol: f64) -> Vec<PathPiece> {
    match points {
        None => vec![PathPiece { t0: shot.0, t1: shot.1, c0: 0.5, c1: 0.5 }],
        Some(pts) => simplify(pts, tol)
            .windows(2)
            .filter(|w| w[1].0 > w[0].0)
            .map(|w| PathPiece { t0: w[0].0, t1: w[1].0, c0: w[0].1, c1: w[1].1 })
            .collect(),
    }
}

/// Plan the camera over a video `duration` seconds long from per-sample
/// `targets` (see [`MotionTrack::targets`]), cut into shots at `cuts`.
/// Never more than [`MAX_PIECES`] pieces: the simplification tolerance
/// doubles until the path fits; past [`MAX_TOLERANCE`] each shot holds its
/// mean position, and the closest neighbouring shots merge.
pub fn plan_path(
    targets: &[(f64, Option<f64>)],
    cuts: &[f64],
    duration: f64,
    axis: PanAxis,
    window: f64,
    hz: f64,
    params: &PathParams,
) -> CameraPath {
    let shots = shot_bounds(cuts, duration);
    let n = shots.len();
    let followed: Vec<Option<Vec<(f64, f64)>>> = shots
        .iter()
        .enumerate()
        .map(|(k, &s)| follow_shot(targets, s, (k > 0, k + 1 < n), window, hz, params))
        .collect();
    let followed_shots = followed.iter().filter(|f| f.is_some()).count();
    let mut tol = BASE_TOLERANCE;
    let mut pieces: Vec<PathPiece>;
    loop {
        pieces = shots.iter().zip(&followed).flat_map(|(&s, f)| shot_pieces(s, f.as_ref(), tol)).collect();
        if pieces.len() <= MAX_PIECES || tol >= MAX_TOLERANCE {
            break;
        }
        tol *= 2.0;
    }
    if pieces.len() > MAX_PIECES {
        pieces = flatten_shots(&shots, &followed);
    }
    CameraPath { axis, window, duration_secs: duration, pieces, shots: shots.len(), followed_shots }
}

/// Last resort for a path with more shots than [`MAX_PIECES`]: each shot
/// holds its mean position, and the neighbours closest in position merge
/// (time-weighted) until it fits.
fn flatten_shots(shots: &[(f64, f64)], followed: &[Option<Vec<(f64, f64)>>]) -> Vec<PathPiece> {
    let mut flat: Vec<(f64, f64, f64)> = shots
        .iter()
        .zip(followed)
        .map(|(&(a, b), f)| {
            let c = match f {
                Some(pts) if !pts.is_empty() => pts.iter().map(|p| p.1).sum::<f64>() / pts.len() as f64,
                _ => 0.5,
            };
            (a, b, c)
        })
        .collect();
    while flat.len() > MAX_PIECES {
        let i = (0..flat.len() - 1)
            .min_by(|&i, &j| {
                let di = (flat[i].2 - flat[i + 1].2).abs();
                let dj = (flat[j].2 - flat[j + 1].2).abs();
                di.total_cmp(&dj)
            })
            .unwrap_or(0);
        let (a, m, c1) = flat[i];
        let (_, b, c2) = flat[i + 1];
        let (w1, w2) = (m - a, b - m);
        let c = if w1 + w2 > 0.0 { (c1 * w1 + c2 * w2) / (w1 + w2) } else { c1 };
        flat[i] = (a, b, c);
        flat.remove(i + 1);
    }
    flat.into_iter().map(|(a, b, c)| PathPiece { t0: a, t1: b, c0: c, c1: c }).collect()
}

// ---------- rendering ----------

/// A time for the expression, floored to the millisecond: a boundary at a
/// frame's exact time (a hard cut) then still includes that frame.
fn ms_floor(t: f64) -> String {
    format!("{:.3}", (t.max(0.0) * 1000.0 + 1e-6).floor() / 1000.0)
}

/// The crop offset, as a share of its free range, as an ffmpeg expression
/// in the crop filter's `t`, over `[from, to)` of `path` (`to = None`: to
/// the end), with `t` counted from `from` (a piece of a split timeline
/// starts its own clock at 0). A sum of gated linear terms:
/// `lt(t,T1)*(P0+S*(t-T0))+gte(t,T1)*lt(t,T2)*P1+…`; the first term has no
/// lower gate and the last no upper one, so every frame gets exactly one.
pub fn offset_expr(path: &CameraPath, from: f64, to: Option<f64>) -> String {
    let to = to.unwrap_or(f64::INFINITY);
    let w = path.window;
    // (start, end, offset at start, slope), in piece time.
    let mut terms: Vec<(f64, f64, f64, f64)> = Vec::new();
    for pc in &path.pieces {
        let (a, b) = (pc.t0.max(from), pc.t1.min(to));
        if b <= a {
            continue;
        }
        let at = |t: f64| {
            let f = if pc.t1 > pc.t0 { (t - pc.t0) / (pc.t1 - pc.t0) } else { 0.0 };
            offset_of(pc.c0 + (pc.c1 - pc.c0) * f, w)
        };
        let (pa, pb) = (at(a), at(b));
        let slope = if b > a && b.is_finite() { (pb - pa) / (b - a) } else { 0.0 };
        let term = (a - from, b - from, pa, slope);
        // Two constant pieces at the same offset are one.
        match terms.last_mut() {
            Some(last) if last.3.abs() < 1e-7 && slope.abs() < 1e-7 && (last.2 - pa).abs() < 5e-5 => last.1 = term.1,
            _ => terms.push(term),
        }
    }
    if terms.is_empty() {
        return format!("{:.4}", path.offset_at(from));
    }
    let n = terms.len();
    terms
        .iter()
        .enumerate()
        .map(|(k, &(a, b, p, s))| {
            let body = if s.abs() < 1e-7 {
                format!("{p:.4}")
            } else {
                format!("({p:.4}{s:+.6}*(t-{}))", ms_floor(a))
            };
            let mut gate = String::new();
            if k > 0 {
                gate.push_str(&format!("gte(t,{})*", ms_floor(a)));
            }
            if k + 1 < n {
                gate.push_str(&format!("lt(t,{})*", ms_floor(b)));
            }
            format!("{gate}{body}")
        })
        .collect::<Vec<_>>()
        .join("+")
}

// ---------- decoding ----------

/// Decode `video` (a `src`-shaped finished render) to small grey frames
/// at [`ANALYSIS_HZ`] and analyse every pair. Reports 0..1 of the video
/// read through `on_progress`. Cancellable like the render's ffmpeg runs
/// ([`crate::cancel`]): a cancelled token kills the decode at once.
pub fn analyse_video(video: &Utf8Path, src: (u32, u32), on_progress: &mut dyn FnMut(f32)) -> Result<MotionTrack> {
    crate::cancel::check()?;
    let (w, h) = analysis_size(src);
    let expected = (crate::media_probe::probe_video_duration(video) * ANALYSIS_HZ).ceil().max(1.0);
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i", video.as_str(), "-an", "-sn", "-dn", "-vf"])
        .arg(format!("fps={ANALYSIS_HZ},scale={w}:{h}:flags=area,format=gray"))
        .args(["-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning ffmpeg to analyse {video} for a smart reframe"))?;
    let mut stdout = child.stdout.take().context("ffmpeg stdout")?;
    let mut stderr = child.stderr.take().context("ffmpeg stderr")?;
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let read = Arc::new(AtomicUsize::new(0));
    let counter = read.clone();
    let (fw, fh) = (w as usize, h as usize);
    // Frames are analysed on a reader thread while this one watches for a
    // Stop; killing ffmpeg closes the pipe, which ends the reader.
    let reader = std::thread::spawn(move || -> std::io::Result<MotionTrack> {
        let mut builder = TrackBuilder::new(fw, fh, ANALYSIS_HZ);
        let mut frame = vec![0u8; fw * fh];
        loop {
            match stdout.read_exact(&mut frame) {
                Ok(()) => {
                    builder.push(&frame);
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }
        }
        Ok(builder.finish())
    });
    let token = crate::progress::cancel_token();
    let status = loop {
        if token.as_ref().is_some_and(|t| t.is_cancelled()) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(crate::cancel::Cancelled.into());
        }
        if let Some(status) = child.try_wait().context("waiting for ffmpeg")? {
            break status;
        }
        on_progress((read.load(Ordering::Relaxed) as f64 / expected).min(1.0) as f32);
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let track = reader
        .join()
        .map_err(|_| anyhow::anyhow!("smart-reframe reader panicked"))?
        .context("reading analysis frames from ffmpeg")?;
    let stderr = err_reader.join().unwrap_or_default();
    if !status.success() {
        anyhow::bail!(
            "ffmpeg analysis decode of {video} failed (exit {}): {}",
            status.code().unwrap_or(-1),
            stderr.trim()
        );
    }
    on_progress(1.0);
    Ok(track)
}

/// Analyses a finished video once, on first use, and plans a camera path
/// for each output shape asked for.
pub struct Follower {
    video: Utf8PathBuf,
    src: (u32, u32),
    cuts: Vec<f64>,
    track: Option<MotionTrack>,
    pub params: PathParams,
}

impl Follower {
    /// `cuts`: the video's known cuts, output seconds (the manifest
    /// timeline's, [`timeline_cuts`]); hard cuts it misses are detected.
    pub fn new(video: &Utf8Path, src: (u32, u32), cuts: Vec<f64>) -> Self {
        Follower { video: video.to_owned(), src, cuts, track: None, params: PathParams::default() }
    }

    /// The camera path for a crop to `target`'s shape; `None` when the
    /// shapes match (nothing to crop). Analyses the video the first time.
    pub fn path_for(&mut self, target: (u32, u32), on_progress: &mut dyn FnMut(f32)) -> Result<Option<CameraPath>> {
        let Some((axis, window)) = crop_window(self.src, target) else {
            return Ok(None);
        };
        if self.track.is_none() {
            self.track = Some(analyse_video(&self.video, self.src, on_progress)?);
        }
        let Some(track) = self.track.as_ref() else { return Ok(None) };
        let cuts = merge_cuts(&self.cuts, &track.detected_cuts());
        let targets = track.targets(axis, window, self.params.min_confidence);
        Ok(Some(plan_path(&targets, &cuts, track.duration_secs(), axis, window, track.sample_hz, &self.params)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 192;
    const H: usize = 108;

    /// A static, textured, non-repeating background (4 px blocks of
    /// hashed grey), so shifts are unambiguous.
    fn background(w: usize, h: usize, seed: u32) -> Vec<u8> {
        let mut f = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut v = ((x / 4) as u32).wrapping_mul(73_856_093) ^ ((y / 4) as u32).wrapping_mul(19_349_663) ^ seed;
                v ^= v >> 13;
                v = v.wrapping_mul(0x5bd1_e995);
                v ^= v >> 15;
                f[y * w + x] = 60 + (v % 100) as u8;
            }
        }
        f
    }

    /// `bg` shifted right by `dx` (the camera panned left), edges clamped.
    fn shifted(bg: &[u8], w: usize, h: usize, dx: i32) -> Vec<u8> {
        let mut f = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let sx = (x as i32 - dx).clamp(0, w as i32 - 1) as usize;
                f[y * w + x] = bg[y * w + sx];
            }
        }
        f
    }

    fn square(f: &mut [u8], w: usize, cx: f64, cy: usize, size: usize, val: u8) {
        let x0 = (cx - size as f64 / 2.0).round().max(0.0) as usize;
        for y in cy - size / 2..cy + size / 2 {
            for x in x0..(x0 + size).min(w) {
                f[y * w + x] = val;
            }
        }
    }

    /// Frames of a bright square crossing a static background left to
    /// right, `speed` analysis px a frame: (frames, square centre x per frame).
    fn moving_square(n: usize, speed: f64) -> (Vec<Vec<u8>>, Vec<f64>) {
        let bg = background(W, H, 7);
        let mut frames = Vec::new();
        let mut xs = Vec::new();
        for i in 0..n {
            let cx = 20.0 + speed * i as f64;
            let mut f = bg.clone();
            square(&mut f, W, cx, 54, 14, 240);
            frames.push(f);
            xs.push(cx);
        }
        (frames, xs)
    }

    fn track_of(frames: &[Vec<u8>]) -> MotionTrack {
        let mut b = TrackBuilder::new(W, H, ANALYSIS_HZ);
        for f in frames {
            b.push(f);
        }
        b.finish()
    }

    const NINE_SIXTEEN: f64 = (9.0 / 16.0) / (16.0 / 9.0);

    #[test]
    fn windows_by_shape() {
        let (axis, w) = crop_window((1920, 1080), (1080, 1920)).unwrap();
        assert_eq!(axis, PanAxis::X);
        assert!((w - NINE_SIXTEEN).abs() < 1e-9, "{w}");
        let (_, sq) = crop_window((1920, 1080), (1080, 1080)).unwrap();
        assert!((sq - 0.5625).abs() < 1e-9);
        let (_, p45) = crop_window((1920, 1080), (1080, 1350)).unwrap();
        assert!((p45 - 0.45).abs() < 1e-9);
        // A wider shape slides up and down.
        let (axis, w) = crop_window((1080, 1920), (1920, 1080)).unwrap();
        assert_eq!(axis, PanAxis::Y);
        assert!((w - NINE_SIXTEEN).abs() < 1e-9);
        assert_eq!(crop_window((1920, 1080), (1280, 720)), None);
        assert_eq!(analysis_size((1920, 1080)), (192, 108));
        assert_eq!(analysis_size((1080, 1920)), (108, 192));
        assert_eq!(analysis_size((3840, 2160)), (192, 108));
    }

    #[test]
    fn roi_follows_a_moving_bright_square() {
        let (frames, xs) = moving_square(40, 4.0);
        let track = track_of(&frames);
        assert_eq!(track.samples.len(), 39);
        for (i, s) in track.samples.iter().enumerate() {
            let r = roi(s, PanAxis::X, NINE_SIXTEEN).expect("the square's motion is clear");
            // The pair spans the square's two positions; the window centres
            // on the motion between them.
            let truth = (xs[i] + xs[i + 1]) / 2.0 / W as f64;
            let want = truth.clamp(NINE_SIXTEEN / 2.0, 1.0 - NINE_SIXTEEN / 2.0);
            assert!((r.centre - want).abs() < 0.03, "frame {i}: centre {} vs {want}", r.centre);
            assert!(r.confidence > 0.8, "frame {i}: confidence {}", r.confidence);
        }
    }

    #[test]
    fn a_still_frame_has_no_target() {
        let bg = background(W, H, 3);
        let mut noisy = bg.clone();
        // Compression-sized noise everywhere: not action.
        for (i, v) in noisy.iter_mut().enumerate() {
            *v = v.saturating_add((i % 3) as u8);
        }
        let s = analyse_pair(&bg, &noisy, W, H, 0.05);
        assert_eq!(roi(&s, PanAxis::X, 0.5), None);
    }

    #[test]
    fn the_cameras_own_pan_is_taken_out() {
        // The camera pans with the subject: the background slides 3 px a
        // frame while the square holds still at x = 150 in the frame.
        let bg = background(W + 200, H, 11);
        let frames: Vec<Vec<u8>> = (0..12)
            .map(|i| {
                let mut f = vec![0u8; W * H];
                for y in 0..H {
                    for x in 0..W {
                        f[y * W + x] = bg[y * (W + 200) + x + 100 - 3 * i];
                    }
                }
                square(&mut f, W, 150.0, 54, 16, 245);
                f
            })
            .collect();
        let track = track_of(&frames);
        for s in &track.samples {
            let r = roi(s, PanAxis::X, NINE_SIXTEEN).expect("the subject stands out from the pan");
            assert!((r.centre - 150.0 / W as f64).abs() < 0.04, "centre {}", r.centre);
            assert!(r.confidence > 0.6, "confidence {}", r.confidence);
        }
        // The shift itself: the background moved right by 3.
        let p = profile(&frames[0], W, H, PanAxis::X);
        let q = profile(&frames[1], W, H, PanAxis::X);
        assert_eq!(best_shift(&p, &q, 15), 3);
        let still = shifted(&background(W, H, 5), W, H, 0);
        let p = profile(&still, W, H, PanAxis::X);
        assert_eq!(best_shift(&p, &p, 15), 0);
    }

    #[test]
    fn motion_everywhere_is_not_confident() {
        // Motion over the whole frame (everything changes): no window
        // holds more of it than an even spread would.
        let a = background(W, H, 1);
        let b = background(W, H, 2);
        let s = analyse_pair(&a, &b, W, H, 0.05);
        if let Some(r) = roi(&s, PanAxis::X, NINE_SIXTEEN) {
            assert!(r.confidence < 0.3, "{r:?}");
        }
    }

    #[test]
    fn hard_cuts_are_seen_in_the_pictures() {
        let (mut frames, _) = moving_square(20, 2.0);
        let other = background(W, H, 99);
        for i in 0..15 {
            let mut f = other.clone();
            square(&mut f, W, 100.0 + i as f64, 30, 10, 20);
            frames.push(f);
        }
        let track = track_of(&frames);
        let cuts = track.detected_cuts();
        // Frame 20 is the first of the new scene: the pair (19, 20) sits
        // at 1.95 s.
        assert_eq!(cuts.len(), 1, "{cuts:?}");
        assert!((cuts[0] - 1.95).abs() < 1e-9, "{cuts:?}");
        // A manifest cut nearby is the same cut; a far one is another.
        assert_eq!(merge_cuts(&[2.0], &cuts), vec![2.0]);
        assert_eq!(merge_cuts(&[5.0], &cuts), vec![1.95, 5.0]);
    }

    fn params() -> PathParams {
        PathParams::default()
    }

    /// Samples at the analysis rate over `secs`, target from `f`.
    fn targets(secs: f64, f: impl Fn(f64) -> Option<f64>) -> Vec<(f64, Option<f64>)> {
        let n = (secs * ANALYSIS_HZ) as usize;
        (1..n).map(|i| (i as f64 - 0.5) / ANALYSIS_HZ).map(|t| (t, f(t))).collect()
    }

    fn plan(t: &[(f64, Option<f64>)], cuts: &[f64], secs: f64, window: f64) -> CameraPath {
        plan_path(t, cuts, secs, PanAxis::X, window, ANALYSIS_HZ, &params())
    }

    /// The path every 1/30 s over `[a, b)`.
    fn sampled(path: &CameraPath, a: f64, b: f64) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        let mut t = a;
        while t < b {
            out.push((t, path.at(t)));
            t += 1.0 / 30.0;
        }
        out
    }

    #[test]
    fn small_moves_inside_the_dead_zone_never_move_the_camera() {
        let w = NINE_SIXTEEN;
        // The subject wanders ±40 % of the dead zone around 0.3.
        let dz = params().dead_zone * w;
        let t = targets(10.0, |t| Some(0.3 + 0.4 * dz * (t * 3.0).sin()));
        let path = plan(&t, &[], 10.0, w);
        assert_eq!(path.followed_shots, 1);
        let (lo, hi) = sampled(&path, 0.0, 10.0)
            .iter()
            .fold((f64::MAX, f64::MIN), |(lo, hi), &(_, c)| (lo.min(c), hi.max(c)));
        assert!(hi - lo < 1e-9, "the camera moved {} with the subject inside the dead zone", hi - lo);
        assert_eq!(path.pieces.len(), 1, "{:?}", path.pieces);
    }

    #[test]
    fn pans_are_capped_at_the_maximum_speed_and_arrive() {
        let w = NINE_SIXTEEN;
        // The subject jumps across the frame at 3 s.
        let t = targets(10.0, |t| Some(if t < 3.0 { 0.2 } else { 0.8 }));
        let path = plan(&t, &[], 10.0, w);
        let vmax = params().max_speed * w;
        let s = sampled(&path, 0.0, 10.0);
        for p in s.windows(2) {
            let speed = (p[1].1 - p[0].1).abs() / (p[1].0 - p[0].0);
            assert!(speed <= vmax * 1.001, "{speed} > {vmax} at {}", p[0].0);
        }
        let lo = w / 2.0;
        assert!((path.at(0.0) - lo.max(0.2)).abs() < 1e-6, "{}", path.at(0.0));
        // Gets there (within the dead zone), with no overshoot past it.
        let end = path.at(9.9);
        assert!((end - 0.8).abs() < 0.01, "{end}");
        assert!(s.iter().all(|&(_, c)| c <= 0.8 + 1e-9));
        // It looks ahead: the pan starts before the subject moves.
        assert!(path.at(2.95) > path.at(2.6) + 1e-4, "{} {}", path.at(2.6), path.at(2.95));
    }

    #[test]
    fn the_camera_snaps_at_a_cut_instead_of_panning_across_it() {
        let w = NINE_SIXTEEN;
        let t = targets(10.0, |t| Some(if t < 5.0 { 0.25 } else { 0.75 }));
        let path = plan(&t, &[5.0], 10.0, w);
        assert_eq!((path.shots, path.followed_shots), (2, 2));
        assert!((path.at(4.99) - 0.25).abs() < 1e-6, "{}", path.at(4.99));
        assert!((path.at(5.0) - 0.75).abs() < 1e-6, "{}", path.at(5.0));
        // Flat on both sides: no pan towards or away from the cut.
        assert_eq!(path.pieces.len(), 2, "{:?}", path.pieces);
        let s = path.smoothness(30.0);
        assert_eq!((s.jumps, s.max_step, s.reversals), (vec![5.0], 0.0, 0));
    }

    #[test]
    fn low_motion_holds_and_no_clear_action_keeps_the_centre() {
        let w = NINE_SIXTEEN;
        // Clear subject at 0.7 for 3 s, then nothing: hold.
        let t = targets(8.0, |t| (t < 3.0).then_some(0.7));
        let path = plan(&t, &[], 8.0, w);
        assert!((path.at(7.5) - 0.7).abs() < 1e-6, "{}", path.at(7.5));
        // Before the subject shows, aim where it will be.
        let t = targets(8.0, |t| (t > 1.0).then_some(0.7));
        assert!((plan(&t, &[], 8.0, w).at(0.0) - 0.7).abs() < 1e-6);
        // Nothing clear at all: the centre crop.
        let none = plan(&targets(8.0, |_| None), &[], 8.0, w);
        assert_eq!(none.followed_shots, 0);
        assert!(none.is_centred(0.0, None));
        assert_eq!(offset_expr(&none, 0.0, None), "0.5000");
        // A shot where the subject only flickers (under the follow share)
        // keeps the centre too, while the next shot follows.
        let t = targets(10.0, |t| {
            if t < 5.0 {
                ((t * 10.0) as i64 % 10 == 0).then_some(0.2)
            } else {
                Some(0.8)
            }
        });
        let path = plan(&t, &[5.0], 10.0, w);
        assert_eq!((path.shots, path.followed_shots), (2, 1));
        assert!(path.is_centred(0.0, Some(5.0)) && !path.is_centred(5.0, None));
    }

    #[test]
    fn a_steady_subject_gets_a_steady_pan_that_settles() {
        let w = NINE_SIXTEEN;
        // Crosses from 0.25 to 0.75 between 2 and 6 s, then stays.
        let t = targets(10.0, |t| Some(0.25 + 0.5 * ((t - 2.0) / 4.0).clamp(0.0, 1.0)));
        let path = plan(&t, &[], 10.0, w);
        // Mid-pan, the camera moves at the subject's pace: no surging.
        let speeds: Vec<f64> =
            (30..50).map(|k| (path.at(k as f64 / 10.0 + 0.1) - path.at(k as f64 / 10.0)) / 0.1).collect();
        let (lo, hi) = speeds.iter().fold((f64::MAX, f64::MIN), |(lo, hi), &s| (lo.min(s), hi.max(s)));
        assert!(lo > 0.1 && hi < 0.15, "pan speed {lo}..{hi} for a subject at 0.125/s");
        // Then it holds where the subject stopped.
        assert!((path.at(7.5) - 0.75).abs() < 0.01, "{}", path.at(7.5));
        assert!((path.at(9.9) - path.at(7.5)).abs() < 1e-3, "{} → {}", path.at(7.5), path.at(9.9));
        // One smooth pan: never turns back, never faster than the cap.
        let s = path.smoothness(30.0);
        assert_eq!((s.jumps.len(), s.reversals), (0, 0), "{s:?}");
        assert!(s.max_step <= params().max_speed * w / 30.0 + 1e-9, "{s:?}");
        // And it stayed with the subject throughout: always inside the window.
        for k in 0..100 {
            let tt = k as f64 / 10.0;
            let subject = 0.25 + 0.5 * ((tt - 2.0) / 4.0).clamp(0.0, 1.0);
            assert!((path.at(tt) - subject).abs() < w / 4.0, "t={tt}: camera {} subject {subject}", path.at(tt));
        }
    }

    #[test]
    fn single_outliers_are_filtered_out() {
        let w = NINE_SIXTEEN;
        // A one-frame flash on the far side.
        let t = targets(6.0, |t| Some(if (t - 3.05).abs() < 0.01 { 0.9 } else { 0.4 }));
        let path = plan(&t, &[], 6.0, w);
        assert!(sampled(&path, 0.0, 6.0).iter().all(|&(_, c)| (c - 0.4).abs() < 1e-6));
    }

    #[test]
    fn shot_bounds_merge_close_cuts() {
        assert_eq!(shot_bounds(&[], 4.0), vec![(0.0, 4.0)]);
        assert_eq!(shot_bounds(&[2.0, 1.0, 2.1, 0.05, 3.95], 4.0), vec![(0.0, 1.0), (1.0, 2.0), (2.0, 4.0)]);
        assert_eq!(shot_bounds(&[f64::NAN], 4.0), vec![(0.0, 4.0)]);
    }

    /// Evaluates what [`offset_expr`] writes, as ffmpeg would: a sum of
    /// `[gte(t,A)*][lt(t,B)*]BODY` terms, BODY a constant or `(P±S*(t-A))`.
    fn eval_expr(e: &str, t: f64) -> f64 {
        let num = |s: &str| s.parse::<f64>().unwrap_or_else(|_| panic!("number {s:?} in {e}"));
        let mut sum = 0.0;
        // Split on top-level '+' only (a body's sign sits inside parens).
        let mut depth = 0;
        let mut start = 0;
        let mut terms = Vec::new();
        for (i, ch) in e.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                '+' if depth == 0 => {
                    terms.push(&e[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        terms.push(&e[start..]);
        for term in terms {
            let mut rest = term;
            let mut gate = 1.0;
            loop {
                if let Some(r) = rest.strip_prefix("gte(t,") {
                    let (v, r) = r.split_once(")*").unwrap();
                    gate *= if t >= num(v) { 1.0 } else { 0.0 };
                    rest = r;
                } else if let Some(r) = rest.strip_prefix("lt(t,") {
                    let (v, r) = r.split_once(")*").unwrap();
                    gate *= if t < num(v) { 1.0 } else { 0.0 };
                    rest = r;
                } else {
                    break;
                }
            }
            let body = match rest.strip_prefix('(') {
                None => num(rest),
                Some(r) => {
                    // P±S*(t-A))
                    let r = r.strip_suffix("))").unwrap();
                    let sign_at = r[1..].find(['+', '-']).unwrap() + 1;
                    let p = num(&r[..sign_at]);
                    let (s, a) = r[sign_at..].split_once("*(t-").unwrap();
                    p + num(s) * (t - num(a))
                }
            };
            sum += gate * body;
        }
        sum
    }

    #[test]
    fn the_crop_expression_traces_the_path() {
        let w = NINE_SIXTEEN;
        let t = targets(12.0, |t| Some(if t < 4.0 { 0.25 } else if t < 7.0 { 0.25 + (t - 4.0) * 0.15 } else { 0.8 }));
        let path = plan(&t, &[7.0], 12.0, w);
        assert!(path.pieces.len() > 2);
        let e = offset_expr(&path, 0.0, None);
        // No ':' — it would end the crop filter's option.
        assert!(!e.contains(':'), "{e}");
        for (t, _) in sampled(&path, 0.0, 12.0) {
            let got = eval_expr(&e, t);
            assert!((got - path.offset_at(t)).abs() < 2e-4, "t={t}: {got} vs {} in {e}", path.offset_at(t));
        }
        // A piece of the timeline, on its own clock.
        let e = offset_expr(&path, 3.0, Some(9.0));
        for (t, _) in sampled(&path, 3.0, 9.0) {
            let got = eval_expr(&e, t - 3.0);
            assert!((got - path.offset_at(t)).abs() < 2e-4, "t={t}: {got} vs {}", path.offset_at(t));
        }
        // A frame right on the cut already has the new shot's crop.
        let at_cut = 7.0;
        assert!((eval_expr(&offset_expr(&path, 0.0, None), at_cut) - path.offset_at(at_cut)).abs() < 2e-4);
    }

    #[test]
    fn the_expression_stays_bounded_however_many_shots() {
        let w = NINE_SIXTEEN;
        // 20 minutes, a cut every 0.4 s, each shot's subject elsewhere.
        let secs = 1200.0;
        let cuts: Vec<f64> = (1..3000).map(|k| k as f64 * 0.4).collect();
        let t = targets(secs, |t| Some(0.2 + 0.6 * (((t / 0.4) as i64 * 7919) % 13) as f64 / 12.0));
        let path = plan(&t, &cuts, secs, w);
        assert!(path.pieces.len() <= MAX_PIECES, "{}", path.pieces.len());
        let e = offset_expr(&path, 0.0, None);
        assert!(e.len() <= MAX_PIECES * 64, "{} chars", e.len());
        // Still covers the whole video in order.
        assert_eq!(path.pieces.first().map(|p| p.t0), Some(0.0));
        assert_eq!(path.pieces.last().map(|p| p.t1), Some(secs));
        assert!(path.pieces.windows(2).all(|p| (p[0].t1 - p[1].t0).abs() < 1e-9));
        // A long, smoothly followed video coarsens instead of flattening.
        let t = targets(600.0, |t| Some(0.5 + 0.3 * (t / 1.7).sin()));
        let path = plan(&t, &[], 600.0, w);
        assert!(path.pieces.len() <= MAX_PIECES, "{}", path.pieces.len());
        assert!(path.pieces.iter().any(|p| (p.c1 - p.c0).abs() > 1e-3));
    }

    #[test]
    fn a_followed_square_stays_in_the_window() {
        // Analysis to path: the square crosses the frame in 6 s.
        let (frames, xs) = moving_square(60, 2.6);
        let track = track_of(&frames);
        let w = NINE_SIXTEEN;
        let t = track.targets(PanAxis::X, w, params().min_confidence);
        let path = plan_path(&t, &[], track.duration_secs(), PanAxis::X, w, ANALYSIS_HZ, &params());
        assert_eq!(path.followed_shots, 1);
        for (i, x) in xs.iter().enumerate() {
            let c = path.at(i as f64 / ANALYSIS_HZ);
            let (l, r) = ((c - w / 2.0) * W as f64, (c + w / 2.0) * W as f64);
            assert!(*x - 7.0 >= l - 0.5 && *x + 7.0 <= r + 0.5, "frame {i}: square {x} outside [{l}, {r}]");
        }
        let j = path.to_json();
        assert_eq!(j["axis"], "x");
        assert_eq!(j["shots"], 1);
        assert!(path.describe().contains("followed the action in 1 of 1 shot"), "{}", path.describe());
    }

    fn ffmpeg_available() -> bool {
        Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// The expression drives ffmpeg's own crop: on a frame whose every
    /// column holds its own x as its grey level, the crop's first column
    /// reads back where the crop sits, frame by frame.
    #[test]
    fn ffmpeg_crops_where_the_path_says() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not on PATH; skipping");
            return;
        }
        let w = 64.0 / 256.0;
        let t = targets(6.0, |t| Some(if t < 2.0 { 0.2 } else if t < 4.0 { 0.5 } else { 0.9 }));
        let path = plan_path(&t, &[4.0], 6.0, PanAxis::X, w, ANALYSIS_HZ, &params());
        assert!(path.pieces.iter().any(|p| p.c1 != p.c0), "the path should pan: {:?}", path.pieces);
        let e = offset_expr(&path, 0.0, None);
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-f", "lavfi", "-i", "nullsrc=s=256x8:r=10:d=6"])
            .args(["-vf"])
            .arg(format!("format=gray,geq=lum='X',crop=64:8:'(in_w-out_w)*({e})':0"))
            .args(["-f", "rawvideo", "-pix_fmt", "gray", "-"])
            .output()
            .expect("ffmpeg");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let frames: Vec<&[u8]> = out.stdout.chunks(64 * 8).collect();
        assert_eq!(frames.len(), 60);
        for (i, f) in frames.iter().enumerate() {
            let t = i as f64 / 10.0;
            let want = path.offset_at(t) * 192.0;
            assert!((f[0] as f64 - want).abs() <= 1.0, "frame {i} (t={t}): x {} vs {want}", f[0]);
        }
    }
}
