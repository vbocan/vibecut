//! Music-led pacing: where the cuts go, given the song as it plays under
//! the video (see `music_structure` for the analysis it reads).
//!
//! The plan works on the soundtrack's beats in *output* time
//! ([`grid_in_output`]), each carrying the song's intensity there and
//! whether it is a downbeat or a phrase start:
//!
//! - **Clip lengths follow the song** ([`plan_cuts`]). Every beat gets a
//!   cut density `2^(3·strength·(intensity − ½))`: loud, busy music asks
//!   for more cuts, quiet music for fewer. The body is split so every clip
//!   covers the same share of that density, so clips are short where the
//!   song is loud and long where it is quiet — within each clip's own
//!   `[min, max]` beats (the configured clip lengths and the footage it
//!   has). Clip count ([`slot_count`]) follows the same density, so a loud
//!   song gets more, shorter clips.
//! - **Cuts prefer bars and phrases.** Each cut lands on a beat; a cut on
//!   a downbeat or a phrase start earns a bonus, bigger when it is a bigger
//!   change (a new source or scene), and a hold that runs through a phrase
//!   start pays a penalty. The cuts are chosen for the whole body at once
//!   (a dynamic program), so no stretch is left with the leftovers.
//! - **Anchors** pin a clip's start to a beat — the best moment on the
//!   downbeat that opens the song's drop ([`place_hero`]).
//! - **The ending** ([`plan_ending`]): the last cut lands on the song's
//!   final hit when the song's own ending fits the video, else on the
//!   phrase start nearest the target length (a downbeat if no phrase start
//!   is near; either may come a little early rather than overrun). The
//!   video then ends on a downbeat — at that cut, or a whole number of bars
//!   into the outro card — so the music's fade completes at the end of a
//!   bar.

use crate::music_structure::MusicStructure;
use crate::video_renderer::SoundtrackSpan;

/// How strongly intensity bends the cut density: at full strength a
/// clip in the loudest music is 2^1.5 ≈ 2.8× shorter than a neutral one,
/// and 2.8× longer in the quietest.
const DENSITY_OCTAVES: f64 = 3.0;
/// Weight of the length error (|ln(actual / ideal)|) against the musical
/// bonuses when choosing a cut.
const LENGTH_WEIGHT: f64 = 2.0;
/// Cut bonuses: (big change, small change).
const PHRASE_BONUS: (f64, f64) = (1.0, 0.35);
const DOWNBEAT_BONUS: (f64, f64) = (0.6, 0.2);
const MAJOR_PHRASE_BONUS: f64 = 0.2;
/// Penalty for each phrase start a clip runs through without a cut.
const PHRASE_CROSS_PENALTY: f64 = 0.4;

/// One beat of the soundtrack as it plays in the output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaceBeat {
    /// Output time, seconds.
    pub t: f64,
    /// 0..1, how loud and busy the song is here.
    pub intensity: f32,
    pub downbeat: bool,
    pub phrase: bool,
    /// An 8-bar phrase boundary.
    pub major: bool,
    /// When a section that could be the drop starts here: its score.
    pub drop_score: Option<f32>,
    /// The song's drop (`MusicStructure::drop`) starts here.
    pub drop: bool,
}

/// The song's beats, with their structure, as the mux lays the soundtrack
/// under the output (`spans`, loop copies included), sorted by time.
pub fn grid_in_output(beats: &[f32], s: &MusicStructure, spans: &[SoundtrackSpan]) -> Vec<PaceBeat> {
    let drop_at: Vec<(usize, f32)> = (0..s.sections.len())
        .filter_map(|i| s.drop_score(i).map(|score| (s.sections[i].start_beat, score)))
        .collect();
    let drop_beat = s.drop.map(|i| s.sections[i].start_beat);
    let major = |k: usize| s.major_phrases.binary_search(&k).is_ok();
    let mut out = Vec::new();
    for span in spans {
        let len = span.output_end - span.output_start;
        for (k, &b) in beats.iter().enumerate() {
            let rel = b - span.source_start;
            if !(0.0..len).contains(&rel) {
                continue;
            }
            out.push(PaceBeat {
                t: (span.output_start + rel) as f64,
                intensity: s.intensity.get(k).copied().unwrap_or(0.5),
                downbeat: s.is_downbeat(k),
                phrase: s.is_phrase_start(k),
                major: major(k),
                drop_score: drop_at.iter().find(|d| d.0 == k).map(|d| d.1),
                drop: drop_beat == Some(k),
            });
        }
    }
    out.sort_by(|a, b| a.t.total_cmp(&b.t));
    out
}

/// Index of the grid beat nearest `t`.
pub fn nearest_beat(grid: &[PaceBeat], t: f64) -> Option<usize> {
    let i = grid.partition_point(|b| b.t < t);
    [i.checked_sub(1), (i < grid.len()).then_some(i)]
        .into_iter()
        .flatten()
        .min_by(|&a, &b| (grid[a].t - t).abs().total_cmp(&(grid[b].t - t).abs()))
}

/// What a clip in one slot of the plan may be.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slot {
    /// Fewest / most beats it may last.
    pub min_beats: usize,
    pub max_beats: usize,
    /// The cut into this clip is a big change (a new source or scene).
    pub big_change: bool,
}

impl Slot {
    pub fn flexible(min_beats: usize, max_beats: usize, big_change: bool) -> Self {
        let min_beats = min_beats.max(1);
        Self { min_beats, max_beats: max_beats.max(min_beats), big_change }
    }

    pub fn fixed(beats: usize, big_change: bool) -> Self {
        Self::flexible(beats, beats, big_change)
    }

    fn is_fixed(&self) -> bool {
        self.min_beats == self.max_beats
    }
}

/// Slot `slot` must start on grid beat `beat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub slot: usize,
    pub beat: usize,
}

/// How the song bends the pacing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pace {
    /// 0 = every clip the neutral length; 1 = the full swing.
    pub strength: f32,
    /// A neutral clip, in beats (the configured clip length).
    pub base_beats: f64,
}

impl Pace {
    fn density(&self, intensity: f32) -> f64 {
        let s = self.strength.clamp(0.0, 1.0) as f64;
        2f64.powf(DENSITY_OCTAVES * s * (intensity as f64 - 0.5))
    }

    /// `prefix[k]` = density of grid beats `0..k`.
    fn prefix(&self, grid: &[PaceBeat]) -> Vec<f64> {
        let mut p = Vec::with_capacity(grid.len() + 1);
        p.push(0.0);
        let mut acc = 0.0;
        for b in grid {
            acc += self.density(b.intensity);
            p.push(acc);
        }
        p
    }
}

/// How many clips the body from grid beat `start` to `end` should have:
/// its total cut density in neutral clips — each beat asking for at most
/// a `min_beats` clip's worth and at least a `max_beats` one's, since no
/// clip can be shorter or longer — within what such clips can fill.
pub fn slot_count(grid: &[PaceBeat], start: usize, end: usize, pace: &Pace, min_beats: usize, max_beats: usize) -> usize {
    if end <= start || end > grid.len() {
        return 0;
    }
    let n = end - start;
    let (min_beats, max_beats) = (min_beats.max(1), max_beats.max(min_beats.max(1)));
    let per_beat = |b: &PaceBeat| {
        (pace.density(b.intensity) / pace.base_beats.max(1.0)).clamp(1.0 / max_beats as f64, 1.0 / min_beats as f64)
    };
    let k = grid[start..end].iter().map(per_beat).sum::<f64>().round().max(1.0) as usize;
    let fewest = n.div_ceil(max_beats);
    let most = (n / min_beats).max(1);
    k.clamp(fewest.min(most), most)
}

/// Plan the cuts: grid indices `b_0 = start, b_1, …, b_K = end`, clip `i`
/// spanning `b_i..b_{i+1}`. `None` when the slots can't fill the body (or
/// reach an anchor) within their `[min, max]` beats.
///
/// The plan is the cheapest over every way of placing the cuts (a dynamic
/// program over (clip, beat), so a hold early on never squeezes the clips
/// at the end): each flexible clip should cover the same share of the
/// cut density — `|ln(covered / share)|`, weighted — less the bonus of the
/// beat it ends on (a phrase start or a downbeat, more for a cut into a
/// new source or scene), plus a penalty for each phrase start it runs
/// through. Ties go to the earliest cuts.
pub fn plan_cuts(
    grid: &[PaceBeat],
    start: usize,
    end: usize,
    slots: &[Slot],
    anchors: &[Anchor],
    pace: &Pace,
) -> Option<Vec<usize>> {
    let k = slots.len();
    if k == 0 || end <= start || end >= grid.len() {
        return None;
    }
    let prefix = pace.prefix(grid);
    // Where each slot must start: the first anchor given for it.
    let mut pin: Vec<Option<usize>> = vec![None; k + 1];
    for a in anchors.iter().filter(|a| a.slot > 0 && a.slot < k && a.beat > start && a.beat < end) {
        pin[a.slot].get_or_insert(a.beat);
    }
    // What the slots before `i` and from `i` on can fill.
    let (mut before, mut rest) = (vec![(0usize, 0usize); k + 1], vec![(0usize, 0usize); k + 1]);
    for i in 0..k {
        before[i + 1] = (before[i].0 + slots[i].min_beats, before[i].1 + slots[i].max_beats);
    }
    for i in (0..k).rev() {
        rest[i] = (rest[i + 1].0 + slots[i].min_beats, rest[i + 1].1 + slots[i].max_beats);
    }
    if !(rest[0].0..=rest[0].1).contains(&(end - start)) {
        return None;
    }
    // The beats slot `i` can start on: reachable from the start, able to
    // fill the rest, its anchor if it has one.
    let mut window = Vec::with_capacity(k + 1);
    for i in 0..=k {
        let lo = (start + before[i].0).max(end.saturating_sub(rest[i].1));
        let hi = (start + before[i].1).min(end - rest[i].0);
        window.push(match pin[i] {
            Some(b) if (lo..=hi).contains(&b) => (b, b),
            Some(_) => return None,
            None if lo <= hi => (lo, hi),
            None => return None,
        });
    }
    // A flexible clip's share of the density; a fixed one counts as its
    // own length in neutral clips.
    let weight = |s: &Slot| if s.is_fixed() { s.min_beats as f64 / pace.base_beats.max(1.0) } else { 1.0 };
    let share = (prefix[end] - prefix[start]) / slots.iter().map(weight).sum::<f64>().max(1e-9);

    // cost[i][p − lo_i]: the cheapest plan of slots 0..i with slot i
    // starting on beat p; from[i][…]: where slot i − 1 started.
    let row = |i: usize| vec![f64::INFINITY; window[i].1 - window[i].0 + 1];
    let mut cost: Vec<Vec<f64>> = (0..=k).map(row).collect();
    let mut from: Vec<Vec<usize>> = (0..=k).map(|i| vec![usize::MAX; window[i].1 - window[i].0 + 1]).collect();
    cost[0][0] = 0.0;
    for i in 0..k {
        let next_big = slots.get(i + 1).map(|s| s.big_change);
        let ((lo, hi), (next_lo, next_hi)) = (window[i], window[i + 1]);
        for p in lo..=hi {
            let c0 = cost[i][p - lo];
            if !c0.is_finite() {
                continue;
            }
            for len in slots[i].min_beats..=slots[i].max_beats {
                let q = p + len;
                if q > next_hi {
                    break;
                }
                if q < next_lo {
                    continue;
                }
                let length = if slots[i].is_fixed() {
                    0.0
                } else {
                    LENGTH_WEIGHT * ((prefix[q] - prefix[p]).max(1e-9) / share.max(1e-9)).ln().abs()
                };
                let c = c0 + length + cut_cost(grid, p, q, next_big);
                if c < cost[i + 1][q - next_lo] - 1e-9 {
                    cost[i + 1][q - next_lo] = c;
                    from[i + 1][q - next_lo] = p;
                }
            }
        }
    }
    if !cost[k][0].is_finite() {
        return None;
    }
    let mut cuts = vec![end];
    let mut q = end;
    for i in (1..=k).rev() {
        q = from[i][q - window[i].0];
        cuts.push(q);
    }
    cuts.reverse();
    Some(cuts)
}

/// The musical part of a clip's cost from grid beat `p` to `q`: the bonus
/// of the beat it ends on when another clip follows (`next_big`: whether
/// that clip is a new source or scene; `None` at the end of the body),
/// and a penalty per phrase start it runs through.
fn cut_cost(grid: &[PaceBeat], p: usize, q: usize, next_big: Option<bool>) -> f64 {
    let mut cost = 0.0;
    if let Some(big) = next_big {
        let at = &grid[q];
        let pick = |b: (f64, f64)| if big { b.0 } else { b.1 };
        if at.phrase {
            cost -= pick(PHRASE_BONUS) + if at.major { MAJOR_PHRASE_BONUS } else { 0.0 };
        } else if at.downbeat {
            cost -= pick(DOWNBEAT_BONUS);
        }
    }
    let crossed = grid[p + 1..q].iter().filter(|b| b.phrase).count();
    cost + PHRASE_CROSS_PENALTY * crossed as f64
}

/// Can slot `anchor.slot` start on `anchor.beat` with every clip within
/// its `[min, max]` beats, the body running from `start` to `end`?
pub fn anchor_fits(slots: &[Slot], start: usize, end: usize, anchor: Anchor) -> bool {
    let (h, b) = (anchor.slot, anchor.beat);
    if h == 0 || h >= slots.len() || b <= start || b >= end {
        return false;
    }
    let span = |s: &[Slot]| (s.iter().map(|x| x.min_beats).sum::<usize>(), s.iter().map(|x| x.max_beats).sum::<usize>());
    let (lo_a, hi_a) = span(&slots[..h]);
    let (lo_b, hi_b) = span(&slots[h..]);
    (lo_a..=hi_a).contains(&(b - start)) && (lo_b..=hi_b).contains(&(end - b))
}

/// What the best moment's first beat is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export, rename_all = "snake_case"))]
pub enum HeroSpot {
    /// The downbeat that opens the song's drop (its biggest section).
    Drop,
    /// The downbeat that opens the biggest section that plays in the
    /// video, when the drop itself doesn't (or can't be reached).
    Section,
    /// A phrase start: the song has no section to put it on.
    Phrase,
}

/// Where the best moment goes, and why.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeroPlacement {
    /// The slot it plays in and the grid beat it starts on.
    pub anchor: Anchor,
    pub spot: HeroSpot,
}

/// Choose the slot and beat for the best moment. `hero` is its slot when
/// the order is fixed (chronological); `None` lets any slot in `allowed`
/// take it (the energy arc is arranged around it afterwards). `natural`
/// is the plan without anchors (`plan_cuts`), for "where it would be".
///
/// The beat is the first downbeat of the song's drop when it plays inside
/// the body and the slot can reach it; else of the biggest section that
/// does — of sections equally big (within `DROP_TIE`), the one nearest
/// 45 % of the body, where the energy arc would peak. Without one, the
/// phrase start nearest where the moment would play anyway (an 8-bar
/// boundary preferred).
pub fn place_hero(
    grid: &[PaceBeat],
    slots: &[Slot],
    start: usize,
    end: usize,
    hero: Option<usize>,
    allowed: &[bool],
    natural: &[usize],
) -> Option<HeroPlacement> {
    let k = slots.len();
    if k < 3 || natural.len() != k + 1 {
        return None;
    }
    let ok_slot = |h: usize| h > 0 && h + 1 < k && allowed.get(h).copied().unwrap_or(true);
    let golden = grid[start].t + 0.45 * (grid[end].t - grid[start].t);
    let mut drops: Vec<(usize, f32)> = (start + 1..end).filter_map(|b| grid[b].drop_score.map(|s| (b, s))).collect();
    let top = drops.iter().map(|d| d.1).fold(f32::NEG_INFINITY, f32::max);
    // The drop first (the copy nearest the arc's peak when the song
    // loops), then the biggest sections (equals within the tie), the rest
    // by score.
    let tier = |s: f32| if s >= top - crate::music_structure::DROP_TIE { f32::NEG_INFINITY } else { -s };
    drops.sort_by(|a, b| {
        grid[b.0]
            .drop
            .cmp(&grid[a.0].drop)
            .then(tier(a.1).total_cmp(&tier(b.1)))
            .then((grid[a.0].t - golden).abs().total_cmp(&(grid[b.0].t - golden).abs()))
    });
    // The slot that would host a moment starting on `beat`.
    let slot_for = |beat: usize| -> Option<usize> {
        match hero {
            Some(h) => (ok_slot(h) && anchor_fits(slots, start, end, Anchor { slot: h, beat })).then_some(h),
            None => (1..k - 1)
                .filter(|&h| ok_slot(h) && anchor_fits(slots, start, end, Anchor { slot: h, beat }))
                .min_by_key(|&h| natural[h].abs_diff(beat)),
        }
    };
    for &(beat, _) in &drops {
        if let Some(slot) = slot_for(beat) {
            let spot = if grid[beat].drop { HeroSpot::Drop } else { HeroSpot::Section };
            return Some(HeroPlacement { anchor: Anchor { slot, beat }, spot });
        }
    }
    // No drop fits: the nearest phrase start to where the moment would be.
    let home = match hero {
        Some(h) if h < k => natural[h],
        Some(_) => return None,
        None => natural[k / 2],
    };
    let mut phrases: Vec<usize> = (start + 1..end).filter(|&b| grid[b].phrase).collect();
    phrases.sort_by_key(|&b| (b.abs_diff(home) as i64 - if grid[b].major { 2 } else { 0 }, b));
    phrases.into_iter().find_map(|beat| {
        let slot = match hero {
            Some(h) => (ok_slot(h) && anchor_fits(slots, start, end, Anchor { slot: h, beat })).then_some(h)?,
            None => slot_for(beat)?,
        };
        Some(HeroPlacement { anchor: Anchor { slot, beat }, spot: HeroSpot::Phrase })
    })
}

/// How the edit ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export, rename_all = "snake_case"))]
pub enum EndingKind {
    /// The song plays to its own end; the last cut is on its final hit.
    SongEnd,
    /// The last cut is on a phrase start.
    PhraseEnd,
    /// The last cut is on a downbeat (no phrase start near the target).
    BarEnd,
    /// The last cut is on a beat (no bars near the target).
    Beat,
}

/// What [`plan_ending`] decided.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ending {
    pub kind: EndingKind,
    /// Grid beat of the last cut: into the outro card, the start of the
    /// ring-out after the final hit, or the end of the video itself.
    pub last_cut: usize,
    /// Where the video ends (output seconds).
    pub video_end_secs: f64,
    /// The outro card's length on the chain (card mode only).
    pub outro_card_secs: Option<f64>,
    /// With no outro card at the song's end: the final clip holds from the
    /// final hit through the ring-out, this long.
    pub ring_out_secs: Option<f64>,
}

/// What the ending has to work with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EndingInput {
    /// The video's target length.
    pub target_secs: f64,
    /// Outro card in card mode: (configured length, crossfade into it).
    pub outro: Option<(f64, f64)>,
    /// When the song plays through un-looped and its own ending fits:
    /// (grid beat of the final hit, output time its sound ends).
    pub song_end: Option<(usize, f64)>,
    /// Earliest and latest grid beat the last cut may be on (what the
    /// footage can fill).
    pub earliest: usize,
    pub latest: usize,
    /// Longest the ring-out after the final hit may hold.
    pub max_ring_out_secs: f64,
    /// `false`: just the nearest beat to the target, the outro card as it
    /// is (`[soundtrack].musical_ending = false`).
    pub musical: bool,
}

/// Does the song's own ending fit a video of `target_secs`? The video
/// would end where the sound ends (or an outro card after the final hit,
/// whichever is later): it may come up to 12 % of the target (or two
/// bars) short, but only 5 % (or a bar) long.
pub fn song_end_fits(final_hit_secs: f64, sound_end_secs: f64, target_secs: f64, outro_secs: Option<f64>, bar_secs: f64) -> bool {
    let end = sound_end_secs.max(final_hit_secs + outro_secs.unwrap_or(0.0));
    let over = end - target_secs;
    over >= -(0.12 * target_secs).max(2.0 * bar_secs) && over <= (0.05 * target_secs).max(bar_secs)
}

/// Plan the ending (see the module docs). `None` when the grid has no
/// beat to end on past `earliest`.
pub fn plan_ending(grid: &[PaceBeat], input: &EndingInput) -> Option<Ending> {
    let last = grid.len().checked_sub(1)?.min(input.latest);
    if input.earliest >= last {
        return None;
    }
    let beat_secs = median_step(grid);
    let bar_secs = 4.0 * beat_secs;
    let song_end = input.song_end.filter(|&(h, _)| input.musical && h >= input.earliest && h <= last);
    if let Some((hit, sound_end)) = song_end {
        let t_hit = grid[hit].t;
        return Some(match input.outro {
            Some((card, seam)) => {
                // The card holds the ring-out: at least its own length, and
                // up to two bars more so the tail can finish under it.
                let len = (sound_end - t_hit + seam * 0.5).clamp(card, card + 2.0 * bar_secs);
                Ending {
                    kind: EndingKind::SongEnd,
                    last_cut: hit,
                    video_end_secs: t_hit - seam * 0.5 + len,
                    outro_card_secs: Some(len),
                    ring_out_secs: None,
                }
            }
            None => {
                let ring = (sound_end - t_hit).clamp(beat_secs, input.max_ring_out_secs.max(beat_secs));
                Ending {
                    kind: EndingKind::SongEnd,
                    last_cut: hit,
                    video_end_secs: t_hit + ring,
                    outro_card_secs: None,
                    ring_out_secs: Some(ring),
                }
            }
        });
    }

    // The cut that the outro card (or the end of the video) follows, as
    // near the target as the footage allows. A musical ending may come a
    // tolerance early but only half that late (lateness counts double):
    // a video a little short beats one that overruns its target.
    let ideal = match input.outro {
        Some((card, seam)) => input.target_secs - card + seam * 0.5,
        None => input.target_secs,
    }
    .clamp(grid[input.earliest].t, grid[last].t);
    let candidates = |pred: &dyn Fn(&PaceBeat) -> bool, tol: f64, major_bonus: f64, late: f64| {
        let off = |k: usize| {
            let d = grid[k].t - ideal;
            if d > 0.0 { late * d } else { -d }
        };
        (input.earliest..=last)
            .filter(|&k| pred(&grid[k]) && off(k) <= tol)
            .min_by(|&a, &b| {
                let cost = |k: usize| off(k) - if grid[k].major { major_bonus } else { 0.0 };
                cost(a).total_cmp(&cost(b)).then(a.cmp(&b))
            })
    };
    let phrase_tol = bar_secs.max(0.08 * input.target_secs);
    let musical = |pred: &dyn Fn(&PaceBeat) -> bool, tol: f64, bonus: f64| {
        if input.musical { candidates(pred, tol, bonus, 2.0) } else { None }
    };
    let (kind, cut) = if let Some(k) = musical(&|b| b.phrase, phrase_tol, 0.5 * bar_secs) {
        (EndingKind::PhraseEnd, k)
    } else if let Some(k) = musical(&|b| b.downbeat, bar_secs, 0.0) {
        (EndingKind::BarEnd, k)
    } else {
        (EndingKind::Beat, candidates(&|_| true, f64::INFINITY, 0.0, 1.0)?)
    };
    let t_cut = grid[cut].t;
    let (video_end_secs, outro_card_secs) = match input.outro {
        Some((card, seam)) if !input.musical => (t_cut - seam * 0.5 + card, None),
        Some((card, seam)) => {
            // A whole number of bars after the cut (at least one), the
            // nearest to the card's own length; the grid's own downbeat
            // when it reaches that far.
            let bars = ((card - seam * 0.5) / bar_secs).round().max(1.0) as usize;
            let end = grid
                .get(cut + 4 * bars)
                .filter(|b| b.downbeat || !grid[cut].downbeat)
                .map_or(t_cut + bars as f64 * bar_secs, |b| b.t);
            (end, Some(end - t_cut + seam * 0.5))
        }
        None => (t_cut, None),
    };
    Some(Ending { kind, last_cut: cut, video_end_secs, outro_card_secs, ring_out_secs: None })
}

// ---------- The plan for one render ----------

/// Two consecutive clips from the same source further apart than this (in
/// recording time) are a new scene: the cut between them is a big change.
const SCENE_GAP_SECS: f64 = 8.0;

/// The music plan for one render, made before the picker runs: the
/// soundtrack's beats as they will play, where the footage starts, how
/// the edit ends and how many clips to look for.
#[derive(Debug, Clone)]
pub struct MusicPlan {
    pub grid: Vec<PaceBeat>,
    /// Grid beat the footage cuts in on (after the intro card).
    pub start: usize,
    pub ending: Ending,
    /// What `ending` was planned from, to re-plan it when the footage
    /// can't fill the body.
    pub ending_input: EndingInput,
    pub pace: Pace,
    pub interval: f64,
    /// `[selection]` clip lengths.
    pub min_clip_secs: f64,
    pub max_clip_secs: f64,
    /// The song plays through once, un-looped (its own ending is used).
    pub play_through: bool,
    /// Put the best moment on the drop.
    pub hero_on_drop: bool,
    /// The intro card's beat-locked length, when the beat lock moves it
    /// (the planned layout uses it).
    pub intro_card_secs: Option<f64>,
    /// Video clips the picker should find.
    pub clips: usize,
}

impl MusicPlan {
    /// Plan the ending and the clip count. `image_secs` is how long the
    /// still images (which keep their length) play.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        grid: Vec<PaceBeat>,
        start: usize,
        mut ending_input: EndingInput,
        pace: Pace,
        interval: f64,
        clip_range: (f64, f64),
        play_through: bool,
        hero_on_drop: bool,
        image_secs: f64,
    ) -> Option<Self> {
        if interval <= 0.0 || start + 1 >= grid.len() {
            return None;
        }
        let mut plan = Self {
            grid,
            start,
            ending: Ending { kind: EndingKind::Beat, last_cut: start, video_end_secs: 0.0, outro_card_secs: None, ring_out_secs: None },
            ending_input,
            pace,
            interval,
            min_clip_secs: clip_range.0,
            max_clip_secs: clip_range.1,
            play_through,
            hero_on_drop,
            clips: 0,
            intro_card_secs: None,
        };
        ending_input.earliest = ending_input.earliest.max(start + plan.min_beats());
        plan.ending_input = ending_input;
        plan.ending = plan_ending(&plan.grid, &ending_input)?;
        let image_beats = (image_secs / interval).round() as usize;
        let flex_end = plan.ending.last_cut.saturating_sub(image_beats).max(start + plan.min_beats());
        plan.clips = slot_count(&plan.grid, start, flex_end.min(plan.grid.len()), &pace, plan.min_beats(), plan.max_beats())
            + usize::from(plan.ending.ring_out_secs.is_some());
        Some(plan)
    }

    pub fn min_beats(&self) -> usize {
        ((self.min_clip_secs / self.interval - 1e-6).ceil().max(1.0)) as usize
    }

    pub fn max_beats(&self) -> usize {
        (((self.max_clip_secs / self.interval) + 1e-6).floor() as usize).max(self.min_beats())
    }

    /// The clip length to ask the picker for: the body's footage time over
    /// the clips (its window size and spacing; each clip's own length is
    /// set once the order is known).
    pub fn pick_secs(&self, image_secs: f64) -> f64 {
        let ring = self.ending.ring_out_secs.unwrap_or(0.0);
        let body = self.grid[self.ending.last_cut].t - self.grid[self.start].t + ring - image_secs;
        (body / self.clips.max(1) as f64).max(self.interval)
    }
}

/// One clip of the ordered cut, as the plan sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipInfo {
    pub source_id: String,
    /// Where it starts in its source's stitched recording time.
    pub virtual_start: f64,
    pub duration: f64,
    /// Where it starts in its chapter file.
    pub in_start: f64,
    /// Length of the chapter it reads from; `None` for a still image.
    pub chapter_secs: Option<f64>,
    /// Keeps its own length (a still image, a forced `[[input.ranges]]`
    /// clip).
    pub fixed: bool,
}

impl ClipInfo {
    fn centre(&self) -> f64 {
        self.virtual_start + self.duration * 0.5
    }
}

/// The slot each clip may fill: fixed clips keep their length (in whole
/// beats, never longer than their footage); a picked clip may last from
/// `min_beats` up to `max_beats`, as long as its footage holds that much
/// around its moment — inside its chapter and clear of the other picks
/// from the same source. A cut into a clip from another source, another
/// scene or a still is a big change.
///
/// "Clear of the other picks" is judged against their shortest length
/// (`strict = false`: two neighbours can't both grow to the gap between
/// them, see [`overlapping`]) or their longest (`strict`: each stays
/// within the gap to its neighbour's moment, never overlapping).
pub fn slots_for(clips: &[ClipInfo], interval: f64, min_beats: usize, max_beats: usize, strict: bool) -> Vec<Slot> {
    let min_len = min_beats as f64 * interval;
    clips
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let big = i == 0 || {
                let p = &clips[i - 1];
                p.source_id != c.source_id
                    || c.chapter_secs.is_none()
                    || p.chapter_secs.is_none()
                    || (c.virtual_start - (p.virtual_start + p.duration)).abs() > SCENE_GAP_SECS
            };
            if c.fixed || c.chapter_secs.is_none() {
                let beats = if c.chapter_secs.is_none() {
                    (c.duration / interval).round()
                } else {
                    (c.duration / interval + 1e-6).floor()
                };
                return Slot::fixed(beats.max(1.0) as usize, big);
            }
            let chapter = c.chapter_secs.unwrap_or(f64::INFINITY);
            let room = clips
                .iter()
                .enumerate()
                .filter(|&(j, o)| j != i && o.source_id == c.source_id)
                .map(|(_, o)| {
                    let gap = (o.centre() - c.centre()).abs();
                    if strict { gap } else { 2.0 * gap - min_len.min(gap) }
                })
                .fold(chapter, f64::min);
            let most = ((room / interval + 1e-6).floor() as usize).clamp(1, max_beats.max(1));
            Slot::flexible(min_beats.min(most), most, big)
        })
        .collect()
}

/// Where a clip of `len` seconds reads, centred on the moment the picker
/// chose and kept inside its chapter: `(in_start, virtual_start)`. Fixed
/// clips and stills keep their start.
pub fn rewindow(c: &ClipInfo, len: f64) -> (f64, f64) {
    match c.chapter_secs {
        Some(chapter) if !c.fixed => {
            let start = (c.in_start + c.duration * 0.5 - len * 0.5).min(chapter - len).max(0.0);
            (start, c.virtual_start + (start - c.in_start))
        }
        _ => (c.in_start, c.virtual_start),
    }
}

/// The ordered clips fitted to the plan: their slots from the footage
/// start to the last cut, and the ending (re-planned when the footage
/// can't fill the planned body).
#[derive(Debug, Clone, PartialEq)]
pub struct Body {
    /// One per clip up to the last cut — every clip but a ring-out.
    pub slots: Vec<Slot>,
    pub start: usize,
    pub end: usize,
    pub ending: Ending,
}

/// Do any two clips from the same source show the same footage once each
/// is re-windowed ([`rewindow`]) to its span's length?
pub fn overlapping(clips: &[ClipInfo], spans: &[(f64, f64)]) -> bool {
    let windows: Vec<(&str, f64, f64)> = clips
        .iter()
        .zip(spans)
        .filter(|(c, _)| c.chapter_secs.is_some())
        .map(|(c, &(a, b))| {
            let v = rewindow(c, b - a).1;
            (c.source_id.as_str(), v, v + (b - a))
        })
        .collect();
    windows.iter().enumerate().any(|(i, x)| {
        windows[i + 1..].iter().any(|y| x.0 == y.0 && x.1 < y.2 - 1e-6 && y.1 < x.2 - 1e-6)
    })
}

/// Fit `clips` (in their final order) to the plan (`strict`: see
/// [`slots_for`]). `None` when they can't fill any body at all.
pub fn body_for(plan: &MusicPlan, clips: &[ClipInfo], strict: bool) -> Option<Body> {
    let all = slots_for(clips, plan.interval, plan.min_beats(), plan.max_beats(), strict);
    let fits = |slots: &[Slot], end: usize| {
        let lo: usize = slots.iter().map(|s| s.min_beats).sum();
        let hi: usize = slots.iter().map(|s| s.max_beats).sum();
        !slots.is_empty() && (lo..=hi).contains(&(end.saturating_sub(plan.start)))
    };
    let ring = plan.ending.ring_out_secs.is_some();
    let planned = if ring { &all[..all.len().saturating_sub(1)] } else { &all[..] };
    if fits(planned, plan.ending.last_cut) {
        return Some(Body { slots: planned.to_vec(), start: plan.start, end: plan.ending.last_cut, ending: plan.ending });
    }
    // The footage can't fill the planned body: end where it can, every
    // clip in the body (no ring-out) — and, when the song plays through
    // un-looped, no later than its final hit, so the video never outlasts it.
    let lo: usize = all.iter().map(|s| s.min_beats).sum();
    let hi: usize = all.iter().map(|s| s.max_beats).sum();
    let song_hit = plan.ending_input.song_end.map_or(usize::MAX, |(hit, _)| hit);
    let input = EndingInput {
        song_end: None,
        earliest: plan.start + lo,
        latest: (plan.start + hi).min(plan.grid.len() - 1).min(song_hit),
        ..plan.ending_input
    };
    let ending = plan_ending(&plan.grid, &input).filter(|e| fits(&all, e.last_cut))?;
    Some(Body { slots: all, start: plan.start, end: ending.last_cut, ending })
}

/// Each clip's visible span `(cut in, cut out)` in output seconds, from
/// the planned cuts (and the ring-out after the final hit).
pub fn clip_spans(plan: &MusicPlan, body: &Body, cuts: &[usize]) -> Vec<(f64, f64)> {
    let mut spans: Vec<(f64, f64)> = cuts.windows(2).map(|w| (plan.grid[w[0]].t, plan.grid[w[1]].t)).collect();
    if let Some(r) = body.ending.ring_out_secs {
        let t = plan.grid[body.end].t;
        spans.push((t, t + r));
    }
    spans
}

// ---------- Manifest record ----------

/// The song's structure as it plays under this video, and what the edit
/// made of it: the manifest's `music` field. Times are output seconds
/// unless named `song_…`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct MusicRecord {
    pub bpm: f32,
    pub beats_per_bar: u32,
    /// 0..1: how clearly the bar's "1" stands out in the song (0 = taken
    /// from where its level steps, or from where the music starts).
    pub downbeat_confidence: f32,
    /// Bar lines and phrase starts in the video.
    pub downbeats: Vec<f64>,
    pub phrases: Vec<f64>,
    pub sections: Vec<SectionRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub song_drop_secs: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub song_final_hit_secs: Option<f32>,
    pub song_sound_end_secs: f32,
    /// Where the best moment went.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub hero: Option<HeroRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub ending: Option<EndingRecord>,
    /// Each clip's length against the song's intensity under it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub pacing: Option<PacingRecord>,
}

/// A section of the song as it plays in the video.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct SectionRecord {
    pub start_secs: f64,
    pub end_secs: f64,
    pub level: crate::music_structure::Level,
    pub intensity: f32,
    /// Starts the song's drop (its biggest section).
    pub drop: bool,
    pub song_start_secs: f32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct HeroRecord {
    /// Index into `segments`.
    pub segment: usize,
    pub start_secs: f64,
    /// What its first beat is: the drop's downbeat, the downbeat of the
    /// biggest section that plays, or a phrase start.
    pub on: HeroSpot,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct EndingRecord {
    pub kind: EndingKind,
    /// The last cut: into the outro card, onto the final hit, or the end
    /// of the video.
    pub last_cut_secs: f64,
    pub video_end_secs: f64,
    /// The music's fade-out, which completes at `video_end_secs`.
    pub fade_secs: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub final_hit_secs: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct PacingRecord {
    /// `[soundtrack].pacing_strength` as applied (0 with
    /// `energy_drives_pacing = false`).
    pub strength: f32,
    pub clips: Vec<PacedClipRecord>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct PacedClipRecord {
    pub segment: usize,
    pub start_secs: f64,
    pub beats: f32,
    /// Mean intensity (0..1) of the song's beats under the clip.
    pub intensity: f32,
}

/// The song's sections as they play in the output (`spans`), up to
/// `until`.
pub fn sections_in_output(s: &MusicStructure, spans: &[SoundtrackSpan], until: f64) -> Vec<SectionRecord> {
    let mut out = Vec::new();
    for span in spans {
        let len = (span.output_end - span.output_start) as f64;
        for (i, sec) in s.sections.iter().enumerate() {
            let a = (sec.start_secs - span.source_start) as f64;
            let b = (sec.end_secs - span.source_start) as f64;
            let (a, b) = (a.max(0.0), b.min(len));
            if b <= a {
                continue;
            }
            let start = span.output_start as f64 + a;
            let end = (span.output_start as f64 + b).min(until);
            // A sliver past the last downbeat (the video ends a frame later) is no section.
            if end - start >= 0.25 {
                out.push(SectionRecord {
                    start_secs: start,
                    end_secs: end,
                    level: sec.level,
                    intensity: sec.intensity,
                    drop: s.drop == Some(i) && a == (sec.start_secs - span.source_start) as f64,
                    song_start_secs: sec.start_secs,
                });
            }
        }
    }
    out.sort_by(|x, y| x.start_secs.total_cmp(&y.start_secs));
    out
}

/// A fade of `secs` rounded to whole beats (at least one; 0 stays 0), so
/// a fade that completes on a downbeat also starts on a beat.
pub fn fade_on_beats(secs: f64, interval: f64) -> f64 {
    if secs <= 0.0 || interval <= 0.0 {
        return secs.max(0.0);
    }
    (secs / interval).round().max(1.0) * interval
}

/// Output time at which song time `t` plays (the first copy that plays it).
fn song_to_output(spans: &[SoundtrackSpan], t: f32) -> Option<f64> {
    spans
        .iter()
        .find(|s| t >= s.source_start && t - s.source_start < s.output_end - s.output_start)
        .map(|s| (s.output_start + (t - s.source_start)) as f64)
}

/// The manifest's `music` record: the song's structure as it plays under
/// the rendered video (`timeline`), and — when the cut was paced — where
/// the best moment went, how it ended and each clip's length against the
/// song's intensity under it.
#[allow(clippy::too_many_arguments)]
pub fn music_record(
    bpm: f32,
    beats: &[f32],
    s: &MusicStructure,
    spans: &[SoundtrackSpan],
    plan: Option<&MusicPlan>,
    hero: Option<(usize, HeroPlacement)>,
    ending: Option<&Ending>,
    timeline: &crate::output_timeline::OutputTimeline,
    fade_secs: f32,
) -> MusicRecord {
    let end = timeline.duration_secs + 1e-6;
    let grid = plan.map_or_else(|| grid_in_output(beats, s, spans), |p| p.grid.clone());
    let at = |pred: &dyn Fn(&PaceBeat) -> bool| grid.iter().filter(|b| pred(b) && b.t <= end).map(|b| b.t).collect();
    let clip_start = |seg: usize| timeline.clips.iter().find(|c| c.segment_index == seg).map(|c| c.cut_in_secs);
    let interval = 60.0 / bpm.max(1.0) as f64;
    MusicRecord {
        bpm,
        beats_per_bar: s.beats_per_bar,
        downbeat_confidence: s.downbeat_confidence,
        downbeats: at(&|b| b.downbeat),
        phrases: at(&|b| b.phrase),
        sections: sections_in_output(s, spans, timeline.duration_secs),
        song_drop_secs: s.drop.map(|i| s.sections[i].start_secs),
        song_final_hit_secs: s.final_hit_secs,
        song_sound_end_secs: s.sound_end_secs,
        hero: hero.and_then(|(seg, p)| {
            Some(HeroRecord { segment: seg, start_secs: clip_start(seg)?, on: p.spot })
        }),
        ending: ending.map(|e| {
            let last_cut_secs = match (&timeline.outro, e.ring_out_secs) {
                (Some(card), _) => card.cut_secs,
                (None, Some(_)) => timeline.clips.last().map_or(timeline.duration_secs, |c| c.cut_in_secs),
                (None, None) => timeline.duration_secs,
            };
            EndingRecord {
                kind: e.kind,
                last_cut_secs,
                video_end_secs: timeline.duration_secs,
                fade_secs,
                final_hit_secs: s.final_hit_secs.and_then(|h| song_to_output(spans, h)).filter(|&t| t <= end),
            }
        }),
        pacing: plan.map(|p| PacingRecord {
            strength: p.pace.strength,
            clips: timeline
                .clips
                .iter()
                .map(|c| PacedClipRecord {
                    segment: c.segment_index,
                    start_secs: c.cut_in_secs,
                    beats: ((c.cut_out_secs - c.cut_in_secs) / interval) as f32,
                    intensity: mean_intensity(&grid, c.cut_in_secs, c.cut_out_secs),
                })
                .collect(),
        }),
    }
}

/// Mean intensity of the grid beats in `[a, b)`.
pub fn mean_intensity(grid: &[PaceBeat], a: f64, b: f64) -> f32 {
    let v: Vec<f32> = grid.iter().filter(|x| x.t >= a - 1e-6 && x.t < b - 1e-6).map(|x| x.intensity).collect();
    if v.is_empty() {
        grid.get(nearest_beat(grid, a).unwrap_or(0)).map_or(0.5, |x| x.intensity)
    } else {
        v.iter().sum::<f32>() / v.len() as f32
    }
}

fn median_step(grid: &[PaceBeat]) -> f64 {
    let mut d: Vec<f64> = grid.windows(2).map(|w| w[1].t - w[0].t).filter(|x| *x > 1e-6).collect();
    if d.is_empty() {
        return 0.5;
    }
    d.sort_by(f64::total_cmp);
    d[d.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 120 BPM grid of `n` beats from `first`, downbeats every 4 from
    /// beat 0, phrases every 16 (majors every 32), with `intensity(k)`.
    fn grid(n: usize, first: f64, intensity: impl Fn(usize) -> f32) -> Vec<PaceBeat> {
        (0..n)
            .map(|k| PaceBeat {
                t: first + k as f64 * 0.5,
                intensity: intensity(k),
                downbeat: k % 4 == 0,
                phrase: k % 16 == 0,
                major: k % 32 == 0,
                drop_score: None,
                drop: false,
            })
            .collect()
    }

    fn lengths(cuts: &[usize]) -> Vec<usize> {
        cuts.windows(2).map(|w| w[1] - w[0]).collect()
    }

    const PACE: Pace = Pace { strength: 1.0, base_beats: 4.0 };

    #[test]
    fn loud_music_gets_short_clips_and_quiet_music_long_ones() {
        // 32 quiet beats, then 32 loud ones.
        let g = grid(80, 0.0, |k| if k < 32 { 0.1 } else { 0.9 });
        let k = slot_count(&g, 0, 64, &PACE, 2, 12);
        let slots = vec![Slot::flexible(2, 12, false); k];
        let cuts = plan_cuts(&g, 0, 64, &slots, &[], &PACE).unwrap();
        assert_eq!(cuts.first(), Some(&0));
        assert_eq!(cuts.last(), Some(&64));
        let quiet: Vec<usize> = cuts.windows(2).filter(|w| w[1] <= 32).map(|w| w[1] - w[0]).collect();
        let loud: Vec<usize> = cuts.windows(2).filter(|w| w[0] >= 32).map(|w| w[1] - w[0]).collect();
        let mean = |v: &[usize]| v.iter().sum::<usize>() as f64 / v.len() as f64;
        assert!(mean(&quiet) >= 2.5 * mean(&loud), "quiet {quiet:?} loud {loud:?}");
        assert!(loud.iter().all(|&l| l <= 4), "loud {loud:?}");
        assert!(quiet.iter().all(|&l| l >= 6), "quiet {quiet:?}");
    }

    #[test]
    fn every_quiet_stretch_gets_its_hold_even_at_the_end() {
        // Two loud bars, two quiet ones, three times over. Each quiet
        // stretch is one long hold — the last one too: the plan is made
        // whole, so the clips before it don't leave it the leftovers.
        let g = grid(80, 0.0, |k| if (k / 8) % 2 == 0 { 0.9 } else { 0.1 });
        let k = slot_count(&g, 0, 48, &PACE, 2, 8);
        assert_eq!(k, 15);
        let slots = vec![Slot::flexible(2, 8, false); k];
        let cuts = plan_cuts(&g, 0, 48, &slots, &[], &PACE).unwrap();
        for w in cuts.windows(2) {
            let loud = (w[0] / 8) % 2 == 0;
            let len = w[1] - w[0];
            assert!(if loud { len <= 3 } else { len >= 6 }, "{} beats at {}: {cuts:?}", len, w[0]);
        }
        assert_eq!(48 - cuts[cuts.len() - 2], 8, "{cuts:?}");
    }

    #[test]
    fn a_loud_song_gets_more_clips_than_a_quiet_one() {
        let loud = grid(80, 0.0, |_| 0.9);
        let quiet = grid(80, 0.0, |_| 0.1);
        let flat = grid(80, 0.0, |_| 0.5);
        let (a, b, c) = (
            slot_count(&loud, 0, 64, &PACE, 2, 16),
            slot_count(&flat, 0, 64, &PACE, 2, 16),
            slot_count(&quiet, 0, 64, &PACE, 2, 16),
        );
        assert!(a > b && b > c, "loud {a}, flat {b}, quiet {c}");
        assert_eq!(b, 16);
        // Strength 0: every song paces like a flat one.
        let off = Pace { strength: 0.0, ..PACE };
        assert_eq!(slot_count(&loud, 0, 64, &off, 2, 16), 16);
    }

    #[test]
    fn clip_lengths_respect_their_limits_and_add_up() {
        let g = grid(120, 0.0, |k| ((k as f32 * 0.37).sin() * 0.5 + 0.5).clamp(0.0, 1.0));
        let slots: Vec<Slot> = (0..20).map(|i| Slot::flexible(3, if i % 3 == 0 { 5 } else { 8 }, i % 2 == 0)).collect();
        let cuts = plan_cuts(&g, 4, 104, &slots, &[], &PACE).unwrap();
        assert_eq!(cuts.len(), 21);
        assert_eq!((cuts[0], cuts[20]), (4, 104));
        for (i, l) in lengths(&cuts).into_iter().enumerate() {
            assert!(l >= slots[i].min_beats && l <= slots[i].max_beats, "slot {i}: {l} beats");
        }
        // Over-full and under-full bodies are refused.
        assert!(plan_cuts(&g, 0, 119, &slots[..5], &[], &PACE).is_none());
        assert!(plan_cuts(&g, 0, 10, &slots, &[], &PACE).is_none());
    }

    #[test]
    fn big_changes_land_on_bars_and_phrases() {
        // Flat song, neutral 6-beat clips: without bar preference the cuts
        // would fall every 6 beats (on a bar every other time, running
        // through both phrase starts); a source change moves them to bars,
        // and always to the phrase starts.
        let g = grid(100, 0.0, |_| 0.5);
        let pace = Pace { strength: 0.5, base_beats: 6.0 };
        let on_bars = |cuts: &[usize]| cuts[1..cuts.len() - 1].iter().filter(|&&c| c % 4 == 0).count();
        let big = vec![Slot::flexible(4, 8, true); 8];
        let cuts = plan_cuts(&g, 0, 48, &big, &[], &pace).unwrap();
        assert!(cuts.contains(&16) && cuts.contains(&32), "phrase starts are cuts: {cuts:?}");
        assert!(on_bars(&cuts) >= 5, "{cuts:?}");
        // The same clips from one continuous shot sequence need bars less.
        let small = vec![Slot::flexible(4, 8, false); 8];
        let free = plan_cuts(&g, 0, 48, &small, &[], &pace).unwrap();
        assert!(on_bars(&free) < on_bars(&cuts), "{free:?} vs {cuts:?}");
    }

    #[test]
    fn fixed_slots_keep_their_length() {
        let g = grid(80, 0.0, |_| 0.5);
        let slots = vec![
            Slot::flexible(2, 8, true),
            Slot::fixed(8, true),
            Slot::flexible(2, 8, true),
            Slot::flexible(2, 8, true),
        ];
        let cuts = plan_cuts(&g, 0, 24, &slots, &[], &PACE).unwrap();
        assert_eq!(cuts[2] - cuts[1], 8, "{cuts:?}");
        assert_eq!(*cuts.last().unwrap(), 24);
    }

    #[test]
    fn an_anchor_pins_a_clip_to_its_beat() {
        let g = grid(100, 0.0, |_| 0.5);
        let slots = vec![Slot::flexible(2, 8, true); 12];
        let a = Anchor { slot: 5, beat: 32 };
        assert!(anchor_fits(&slots, 0, 64, a));
        let cuts = plan_cuts(&g, 0, 64, &slots, &[a], &PACE).unwrap();
        assert_eq!(cuts[5], 32, "{cuts:?}");
        assert_eq!(cuts[12], 64);
        // Five clips can't reach beat 60 at 8 beats each.
        assert!(!anchor_fits(&slots, 0, 64, Anchor { slot: 5, beat: 60 }));
    }

    /// Sections that could be the drop start at `drops` (beat, score);
    /// the song's drop starts at `drop` (when it plays).
    fn with_drops(mut g: Vec<PaceBeat>, drops: &[(usize, f32)], drop: Option<usize>) -> Vec<PaceBeat> {
        for &(k, s) in drops {
            g[k].drop_score = Some(s);
            g[k].drop = drop == Some(k);
        }
        g
    }

    #[test]
    fn the_hero_goes_on_the_biggest_drop_that_plays() {
        let g = with_drops(grid(120, 0.0, |_| 0.5), &[(16, 1.0), (48, 1.4), (100, 0.5)], Some(48));
        let slots = vec![Slot::flexible(2, 8, true); 16];
        let natural = plan_cuts(&g, 0, 80, &slots, &[], &PACE).unwrap();
        let allowed = vec![true; 16];
        // Free order (energy arc): the biggest drop, in the slot nearest it.
        let p = place_hero(&g, &slots, 0, 80, None, &allowed, &natural).unwrap();
        assert_eq!(p.spot, HeroSpot::Drop);
        assert_eq!(p.anchor.beat, 48);
        assert_eq!(natural[p.anchor.slot].abs_diff(48), natural.iter().map(|&c| c.abs_diff(48)).min().unwrap());
        // Fixed order with the hero 3rd: beat 48 can't be reached by two
        // clips of ≤ 8 beats, so the next-biggest section (16) takes it.
        let p = place_hero(&g, &slots, 0, 80, Some(2), &allowed, &natural).unwrap();
        assert_eq!(p.anchor, Anchor { slot: 2, beat: 16 });
        assert_eq!(p.spot, HeroSpot::Section);
    }

    #[test]
    fn the_drop_wins_and_without_it_the_section_where_the_arc_peaks() {
        // Four equally big choruses (within the tie), the first the song's
        // drop: it takes the best moment, far from the middle as it is.
        let choruses = [(8, 1.50), (32, 1.51), (56, 1.52), (72, 1.53)];
        let g = with_drops(grid(120, 0.0, |_| 0.5), &choruses, Some(8));
        let slots = vec![Slot::flexible(2, 8, true); 16];
        let natural = plan_cuts(&g, 0, 80, &slots, &[], &PACE).unwrap();
        let p = place_hero(&g, &slots, 0, 80, None, &vec![true; 16], &natural).unwrap();
        assert_eq!((p.anchor.beat, p.spot), (8, HeroSpot::Drop), "{p:?}");
        // The drop plays before the footage starts: of the rest, the one
        // nearest 45 % of the body (beat 36) — the chorus at 32.
        let g = with_drops(grid(120, 0.0, |_| 0.5), &choruses, None);
        let p = place_hero(&g, &slots, 0, 80, None, &vec![true; 16], &natural).unwrap();
        assert_eq!((p.anchor.beat, p.spot), (32, HeroSpot::Section), "{p:?}");
        // A clearly bigger section wins wherever it is.
        let g = with_drops(grid(120, 0.0, |_| 0.5), &[(8, 1.5), (32, 1.5), (72, 1.7)], None);
        let p = place_hero(&g, &slots, 0, 80, None, &vec![true; 16], &natural).unwrap();
        assert_eq!((p.anchor.beat, p.spot), (72, HeroSpot::Section), "{p:?}");
    }

    #[test]
    fn without_a_drop_the_hero_takes_the_nearest_phrase_start() {
        let g = grid(120, 0.0, |_| 0.5);
        let slots = vec![Slot::flexible(2, 8, true); 16];
        let natural = plan_cuts(&g, 0, 80, &slots, &[], &PACE).unwrap();
        let p = place_hero(&g, &slots, 0, 80, Some(7), &vec![true; 16], &natural).unwrap();
        assert_eq!(p.spot, HeroSpot::Phrase);
        assert!(g[p.anchor.beat].phrase);
        assert_eq!(p.anchor.slot, 7);
        assert!(p.anchor.beat.abs_diff(natural[7]) <= 16, "{p:?} natural {}", natural[7]);
    }

    #[test]
    fn the_ending_takes_the_phrase_start_nearest_the_target() {
        // Phrases every 8 s from 0.25 s. A 33.5 s target: the phrase start
        // at 32.25 s, 1.25 s early (within 8 % = 2.68 s).
        let g = grid(200, 0.25, |_| 0.5);
        let input = EndingInput { target_secs: 33.5, outro: None, song_end: None, earliest: 10, latest: usize::MAX, max_ring_out_secs: 8.0, musical: true };
        let e = plan_ending(&g, &input).unwrap();
        assert_eq!(e.kind, EndingKind::PhraseEnd);
        assert!((e.video_end_secs - 32.25).abs() < 1e-9, "{e:?}");
        assert_eq!(e.outro_card_secs, None);
        // 31.5 s: 0.75 s late is within half the tolerance.
        let e = plan_ending(&g, &EndingInput { target_secs: 31.5, ..input }).unwrap();
        assert_eq!((e.kind, e.video_end_secs), (EndingKind::PhraseEnd, 32.25));
        // 30 s: the phrase start would overrun it by 2.25 s (7.5 %), so the
        // nearest bar line takes it: 30.25 s, half a beat late.
        let e = plan_ending(&g, &EndingInput { target_secs: 30.0, ..input }).unwrap();
        assert_eq!((e.kind, e.video_end_secs), (EndingKind::BarEnd, 30.25));
        // A 20 s target is 3.75 s from the nearest phrase start: a downbeat.
        let e = plan_ending(&g, &EndingInput { target_secs: 20.0, ..input }).unwrap();
        assert_eq!(e.kind, EndingKind::BarEnd);
        assert!(g[e.last_cut].downbeat && (e.video_end_secs - 20.0).abs() <= 1.0, "{e:?}");
    }

    #[test]
    fn an_outro_card_ends_a_whole_number_of_bars_after_the_last_cut() {
        let g = grid(200, 0.25, |_| 0.5);
        // 3 s card, 0.4 s crossfade: the cut belongs near 27.2 s. The
        // nearest phrase start (24.25 s) is 2.95 s off (> 2.4 s), so it
        // takes the nearest downbeat, 26.25 s.
        let input = EndingInput { target_secs: 30.0, outro: Some((3.0, 0.4)), song_end: None, earliest: 10, latest: usize::MAX, max_ring_out_secs: 8.0, musical: true };
        let e = plan_ending(&g, &input).unwrap();
        let t_cut = g[e.last_cut].t;
        assert!(g[e.last_cut].downbeat);
        // (3 − 0.2) / 2 s per bar → 1 bar: the video ends 2 s after the cut.
        assert!((e.video_end_secs - (t_cut + 2.0)).abs() < 1e-9, "{e:?}");
        assert!((e.outro_card_secs.unwrap() - 2.2).abs() < 1e-9, "{e:?}");
        assert!(g[e.last_cut + 4].downbeat);
    }

    #[test]
    fn the_songs_own_ending_puts_the_last_cut_on_the_final_hit() {
        let g = grid(200, 0.0, |_| 0.5);
        assert!(song_end_fits(58.0, 61.0, 60.0, None, 2.0));
        assert!(!song_end_fits(58.0, 61.0, 30.0, None, 2.0));
        // 7 s short of a 60 s target fits; 7 s over doesn't.
        assert!(song_end_fits(50.0, 53.0, 60.0, None, 2.0));
        assert!(!song_end_fits(64.0, 67.0, 60.0, None, 2.0));
        let hit = 116; // 58 s
        let input = EndingInput {
            target_secs: 60.0,
            outro: None,
            song_end: Some((hit, 61.0)),
            earliest: 10,
            latest: usize::MAX,
            max_ring_out_secs: 8.0,
            musical: true,
        };
        let e = plan_ending(&g, &input).unwrap();
        assert_eq!(e.kind, EndingKind::SongEnd);
        assert_eq!(e.last_cut, hit);
        assert_eq!(e.ring_out_secs, Some(3.0));
        assert!((e.video_end_secs - 61.0).abs() < 1e-9);
        // With an outro card: the card starts on the hit and holds the tail.
        let e = plan_ending(&g, &EndingInput { outro: Some((2.0, 0.4)), ..input }).unwrap();
        assert_eq!(e.last_cut, hit);
        assert!((e.outro_card_secs.unwrap() - 3.2).abs() < 1e-9, "{e:?}");
        assert!((e.video_end_secs - 61.0).abs() < 1e-9, "{e:?}");
    }

    #[test]
    fn grid_in_output_follows_loop_copies() {
        use crate::music_structure::{Level, Section};
        let beats: Vec<f32> = (0..32).map(|k| k as f32 * 0.5).collect();
        let s = MusicStructure {
            beats_per_bar: 4,
            downbeat_phase: 0,
            downbeat_confidence: 1.0,
            phrase_bars: 4,
            phrases: vec![0, 16],
            major_phrases: vec![0],
            sections: vec![
                Section { start_beat: 0, end_beat: 16, start_secs: 0.0, end_secs: 8.0, loudness_db: -24.0, intensity: 0.2, level: Level::Quiet, rise: 0.0 },
                Section { start_beat: 16, end_beat: 32, start_secs: 8.0, end_secs: 16.0, loudness_db: -16.0, intensity: 0.9, level: Level::High, rise: 0.7 },
            ],
            drop: Some(1),
            final_hit_secs: Some(15.5),
            sound_end_secs: 16.0,
            intensity: (0..32).map(|k| if k < 16 { 0.2 } else { 0.9 }).collect(),
        };
        // Play 0..16 s, then loop 8..16 s from output 16 s.
        let spans = vec![
            SoundtrackSpan { output_start: 0.0, output_end: 16.0, source_start: 0.0 },
            SoundtrackSpan { output_start: 16.0, output_end: 24.0, source_start: 8.0 },
        ];
        let g = grid_in_output(&beats, &s, &spans);
        assert_eq!(g.len(), 48);
        let drops: Vec<f64> = g.iter().filter(|b| b.drop_score.is_some()).map(|b| b.t).collect();
        assert_eq!(drops, vec![8.0, 16.0]);
        assert!(g.iter().filter(|b| b.t >= 16.0).all(|b| b.intensity > 0.8));
        assert_eq!(nearest_beat(&g, 8.2), Some(16));
    }

    fn clip(source: &str, at: f64, dur: f64, chapter: Option<f64>) -> ClipInfo {
        ClipInfo { source_id: source.into(), virtual_start: at, duration: dur, in_start: at, chapter_secs: chapter, fixed: false }
    }

    #[test]
    fn slots_follow_the_footage_and_the_sources() {
        // 0.5 s beats, 4–12 beat clips.
        let clips = vec![
            clip("A", 10.0, 3.0, Some(60.0)),
            clip("A", 15.0, 3.0, Some(60.0)), // 5 s after A's first moment
            clip("B", 1.0, 3.0, Some(3.5)),   // a 3.5 s chapter
            ClipInfo { fixed: true, ..clip("C", 4.0, 2.2, Some(10.0)) },
            clip("img", 0.0, 4.0, None),
        ];
        let loose = slots_for(&clips, 0.5, 4, 12, false);
        let strict = slots_for(&clips, 0.5, 4, 12, true);
        // A's moments are 5 s apart: strictly each may take the gap (10
        // beats); loosely, the gap less the other's shortest clip, twice.
        assert_eq!((strict[0].max_beats, loose[0].max_beats), (10, 12));
        assert_eq!(strict[1].min_beats, 4);
        // B's chapter holds 7 beats; a forced clip and a still keep theirs.
        assert_eq!((loose[2].min_beats, loose[2].max_beats), (4, 7));
        assert_eq!((loose[3].min_beats, loose[3].max_beats), (4, 4));
        assert_eq!((loose[4].min_beats, loose[4].max_beats), (8, 8));
        // Big changes: the first clip, a new source, stills; not the next
        // moment of the same shot.
        let big: Vec<bool> = loose.iter().map(|s| s.big_change).collect();
        assert_eq!(big, vec![true, false, true, true, true]);
        // A moment 20 s on in the same source is a new scene.
        let far = vec![clip("A", 10.0, 3.0, Some(60.0)), clip("A", 33.0, 3.0, Some(60.0))];
        assert!(slots_for(&far, 0.5, 4, 12, false)[1].big_change);
    }

    #[test]
    fn rewindow_keeps_the_moment_centred_inside_its_chapter() {
        let c = clip("A", 10.0, 3.0, Some(60.0));
        assert_eq!(rewindow(&c, 5.0), (9.0, 9.0));
        let end = clip("A", 56.0, 3.0, Some(60.0));
        assert_eq!(rewindow(&end, 6.0), (54.0, 54.0));
        let fixed = ClipInfo { fixed: true, ..c.clone() };
        assert_eq!(rewindow(&fixed, 2.0), (10.0, 10.0));
        // Two moments 4 s apart, both grown to 5 s, share a second of footage.
        let pair = vec![clip("A", 10.0, 3.0, Some(60.0)), clip("A", 14.0, 3.0, Some(60.0))];
        assert!(overlapping(&pair, &[(0.0, 5.0), (5.0, 10.0)]));
        assert!(!overlapping(&pair, &[(0.0, 4.0), (4.0, 8.0)]));
    }

    fn plan_for(g: Vec<PaceBeat>, target: f64) -> MusicPlan {
        let input = EndingInput {
            target_secs: target,
            outro: None,
            song_end: None,
            earliest: 0,
            latest: usize::MAX,
            max_ring_out_secs: 8.0,
            musical: true,
        };
        MusicPlan::new(g, 4, input, Pace { strength: 0.6, base_beats: 6.0 }, 0.5, (2.0, 4.0), false, true, 0.0).unwrap()
    }

    #[test]
    fn a_plan_counts_its_clips_and_the_body_refits_when_footage_is_short() {
        // Phrases every 8 s from 0.25 s; footage from beat 4 (2.25 s).
        let plan = plan_for(grid(200, 0.25, |_| 0.5), 33.0);
        assert_eq!(plan.ending.kind, EndingKind::PhraseEnd);
        assert!((plan.ending.video_end_secs - 32.25).abs() < 1e-9);
        // 60 beats of body at a neutral 6 beats: 10 clips.
        assert_eq!(plan.clips, 10);
        assert!((plan.pick_secs(0.0) - 3.0).abs() < 1e-9);
        let clips: Vec<ClipInfo> = (0..10).map(|i| clip(&format!("S{i}"), 10.0, 3.0, Some(60.0))).collect();
        let body = body_for(&plan, &clips, false).unwrap();
        assert_eq!((body.start, body.end), (4, plan.ending.last_cut));
        // Six clips of at most 8 beats can't fill 60: the ending moves to
        // what they can reach — the phrase start at 24.25 s (beat 48).
        let body = body_for(&plan, &clips[..6], false).unwrap();
        assert_eq!(body.end, 48);
        assert_eq!(body.ending.kind, EndingKind::PhraseEnd);
        assert!(plan_cuts(&plan.grid, body.start, body.end, &body.slots, &[], &plan.pace).is_some());
    }

    #[test]
    fn a_song_played_through_is_never_outlasted_by_the_video() {
        // The song's final hit is beat 60 (30.25 s) and its sound ends at
        // 31 s: the plan ends with it, a ring-out clip after the hit.
        let g = grid(200, 0.25, |_| 0.5);
        let input = EndingInput {
            target_secs: 31.0,
            outro: None,
            song_end: Some((60, 31.0)),
            earliest: 0,
            latest: usize::MAX,
            max_ring_out_secs: 8.0,
            musical: true,
        };
        let plan = MusicPlan::new(g, 4, input, Pace { strength: 0.6, base_beats: 6.0 }, 0.5, (2.0, 4.0), true, true, 0.0).unwrap();
        assert_eq!((plan.ending.kind, plan.ending.last_cut), (EndingKind::SongEnd, 60));
        assert_eq!(plan.ending.ring_out_secs, Some(0.75));
        // Sixteen clips of at least 4 beats can't end by the hit: no body,
        // rather than one that runs on in silence after the song.
        let many: Vec<ClipInfo> = (0..16).map(|i| clip(&format!("S{i}"), 10.0, 3.0, Some(60.0))).collect();
        assert!(body_for(&plan, &many, false).is_none());
        // Seven can't hold a ring-out as well: they all go in the body,
        // which still ends by the hit, never after it.
        let few: Vec<ClipInfo> = (0..7).map(|i| clip(&format!("S{i}"), 10.0, 3.0, Some(60.0))).collect();
        let body = body_for(&plan, &few, false).unwrap();
        assert!(body.end <= 60 && body.ending.ring_out_secs.is_none(), "{body:?}");
    }

    #[test]
    fn without_a_musical_ending_the_cut_takes_the_nearest_beat() {
        let g = grid(200, 0.25, |_| 0.5);
        let input = EndingInput {
            target_secs: 30.1,
            outro: Some((3.0, 0.4)),
            song_end: Some((100, 51.0)),
            earliest: 10,
            latest: usize::MAX,
            max_ring_out_secs: 8.0,
            musical: false,
        };
        let e = plan_ending(&g, &input).unwrap();
        assert_eq!(e.kind, EndingKind::Beat);
        // Cut near 30.1 − 3 + 0.2 = 27.3 → 27.25; the card keeps its length.
        assert!((g[e.last_cut].t - 27.25).abs() < 1e-9, "{e:?}");
        assert_eq!(e.outro_card_secs, None);
        assert!((e.video_end_secs - (27.25 - 0.2 + 3.0)).abs() < 1e-9);
        // A latest beat the footage allows wins over the target.
        let e = plan_ending(&g, &EndingInput { musical: true, outro: None, song_end: None, latest: 40, ..input }).unwrap();
        assert!(e.last_cut <= 40 && g[e.last_cut].downbeat, "{e:?}");
    }

    #[test]
    fn a_fade_is_whole_beats() {
        assert!((fade_on_beats(1.5, 0.4545) - 3.0 * 0.4545).abs() < 1e-9);
        assert!((fade_on_beats(0.1, 0.5) - 0.5).abs() < 1e-9);
        assert_eq!(fade_on_beats(0.0, 0.5), 0.0);
    }

    #[test]
    fn sections_play_where_the_soundtrack_puts_them() {
        use crate::music_structure::{Level, Section};
        let sec = |a: usize, b: usize, level| Section {
            start_beat: a,
            end_beat: b,
            start_secs: a as f32 * 0.5,
            end_secs: b as f32 * 0.5,
            loudness_db: -20.0,
            intensity: 0.5,
            level,
            rise: 0.3,
        };
        let s = MusicStructure {
            beats_per_bar: 4,
            downbeat_phase: 0,
            downbeat_confidence: 0.0,
            phrase_bars: 4,
            phrases: vec![0, 16],
            major_phrases: vec![0],
            sections: vec![sec(0, 16, Level::Quiet), sec(16, 32, Level::High)],
            drop: Some(1),
            final_hit_secs: None,
            sound_end_secs: 16.0,
            intensity: vec![0.5; 32],
        };
        let spans = vec![
            SoundtrackSpan { output_start: 0.0, output_end: 12.0, source_start: 0.0 },
            SoundtrackSpan { output_start: 12.0, output_end: 20.0, source_start: 10.0 },
        ];
        let out = sections_in_output(&s, &spans, 18.0);
        let got: Vec<(f64, f64, bool)> = out.iter().map(|x| (x.start_secs, x.end_secs, x.drop)).collect();
        // First copy: quiet 0–8, high 8–12 (its drop); the loop copy plays
        // the high section on from song 10 s (mid-section, so no fresh
        // drop), cut at 18 s.
        assert_eq!(got, vec![(0.0, 8.0, false), (8.0, 12.0, true), (12.0, 18.0, false)]);
    }
}
