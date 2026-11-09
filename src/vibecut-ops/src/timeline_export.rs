//! Timeline export: a finished reel as an edit another editor opens —
//! OpenTimelineIO (`.otio`) and Final Cut Pro XML 1.10 (`.fcpxml`, which
//! DaVinci Resolve reads too) — built from the render manifest's exact
//! `timeline` and pointing at the original footage, so the cut can be
//! finished, graded or re-timed there.
//!
//! # One model, two writers
//!
//! [`build_edit`] turns a manifest into an [`Edit`] once; [`to_otio`] and
//! [`to_fcpxml`] only format it, so both files describe the same cut.
//!
//! - **Frame-exact.** Every position on the sequence is a whole frame of
//!   the sequence rate (the render's `fps` unless asked otherwise; NTSC
//!   rates such as 30000/1001 are exact fractions), and every source time
//!   a whole frame of the source's own rate (probed), offset by its
//!   timecode — or a whole sample, for music.
//! - **Cuts.** The render cuts mid-crossfade, often half-way through a
//!   frame. Each cut lands on whichever of the two frames either side is
//!   closer to the music's beat, with its dissolve centred on it (as
//!   editors centre transitions) and kept inside the frames the render
//!   actually blended, so no transition asks for media the render didn't
//!   read.
//! - **Items partition the sequence.** Cards and clips run cut to cut, so
//!   their durations add up to the sequence length exactly.
//! - **Speed ramps** split into the clip at normal speed and its sped-up
//!   tail — a constant-speed `timeMap` in FCPXML, a `LinearTimeWarp` in
//!   OTIO — which every editor reads.
//! - **Cards** (intro/outro titles) are gaps named after their text, with
//!   a marker carrying it: title generators differ per editor, a gap
//!   imports everywhere.
//! - **Music** follows the layout the mux used (loop copies; several
//!   files joined into one track are split back into their own files), on
//!   its own audio track (OTIO) or connected below the storyline on lane
//!   −1 (FCPXML). Beats become markers on the music, the render's duck
//!   windows (where the music dips under a clip's own sound) range
//!   markers; in FCPXML only the ducked moments keep their camera sound
//!   up, as in the render.
//!
//! Paths are written as `file://` URLs of the paths the render read. In
//! Docker those are container paths (`/footage/…`); set
//! `VIBECUT_HOST_FOOTAGE` / `VIBECUT_HOST_PROJECTS` to the host folders
//! mounted there and the export points at the host files instead.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use vibecut::output_timeline::{OutputTimeline, TimelineClip, TimelineMusic};
use vibecut::selection_planner::Manifest;

use crate::error::OpsError;
use crate::paths::{resolve_projects, FOOTAGE_ROOT, PROJECTS_ROOT};

// ---------- Exact time ----------

/// A frame (or sample) rate as an exact fraction: `num / den` per second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rate {
    pub num: u32,
    pub den: u32,
}

impl Rate {
    pub const fn new(num: u32, den: u32) -> Self {
        Rate { num, den }
    }

    pub fn fps(self) -> f64 {
        self.num as f64 / self.den.max(1) as f64
    }

    /// The nearest whole frame to `secs`.
    pub fn frames(self, secs: f64) -> i64 {
        (secs * self.fps()).round() as i64
    }

    pub fn secs(self, frames: i64) -> f64 {
        frames as f64 / self.fps()
    }

    /// `30`, `29.97`, `30000/1001`, `23.976`, `59.94`, `25`.
    pub fn parse(s: &str) -> Option<Rate> {
        let s = s.trim();
        match s.split_once('/') {
            Some((a, b)) => {
                let (num, den): (u32, u32) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
                if num == 0 || den == 0 {
                    return None;
                }
                let g = gcd(num as i64, den as i64) as u32;
                let r = Rate::new(num / g, den / g);
                if r.den == 1 || r.den == 1001 {
                    Some(r)
                } else {
                    Rate::from_fps(r.fps())
                }
            }
            None => Rate::from_fps(s.parse().ok()?),
        }
    }

    /// A measured rate: whole and NTSC (`n·1000/1001`) rates exactly,
    /// anything else to the millihertz.
    pub fn from_fps(f: f64) -> Option<Rate> {
        if !(f.is_finite() && (1.0..=1000.0).contains(&f)) {
            return None;
        }
        if (f - f.round()).abs() < 0.002 {
            return Some(Rate::new(f.round() as u32, 1));
        }
        let n = (f * 1.001).round();
        if (f - n * 1000.0 / 1001.0).abs() < 0.002 {
            return Some(Rate::new(n as u32 * 1000, 1001));
        }
        let num = (f * 1000.0).round() as i64;
        let g = gcd(num, 1000);
        Some(Rate::new((num / g) as u32, (1000 / g) as u32))
    }

    /// `30`, `29.97`, `23.976`.
    pub fn label(self) -> String {
        if self.den == 1 {
            return self.num.to_string();
        }
        let s = format!("{:.3}", self.fps());
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }

    /// Frames a timecode counts per second (30 for 29.97).
    fn timecode_base(self) -> i64 {
        (self.num as i64 + self.den as i64 - 1) / self.den.max(1) as i64
    }
}

fn gcd(a: i64, b: i64) -> i64 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

/// Exact seconds, `num / den` — written in FCPXML as `"num/dens"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rt {
    pub num: i64,
    pub den: i64,
}

impl Rt {
    pub const ZERO: Rt = Rt { num: 0, den: 1 };

    /// `n` frames (or samples) of `rate`: `n·den/num` seconds, kept over
    /// the rate's own timebase (`17/30s`, `17017/30000s`).
    pub fn units(n: i64, rate: Rate) -> Rt {
        Rt { num: n * rate.den as i64, den: rate.num.max(1) as i64 }
    }

    /// Exact sum, over the two timebases' common multiple (`N/44100s` +
    /// `M/30s` → `…/44100s`); a zero side keeps the other's timebase.
    pub fn add(self, o: Rt) -> Rt {
        if self.num == 0 {
            return o;
        }
        if o.num == 0 {
            return self;
        }
        if self.den == o.den {
            return Rt { num: self.num + o.num, den: self.den };
        }
        let l = self.den / gcd(self.den, o.den) * o.den;
        Rt { num: self.num * (l / self.den) + o.num * (l / o.den), den: l }
    }

    /// `self × p / q`, reduced.
    pub fn scale(self, p: i64, q: i64) -> Rt {
        let (num, den) = (self.num * p, self.den * q);
        let g = gcd(num, den);
        let s = if den < 0 { -1 } else { 1 };
        Rt { num: s * num / g, den: s * den / g }
    }

    pub fn secs(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    /// FCPXML time: `0s`, `5s`, `17/30s`, `1001/30000s`.
    pub fn fcpxml(self) -> String {
        if self.num % self.den == 0 {
            format!("{}s", self.num / self.den)
        } else {
            format!("{}/{}s", self.num, self.den)
        }
    }
}

/// A playback speed as a fraction (`3/1`, `1/2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Speed {
    pub p: i64,
    pub q: i64,
}

impl Speed {
    pub const NORMAL: Speed = Speed { p: 1, q: 1 };

    /// To a thousandth.
    pub fn from_f64(x: f64) -> Speed {
        if !(x.is_finite() && x > 0.0) {
            return Speed::NORMAL;
        }
        let p = (x * 1000.0).round().max(1.0) as i64;
        let g = gcd(p, 1000);
        Speed { p: p / g, q: 1000 / g }
    }

    pub fn is_normal(self) -> bool {
        self.p == self.q
    }

    pub fn value(self) -> f64 {
        self.p as f64 / self.q as f64
    }
}

// ---------- Media ----------

/// What a probe of a source file says (all optional: a file that is gone
/// still exports, with the sequence's format assumed).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaFacts {
    pub video: Option<VideoFacts>,
    pub audio: Option<AudioFacts>,
    pub duration_secs: Option<f64>,
    /// Timecode of the first frame as the file carries it (`HH:MM:SS:FF`,
    /// `;` before the frames for drop-frame).
    pub timecode: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoFacts {
    pub rate: Rate,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioFacts {
    pub sample_rate: u32,
    pub channels: u32,
}

/// Frames since midnight of a timecode at `rate` (drop-frame aware).
pub fn timecode_frames(tc: &str, rate: Rate) -> Option<i64> {
    let tc = tc.trim();
    let drop = tc.contains(';') && rate.den == 1001;
    let parts: Vec<i64> = tc.split([':', ';', '.']).map(|p| p.trim().parse().ok()).collect::<Option<_>>()?;
    let [h, m, s, f] = parts[..] else { return None };
    let base = rate.timecode_base();
    if m >= 60 || s >= 60 || f >= base || h < 0 || m < 0 || s < 0 || f < 0 {
        return None;
    }
    let mut frames = ((h * 60 + m) * 60 + s) * base + f;
    if drop {
        // Two frame numbers (four at 59.94) skipped every minute except
        // every tenth.
        let per_min = base / 15;
        let minutes = h * 60 + m;
        frames -= per_min * (minutes - minutes / 10);
    }
    Some(frames)
}

/// One file the edit references.
#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    /// The path the render read.
    pub path: String,
    /// `file://` URL of the path an editor should open (see the path map).
    pub url: String,
    /// File stem.
    pub name: String,
    pub video: Option<VideoFacts>,
    pub audio: Option<AudioFacts>,
    /// Source times are whole units of this: the video rate, the sample
    /// rate for music.
    pub grid: Rate,
    /// Media time of the first frame, in `grid` units (its timecode).
    pub start_units: i64,
    /// Length in `grid` units, when known.
    pub duration_units: Option<i64>,
    /// Furthest source time the edit reads, in `grid` units (handles
    /// included): the length to declare when the file couldn't be probed.
    pub used_end_units: i64,
    pub is_music: bool,
    /// The probe found the file.
    pub probed: bool,
}

impl Media {
    /// Source units at `secs` into the file.
    fn units_at(&self, secs: f64) -> i64 {
        self.start_units + (secs.max(0.0) * self.grid.fps()).round() as i64
    }
}

// ---------- The edit ----------

/// A reel as an edit: the sequence, its video items (cards, clips and
/// the dissolves between them), the music and the markers.
#[derive(Debug, Clone, PartialEq)]
pub struct Edit {
    pub name: String,
    pub rate: Rate,
    pub width: u32,
    pub height: u32,
    /// Sequence length in frames.
    pub frames: i64,
    pub media: Vec<Media>,
    /// The storyline, in order: cards and clips cut to cut, a dissolve
    /// between two of them where the render crossfaded.
    pub video: Vec<VideoItem>,
    /// Music clips in order, back to back.
    pub music: Vec<MusicClip>,
    pub markers: Vec<Marker>,
    /// What the export could only approximate.
    pub notes: Vec<String>,
    /// Provenance for the file's metadata.
    pub generator: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VideoItem {
    Clip(VideoClip),
    Gap(Gap),
    Transition(Transition),
}

#[derive(Debug, Clone, PartialEq)]
pub struct VideoClip {
    pub name: String,
    pub media: usize,
    /// First sequence frame (the cut in).
    pub offset: i64,
    /// Frames to the cut out.
    pub duration: i64,
    /// Source time at the cut in, in the media's grid units (timecode
    /// included).
    pub start_units: i64,
    pub speed: Speed,
    pub segment_index: usize,
    pub burst_part: Option<u32>,
    /// The sped-up tail of a speed ramp.
    pub ramp_tail: bool,
    pub source_id: String,
    /// Why the moment was picked (the manifest segment's `reason`).
    pub why: String,
    /// The render let this moment's own sound through (a duck window).
    pub camera_sound: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gap {
    pub name: String,
    pub offset: i64,
    pub duration: i64,
    /// `intro` / `outro` for a title card.
    pub role: Option<String>,
    pub lines: Vec<String>,
}

/// A dissolve centred on a cut: `[cut − half, cut + half)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transition {
    pub cut: i64,
    pub half: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MusicClip {
    pub name: String,
    pub media: usize,
    pub offset: i64,
    pub duration: i64,
    pub start_units: i64,
    /// Crossfade from the previous clip, frames each side of `offset`
    /// (a loop seam); 0 = straight splice.
    pub crossfade_half: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerKind {
    Cut,
    Beat,
    Duck,
    Title,
    Fade,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub frame: i64,
    /// Frames; 0 for a point.
    pub duration: i64,
    pub name: String,
    pub kind: MarkerKind,
}

impl Edit {
    /// Video items that take time (clips and gaps).
    pub fn spine(&self) -> impl Iterator<Item = (i64, i64)> + '_ {
        self.video.iter().filter_map(|v| match v {
            VideoItem::Clip(c) => Some((c.offset, c.duration)),
            VideoItem::Gap(g) => Some((g.offset, g.duration)),
            VideoItem::Transition(_) => None,
        })
    }

    pub fn summary(&self) -> Value {
        let count = |f: fn(&VideoItem) -> bool| self.video.iter().filter(|v| f(v)).count();
        json!({
            "name": self.name,
            "fps": self.rate.label(),
            "frames": self.frames,
            "duration_secs": self.rate.secs(self.frames),
            "clips": count(|v| matches!(v, VideoItem::Clip(_))),
            "gaps": count(|v| matches!(v, VideoItem::Gap(_))),
            "transitions": count(|v| matches!(v, VideoItem::Transition(_))),
            "music_clips": self.music.len(),
            "markers": self.markers.len(),
            "media": self.media.iter().map(|m| json!({ "path": m.path, "url": m.url, "found": m.probed })).collect::<Vec<_>>(),
            "notes": self.notes,
        })
    }
}

// ---------- Building the edit ----------

/// How to export.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExportOptions {
    /// Sequence rate; `None` = the render's.
    pub rate: Option<Rate>,
    /// Rewrite recorded paths before they become URLs: `(from, to)`
    /// prefixes, whole path components (`/footage` → `E:/GoPro`).
    pub path_map: Vec<(String, String)>,
}

/// One thing on the storyline before cut points are settled.
enum Piece<'a> {
    Card { role: &'static str },
    Clip(&'a TimelineClip),
}

/// Turn a render manifest into an [`Edit`]. `media` holds what probes
/// found per recorded path; anything missing is assumed to match the
/// sequence. Fails only when the manifest has no timeline.
pub fn build_edit(m: &Manifest, media: &HashMap<String, MediaFacts>, opts: &ExportOptions) -> Result<Edit, OpsError> {
    let tl: &OutputTimeline = m.timeline.as_ref().ok_or_else(|| {
        OpsError::invalid_with_hint(
            "this render's manifest has no timeline to export",
            "render it again with this version of Vibecut",
        )
    })?;
    let rate = opts.rate.unwrap_or(Rate::new(tl.fps.max(1), 1));
    let (width, height) = vibecut::selection_planner::parse_resolution(&m.resolution).unwrap_or((1920, 1080));
    let total = rate.frames(tl.duration_secs).max(1);
    let mut notes = Vec::new();
    let mut reg = MediaRegistry { media: Vec::new(), index: HashMap::new(), facts: media, opts, seq: rate };

    // Beats in output time: recorded with the music, else the beats the
    // beat-sync check matched the cuts to.
    let beats: Vec<f64> = match &tl.music {
        Some(mu) => mu.beats_secs.clone(),
        None => {
            let mut b: Vec<f64> =
                m.beat_sync.iter().flat_map(|r| r.cuts.iter().map(|c| c.beat_secs as f64)).collect();
            b.sort_by(f64::total_cmp);
            b.dedup();
            b
        }
    };

    // The storyline before cut points: cards and clips in output order.
    let mut pieces: Vec<(Piece, f64, f64, f64)> = Vec::new(); // (piece, out_start, out_end, cut_in)
    if let Some(c) = &tl.intro {
        pieces.push((Piece::Card { role: "intro" }, c.output_start_secs, c.output_end_secs, 0.0));
    }
    for c in &tl.clips {
        pieces.push((Piece::Clip(c), c.output_start_secs, c.output_end_secs, c.cut_in_secs));
    }
    if let Some(c) = &tl.outro {
        pieces.push((Piece::Card { role: "outro" }, c.output_start_secs, c.output_end_secs, c.cut_secs));
    }
    if pieces.is_empty() {
        return Err(OpsError::invalid("this render's timeline has no clips"));
    }
    let n = pieces.len();

    // Cut frames: c[0] = 0, c[n] = the end, c[j] between pieces j−1 and j,
    // inside the frames both pieces share (the render's crossfade).
    let mut cuts = vec![0i64; n + 1];
    let mut overlap = vec![(0i64, 0i64); n + 1];
    cuts[n] = total;
    for j in 1..n {
        let a = rate.frames(pieces[j].1);
        let b = rate.frames(pieces[j - 1].2).max(a);
        overlap[j] = (a, b);
        cuts[j] = choose_cut(pieces[j].3, a, b, &beats, rate);
    }
    // Strictly increasing, every piece at least a frame.
    for j in 1..n {
        let lo = cuts[j - 1] + 1;
        let hi = total - (n - j) as i64;
        cuts[j] = cuts[j].max(lo).min(hi.max(lo));
    }
    // Dissolves: centred on the cut, inside the shared frames, and at most
    // half of either neighbour, so dissolves never overlap.
    let mut halves = vec![0i64; n + 1];
    for j in 1..n {
        let (a, b) = overlap[j];
        let c = cuts[j];
        let h = (c - a).min(b - c).min((c - cuts[j - 1]) / 2).min((cuts[j + 1] - c) / 2);
        halves[j] = h.max(0);
    }

    // Camera sound stays up only where the render ducked the music for it.
    let ducked: Vec<usize> = m.duck_windows.iter().map(|w| w.segment_index).collect();

    let mut video = Vec::new();
    for (j, (piece, ..)) in pieces.iter().enumerate() {
        if j > 0 && halves[j] > 0 {
            video.push(VideoItem::Transition(Transition { cut: cuts[j], half: halves[j] }));
        }
        let (ci, co) = (cuts[j], cuts[j + 1]);
        match piece {
            Piece::Card { role } => {
                let lines = tl
                    .titles
                    .iter()
                    .find(|t| t.role == *role && !t.overlay)
                    .map(|t| t.lines.clone())
                    .unwrap_or_default();
                let name = if lines.is_empty() {
                    format!("{} card", if *role == "intro" { "Intro" } else { "Outro" })
                } else {
                    lines.join(" · ")
                };
                video.push(VideoItem::Gap(Gap { name, offset: ci, duration: co - ci, role: Some(role.to_string()), lines }));
            }
            Piece::Clip(c) => {
                let why = m.segments.get(c.segment_index).map(|s| s.reason.clone()).unwrap_or_default();
                let handle_out = if j + 1 < n { halves[j + 1] } else { 0 };
                for part in clip_parts(c, ci, co, rate) {
                    let mi = reg.footage(c.source_path.as_str());
                    let med = &reg.media[mi];
                    let start_units = med.units_at(media_secs(c, rate.secs(part.lo)));
                    // Furthest source read, the dissolve out included.
                    let last = if part.hi == co { co + handle_out } else { part.hi };
                    let end_units = med.units_at(media_secs(c, rate.secs(last)));
                    reg.media[mi].used_end_units = reg.media[mi].used_end_units.max(end_units);
                    let stem = reg.media[mi].name.clone();
                    let name = match (c.burst_part, part.tail) {
                        (_, true) => format!("{stem} · {}× ramp", trim_float(part.speed.value())),
                        (Some(p), _) => format!("{stem} · burst {}", p + 1),
                        _ => stem,
                    };
                    video.push(VideoItem::Clip(VideoClip {
                        name,
                        media: mi,
                        offset: part.lo,
                        duration: part.hi - part.lo,
                        start_units,
                        speed: part.speed,
                        segment_index: c.segment_index,
                        burst_part: c.burst_part,
                        ramp_tail: part.tail,
                        source_id: c.source_id.clone(),
                        why: why.clone(),
                        camera_sound: ducked.contains(&c.segment_index),
                    }));
                }
            }
        }
    }

    // Music.
    let music = match &tl.music {
        Some(mu) => music_clips(mu, m, rate, total, &mut reg),
        None => match m.soundtrack.first() {
            Some(credit) => {
                // Manifests from before the layout was recorded: the track
                // from its start, which is what plays unless it looped.
                let mi = reg.music(credit.file.as_str());
                let med = &reg.media[mi];
                let track = med.duration_units.map(|u| rate.frames((u - med.start_units) as f64 / med.grid.fps()));
                let duration = track.unwrap_or(total).min(total).max(1);
                let start_units = med.start_units;
                let end_units = med.units_at(rate.secs(duration));
                reg.media[mi].used_end_units = reg.media[mi].used_end_units.max(end_units);
                notes.push(
                    "music placed from the start of the track: this render predates the recorded music layout (render again for loops)".into(),
                );
                vec![MusicClip { name: credit.title.clone(), media: mi, offset: 0, duration, start_units, crossfade_half: 0 }]
            }
            None => Vec::new(),
        },
    };

    // Markers.
    let mut markers = Vec::new();
    for (j, &c) in cuts.iter().enumerate().take(n).skip(1) {
        let name = match nearest(&beats, rate.secs(c)) {
            Some(b) if (b - rate.secs(c)).abs() < 0.25 => {
                format!("Cut {j} · {:+.0} ms from the beat", (rate.secs(c) - b) * 1000.0)
            }
            _ => format!("Cut {j}"),
        };
        markers.push(Marker { frame: c, duration: 0, name, kind: MarkerKind::Cut });
    }
    if !music.is_empty() {
        for (k, &b) in beats.iter().enumerate() {
            let f = rate.frames(b);
            if (0..total).contains(&f) {
                markers.push(Marker { frame: f, duration: 0, name: format!("Beat {}", k + 1), kind: MarkerKind::Beat });
            }
        }
    }
    for w in &m.duck_windows {
        let (a, b) = (rate.frames(w.start_secs as f64).max(0), rate.frames(w.end_secs as f64).min(total));
        if b > a {
            markers.push(Marker {
                frame: a,
                duration: b - a,
                name: format!("Clip sound up ({}): the render dips the music here", w.source_id),
                kind: MarkerKind::Duck,
            });
        }
    }
    for t in tl.titles.iter().filter(|t| t.overlay) {
        let (a, b) = (rate.frames(t.output_start_secs).max(0), rate.frames(t.output_end_secs).min(total));
        if b > a {
            let text = if t.lines.is_empty() { format!("{} title", t.role) } else { t.lines.join(" / ") };
            markers.push(Marker { frame: a, duration: b - a, name: format!("Title: {text}"), kind: MarkerKind::Title });
        }
    }
    if let Some(mu) = &tl.music {
        let d = rate.frames(mu.fade_out_secs);
        if d > 0 && !music.is_empty() {
            markers.push(Marker { frame: total - d.min(total), duration: d.min(total), name: "Music fades out".into(), kind: MarkerKind::Fade });
        }
    }
    markers.sort_by_key(|mk| mk.frame);

    for med in &reg.media {
        if !med.probed {
            notes.push(format!("{}: not found when exporting; its format was assumed", med.path));
        }
    }

    Ok(Edit {
        name: m.project_name.clone(),
        rate,
        width,
        height,
        frames: total,
        media: reg.media,
        video,
        music,
        markers,
        notes,
        generator: m.generator.clone(),
    })
}

/// The music layout as clips: one per copy of the track, split where the
/// joined track changes file.
fn music_clips(mu: &TimelineMusic, m: &Manifest, rate: Rate, total: i64, reg: &mut MediaRegistry) -> Vec<MusicClip> {
    // Where each file starts in the joined track.
    let mut bounds = Vec::with_capacity(mu.files.len());
    let mut acc = 0.0;
    for f in &mu.files {
        bounds.push((acc, f));
        acc += f.duration_secs.max(0.0);
    }
    let half_x = rate.frames(mu.crossfade_secs * 0.5);
    let mut out: Vec<MusicClip> = Vec::new();
    for (si, span) in mu.spans.iter().enumerate() {
        let len = span.output_end_secs - span.output_start_secs;
        // Track times where this copy changes file, as output times.
        let mut pieces = Vec::new();
        for (k, &(fstart, file)) in bounds.iter().enumerate() {
            let fend = if k + 1 < bounds.len() { bounds[k + 1].0 } else { f64::INFINITY };
            let a = span.source_start_secs.max(fstart);
            let b = (span.source_start_secs + len).min(fend);
            if b > a {
                pieces.push((file, a - fstart, span.output_start_secs + (a - span.source_start_secs), span.output_start_secs + (b - span.source_start_secs)));
            }
        }
        for (pi, (file, local, oa, ob)) in pieces.into_iter().enumerate() {
            let (fa, fb) = (rate.frames(oa).max(0), rate.frames(ob).min(total));
            if fb <= fa {
                continue;
            }
            let mi = reg.music(file.path.as_str());
            let med = &reg.media[mi];
            let start_units = med.units_at(local);
            let end_units = med.units_at(local + rate.secs(fb - fa) + mu.crossfade_secs * 0.5);
            reg.media[mi].used_end_units = reg.media[mi].used_end_units.max(end_units);
            let title = m
                .soundtrack
                .iter()
                .find(|c| c.file == file.path)
                .map(|c| c.title.clone())
                .unwrap_or_else(|| reg.media[mi].name.clone());
            let crossfade_half = if si > 0 && pi == 0 { half_x } else { 0 };
            out.push(MusicClip { name: title, media: mi, offset: fa, duration: fb - fa, start_units, crossfade_half });
        }
    }
    // A crossfade fits inside both neighbours.
    for i in 0..out.len() {
        if out[i].crossfade_half > 0 {
            let prev = if i > 0 { out[i - 1].duration } else { 0 };
            let max = (prev / 2).min(out[i].duration / 2);
            out[i].crossfade_half = out[i].crossfade_half.min(max);
        }
    }
    out
}

struct ClipPart {
    lo: i64,
    hi: i64,
    speed: Speed,
    tail: bool,
}

/// A clip's visible span `[ci, co)` as constant-speed parts: the clip,
/// or the clip and its sped-up ramp tail.
fn clip_parts(c: &TimelineClip, ci: i64, co: i64, rate: Rate) -> Vec<ClipPart> {
    let body = Speed::from_f64(c.speed as f64);
    let Some(r) = c.ramp.filter(|r| r.factor > 1.0) else {
        return vec![ClipPart { lo: ci, hi: co, speed: body, tail: false }];
    };
    let fast = Speed::from_f64(c.speed as f64 * r.factor as f64);
    let rf = rate.frames(r.output_start_secs);
    if rf <= ci {
        vec![ClipPart { lo: ci, hi: co, speed: fast, tail: true }]
    } else if rf >= co {
        vec![ClipPart { lo: ci, hi: co, speed: body, tail: false }]
    } else {
        vec![
            ClipPart { lo: ci, hi: rf, speed: body, tail: false },
            ClipPart { lo: rf, hi: co, speed: fast, tail: true },
        ]
    }
}

/// Source seconds a clip shows at output time `t` (ramp included).
fn media_secs(c: &TimelineClip, t: f64) -> f64 {
    let s = c.speed as f64;
    let dt = t - c.output_start_secs;
    match c.ramp.filter(|r| r.factor > 1.0) {
        Some(r) if t > r.output_start_secs => {
            let body = (r.output_start_secs - c.output_start_secs).max(0.0);
            c.source_in_secs + body * s + (t - c.output_start_secs - body) * s * r.factor as f64
        }
        _ => c.source_in_secs + dt * s,
    }
}

/// The cut frame for a cut at `secs`, inside `[lo, hi]`: on a whole frame
/// as is; between two, the one nearer the music's beat (else the nearer).
fn choose_cut(secs: f64, lo: i64, hi: i64, beats: &[f64], rate: Rate) -> i64 {
    let x = secs * rate.fps();
    let pick = if (x - x.round()).abs() < 1e-6 {
        x.round() as i64
    } else {
        let (f, c) = (x.floor() as i64, x.ceil() as i64);
        match nearest(beats, secs) {
            Some(b) if (b - secs).abs() < 0.25 => {
                if (rate.secs(f) - b).abs() <= (rate.secs(c) - b).abs() {
                    f
                } else {
                    c
                }
            }
            _ => x.round() as i64,
        }
    };
    pick.clamp(lo.min(hi), hi.max(lo))
}

fn nearest(sorted: &[f64], t: f64) -> Option<f64> {
    let i = sorted.partition_point(|&b| b < t);
    [i.checked_sub(1), Some(i)]
        .into_iter()
        .flatten()
        .filter_map(|k| sorted.get(k).copied())
        .min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs()))
}

fn trim_float(x: f64) -> String {
    let s = format!("{x:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

struct MediaRegistry<'a> {
    media: Vec<Media>,
    index: HashMap<String, usize>,
    facts: &'a HashMap<String, MediaFacts>,
    opts: &'a ExportOptions,
    seq: Rate,
}

impl MediaRegistry<'_> {
    fn footage(&mut self, path: &str) -> usize {
        self.add(path, false)
    }

    fn music(&mut self, path: &str) -> usize {
        self.add(path, true)
    }

    fn add(&mut self, path: &str, is_music: bool) -> usize {
        if let Some(&i) = self.index.get(path) {
            return i;
        }
        let facts = self.facts.get(path);
        let video = if is_music { None } else { facts.and_then(|f| f.video) };
        let audio = facts.and_then(|f| f.audio);
        let grid = match (video, audio) {
            (Some(v), _) => v.rate,
            (None, Some(a)) if is_music => Rate::new(a.sample_rate.max(1), 1),
            (None, _) if is_music => Rate::new(48_000, 1),
            _ => self.seq,
        };
        let start_units = facts
            .and_then(|f| f.timecode.as_deref())
            .filter(|_| video.is_some())
            .and_then(|tc| timecode_frames(tc, grid))
            .unwrap_or(0);
        let duration_units = facts.and_then(|f| f.duration_secs).map(|d| start_units + (d * grid.fps()).round() as i64);
        let shown = map_path(path, &self.opts.path_map);
        let name = Path::new(&shown.replace('\\', "/"))
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| shown.clone());
        self.media.push(Media {
            path: path.to_string(),
            url: file_url(&shown),
            name,
            video,
            audio,
            grid,
            start_units,
            duration_units,
            used_end_units: start_units,
            is_music,
            probed: facts.is_some(),
        });
        let i = self.media.len() - 1;
        self.index.insert(path.to_string(), i);
        i
    }
}

/// Apply the first matching `(from, to)` prefix (whole components).
pub fn map_path(path: &str, map: &[(String, String)]) -> String {
    let norm = path.replace('\\', "/");
    for (from, to) in map {
        let from = from.trim_end_matches(['/', '\\']).replace('\\', "/");
        if from.is_empty() {
            continue;
        }
        if let Some(rest) = norm.strip_prefix(&from) {
            if rest.is_empty() || rest.starts_with('/') {
                return format!("{}{rest}", to.trim_end_matches(['/', '\\']).replace('\\', "/"));
            }
        }
    }
    path.to_string()
}

/// A `file://` URL: percent-encoded UTF-8 (`%20` for a space), a drive
/// letter kept as `file:///C:/…`.
pub fn file_url(path: &str) -> String {
    let p = path.replace('\\', "/");
    let p = if p.starts_with('/') { p } else { format!("/{p}") };
    let mut out = String::from("file://");
    for b in p.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------- OpenTimelineIO ----------

fn otio_time(value: i64, rate: Rate) -> Value {
    json!({ "OTIO_SCHEMA": "RationalTime.1", "rate": rate.fps(), "value": value as f64 })
}

fn otio_range(start: Value, duration: Value) -> Value {
    json!({ "OTIO_SCHEMA": "TimeRange.1", "start_time": start, "duration": duration })
}

fn otio_marker(name: &str, color: &str, start: i64, duration: i64, rate: Rate, meta: Value) -> Value {
    json!({
        "OTIO_SCHEMA": "Marker.1",
        "name": name,
        "metadata": meta,
        "color": color,
        "marked_range": otio_range(otio_time(start, rate), otio_time(duration, rate)),
    })
}

fn otio_reference(med: &Media) -> Value {
    let available = med
        .duration_units
        .map(|end| otio_range(otio_time(med.start_units, med.grid), otio_time(end - med.start_units, med.grid)));
    json!({
        "OTIO_SCHEMA": "ExternalReference.1",
        "name": med.name,
        "metadata": {},
        "available_range": available,
        "target_url": med.url,
    })
}

fn marker_color(kind: MarkerKind) -> &'static str {
    match kind {
        MarkerKind::Cut => "RED",
        MarkerKind::Beat => "GREEN",
        MarkerKind::Duck => "CYAN",
        MarkerKind::Title => "PURPLE",
        MarkerKind::Fade => "YELLOW",
    }
}

/// The edit as OpenTimelineIO JSON: a timeline with a video track (clips,
/// dissolves, card gaps) and a music track. Schema versions are the
/// oldest current readers accept (`Clip.1`, `Marker.1`), so OTIO 0.12
/// and later — and the editors built on it — load it.
pub fn to_otio(e: &Edit) -> Value {
    let rate = e.rate;
    let mut video_children = Vec::new();
    for item in &e.video {
        video_children.push(match item {
            VideoItem::Clip(c) => {
                let med = &e.media[c.media];
                let effects = if c.speed.is_normal() {
                    vec![]
                } else {
                    vec![json!({
                        "OTIO_SCHEMA": "LinearTimeWarp.1",
                        "name": "Speed",
                        "metadata": {},
                        "effect_name": "LinearTimeWarp",
                        "time_scalar": c.speed.value(),
                    })]
                };
                json!({
                    "OTIO_SCHEMA": "Clip.1",
                    "name": c.name,
                    "metadata": { "vibecut": {
                        "segment_index": c.segment_index,
                        "burst_part": c.burst_part,
                        "source_id": c.source_id,
                        "ramp_tail": c.ramp_tail,
                        "camera_sound": c.camera_sound,
                        "why": c.why,
                    }},
                    // Source time in the file's own frames; length in the
                    // sequence's (a clip plays in real time).
                    "source_range": otio_range(otio_time(c.start_units, med.grid), otio_time(c.duration, rate)),
                    "effects": effects,
                    "markers": [],
                    "enabled": true,
                    "media_reference": otio_reference(med),
                })
            }
            VideoItem::Gap(g) => {
                let markers: Vec<Value> = if g.lines.is_empty() {
                    vec![]
                } else {
                    vec![otio_marker(&format!("Title: {}", g.lines.join(" / ")), "PURPLE", 0, g.duration, rate, json!({}))]
                };
                json!({
                    "OTIO_SCHEMA": "Gap.1",
                    "name": g.name,
                    "metadata": { "vibecut": { "card": g.role, "lines": g.lines } },
                    "source_range": otio_range(otio_time(0, rate), otio_time(g.duration, rate)),
                    "effects": [],
                    "markers": markers,
                    "enabled": true,
                })
            }
            VideoItem::Transition(t) => otio_dissolve(t.half, rate),
        });
    }

    let mut audio_children = Vec::new();
    let mut at = 0i64;
    for (i, mc) in e.music.iter().enumerate() {
        if mc.offset > at {
            audio_children.push(otio_gap(mc.offset - at, rate));
        } else if i > 0 && mc.crossfade_half > 0 {
            audio_children.push(otio_dissolve(mc.crossfade_half, rate));
        }
        let med = &e.media[mc.media];
        audio_children.push(json!({
            "OTIO_SCHEMA": "Clip.1",
            "name": mc.name,
            "metadata": {},
            "source_range": otio_range(otio_time(mc.start_units, med.grid), otio_time(mc.duration, rate)),
            "effects": [],
            "markers": [],
            "enabled": true,
            "media_reference": otio_reference(med),
        }));
        at = mc.offset + mc.duration;
    }

    let marker = |mk: &Marker| otio_marker(&mk.name, marker_color(mk.kind), mk.frame, mk.duration, rate, json!({}));
    let on_music = |k: MarkerKind| matches!(k, MarkerKind::Beat | MarkerKind::Duck | MarkerKind::Fade);
    let has_music = !e.music.is_empty();
    let video_markers: Vec<Value> =
        e.markers.iter().filter(|mk| !(has_music && on_music(mk.kind))).map(marker).collect();
    let audio_markers: Vec<Value> = e.markers.iter().filter(|mk| has_music && on_music(mk.kind)).map(marker).collect();

    let track = |name: &str, kind: &str, children: Vec<Value>, markers: Vec<Value>| {
        json!({
            "OTIO_SCHEMA": "Track.1",
            "name": name,
            "metadata": {},
            "source_range": null,
            "effects": [],
            "markers": markers,
            "enabled": true,
            "kind": kind,
            "children": children,
        })
    };
    let mut tracks = vec![track("Video 1", "Video", video_children, video_markers)];
    if has_music {
        tracks.push(track("Music", "Audio", audio_children, audio_markers));
    }
    json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": e.name,
        "metadata": { "vibecut": {
            "generator": e.generator,
            "fps": e.rate.label(),
            "width": e.width,
            "height": e.height,
            "notes": e.notes,
        }},
        "global_start_time": otio_time(0, rate),
        "tracks": {
            "OTIO_SCHEMA": "Stack.1",
            "name": "tracks",
            "metadata": {},
            "source_range": null,
            "effects": [],
            "markers": [],
            "enabled": true,
            "children": tracks,
        },
    })
}

fn otio_dissolve(half: i64, rate: Rate) -> Value {
    json!({
        "OTIO_SCHEMA": "Transition.1",
        "name": "Cross Dissolve",
        "metadata": {},
        "transition_type": "SMPTE_Dissolve",
        "in_offset": otio_time(half, rate),
        "out_offset": otio_time(half, rate),
    })
}

fn otio_gap(frames: i64, rate: Rate) -> Value {
    json!({
        "OTIO_SCHEMA": "Gap.1",
        "name": "",
        "metadata": {},
        "source_range": otio_range(otio_time(0, rate), otio_time(frames, rate)),
        "effects": [],
        "markers": [],
        "enabled": true,
    })
}

// ---------- FCPXML 1.10 ----------

/// XML-escape text or an attribute value; drops characters XML 1.0
/// can't carry.
pub fn xml_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            '\n' => o.push_str("&#10;"),
            '\r' => o.push_str("&#13;"),
            '\t' => o.push_str("&#9;"),
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
            c => o.push(c),
        }
    }
    o
}

/// Indented XML, one element per line.
struct Xml {
    out: String,
    depth: usize,
}

impl Xml {
    fn tag(&mut self, name: &str, attrs: &[(&str, String)], close: bool) {
        for _ in 0..self.depth {
            self.out.push_str("    ");
        }
        self.out.push('<');
        self.out.push_str(name);
        for (k, v) in attrs {
            self.out.push_str(&format!(" {k}=\"{}\"", xml_escape(v)));
        }
        self.out.push_str(if close { "/>\n" } else { ">\n" });
        if !close {
            self.depth += 1;
        }
    }

    fn open(&mut self, name: &str, attrs: &[(&str, String)]) {
        self.tag(name, attrs, false);
    }

    fn empty(&mut self, name: &str, attrs: &[(&str, String)]) {
        self.tag(name, attrs, true);
    }

    fn close(&mut self, name: &str) {
        self.depth -= 1;
        for _ in 0..self.depth {
            self.out.push_str("    ");
        }
        self.out.push_str(&format!("</{name}>\n"));
    }

    fn text(&mut self, name: &str, text: &str) {
        for _ in 0..self.depth {
            self.out.push_str("    ");
        }
        self.out.push_str(&format!("<{name}>{}</{name}>\n", xml_escape(text)));
    }
}

/// Final Cut Pro's built-in Cross Dissolve and the audio crossfade that
/// goes with it.
const CROSS_DISSOLVE_UID: &str = "FxPlug:4731E73A-8DAC-4113-9A30-AE85B1761265";
const AUDIO_CROSSFADE_UID: &str = "FFAudioTransition";

/// The edit as FCPXML 1.10: one asset per source file, a project whose
/// spine holds the clips, card gaps and cross dissolves, the music
/// connected below on lane −1 with its beats as markers.
pub fn to_fcpxml(e: &Edit) -> String {
    let rate = e.rate;
    let frames = |n: i64| Rt::units(n, rate).fcpxml();
    let one_frame = || Rt::units(1, rate).fcpxml();
    let mut x = Xml { out: String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE fcpxml>\n\n"), depth: 0 };
    x.open("fcpxml", &[("version", "1.10".into())]);

    // Resources: formats, assets, effects.
    x.open("resources", &[]);
    let mut next_id = 1;
    let mut id = || {
        let s = format!("r{next_id}");
        next_id += 1;
        s
    };
    let seq_format = id();
    x.empty(
        "format",
        &[
            ("id", seq_format.clone()),
            ("frameDuration", Rt::units(1, rate).fcpxml()),
            ("width", e.width.to_string()),
            ("height", e.height.to_string()),
        ],
    );
    let mut formats: Vec<((Rate, u32, u32), String)> = vec![((rate, e.width, e.height), seq_format.clone())];
    let mut asset_ids = Vec::with_capacity(e.media.len());
    for med in &e.media {
        let format = match med.video {
            Some(v) => match formats.iter().find(|(k, _)| *k == (v.rate, v.width, v.height)) {
                Some((_, fid)) => Some(fid.clone()),
                None => {
                    let fid = id();
                    x.empty(
                        "format",
                        &[
                            ("id", fid.clone()),
                            ("frameDuration", Rt::units(1, v.rate).fcpxml()),
                            ("width", v.width.to_string()),
                            ("height", v.height.to_string()),
                        ],
                    );
                    formats.push(((v.rate, v.width, v.height), fid.clone()));
                    Some(fid)
                }
            },
            // Footage that couldn't be probed: assume the sequence's format.
            None if !med.is_music => Some(seq_format.clone()),
            None => None,
        };
        let aid = id();
        let end = med.duration_units.unwrap_or(med.used_end_units).max(med.used_end_units);
        let mut attrs = vec![
            ("id", aid.clone()),
            ("name", med.name.clone()),
            ("start", Rt::units(med.start_units, med.grid).fcpxml()),
            ("duration", Rt::units(end - med.start_units, med.grid).fcpxml()),
        ];
        if let Some(f) = &format {
            attrs.push(("hasVideo", "1".into()));
            attrs.push(("format", f.clone()));
            attrs.push(("videoSources", "1".into()));
        }
        let audio = med.audio.or(if med.probed { None } else { Some(AudioFacts { sample_rate: 48_000, channels: 2 }) });
        if let Some(a) = audio {
            attrs.push(("hasAudio", "1".into()));
            attrs.push(("audioSources", "1".into()));
            attrs.push(("audioChannels", a.channels.max(1).to_string()));
            attrs.push(("audioRate", a.sample_rate.to_string()));
        }
        x.open("asset", &attrs);
        x.empty("media-rep", &[("kind", "original-media".into()), ("src", med.url.clone())]);
        x.close("asset");
        asset_ids.push(aid);
    }
    let has_transitions = e.video.iter().any(|v| matches!(v, VideoItem::Transition(_)));
    let (dissolve_id, crossfade_id) = if has_transitions {
        let (d, c) = (id(), id());
        x.empty("effect", &[("id", d.clone()), ("name", "Cross Dissolve".into()), ("uid", CROSS_DISSOLVE_UID.into())]);
        x.empty("effect", &[("id", c.clone()), ("name", "Audio Crossfade".into()), ("uid", AUDIO_CROSSFADE_UID.into())]);
        (d, c)
    } else {
        (String::new(), String::new())
    };
    x.close("resources");

    x.open("library", &[]);
    x.open("event", &[("name", "Vibecut".into())]);
    x.open("project", &[("name", e.name.clone())]);
    x.open(
        "sequence",
        &[
            ("format", seq_format.clone()),
            ("duration", frames(e.frames)),
            ("tcStart", "0s".into()),
            ("tcFormat", "NDF".into()),
            ("audioLayout", "stereo".into()),
            ("audioRate", "48k".into()),
        ],
    );
    x.open("spine", &[]);

    // The music is connected to the storyline's first item (it runs from
    // the top, and trimming a clip in the middle must not drag it along);
    // markers go on the music (beats, ducks, the fade) or on the storyline
    // item they fall in.
    let spine_at = |f: i64| -> Option<usize> {
        e.video.iter().position(|v| match v {
            VideoItem::Clip(c) => (c.offset..c.offset + c.duration).contains(&f),
            VideoItem::Gap(g) => (g.offset..g.offset + g.duration).contains(&f),
            VideoItem::Transition(_) => false,
        })
    };
    let has_music = !e.music.is_empty();
    let on_music = |k: MarkerKind| has_music && matches!(k, MarkerKind::Beat | MarkerKind::Duck | MarkerKind::Fade);
    let music_at = |f: i64| e.music.iter().position(|mc| (mc.offset..mc.offset + mc.duration).contains(&f));
    let mut music_by_item: HashMap<usize, Vec<usize>> = HashMap::new();
    if let Some(first) = spine_at(0) {
        music_by_item.insert(first, (0..e.music.len()).collect());
    }
    let mut markers_by_item: HashMap<usize, Vec<&Marker>> = HashMap::new();
    let mut markers_by_music: HashMap<usize, Vec<&Marker>> = HashMap::new();
    for mk in &e.markers {
        if mk.kind == MarkerKind::Cut {
            continue; // the edit points themselves show the cuts
        }
        if on_music(mk.kind) {
            if let Some(mi) = music_at(mk.frame) {
                markers_by_music.entry(mi).or_default().push(mk);
                continue;
            }
        }
        if let Some(s) = spine_at(mk.frame) {
            markers_by_item.entry(s).or_default().push(mk);
        }
    }
    let marker_attrs = |local: Rt, mk: &Marker| -> Vec<(&str, String)> {
        vec![
            ("start", local.fcpxml()),
            ("duration", if mk.duration > 0 { frames(mk.duration) } else { one_frame() }),
            ("value", mk.name.clone()),
        ]
    };

    // Anchored music, then markers, inside a storyline item whose local
    // time at sequence frame `offset` is `local_start`.
    let children = |x: &mut Xml, idx: usize, offset: i64, local_start: Rt| {
        let local = |f: i64| local_start.add(Rt::units(f - offset, rate));
        for &mi in music_by_item.get(&idx).map(Vec::as_slice).unwrap_or(&[]) {
            let mc = &e.music[mi];
            let med = &e.media[mc.media];
            let start = Rt::units(mc.start_units, med.grid);
            let attrs = vec![
                ("ref", asset_ids[mc.media].clone()),
                ("lane", "-1".to_string()),
                ("offset", local(mc.offset).fcpxml()),
                ("name", mc.name.clone()),
                ("start", start.fcpxml()),
                ("duration", frames(mc.duration)),
                ("audioRole", "music".to_string()),
            ];
            let marks = markers_by_music.get(&mi).map(Vec::as_slice).unwrap_or(&[]);
            if marks.is_empty() {
                x.empty("asset-clip", &attrs);
            } else {
                x.open("asset-clip", &attrs);
                for mk in marks {
                    x.empty("marker", &marker_attrs(start.add(Rt::units(mk.frame - mc.offset, rate)), mk));
                }
                x.close("asset-clip");
            }
        }
        for mk in markers_by_item.get(&idx).map(Vec::as_slice).unwrap_or(&[]) {
            x.empty("marker", &marker_attrs(local(mk.frame), mk));
        }
    };

    // Half of the dissolve after each item: the source a retimed clip's
    // time map must reach past its cut out.
    let half_after: Vec<i64> = (0..e.video.len())
        .map(|i| match e.video.get(i + 1) {
            Some(VideoItem::Transition(t)) => t.half,
            _ => 0,
        })
        .collect();

    for (i, v) in e.video.iter().enumerate() {
        match v {
            VideoItem::Gap(g) => {
                x.open("gap", &[("name", g.name.clone()), ("offset", frames(g.offset)), ("duration", frames(g.duration))]);
                if !g.lines.is_empty() {
                    x.text("note", &g.lines.join("\n"));
                }
                children(&mut x, i, g.offset, Rt::ZERO);
                if !g.lines.is_empty() {
                    x.empty(
                        "marker",
                        &[
                            ("start", "0s".into()),
                            ("duration", frames(g.duration)),
                            ("value", format!("Title: {}", g.lines.join(" / "))),
                        ],
                    );
                }
                x.close("gap");
            }
            VideoItem::Clip(c) => {
                let med = &e.media[c.media];
                let media_start = Rt::units(c.start_units, med.grid);
                // A retimed clip's local time runs with the sequence; its
                // time map takes it to source time (×speed through 0).
                let local_start = if c.speed.is_normal() { media_start } else { media_start.scale(c.speed.q, c.speed.p) };
                x.open(
                    "asset-clip",
                    &[
                        ("ref", asset_ids[c.media].clone()),
                        ("offset", frames(c.offset)),
                        ("name", c.name.clone()),
                        ("start", local_start.fcpxml()),
                        ("duration", frames(c.duration)),
                        ("tcFormat", "NDF".into()),
                    ],
                );
                if !c.why.is_empty() {
                    x.text("note", &c.why);
                }
                if !c.speed.is_normal() {
                    let end = local_start.add(Rt::units(c.duration + half_after[i] + 1, rate));
                    x.open("timeMap", &[]);
                    x.empty("timept", &[("time", "0s".into()), ("value", "0s".into()), ("interp", "linear".into())]);
                    x.empty(
                        "timept",
                        &[("time", end.fcpxml()), ("value", end.scale(c.speed.p, c.speed.q).fcpxml()), ("interp", "linear".into())],
                    );
                    x.close("timeMap");
                }
                if !c.camera_sound && (med.audio.is_some() || !med.probed) {
                    // The render keeps camera sound only where it ducked
                    // the music for it.
                    x.empty("adjust-volume", &[("amount", "-96dB".into())]);
                }
                children(&mut x, i, c.offset, local_start);
                x.close("asset-clip");
            }
            VideoItem::Transition(t) => {
                x.open(
                    "transition",
                    &[("name", "Cross Dissolve".into()), ("offset", frames(t.cut - t.half)), ("duration", frames(2 * t.half))],
                );
                x.empty("filter-video", &[("ref", dissolve_id.clone()), ("name", "Cross Dissolve".into())]);
                x.empty("filter-audio", &[("ref", crossfade_id.clone()), ("name", "Audio Crossfade".into())]);
                x.close("transition");
            }
        }
    }
    x.close("spine");
    x.close("sequence");
    x.close("project");
    x.close("event");
    x.close("library");
    x.close("fcpxml");
    x.out
}

// ---------- The operation ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Otio,
    Fcpxml,
}

impl ExportFormat {
    pub fn parse(s: &str) -> Result<ExportFormat, OpsError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "otio" | "opentimelineio" => Ok(ExportFormat::Otio),
            "fcpxml" | "fcpx" | "finalcut" | "resolve" => Ok(ExportFormat::Fcpxml),
            other => Err(OpsError::invalid_with_hint(format!("unknown timeline format {other:?}"), "use otio or fcpxml")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ExportFormat::Otio => "otio",
            ExportFormat::Fcpxml => "fcpxml",
        }
    }

    pub fn extension(self) -> &'static str {
        self.name()
    }

    pub fn content_type(self) -> &'static str {
        match self {
            ExportFormat::Otio => "application/json",
            ExportFormat::Fcpxml => "application/xml",
        }
    }
}

/// An exported timeline, ready to download or write.
#[derive(Debug, Clone)]
pub struct ExportedTimeline {
    pub format: ExportFormat,
    /// `<video stem>.<otio|fcpxml>`.
    pub file_name: String,
    pub manifest_path: PathBuf,
    pub body: String,
    pub summary: Value,
}

/// The manifest of a rendered video (`<stem>.manifest.json`; a platform
/// version's is its render's), or the path itself when it is one.
pub fn manifest_for(path: &Path) -> Option<PathBuf> {
    if path.to_string_lossy().ends_with(".manifest.json") {
        return path.is_file().then(|| path.to_path_buf());
    }
    [path.to_path_buf(), vibecut::output_format::reformat_source(path)]
        .into_iter()
        .map(|p| p.with_extension("manifest.json"))
        .find(|m| m.is_file())
}

/// Host folders for the container mounts, from `VIBECUT_HOST_FOOTAGE` and
/// `VIBECUT_HOST_PROJECTS`.
pub fn path_map_from_env() -> Vec<(String, String)> {
    path_map(std::env::var("VIBECUT_HOST_FOOTAGE").ok().as_deref(), std::env::var("VIBECUT_HOST_PROJECTS").ok().as_deref())
}

/// [`path_map_from_env`] given the two values. Only absolute host paths
/// count (`/Users/…`, `E:/…`, `\\nas\…`): compose's default `./footage`
/// says nothing about where the folder is.
pub fn path_map(host_footage: Option<&str>, host_projects: Option<&str>) -> Vec<(String, String)> {
    let absolute = |p: &str| {
        let b = p.as_bytes();
        p.starts_with('/') || p.starts_with('\\') || (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'/' | b'\\'))
    };
    [(host_footage, FOOTAGE_ROOT), (host_projects, PROJECTS_ROOT)]
        .into_iter()
        .filter_map(|(host, root)| {
            let host = host?.trim();
            absolute(host).then(|| (root.to_string(), host.to_string()))
        })
        .collect()
}

/// Probe a file for what an export declares about it. `None` when it
/// isn't there or ffprobe can't read it.
pub fn probe_media(path: &Path) -> Option<MediaFacts> {
    if !path.is_file() {
        return None;
    }
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    Some(facts_from_ffprobe(&v))
}

/// [`probe_media`]'s reading of ffprobe's JSON.
pub fn facts_from_ffprobe(v: &Value) -> MediaFacts {
    let streams = v.get("streams").and_then(Value::as_array).cloned().unwrap_or_default();
    let of = |kind: &str| streams.iter().find(|s| s.get("codec_type").and_then(Value::as_str) == Some(kind));
    let num = |s: &Value, k: &str| -> Option<f64> {
        s.get(k).and_then(|x| x.as_f64().or_else(|| x.as_str().and_then(|t| t.parse().ok())))
    };
    let video = of("video")
        .filter(|s| s.get("disposition").and_then(|d| d.get("attached_pic")).and_then(Value::as_i64) != Some(1))
        .and_then(|s| {
            let rate = ["r_frame_rate", "avg_frame_rate"]
                .iter()
                .filter_map(|k| s.get(*k).and_then(Value::as_str))
                .filter_map(Rate::parse)
                .find(|r| (1.0..=240.0).contains(&r.fps()))?;
            Some(VideoFacts { rate, width: num(s, "width")? as u32, height: num(s, "height")? as u32 })
        });
    let audio = of("audio").map(|s| AudioFacts {
        sample_rate: num(s, "sample_rate").unwrap_or(48_000.0) as u32,
        channels: num(s, "channels").unwrap_or(2.0) as u32,
    });
    let duration_secs = v.get("format").and_then(|f| num(f, "duration")).filter(|d| d.is_finite() && *d > 0.0);
    let tag = |o: &Value| o.get("tags").and_then(|t| t.get("timecode")).and_then(Value::as_str).map(str::to_string);
    let timecode = v.get("format").and_then(tag).or_else(|| streams.iter().find_map(tag));
    MediaFacts { video, audio, duration_secs, timecode }
}

/// Export the timeline of the render at `output_path` (a rendered video,
/// one of its platform versions or its manifest; relative to /projects).
/// Blocking: reads the manifest and probes the files it references.
pub fn export_timeline(output_path: &str, format: &str, fps: Option<&str>) -> Result<ExportedTimeline, OpsError> {
    let format = ExportFormat::parse(format)?;
    let rate = match fps.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Some(Rate::parse(s).ok_or_else(|| {
            OpsError::invalid_with_hint(format!("unknown frame rate {s:?}"), "use e.g. 30, 29.97, 25 or 30000/1001")
        })?),
        None => None,
    };
    let video = resolve_projects(output_path);
    let manifest_path = manifest_for(&video)
        .ok_or_else(|| OpsError::not_found_with("manifest", video.with_extension("manifest.json").to_string_lossy().into_owned()))?;
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| OpsError::internal(format!("could not read {}: {e}", manifest_path.display())))?;
    let manifest: Manifest = serde_json::from_str(&text)
        .map_err(|e| OpsError::invalid(format!("{} is not a render manifest: {e}", manifest_path.display())))?;

    let mut paths: Vec<String> = Vec::new();
    if let Some(tl) = &manifest.timeline {
        paths.extend(tl.clips.iter().map(|c| c.source_path.to_string()));
        if let Some(mu) = &tl.music {
            paths.extend(mu.files.iter().map(|f| f.path.to_string()));
        }
    }
    paths.extend(manifest.soundtrack.iter().map(|c| c.file.to_string()));
    paths.sort();
    paths.dedup();
    let facts: HashMap<String, MediaFacts> =
        paths.into_iter().filter_map(|p| probe_media(Path::new(&p)).map(|f| (p, f))).collect();

    let opts = ExportOptions { rate, path_map: path_map_from_env() };
    let edit = build_edit(&manifest, &facts, &opts)?;
    let body = match format {
        ExportFormat::Otio => serde_json::to_string_pretty(&to_otio(&edit)).unwrap_or_default(),
        ExportFormat::Fcpxml => to_fcpxml(&edit),
    };
    let stem = video
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "timeline".into());
    let stem = stem.strip_suffix(".manifest").unwrap_or(&stem).to_string();
    Ok(ExportedTimeline {
        format,
        file_name: format!("{stem}.{}", format.extension()),
        manifest_path,
        body,
        summary: edit.summary(),
    })
}

/// [`export_timeline`] off the async runtime.
pub async fn export_timeline_async(
    output_path: &str,
    format: &str,
    fps: Option<&str>,
) -> Result<ExportedTimeline, OpsError> {
    let (p, f, r) = (output_path.to_string(), format.to_string(), fps.map(str::to_string));
    tokio::task::spawn_blocking(move || export_timeline(&p, &f, r.as_deref()))
        .await
        .map_err(|e| OpsError::internal(format!("export task panicked: {e}")))?
}

/// Export and write the file next to the manifest (what the MCP tool
/// does: an assistant can't take a download). Returns where it went and
/// what it holds.
pub async fn export_timeline_to_file(output_path: &str, format: &str, fps: Option<&str>) -> Result<Value, OpsError> {
    let t = export_timeline_async(output_path, format, fps).await?;
    let dir = t.manifest_path.parent().map(Path::to_path_buf).unwrap_or_default();
    let path = dir.join(&t.file_name);
    tokio::fs::write(&path, t.body.as_bytes())
        .await
        .map_err(|e| OpsError::internal(format!("could not write {}: {e}", path.display())))?;
    Ok(json!({
        "ok": true,
        "format": t.format.name(),
        "path": path.to_string_lossy(),
        "manifest_path": t.manifest_path.to_string_lossy(),
        "summary": t.summary,
    }))
}

#[cfg(test)]
mod tests;
