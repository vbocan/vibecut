//! Bars, phrases, sections and the song's ending, read from the fitted beat
//! grid (see `audio_analysis`).
//!
//! Everything here is arithmetic on per-beat measurements
//! ([`BeatFeature`], taken by [`beat_features`] from the analyser's onset
//! and energy envelopes), so it is tested on synthetic curves:
//!
//! - **Downbeats** ([`downbeat_phase`]). 4/4 is assumed. Every beat gets an
//!   accent score — its onset strength, its low-band (kick / bass) energy,
//!   and how much the low band and the overall level step up into it — and
//!   the grid phase (beat 0, 1, 2 or 3) whose beats carry the most accent
//!   is the bar's "1". The confidence says how clearly that phase beats the
//!   runner-up over the whole song (a t-statistic mapped to 0..1). The
//!   song's level steps outrank it unless it is very clear: sections change
//!   on a "1", so when one phase clearly carries the steps ([`step_phase`])
//!   that is the bar's "1". With neither, the first audible beat is: most
//!   songs start on one.
//! - **Phrases** ([`phrase_grid`]). 4-bar groups, the grid's offset chosen
//!   where the level steps up most consistently (new phrases tend to enter
//!   louder, or with a crash on the "1"); every other phrase start is an
//!   8-bar ("major") boundary, again the parity with the most rise.
//! - **Sections** ([`find_sections`]). Bars where the level over the next
//!   two bars differs from the previous two by a clear step (novelty
//!   peaks), snapped to a phrase start within a bar. Each section is rated
//!   quiet / mid / high against the song's own dynamic range, so a song
//!   that never changes level is one mid section.
//! - **The drop** ([`pick_drop`]). The high section with the most level,
//!   the biggest rise into it and the most length: the downbeat that opens
//!   the song's biggest section.
//! - **The final hit** ([`find_final_hit`]). The last strong onset before
//!   the song's sound ends (its tail decays below −40 dB of the music),
//!   snapped to the beat grid when it sits on it.

use serde::{Deserialize, Serialize};

/// The analysis assumes 4/4.
pub const BEATS_PER_BAR: usize = 4;
/// Bars per phrase; every other phrase start is an 8-bar boundary.
pub const PHRASE_BARS: usize = 4;

/// Smallest dynamic range (dB) the intensity scale is stretched over: a
/// song whose loudest and quietest bars differ by less than this never
/// reaches "quiet" or "high".
const MIN_RANGE_DB: f32 = 6.0;
/// Smallest level step (dB, 2-bar windows) that starts a new section.
const MIN_SECTION_STEP_DB: f32 = 2.5;
/// Neighbouring sections closer than this in level (dB) are one section.
const SECTION_MERGE_DB: f32 = 1.5;
/// Intensity thresholds for the section levels.
const HIGH_INTENSITY: f32 = 0.62;
const QUIET_INTENSITY: f32 = 0.38;
/// Least intensity a section must gain over the one before it to be a drop.
const MIN_DROP_RISE: f32 = 0.15;
/// Drop scores ([`MusicStructure::drop_score`]) this close are equally big
/// (a song whose choruses all hit as hard): the earliest is the drop.
pub const DROP_TIE: f32 = 0.05;
/// A phrase offset (or 8-bar parity) other than the first bar's must carry
/// this much more rise to be chosen.
const GRID_PREFERENCE: f32 = 1.25;
/// Below this t-statistic the downbeat evidence is not significant and the
/// first audible beat is taken as a "1".
const DOWNBEAT_MIN_T: f32 = 3.0;
/// Accent evidence this clear (t ≈ 6) outranks the level steps.
const CLEAR_ACCENT_CONFIDENCE: f32 = 0.86;
/// Sound end: the last frame within this many dB of the music's level.
const SOUND_FLOOR_DB: f32 = -40.0;
/// Final hit: a hit this far below the music's level is part of the tail.
const HIT_FLOOR_DB: f32 = -20.0;
/// Final hit: least onset strength, relative to a typical beat's.
const HIT_MIN_ACCENT: f32 = 0.35;

/// Per-beat measurements the structure analysis works from.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BeatFeature {
    /// Onset strength at the beat (the analyser's normalized envelope).
    pub accent: f32,
    /// Low-band (< ~150 Hz) power over the beat's attack: kick, bass note.
    pub low: f32,
    /// Mean power over the beat, up to the next one.
    pub energy: f32,
    /// Mean onset envelope over the beat: how busy it is.
    pub activity: f32,
}

/// How loud a section is against the rest of the song.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export, rename_all = "snake_case"))]
pub enum Level {
    Quiet,
    Mid,
    High,
}

/// A stretch of the song at one level: whole bars, in song time.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Section {
    /// First beat (a downbeat) and the beat after the last.
    pub start_beat: usize,
    pub end_beat: usize,
    pub start_secs: f32,
    pub end_secs: f32,
    /// Mean level of its bars (dB, relative to full scale).
    pub loudness_db: f32,
    /// 0..1 against the song's own dynamic range.
    pub intensity: f32,
    pub level: Level,
    /// Intensity gained over the section before it (0 for the first).
    pub rise: f32,
}

/// Bars, phrases, sections, the drop and the final hit of a song. Beat
/// indices refer to `AnalyzedAudio::beats`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MusicStructure {
    pub beats_per_bar: u32,
    /// Beat index (0..4) of the first downbeat on the grid.
    pub downbeat_phase: usize,
    /// 0..1: how clearly the "1" stands out (0 = taken from where the
    /// level steps, or from the first audible beat).
    pub downbeat_confidence: f32,
    pub phrase_bars: u32,
    /// Beat indices of the phrase starts (each a downbeat), sorted.
    pub phrases: Vec<usize>,
    /// The phrase starts that are also 8-bar boundaries.
    pub major_phrases: Vec<usize>,
    pub sections: Vec<Section>,
    /// Index into `sections` of the drop, when the song has one.
    pub drop: Option<usize>,
    /// The last strong onset before the tail, in seconds.
    pub final_hit_secs: Option<f32>,
    /// Where the song's sound ends (its tail decays below −40 dB).
    pub sound_end_secs: f32,
    /// Per beat: 0..1 how loud and busy the music is around it, against
    /// the song's own range (0.5 when it never changes).
    pub intensity: Vec<f32>,
}

impl MusicStructure {
    pub fn is_downbeat(&self, beat: usize) -> bool {
        beat >= self.downbeat_phase && (beat - self.downbeat_phase) % self.beats_per_bar.max(1) as usize == 0
    }

    pub fn is_phrase_start(&self, beat: usize) -> bool {
        self.phrases.binary_search(&beat).is_ok()
    }

    /// Downbeat beat indices below `n_beats`.
    pub fn downbeats(&self, n_beats: usize) -> Vec<usize> {
        (self.downbeat_phase..n_beats).step_by(self.beats_per_bar.max(1) as usize).collect()
    }

    /// How big a moment the section's first downbeat is, for placing the
    /// best clip there: level, rise into it and length. `None` for a
    /// section that is not high or doesn't rise out of the one before.
    pub fn drop_score(&self, section: usize) -> Option<f32> {
        let s = self.sections.get(section)?;
        if s.level != Level::High || section == 0 || s.rise < MIN_DROP_RISE {
            return None;
        }
        let bars = (s.end_beat - s.start_beat) as f32 / self.beats_per_bar.max(1) as f32;
        Some(s.intensity + 0.6 * s.rise + 0.05 * bars.min(16.0) / 4.0)
    }
}

/// Analyse a song's structure from its beat grid. `beats` and `feats` are
/// parallel; `onset` / `frame_energy` are the analyser's envelopes at
/// `frame_rate` Hz (frame `i` centred near `(i + 1) / frame_rate`).
/// `None` when the song has too few beats for bars to mean anything.
pub fn analyze(
    beats: &[f32],
    feats: &[BeatFeature],
    onset: &[f32],
    frame_energy: &[f32],
    frame_rate: f32,
) -> Option<MusicStructure> {
    let n = beats.len();
    if n < 4 * BEATS_PER_BAR || feats.len() != n || frame_rate <= 0.0 {
        return None;
    }
    let interval = median_interval(beats)?;
    let hit = find_final_hit(onset, frame_energy, frame_rate, beats, interval);
    let n_music = beats.partition_point(|&b| b < hit.sound_end_secs).clamp(BEATS_PER_BAR, n);
    // The first audible beat may sit one beat before the grid's first
    // (the grid starts at t ≥ 0, a song's first hit is often right at 0).
    let first_k = first_audible_beat(frame_energy, frame_rate, beats, interval);
    let first = first_k.max(0) as usize;
    let prior = first_k.rem_euclid(BEATS_PER_BAR as i64) as usize;
    let (phase, confidence) = pick_phase(downbeat_phase(&feats[..n_music], prior), step_phase(&feats[..n_music]));

    // Whole bars from the first downbeat to the sound end.
    let n_bars = n_music.saturating_sub(phase) / BEATS_PER_BAR;
    if n_bars < 2 {
        return None;
    }
    let bar_beats = |b: usize| phase + b * BEATS_PER_BAR..phase + (b + 1) * BEATS_PER_BAR;
    let bar_db: Vec<f32> = (0..n_bars)
        .map(|b| db(mean(feats[bar_beats(b)].iter().map(|f| f.energy))))
        .collect();
    let bar_accent: Vec<f32> = (0..n_bars).map(|b| feats[phase + b * BEATS_PER_BAR].accent).collect();
    // The level scale ignores a silent lead-in before the first hit.
    let first_bar = first.saturating_sub(phase) / BEATS_PER_BAR;
    let scale = LevelScale::from_values(&bar_db[first_bar.min(n_bars - 1)..]);

    // Without evidence, phrases count from the bar the music starts in
    // (bar −1 when it starts before the grid's first downbeat).
    let music_bar = (first_k - phase as i64).div_euclid(BEATS_PER_BAR as i64);
    let rise = rise_novelty(&bar_db, &bar_accent);
    let (offset, major_parity) = phrase_grid(&rise, music_bar);
    let mut phrase_bars: Vec<usize> = (offset..n_bars).step_by(PHRASE_BARS).collect();
    if music_bar >= 0 && (music_bar as usize) < offset {
        phrase_bars.insert(0, music_bar as usize); // a short lead-in phrase
    }
    let section_bars = find_sections(&bar_db, &phrase_bars);

    let to_beat = |bar: usize| phase + bar * BEATS_PER_BAR;
    let mut sections: Vec<Section> = Vec::with_capacity(section_bars.len());
    for &(b0, b1) in &section_bars {
        let loudness = db(mean((b0..b1).flat_map(|b| feats[bar_beats(b)].iter().map(|f| f.energy))));
        let intensity = scale.rel(loudness);
        let rise = sections.last().map_or(0.0, |p: &Section| intensity - p.intensity);
        let (start_beat, end_beat) = (to_beat(b0), to_beat(b1));
        sections.push(Section {
            start_beat,
            end_beat,
            start_secs: beats[start_beat],
            end_secs: beats.get(end_beat).copied().unwrap_or(hit.sound_end_secs),
            loudness_db: loudness,
            intensity,
            level: level_of(intensity),
            rise,
        });
    }

    let phrases: Vec<usize> = phrase_bars.iter().map(|&b| to_beat(b)).collect();
    let major_phrases: Vec<usize> = phrase_bars
        .iter()
        .filter(|&&b| b >= offset && ((b - offset) / PHRASE_BARS) % 2 == major_parity)
        .map(|&b| to_beat(b))
        .collect();

    let mut structure = MusicStructure {
        beats_per_bar: BEATS_PER_BAR as u32,
        downbeat_phase: phase,
        downbeat_confidence: confidence,
        phrase_bars: PHRASE_BARS as u32,
        phrases,
        major_phrases,
        sections,
        drop: None,
        final_hit_secs: hit.hit_secs,
        sound_end_secs: hit.sound_end_secs,
        intensity: beat_intensity(feats, n_music, first),
    };
    structure.drop = pick_drop(&structure);
    Some(structure)
}

/// Per-beat features from the analyser's envelopes (see [`analyze`]).
/// `low` is the low-band power per frame ([`low_band_frame_energy`]).
pub fn beat_features(
    beats: &[f32],
    onset: &[f32],
    frame_energy: &[f32],
    low: &[f32],
    frame_rate: f32,
) -> Vec<BeatFeature> {
    let interval = median_interval(beats).unwrap_or(0.5);
    let n = frame_energy.len().min(onset.len()).min(low.len());
    // Frame whose centre sits at time t.
    let frame_at = |t: f32| t * frame_rate - 1.0;
    let range = |a: f32, b: f32| {
        let lo = a.round().max(0.0) as usize;
        let hi = (b.round().max(0.0) as usize).min(n);
        lo.min(hi)..hi.max(lo.min(hi) + 1).min(n)
    };
    beats
        .iter()
        .enumerate()
        .map(|(k, &t)| {
            let next = beats.get(k + 1).copied().unwrap_or(t + interval);
            let c = frame_at(t);
            let attack = range(c - 1.5, c + 2.5);
            let accent = onset.get(attack.clone()).map_or(0.0, |w| w.iter().cloned().fold(0.0, f32::max));
            let low_win = range(c - 0.5, c + 0.25 * interval * frame_rate + 0.5);
            let beat_win = range(c, frame_at(next));
            BeatFeature {
                accent,
                low: low.get(low_win).map_or(0.0, |w| mean(w.iter().cloned())),
                energy: frame_energy.get(beat_win.clone()).map_or(0.0, |w| mean(w.iter().cloned())),
                activity: onset.get(beat_win).map_or(0.0, |w| mean(w.iter().cloned())),
            }
        })
        .collect()
}

/// Mean low-band power per analysis frame (`frame` samples every `hop`):
/// the signal through two one-pole low-passes at `cutoff_hz` (−12 dB/oct),
/// enough to keep a kick or a bass note and lose hats and vocals.
pub fn low_band_frame_energy(pcm: &[f32], sample_rate: u32, frame: usize, hop: usize, cutoff_hz: f32) -> Vec<f32> {
    if pcm.len() < frame || hop == 0 || sample_rate == 0 {
        return Vec::new();
    }
    let a = 1.0 - (-2.0 * std::f64::consts::PI * cutoff_hz as f64 / sample_rate as f64).exp();
    let (mut y1, mut y2) = (0.0_f64, 0.0_f64);
    let mut prefix = Vec::with_capacity(pcm.len() + 1);
    prefix.push(0.0_f64);
    let mut acc = 0.0_f64;
    for &x in pcm {
        y1 += a * (x as f64 - y1);
        y2 += a * (y1 - y2);
        acc += y2 * y2;
        prefix.push(acc);
    }
    let n_frames = (pcm.len() - frame) / hop + 1;
    (0..n_frames)
        .map(|i| {
            let s = i * hop;
            ((prefix[s + frame] - prefix[s]) / frame as f64) as f32
        })
        .collect()
}

/// The grid phase (0..4) of the bar's "1" and the confidence (0..1) that
/// it stands out. `prior` is the phase taken when the evidence isn't
/// significant (the first audible beat's).
pub fn downbeat_phase(feats: &[BeatFeature], prior: usize) -> (usize, f32) {
    let n = feats.len();
    let prior = prior % BEATS_PER_BAR;
    if n < 2 * BEATS_PER_BAR {
        return (prior, 0.0);
    }
    // Levels floored 40 dB under the typical beat, so a silent beat (a
    // break, the grid running past the audio) doesn't swamp the z-scores.
    let floored = |v: Vec<f32>| {
        let floor = median(&v) - 40.0;
        v.into_iter().map(|x| x.max(floor)).collect::<Vec<f32>>()
    };
    let low_db = floored(feats.iter().map(|f| db(f.low)).collect());
    let e_db = floored(feats.iter().map(|f| db(f.energy)).collect());
    let step_up = |v: &[f32]| -> Vec<f32> {
        (0..v.len()).map(|k| if k == 0 { 0.0 } else { (v[k] - v[k - 1]).max(0.0) }).collect()
    };
    let accent: Vec<f32> = feats.iter().map(|f| f.accent).collect();
    let signals = [
        (zscores(&accent), 1.0),
        (zscores(&low_db), 1.0),
        (zscores(&step_up(&low_db)), 1.0),
        (zscores(&step_up(&e_db)), 0.5),
    ];
    let score: Vec<f32> = (0..n).map(|k| signals.iter().map(|(z, w)| z[k] * w).sum()).collect();

    let mut sums = [0.0_f64; BEATS_PER_BAR];
    let mut counts = [0usize; BEATS_PER_BAR];
    for (k, &s) in score.iter().enumerate() {
        sums[k % BEATS_PER_BAR] += s as f64;
        counts[k % BEATS_PER_BAR] += 1;
    }
    let means: Vec<f64> = (0..BEATS_PER_BAR).map(|p| sums[p] / counts[p].max(1) as f64).collect();
    // Pooled spread of the scores around their phase's mean.
    let ss: f64 = score
        .iter()
        .enumerate()
        .map(|(k, &s)| (s as f64 - means[k % BEATS_PER_BAR]).powi(2))
        .sum();
    let sigma = (ss / (n.saturating_sub(BEATS_PER_BAR)).max(1) as f64).sqrt().max(1e-6);
    let t_between = |a: usize, b: usize| {
        let se = sigma * (1.0 / counts[a].max(1) as f64 + 1.0 / counts[b].max(1) as f64).sqrt();
        ((means[a] - means[b]) / se) as f32
    };
    let best = (0..BEATS_PER_BAR)
        .max_by(|&a, &b| means[a].total_cmp(&means[b]).then(b.cmp(&a)))
        .unwrap_or(prior);
    let chosen = if best != prior && t_between(best, prior) < DOWNBEAT_MIN_T { prior } else { best };
    let runner_up = (0..BEATS_PER_BAR)
        .filter(|&p| p != chosen)
        .max_by(|&a, &b| means[a].total_cmp(&means[b]))
        .unwrap_or(chosen);
    let t = t_between(chosen, runner_up).max(0.0);
    (chosen, 1.0 - (-t / 3.0).exp())
}

/// The grid phase (0..4) where the song's level steps happen, when one
/// phase clearly carries them, and how clearly (0.5..1): beats where the
/// level over the next two bars departs from the previous two by at least
/// `MIN_SECTION_STEP_DB` (local peaks), summed per phase; the best phase
/// must carry at least twice the next one. Sections change on a bar's
/// "1", so this places the bars of a song whose beats all hit alike.
pub fn step_phase(feats: &[BeatFeature]) -> Option<(usize, f32)> {
    let w = 2 * BEATS_PER_BAR;
    let n = feats.len();
    if n < 2 * w + 1 {
        return None;
    }
    let raw: Vec<f32> = feats.iter().map(|f| db(f.energy)).collect();
    let floor = median(&raw) - 40.0;
    let level: Vec<f32> = raw.into_iter().map(|x| x.max(floor)).collect();
    let novelty: Vec<f32> = (0..n)
        .map(|k| {
            if k < w || k + w > n {
                return 0.0;
            }
            (mean(level[k..k + w].iter().cloned()) - mean(level[k - w..k].iter().cloned())).abs()
        })
        .collect();
    let mut mass = [0.0_f32; BEATS_PER_BAR];
    for k in w..=n - w {
        let v = novelty[k];
        if v >= MIN_SECTION_STEP_DB && v >= novelty[k - 1] && v > novelty.get(k + 1).copied().unwrap_or(0.0) {
            mass[k % BEATS_PER_BAR] += v;
        }
    }
    let best = (0..BEATS_PER_BAR).max_by(|&a, &b| mass[a].total_cmp(&mass[b]).then(b.cmp(&a)))?;
    let runner_up = (0..BEATS_PER_BAR).filter(|&p| p != best).map(|p| mass[p]).fold(0.0, f32::max);
    (mass[best] > 0.0 && mass[best] >= 2.0 * runner_up).then(|| (best, 1.0 - runner_up / mass[best]))
}

/// The bar's "1" from the accents ([`downbeat_phase`]) and the level steps
/// ([`step_phase`]), each `(phase, confidence)`: the steps win unless the
/// accents are very clear about another phase.
pub fn pick_phase(accents: (usize, f32), steps: Option<(usize, f32)>) -> (usize, f32) {
    match steps {
        Some((p, c)) if p == accents.0 => (p, accents.1.max(c)),
        Some(s) if accents.1 < CLEAR_ACCENT_CONFIDENCE => s,
        _ => accents,
    }
}

/// Per bar: how much the level steps up into it (mean of the next two
/// bars over the previous two, dB, rises only) plus its downbeat's accent
/// above the song's typical one (a crash on the "1"), up to 3 dB.
pub fn rise_novelty(bar_db: &[f32], bar_accent: &[f32]) -> Vec<f32> {
    let n = bar_db.len();
    let typical = median(bar_accent).max(1e-6);
    (0..n)
        .map(|b| {
            if b == 0 {
                return 0.0;
            }
            let before = mean(bar_db[b.saturating_sub(2)..b].iter().cloned());
            let after = mean(bar_db[b..(b + 2).min(n)].iter().cloned());
            let accent = bar_accent.get(b).map_or(0.0, |&a| (3.0 * (a / typical - 1.0)).clamp(0.0, 3.0));
            (after - before).max(0.0) + accent
        })
        .collect()
}

/// The phrase grid: the bar offset (0..4) of the first full phrase and the
/// parity (0 or 1) of the phrases that start 8-bar groups, both chosen
/// where `rise` (see [`rise_novelty`]) is highest. Phrases count from
/// `music_bar`, the bar the music starts in (−1 when it starts before the
/// first downbeat), unless another offset or parity is clearly stronger.
pub fn phrase_grid(rise: &[f32], music_bar: i64) -> (usize, usize) {
    let total = |pred: &dyn Fn(usize) -> bool| -> f32 {
        rise.iter().enumerate().filter(|&(b, _)| pred(b)).map(|(_, &r)| r).sum()
    };
    // Bar 0 has no "before" (its rise is 0), so offset 0 is judged by the
    // phrase starts after it.
    let offset_score = |o: usize| total(&|b| b >= o.max(1) && (b - o) % PHRASE_BARS == 0);
    let prior = music_bar.rem_euclid(PHRASE_BARS as i64) as usize;
    let base = offset_score(prior);
    let mut offset = prior;
    let mut best = base;
    for o in (0..PHRASE_BARS).filter(|&o| o != prior) {
        let s = offset_score(o);
        if s > base * GRID_PREFERENCE + 0.5 && s > best {
            best = s;
            offset = o;
        }
    }
    let parity_score = |q: usize| {
        total(&|b| b >= offset && (b - offset) % PHRASE_BARS == 0 && ((b - offset) / PHRASE_BARS) % 2 == q)
    };
    // The phrase the music starts in opens an 8-bar group by default.
    let prior_parity = (music_bar - offset as i64).div_euclid(PHRASE_BARS as i64).rem_euclid(2) as usize;
    let (pri, alt) = (parity_score(prior_parity), parity_score(1 - prior_parity));
    let parity = if alt > pri * GRID_PREFERENCE + 0.5 { 1 - prior_parity } else { prior_parity };
    (offset, parity)
}

/// Section spans `(first bar, bar after the last)`: boundaries where the
/// level over the next two bars steps away from the previous two by a
/// clear margin, snapped to a phrase start within a bar, at least two
/// bars apart; neighbours that end up at the same level are merged.
pub fn find_sections(bar_db: &[f32], phrase_bars: &[usize]) -> Vec<(usize, usize)> {
    let n = bar_db.len();
    if n == 0 {
        return Vec::new();
    }
    let novelty: Vec<f32> = (0..n)
        .map(|b| {
            if b == 0 {
                return 0.0;
            }
            let before = mean(bar_db[b.saturating_sub(2)..b].iter().cloned());
            let after = mean(bar_db[b..(b + 2).min(n)].iter().cloned());
            (after - before).abs()
        })
        .collect();
    let scale = LevelScale::from_values(bar_db);
    let threshold = MIN_SECTION_STEP_DB.max(0.3 * scale.range);
    // Novelty peaks, strongest first, then snapped and spaced.
    let mut peaks: Vec<usize> = (1..n)
        .filter(|&b| {
            let v = novelty[b];
            v >= threshold && v >= novelty[b - 1] && (b + 1 >= n || v > novelty[b + 1])
        })
        .collect();
    peaks.sort_by(|&a, &b| novelty[b].total_cmp(&novelty[a]).then(a.cmp(&b)));
    let mut bounds: Vec<usize> = Vec::new();
    for b in peaks {
        let snapped = phrase_bars
            .iter()
            .copied()
            .filter(|&p| p > 0 && p.abs_diff(b) <= 1)
            .min_by_key(|&p| p.abs_diff(b))
            .unwrap_or(b);
        if bounds.iter().all(|&k| k.abs_diff(snapped) >= 2) && snapped < n {
            bounds.push(snapped);
        }
    }
    bounds.sort_unstable();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut start = 0;
    for b in bounds.into_iter().chain(std::iter::once(n)) {
        if b > start {
            spans.push((start, b));
            start = b;
        }
    }
    // Merge neighbours at (nearly) the same level.
    let level = |(a, b): (usize, usize)| mean(bar_db[a..b].iter().cloned());
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
    for s in spans {
        match merged.last_mut() {
            Some(prev) if (level(*prev) - level(s)).abs() < SECTION_MERGE_DB => prev.1 = s.1,
            _ => merged.push(s),
        }
    }
    merged
}

/// The drop: the section with the best [`MusicStructure::drop_score`];
/// of sections within [`DROP_TIE`] of it, the earliest.
pub fn pick_drop(s: &MusicStructure) -> Option<usize> {
    let scores: Vec<(usize, f32)> = (0..s.sections.len()).filter_map(|i| s.drop_score(i).map(|x| (i, x))).collect();
    let best = scores.iter().map(|x| x.1).fold(f32::NEG_INFINITY, f32::max);
    scores.into_iter().find(|&(_, x)| x >= best - DROP_TIE).map(|(i, _)| i)
}

/// Where a song's sound ends and its last strong hit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FinalHit {
    pub hit_secs: Option<f32>,
    pub sound_end_secs: f32,
}

/// The song's last strong onset before its tail, and where its sound ends
/// (the last frame within 40 dB of the music's level). A hit counts when
/// its onset is at least a third of a typical beat's and the music is
/// still within 20 dB of its level there. Snapped to a grid beat within
/// 30 % of a beat (the onset envelope is frame-quantized).
pub fn find_final_hit(onset: &[f32], frame_energy: &[f32], frame_rate: f32, beats: &[f32], interval: f32) -> FinalHit {
    let n = frame_energy.len().min(onset.len());
    if n == 0 || frame_rate <= 0.0 {
        return FinalHit { hit_secs: None, sound_end_secs: 0.0 };
    }
    let level = percentile(&frame_energy[..n], 0.9);
    let floor = level * 10f32.powf(SOUND_FLOOR_DB / 10.0);
    let Some(last) = (0..n).rev().find(|&i| frame_energy[i] >= floor && frame_energy[i] > 0.0) else {
        return FinalHit { hit_secs: None, sound_end_secs: 0.0 };
    };
    // A frame spans two hops; it ends one hop after its centre.
    let sound_end = ((last + 2) as f32 / frame_rate).min((n + 1) as f32 / frame_rate);
    let typical = {
        let at_beats: Vec<f32> = beats
            .iter()
            .filter(|&&b| b < sound_end)
            .map(|&b| {
                let c = b * frame_rate - 1.0;
                let lo = (c - 1.5).round().max(0.0) as usize;
                let hi = ((c + 2.5).round().max(0.0) as usize).min(n);
                onset[lo.min(hi)..hi].iter().cloned().fold(0.0, f32::max)
            })
            .collect();
        median(&at_beats)
    };
    let hit_floor = level * 10f32.powf(HIT_FLOOR_DB / 10.0);
    let strong = (HIT_MIN_ACCENT * typical).max(0.02);
    let hit_frame = (1..=last.min(n - 1)).rev().find(|&i| {
        let v = onset[i];
        let peak = v >= onset[i - 1] && (i + 1 >= n || v >= onset[i + 1]);
        let loud = frame_energy[i..(i + 3).min(n)].iter().cloned().fold(0.0, f32::max) >= hit_floor;
        peak && v >= strong && loud
    });
    let hit_secs = hit_frame.map(|i| {
        let raw = (i as f32 + 1.0) / frame_rate;
        beats
            .iter()
            .copied()
            .filter(|b| (b - raw).abs() <= 0.3 * interval)
            .min_by(|a, b| (a - raw).abs().total_cmp(&(b - raw).abs()))
            .unwrap_or(raw)
    });
    FinalHit { hit_secs, sound_end_secs: sound_end }
}

/// Per-beat intensity (0..1): the level over the surrounding bar, plus
/// half a dB-equivalent per step of onset density (twice as busy ≈ +3 dB),
/// against the song's own range between its first audible beat and its
/// sound end (`n_music`).
fn beat_intensity(feats: &[BeatFeature], n_music: usize, first: usize) -> Vec<f32> {
    let n = feats.len();
    let half = BEATS_PER_BAR / 2;
    let window = |k: usize| k.saturating_sub(half)..(k + half).min(n).max(k + 1);
    let typical_activity = median(&feats[first.min(n_music - 1)..n_music].iter().map(|f| f.activity).collect::<Vec<_>>()).max(1e-6);
    let x: Vec<f32> = (0..n)
        .map(|k| {
            let w = window(k);
            let level = db(mean(feats[w.clone()].iter().map(|f| f.energy)));
            let busy = mean(feats[w].iter().map(|f| f.activity)).max(1e-9);
            level + 0.5 * (3.0 * (busy / typical_activity).log2()).clamp(-6.0, 6.0)
        })
        .collect();
    let scale = LevelScale::from_values(&x[first.min(n_music - 1)..n_music]);
    x.iter().map(|&v| scale.rel(v)).collect()
}

fn level_of(intensity: f32) -> Level {
    if intensity >= HIGH_INTENSITY {
        Level::High
    } else if intensity <= QUIET_INTENSITY {
        Level::Quiet
    } else {
        Level::Mid
    }
}

/// Maps a level (dB) onto 0..1 against a song's own range: its 10th to
/// 90th percentile, stretched to at least `MIN_RANGE_DB` so a song that
/// barely changes stays near the middle.
#[derive(Debug, Clone, Copy)]
struct LevelScale {
    mid: f32,
    range: f32,
}

impl LevelScale {
    fn from_values(v: &[f32]) -> Self {
        if v.is_empty() {
            return Self { mid: 0.0, range: MIN_RANGE_DB };
        }
        let lo = percentile(v, 0.1);
        let hi = percentile(v, 0.9);
        Self { mid: 0.5 * (lo + hi), range: hi - lo }
    }

    fn rel(&self, x: f32) -> f32 {
        (0.5 + (x - self.mid) / self.range.max(MIN_RANGE_DB)).clamp(0.0, 1.0)
    }
}

/// Index of the beat where the music starts — the first frame within
/// 20 dB of the music's level, counted in beats from `beats[0]`. −1 when
/// the music starts a beat before the grid's first beat (the grid only
/// holds beats at t ≥ 0; a song's first hit is often right at 0).
pub fn first_audible_beat(frame_energy: &[f32], frame_rate: f32, beats: &[f32], interval: f32) -> i64 {
    let (Some(&b0), true) = (beats.first(), interval > 0.0 && frame_rate > 0.0) else {
        return 0;
    };
    let level = percentile(frame_energy, 0.9);
    let floor = level * 10f32.powf(HIT_FLOOR_DB / 10.0);
    let Some(i) = frame_energy.iter().position(|&e| e >= floor && e > 0.0) else {
        return 0;
    };
    // A frame starts at i / rate; a hit inside it is about half a hop in.
    let t = (i as f32 + 0.5) / frame_rate;
    (((t - b0) / interval).round() as i64).max(-1)
}

fn median_interval(beats: &[f32]) -> Option<f32> {
    let diffs: Vec<f32> = beats.windows(2).map(|w| w[1] - w[0]).filter(|d| *d > 0.0).collect();
    (!diffs.is_empty()).then(|| median(&diffs))
}

fn db(power: f32) -> f32 {
    10.0 * (power.max(1e-12)).log10()
}

fn mean(it: impl Iterator<Item = f32>) -> f32 {
    let (s, n) = it.fold((0.0_f64, 0usize), |(s, n), x| (s + x as f64, n + 1));
    if n == 0 { 0.0 } else { (s / n as f64) as f32 }
}

fn median(v: &[f32]) -> f32 {
    percentile(v, 0.5)
}

fn percentile(v: &[f32], q: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(f32::total_cmp);
    let i = ((s.len() - 1) as f32 * q.clamp(0.0, 1.0)).round() as usize;
    s[i]
}

fn zscores(v: &[f32]) -> Vec<f32> {
    let m = mean(v.iter().cloned());
    let sd = (mean(v.iter().map(|x| (x - m) * (x - m)))).sqrt();
    if sd < 1e-6 {
        return vec![0.0; v.len()];
    }
    v.iter().map(|x| (x - m) / sd).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` beats with a repeating per-phase accent pattern starting at
    /// grid phase `phase` (the beat with pattern[0] is a downbeat).
    fn feats_with_pattern(n: usize, phase: usize, accent: [f32; 4], low: [f32; 4]) -> Vec<BeatFeature> {
        (0..n)
            .map(|k| {
                let p = (k + BEATS_PER_BAR - phase) % BEATS_PER_BAR;
                BeatFeature { accent: accent[p], low: low[p], energy: 0.01, activity: 0.1 }
            })
            .collect()
    }

    #[test]
    fn downbeat_phase_finds_the_accented_beat() {
        // Kick-heavy "1", softer 2 and 4, a snare-ish 3: phase 1.
        let f = feats_with_pattern(64, 1, [1.0, 0.6, 0.8, 0.6], [0.02, 0.008, 0.012, 0.008]);
        let (phase, conf) = downbeat_phase(&f, 0);
        assert_eq!(phase, 1);
        assert!(conf > 0.9, "confidence {conf}");
    }

    #[test]
    fn downbeat_phase_reads_a_bass_note_change_on_the_one() {
        // Every beat has the same onset; the bass steps up on the "1" only.
        let f = feats_with_pattern(80, 3, [0.7; 4], [0.03, 0.01, 0.01, 0.01]);
        assert_eq!(downbeat_phase(&f, 0).0, 3);
    }

    #[test]
    fn flat_beats_fall_back_to_the_prior_with_no_confidence() {
        let f = feats_with_pattern(64, 0, [0.5; 4], [0.01; 4]);
        let (phase, conf) = downbeat_phase(&f, 2);
        assert_eq!(phase, 2);
        assert!(conf < 0.05, "confidence {conf}");
    }

    #[test]
    fn weak_evidence_keeps_the_prior_but_strong_evidence_overrides_it() {
        // A 2 % accent on phase 1, with noise: not significant over 16 beats.
        let mut f = feats_with_pattern(16, 1, [0.51, 0.5, 0.5, 0.5], [0.01; 4]);
        for (k, x) in f.iter_mut().enumerate() {
            x.accent += 0.03 * ((k * 7919) % 13) as f32 / 13.0;
        }
        assert_eq!(downbeat_phase(&f, 0).0, 0);
        let strong = feats_with_pattern(64, 1, [1.0, 0.5, 0.5, 0.5], [0.01; 4]);
        assert_eq!(downbeat_phase(&strong, 0).0, 1);
    }

    #[test]
    fn level_steps_place_the_one_when_every_beat_hits_alike() {
        // Same onset and bass on every beat; the level changes every four
        // bars, on beats 14, 30, 46, … (≡ 2 mod 4).
        let feats: Vec<BeatFeature> = (0..96)
            .map(|k| {
                let level = if ((k + 2) / 16) % 2 == 0 { -26.0_f32 } else { -20.0 };
                BeatFeature { accent: 0.6, low: 0.01, energy: 10f32.powf(level / 10.0), activity: 0.1 }
            })
            .collect();
        assert_eq!(step_phase(&feats), Some((2, 1.0)));
        // A steady level, or steps that land on two phases alike: no say.
        let flat: Vec<BeatFeature> = feats.iter().map(|f| BeatFeature { energy: 0.01, ..*f }).collect();
        assert_eq!(step_phase(&flat), None);
        let mixed: Vec<BeatFeature> = (0..96)
            .map(|k| {
                let level = if k < 30 { -26.0_f32 } else if k < 61 { -20.0 } else { -26.0 };
                BeatFeature { accent: 0.6, low: 0.01, energy: 10f32.powf(level / 10.0), activity: 0.1 }
            })
            .collect();
        assert_eq!(step_phase(&mixed), None, "steps at 30 (≡ 2) and 61 (≡ 1)");
        // The steps outrank accents that aren't very clear about the "1".
        assert_eq!(pick_phase((0, 0.48), Some((3, 0.93))), (3, 0.93));
        assert_eq!(pick_phase((3, 0.3), Some((3, 0.9))), (3, 0.9));
        assert_eq!(pick_phase((0, 0.95), Some((3, 0.93))), (0, 0.95));
        assert_eq!(pick_phase((1, 0.2), None), (1, 0.2));
    }

    #[test]
    fn phrase_grid_follows_where_the_level_steps_up() {
        // 24 bars of a 4-bar cycle that steps up at bars 2, 6, 10, …
        // (a two-bar lead-in before the first full phrase).
        let bar_db: Vec<f32> = (0..24).map(|b| if (b + 2) % 4 < 2 { -14.0 } else { -20.0 }).collect();
        let accent = vec![0.5; 24];
        let rise = rise_novelty(&bar_db, &accent);
        let (offset, _) = phrase_grid(&rise, 0);
        assert_eq!(offset, 2, "rise {rise:?}");
    }

    #[test]
    fn phrase_grid_prefers_the_first_bar_without_evidence() {
        let bar_db = vec![-18.0; 32];
        let rise = rise_novelty(&bar_db, &vec![0.5; 32]);
        assert_eq!(phrase_grid(&rise, 0), (0, 0));
        // Music that starts a bar before the first downbeat: phrases (and
        // 8-bar groups) count from there.
        assert_eq!(phrase_grid(&rise, -1), (3, 1));
    }

    #[test]
    fn crashes_every_eight_bars_mark_the_major_phrases() {
        // Flat level; a crash (accent ×2) on bars 4, 12, 20, 28.
        let bar_db = vec![-18.0; 32];
        let accent: Vec<f32> = (0..32).map(|b| if b % 8 == 4 { 1.0 } else { 0.5 }).collect();
        let rise = rise_novelty(&bar_db, &accent);
        let (offset, parity) = phrase_grid(&rise, 0);
        assert_eq!(offset, 0);
        assert_eq!(parity, 1, "8-bar groups start on bar 4");
    }

    /// Bars of a song: 8 quiet intro, 8 verse, 8 chorus, 4 breakdown,
    /// 8 final chorus (louder).
    fn song_bars() -> Vec<f32> {
        let mut v = vec![-30.0; 8];
        v.extend(vec![-22.0; 8]);
        v.extend(vec![-16.0; 8]);
        v.extend(vec![-26.0; 4]);
        v.extend(vec![-14.0; 8]);
        v
    }

    #[test]
    fn sections_split_where_the_level_steps() {
        let bars = song_bars();
        let phrases: Vec<usize> = (0..bars.len()).step_by(4).collect();
        let s = find_sections(&bars, &phrases);
        assert_eq!(s, vec![(0, 8), (8, 16), (16, 24), (24, 28), (28, 36)]);
    }

    #[test]
    fn a_smeared_step_snaps_to_the_phrase_start() {
        // The new section starts on the phrase at bar 8, but its first bar
        // only half-arrives: the sharpest step is a bar late, at bar 9.
        let mut bars = vec![-24.0; 16];
        bars[8] = -22.0;
        for b in bars.iter_mut().skip(9) {
            *b = -16.0;
        }
        let s = find_sections(&bars, &[0, 4, 8, 12]);
        assert_eq!(s, vec![(0, 8), (8, 16)]);
    }

    #[test]
    fn a_steady_song_is_one_section() {
        let bars: Vec<f32> = (0..32).map(|b| -18.0 + 0.4 * ((b % 3) as f32 - 1.0)).collect();
        assert_eq!(find_sections(&bars, &[0, 4, 8, 12, 16, 20, 24, 28]), vec![(0, 32)]);
    }

    /// Features for a whole song built from per-bar levels: kick on every
    /// beat, accent on the "1".
    fn song_feats(bar_db: &[f32], phase: usize) -> Vec<BeatFeature> {
        let mut f = Vec::new();
        for _ in 0..phase {
            f.push(BeatFeature { accent: 0.6, low: 0.01, energy: 10f32.powf(bar_db[0] / 10.0), activity: 0.1 });
        }
        for &l in bar_db {
            let e = 10f32.powf(l / 10.0);
            for p in 0..4 {
                f.push(BeatFeature {
                    accent: if p == 0 { 1.0 } else { 0.6 },
                    low: if p == 0 { e * 2.0 } else { e },
                    energy: e,
                    activity: 0.1,
                });
            }
        }
        f
    }

    /// Onset / energy envelopes at 43 Hz with a hit on every beat up to
    /// `last_beat`, then a decaying tail of `tail_secs` and silence.
    fn envelopes(beats: &[f32], last_beat: usize, tail_secs: f32, rate: f32, total_secs: f32) -> (Vec<f32>, Vec<f32>) {
        let n = (total_secs * rate) as usize;
        let mut onset = vec![0.0_f32; n];
        let mut energy = vec![0.0_f32; n];
        let end = beats[last_beat];
        for (i, e) in energy.iter_mut().enumerate() {
            let t = (i as f32 + 1.0) / rate;
            *e = if t <= end { 0.01 } else if t <= end + tail_secs { 0.01 * (-(t - end) * 4.0).exp() } else { 0.0 };
        }
        for &b in &beats[..=last_beat] {
            let i = (b * rate - 1.0).round() as usize;
            if i < n {
                onset[i] = 1.0;
            }
        }
        (onset, energy)
    }

    #[test]
    fn the_drop_is_the_loudest_section_that_rises_out_of_a_quieter_one() {
        let bars = song_bars();
        let feats = song_feats(&bars, 0);
        let interval = 0.5;
        let beats: Vec<f32> = (0..feats.len()).map(|k| k as f32 * interval).collect();
        let rate = 22_050.0 / 512.0;
        let (onset, energy) = envelopes(&beats, beats.len() - 1, 2.0, rate, beats.len() as f32 * interval + 3.0);
        let s = analyze(&beats, &feats, &onset, &energy, rate).expect("structure");
        assert_eq!(s.downbeat_phase, 0);
        let levels: Vec<Level> = s.sections.iter().map(|x| x.level).collect();
        assert_eq!(levels, vec![Level::Quiet, Level::Mid, Level::High, Level::Quiet, Level::High]);
        // The final chorus: loudest, and the biggest rise (out of the breakdown).
        let drop = s.drop.expect("drop");
        assert_eq!(s.sections[drop].start_beat, 28 * 4);
        assert!((s.sections[drop].start_secs - 56.0).abs() < 1e-4);
        assert!(s.is_downbeat(s.sections[drop].start_beat));
        assert!(s.is_phrase_start(s.sections[drop].start_beat));
    }

    #[test]
    fn equally_big_choruses_make_the_first_one_the_drop() {
        // Quiet and loud phrases alternate; the later choruses are a hair
        // louder (0.2 dB), not a bigger section: the first chorus is the drop.
        let mut bars = Vec::new();
        for (k, high) in [-20.0_f32, -19.9, -19.8].into_iter().enumerate() {
            bars.extend(vec![-26.0 + 0.1 * k as f32; 4]);
            bars.extend(vec![high; 4]);
        }
        let feats = song_feats(&bars, 0);
        let beats: Vec<f32> = (0..feats.len()).map(|k| k as f32 * 0.5).collect();
        let rate = 22_050.0 / 512.0;
        let (onset, energy) = envelopes(&beats, beats.len() - 1, 2.0, rate, 52.0);
        let s = analyze(&beats, &feats, &onset, &energy, rate).expect("structure");
        let levels: Vec<Level> = s.sections.iter().map(|x| x.level).collect();
        assert_eq!(levels, vec![Level::Quiet, Level::High, Level::Quiet, Level::High, Level::Quiet, Level::High]);
        let scores: Vec<f32> = (0..6).filter_map(|i| s.drop_score(i)).collect();
        assert!(scores[2] > scores[0], "{scores:?}");
        assert_eq!(s.drop, Some(1));
    }

    #[test]
    fn a_steady_song_has_no_drop() {
        let bars = vec![-18.0; 24];
        let feats = song_feats(&bars, 2);
        let beats: Vec<f32> = (0..feats.len()).map(|k| 0.3 + k as f32 * 0.5).collect();
        let rate = 22_050.0 / 512.0;
        let (onset, energy) = envelopes(&beats, beats.len() - 1, 1.0, rate, 55.0);
        let s = analyze(&beats, &feats, &onset, &energy, rate).expect("structure");
        assert_eq!(s.downbeat_phase, 2);
        assert!(s.downbeat_confidence > 0.9, "{}", s.downbeat_confidence);
        assert_eq!(s.sections.len(), 1);
        assert_eq!(s.sections[0].level, Level::Mid);
        assert_eq!(s.drop, None);
        assert!(s.intensity.iter().all(|&i| (i - 0.5).abs() < 0.05), "{:?}", s.intensity);
    }

    #[test]
    fn intensity_follows_the_sections() {
        let bars = song_bars();
        let feats = song_feats(&bars, 0);
        let beats: Vec<f32> = (0..feats.len()).map(|k| k as f32 * 0.5).collect();
        let rate = 22_050.0 / 512.0;
        let (onset, energy) = envelopes(&beats, beats.len() - 1, 2.0, rate, 75.0);
        let s = analyze(&beats, &feats, &onset, &energy, rate).unwrap();
        let at = |bar: usize| s.intensity[bar * 4 + 2];
        assert!(at(2) < 0.2, "intro {}", at(2));
        assert!(at(20) > at(10), "chorus {} vs verse {}", at(20), at(10));
        assert!(at(32) > 0.8, "final chorus {}", at(32));
    }

    #[test]
    fn final_hit_is_the_last_strong_onset_before_the_tail() {
        let rate = 22_050.0 / 512.0;
        let beats: Vec<f32> = (0..80).map(|k| 0.2 + k as f32 * 0.5).collect();
        // Music up to beat 60 (30.2 s), then a 3 s ring-out and silence;
        // a faint tick in the tail must not count.
        let (mut onset, energy) = envelopes(&beats, 60, 3.0, rate, 40.0);
        let tick = ((beats[60] + 1.5) * rate) as usize;
        onset[tick] = 0.1;
        let hit = find_final_hit(&onset, &energy, rate, &beats, 0.5);
        assert_eq!(hit.hit_secs, Some(beats[60]));
        // −40 dB of 0.01 power: e^(−4 t) = 1e−4 → t ≈ 2.3 s after the hit.
        assert!(
            (hit.sound_end_secs - (beats[60] + 2.3)).abs() < 0.1,
            "sound end {}",
            hit.sound_end_secs
        );
    }

    #[test]
    fn final_hit_of_a_song_that_stops_dead_is_its_last_beat() {
        let rate = 22_050.0 / 512.0;
        let beats: Vec<f32> = (0..60).map(|k| k as f32 * 0.5).collect();
        let (onset, energy) = envelopes(&beats, 59, 0.0, rate, 29.6);
        let hit = find_final_hit(&onset, &energy, rate, &beats, 0.5);
        assert_eq!(hit.hit_secs, Some(29.5));
        assert!(hit.sound_end_secs >= 29.5 && hit.sound_end_secs <= 29.7, "{}", hit.sound_end_secs);
    }

    #[test]
    fn music_starting_at_zero_counts_the_beat_before_the_grid() {
        let rate = 22_050.0 / 512.0;
        // Grid from 0.4545 s (the hit at 0 s is not on it); sound from 0.
        let beats: Vec<f32> = (1..100).map(|k| k as f32 * 0.4545).collect();
        let energy = vec![0.01_f32; 2000];
        assert_eq!(first_audible_beat(&energy, rate, &beats, 0.4545), -1);
        // A 5 s silent lead-in: the music starts at beat 10 of the grid.
        let mut late = energy.clone();
        for e in late.iter_mut().take((5.0 * rate) as usize) {
            *e = 0.0;
        }
        assert_eq!(first_audible_beat(&late, rate, &beats, 0.4545), 10);
    }

    #[test]
    fn low_band_keeps_a_bass_note_and_drops_a_hi_hat() {
        let sr = 22_050u32;
        let bass: Vec<f32> = (0..sr).map(|i| 0.5 * (2.0 * std::f32::consts::PI * 60.0 * i as f32 / sr as f32).sin()).collect();
        let hat: Vec<f32> = (0..sr).map(|i| 0.5 * (2.0 * std::f32::consts::PI * 6000.0 * i as f32 / sr as f32).sin()).collect();
        let lb = low_band_frame_energy(&bass, sr, 1024, 512, 150.0);
        let lh = low_band_frame_energy(&hat, sr, 1024, 512, 150.0);
        let (b, h) = (lb[20], lh[20]);
        assert!(b > 0.08, "bass power {b}");
        assert!(h < b * 1e-3, "hat {h} vs bass {b}");
    }
}
