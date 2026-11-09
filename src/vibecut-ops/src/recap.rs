//! "Your year" recap: the best moment of each month — or of each trip, when
//! the footage spans only a few months — gathered from the versions every
//! render keeps, cut to a library song like any reel.
//!
//! Every async render keeps `<output dir>/versions/<stem>__<tag>.mp4` plus
//! its manifest ([`crate::render`]). A manifest lists the segments the
//! render picked, with their scores and source windows, and the probed
//! recording time of every footage file. The recap:
//!
//! 1. **collects** every kept manifest under `/projects` (not the recaps'
//!    own, in [`RECAP_DIR`]) and takes each version's best segments by
//!    score, scored against the best pick of their own version. Each is
//!    dated by when it was filmed — the file's `creation_time` plus the
//!    offset into it, else the file's modification time — never by when
//!    it was rendered. Footage that is gone can't be rendered and is left
//!    out.
//! 2. **de-duplicates**: the same footage file with an overlapping range is
//!    one moment, however many versions (or projects) picked it; being
//!    picked again counts a little in its favour.
//! 3. **plans**: one bucket per month when the moments span at least three
//!    months, else one per trip (project). Every bucket gets its best
//!    moment, the rest of the length budget goes round the buckets, and
//!    each moment is sized so the reel lands near the asked length. Months
//!    play in the order they were filmed, the first moment of each with
//!    the month (or trip) as a lower third.
//! 4. **writes an ordinary project config** — `[input] ranges_only` with
//!    one `[[input.ranges]]` per moment, a title card, the auto-picked
//!    song, beat-locked cuts — so the standard pipeline renders it. There
//!    is no second renderer: the config can be edited, rendered, tweaked
//!    and kept like any other.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize, Serializer};
use vibecut::project_config::Config;
/// A window left out of a recap (`[[selection.exclude]]`, see
/// [`RecapArgs::exclude`]), and the crossfades and ramps a remake keeps
/// ([`RecapArgs::transition`]).
pub use vibecut::project_config::{SelectionExclude, Transition};

use crate::error::OpsError;
use crate::music::{AutoPick, TrackInfo};
use crate::paths::{slugify, PROJECTS_ROOT};
use crate::propose::{self, ProposedSoundtrack};
use crate::vibes;

/// Folder inside `/projects` holding the recaps: their configs, and their
/// renders under `out/`. The collector never reads it, so a recap never
/// feeds on an earlier recap.
pub const RECAP_DIR: &str = "recaps";
/// A recap needs moments from at least this many months — or trips.
pub const MIN_BUCKETS: usize = 3;
/// Moments from at least this many months make one bucket per month;
/// fewer, one per trip.
const MIN_MONTHS: usize = 3;
/// Best segments taken from each kept version.
const BEST_PER_VERSION: usize = 4;
/// Length when the caller doesn't ask for one, and the range it may ask for.
pub const DEFAULT_LENGTH_SECS: f64 = 60.0;
const MIN_LENGTH_SECS: f64 = 20.0;
const MAX_LENGTH_SECS: f64 = 300.0;
/// The title card, and the crossfade into each clip (the config's
/// `[transition] duration`).
const TITLE_SECS: f64 = 3.0;
const SEAM_SECS: f64 = 0.3;
/// How long one moment plays: what the planner aims for, and its bounds.
const MOMENT_IDEAL_SECS: f64 = 4.0;
const MOMENT_MIN_SECS: f64 = 2.5;
const MOMENT_MAX_SECS: f64 = 6.0;
/// The vibe whose music profile picks the song unless the caller names one.
pub const DEFAULT_VIBE: &str = "family_chronicle";
/// How deep below `/projects` to look for `versions/` folders.
const MAX_SCAN_DEPTH: usize = 6;
/// How far a shuffle may lift a moment's rank: enough to bring up a
/// month's other strong moments, rarely its weak ones.
const SHUFFLE_SPREAD: f32 = 0.6;

// ---------- Moments ----------

/// Where a moment's date came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DatedBy {
    /// The camera's recording time (the file's `creation_time`).
    Footage,
    /// The footage file's modification time: the camera left no usable date.
    FileTime,
}

/// One stretch of footage a render picked.
#[derive(Debug, Clone, Serialize)]
pub struct Moment {
    /// The footage file.
    pub path: String,
    /// The window the render picked, seconds into the file.
    pub start_secs: f64,
    pub end_secs: f64,
    /// Length of the footage file, when the manifest knew it.
    pub file_secs: Option<f64>,
    /// When it was filmed.
    #[serde(serialize_with = "ser_time")]
    pub recorded_at: DateTime<Utc>,
    pub dated_by: DatedBy,
    /// Score against the best pick of its version, 0..=1.
    pub score: f32,
    /// Kept versions that picked this stretch of footage.
    pub versions: u32,
    /// The trip (project) it comes from.
    pub trip: String,
}

impl Moment {
    /// Ordering within a bucket: the score, plus a little for every other
    /// version that picked it too.
    fn rank(&self) -> f32 {
        self.score + 0.05 * self.versions.saturating_sub(1).min(4) as f32
    }

    /// The same footage file, with ranges that share some time.
    fn overlaps(&self, o: &Moment) -> bool {
        self.path == o.path && self.end_secs.min(o.end_secs) - self.start_secs.max(o.start_secs) > 0.0
    }
}

fn ser_time<S: Serializer>(t: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&t.to_rfc3339_opts(SecondsFormat::Secs, true))
}

/// A kept manifest, read leniently: only what the recap needs, so older
/// manifests (no scores, no timeline) still count.
#[derive(Debug, Deserialize)]
struct KeptManifest {
    #[serde(default)]
    project_name: String,
    #[serde(default)]
    sources: Vec<KeptSource>,
    #[serde(default)]
    segments: Vec<KeptSegment>,
}

#[derive(Debug, Deserialize)]
struct KeptSource {
    #[serde(default)]
    chapters: Vec<KeptChapter>,
}

#[derive(Debug, Deserialize)]
struct KeptChapter {
    path: String,
    #[serde(default)]
    duration_secs: f64,
    #[serde(default)]
    creation_time: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KeptSegment {
    #[serde(default)]
    source_id: String,
    chapter_path: String,
    in_chapter_start_secs: f64,
    duration_secs: f64,
    #[serde(default)]
    score: Option<f32>,
}

/// What the collector needs to know about a footage file.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FootageFile {
    pub modified: Option<DateTime<Utc>>,
}

/// Looks a footage file up: `None` when it is gone.
pub(crate) type FootageLookup<'a> = &'a dyn Fn(&str) -> Option<FootageFile>;

fn footage_on_disk(path: &str) -> Option<FootageFile> {
    let meta = std::fs::metadata(path).ok()?;
    meta.is_file().then(|| FootageFile { modified: meta.modified().ok().map(DateTime::<Utc>::from) })
}

/// A camera's `creation_time` (RFC 3339, or ffprobe's `…T…Z` with
/// microseconds); `None` when missing, unreadable or before 2000 — the
/// zero dates of a camera whose clock was never set.
pub(crate) fn parse_recorded(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    let t = DateTime::parse_from_rfc3339(s).map(|t| t.with_timezone(&Utc)).ok().or_else(|| {
        let naive = s.trim_end_matches('Z');
        ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S"]
            .iter()
            .find_map(|f| NaiveDateTime::parse_from_str(naive, f).ok())
            .map(|n| n.and_utc())
    })?;
    (t.year() >= 2000).then_some(t)
}

/// When a moment `offset_secs` into a file was filmed: the file's
/// recording time plus the offset, else the file's modification time.
pub(crate) fn date_moment(
    creation_time: Option<&str>,
    offset_secs: f64,
    modified: Option<DateTime<Utc>>,
) -> Option<(DateTime<Utc>, DatedBy)> {
    if let Some(t) = creation_time.and_then(parse_recorded) {
        let ms = (offset_secs.max(0.0) * 1000.0).round() as i64;
        return Some((t + chrono::Duration::milliseconds(ms), DatedBy::Footage));
    }
    modified.map(|t| (t, DatedBy::FileTime))
}

/// The trip a project stands for: its name before the ` · date` a
/// proposal appends ("Alps Ski Day · 2026-03-14" → "Alps Ski Day").
fn trip_label(project_name: &str) -> Option<String> {
    let t = project_name.split(" · ").next()?.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn is_image(path: &str) -> bool {
    vibecut::media_probe::is_image_path(camino::Utf8Path::new(path))
}

/// One version's best segments as moments: the top [`BEST_PER_VERSION`]
/// by score whose footage is still there. `missing` counts the ones whose
/// footage is gone.
fn version_moments(
    m: &KeptManifest,
    fallback_trip: &str,
    footage: FootageLookup<'_>,
    missing: &mut usize,
) -> Vec<Moment> {
    let chapters: HashMap<&str, &KeptChapter> =
        m.sources.iter().flat_map(|s| &s.chapters).map(|c| (c.path.as_str(), c)).collect();
    let trip = trip_label(&m.project_name).unwrap_or_else(|| fallback_trip.to_string());
    // Footage only: not photos, and not a folded beat-burst — a render
    // intermediate (`<workspace>/beat_burst.mp4`, id `burst:…`, a stand-in
    // score of 1) that would also skew every other score.
    let footage_file = |s: &KeptSegment| {
        !s.source_id.starts_with("burst:")
            && !is_image(&s.chapter_path)
            && (chapters.is_empty() || chapters.contains_key(s.chapter_path.as_str()))
    };
    let segs: Vec<&KeptSegment> = m
        .segments
        .iter()
        .filter(|s| s.duration_secs > 0.0 && s.in_chapter_start_secs >= 0.0 && footage_file(s))
        .collect();
    let best = segs.iter().filter_map(|s| s.score).fold(0.0_f32, f32::max);
    let mut ranked: Vec<(&KeptSegment, f32)> = segs
        .iter()
        .map(|s| {
            let rel = if best > 0.0 { (s.score.unwrap_or(0.0) / best).clamp(0.0, 1.0) } else { 0.5 };
            (*s, rel)
        })
        .collect();
    // Stable: equal scores keep the manifest's order.
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));

    let mut out: Vec<Moment> = Vec::new();
    for (s, score) in ranked {
        if out.len() >= BEST_PER_VERSION {
            break;
        }
        let Some(file) = footage(&s.chapter_path) else {
            *missing += 1;
            continue;
        };
        let ch = chapters.get(s.chapter_path.as_str());
        let Some((recorded_at, dated_by)) =
            date_moment(ch.and_then(|c| c.creation_time.as_deref()), s.in_chapter_start_secs, file.modified)
        else {
            continue;
        };
        let moment = Moment {
            path: s.chapter_path.clone(),
            start_secs: s.in_chapter_start_secs,
            end_secs: s.in_chapter_start_secs + s.duration_secs,
            file_secs: ch.map(|c| c.duration_secs).filter(|d| *d > 0.0),
            recorded_at,
            dated_by,
            score,
            versions: 1,
            trip: trip.clone(),
        };
        if !out.iter().any(|o| o.overlaps(&moment)) {
            out.push(moment);
        }
    }
    out
}

/// One moment per stretch of footage: candidates on the same file whose
/// ranges overlap merge into the best-scored one, which counts the
/// versions that picked it.
pub(crate) fn dedup(mut candidates: Vec<Moment>) -> Vec<Moment> {
    candidates.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.recorded_at.cmp(&b.recorded_at))
            .then(a.path.cmp(&b.path))
            .then(a.start_secs.total_cmp(&b.start_secs))
    });
    let mut kept: Vec<Moment> = Vec::new();
    for c in candidates {
        match kept.iter_mut().find(|k| k.overlaps(&c)) {
            Some(k) => k.versions += c.versions,
            None => kept.push(c),
        }
    }
    kept
}

/// Every moment the kept versions under a projects folder offer.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Collection {
    /// De-duplicated.
    pub moments: Vec<Moment>,
    /// Kept versions read.
    pub versions: usize,
    /// Picks whose footage is no longer there.
    pub missing_footage: usize,
    /// Manifests that couldn't be read.
    pub unreadable: Vec<String>,
}

/// Read every kept version under `root` (skipping `root/recaps`, hidden
/// folders, the music library and session logs) into moments.
pub(crate) fn collect(root: &Path, footage: FootageLookup<'_>) -> Collection {
    let mut manifests = Vec::new();
    find_kept_manifests(root, &root.join(RECAP_DIR), 0, &mut manifests);
    manifests.sort();
    let mut c = Collection::default();
    let mut candidates = Vec::new();
    for p in manifests {
        let parsed = std::fs::read_to_string(&p)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<KeptManifest>(&t).map_err(|e| e.to_string()));
        let m = match parsed {
            Ok(m) => m,
            Err(e) => {
                c.unreadable.push(format!("{}: {e}", p.display()));
                continue;
            }
        };
        c.versions += 1;
        // "coastline_bike_ride__20261108T…" → "coastline_bike_ride".
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let stem = name.trim_end_matches(".manifest.json");
        let fallback = stem.split("__").next().unwrap_or(stem);
        candidates.extend(version_moments(&m, fallback, footage, &mut c.missing_footage));
    }
    c.moments = dedup(candidates);
    c
}

fn find_kept_manifests(dir: &Path, skip: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_SCAN_DEPTH || dir == skip {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let in_versions = dir.file_name().is_some_and(|n| n == "versions");
    for e in entries.flatten() {
        let Ok(kind) = e.file_type() else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        if kind.is_dir() {
            let skipped = name.starts_with('.') || (depth == 0 && (name == "music" || name == "sessions"));
            if !skipped {
                find_kept_manifests(&e.path(), skip, depth + 1, out);
            }
        } else if in_versions && kind.is_file() && name.ends_with(".manifest.json") {
            out.push(e.path());
        }
    }
}

// ---------- Period ----------

/// The days a recap covers, inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    pub from: NaiveDate,
    pub to: NaiveDate,
    /// A whole calendar year.
    pub year: Option<i32>,
}

impl Span {
    fn year(y: i32) -> Option<Span> {
        Some(Span {
            from: NaiveDate::from_ymd_opt(y, 1, 1)?,
            to: NaiveDate::from_ymd_opt(y, 12, 31)?,
            year: Some(y),
        })
    }

    fn contains(&self, t: &DateTime<Utc>) -> bool {
        let d = t.date_naive();
        self.from <= d && d <= self.to
    }

    /// "2026 in moments", "March – June 2026", "Nov 2025 – Feb 2026".
    fn title(&self) -> String {
        if let Some(y) = self.year {
            return format!("{y} in moments");
        }
        let (a, b) = (self.from, self.to);
        if (a.year(), a.month()) == (b.year(), b.month()) {
            a.format("%B %Y").to_string()
        } else if a.year() == b.year() {
            format!("{} – {}", a.format("%B"), b.format("%B %Y"))
        } else {
            format!("{} – {}", a.format("%b %Y"), b.format("%b %Y"))
        }
    }

    /// File-name part: "2026" or "2026-03_2026-06".
    fn slug(&self) -> String {
        match self.year {
            Some(y) => y.to_string(),
            None => format!("{}_{}", self.from.format("%Y-%m"), self.to.format("%Y-%m")),
        }
    }
}

/// "2026", "2026-03" or "2026-03-14" as a day: the first of that span, or
/// with `end` its last.
fn parse_day(s: &str, end: bool) -> Result<NaiveDate, OpsError> {
    let bad = || OpsError::invalid_with_hint(format!("not a date: {s:?}"), "use 2026, 2026-03 or 2026-03-14");
    let parts: Vec<&str> = s.trim().split('-').collect();
    let num = |p: &str| p.parse::<u32>().map_err(|_| bad());
    let y = parts.first().ok_or_else(|| bad())?.parse::<i32>().map_err(|_| bad())?;
    let day = match parts.len() {
        1 => NaiveDate::from_ymd_opt(y, if end { 12 } else { 1 }, if end { 31 } else { 1 }),
        2 => {
            let m = num(parts[1])?;
            let first = NaiveDate::from_ymd_opt(y, m, 1);
            if end {
                first.and_then(|f| f.checked_add_months(chrono::Months::new(1))).and_then(|n| n.pred_opt())
            } else {
                first
            }
        }
        3 => NaiveDate::from_ymd_opt(y, num(parts[1])?, num(parts[2])?),
        _ => None,
    };
    day.filter(|d| (1900..=9999).contains(&d.year())).ok_or_else(|| bad())
}

/// The period asked for: a `year`, a `from` / `to` range (either end may
/// be left open: it then reaches the earliest / latest moment), or —
/// with neither — the latest year with any moments.
pub(crate) fn resolve_period(
    year: Option<i32>,
    from: Option<&str>,
    to: Option<&str>,
    moments: &[Moment],
) -> Result<Span, OpsError> {
    let from = from.map(str::trim).filter(|s| !s.is_empty());
    let to = to.map(str::trim).filter(|s| !s.is_empty());
    if year.is_some() && (from.is_some() || to.is_some()) {
        return Err(OpsError::invalid("give a year or a from/to range, not both"));
    }
    if let Some(y) = year {
        return Span::year(y)
            .filter(|_| (1900..=9999).contains(&y))
            .ok_or_else(|| OpsError::invalid(format!("not a year: {y}")));
    }
    let earliest = moments.iter().map(|m| m.recorded_at.date_naive()).min();
    let latest = moments.iter().map(|m| m.recorded_at.date_naive()).max();
    if from.is_none() && to.is_none() {
        let latest = latest.ok_or_else(no_versions)?;
        return Span::year(latest.year()).ok_or_else(no_versions);
    }
    let asked_from = from.map(|s| parse_day(s, false)).transpose()?;
    let asked_to = to.map(|s| parse_day(s, true)).transpose()?;
    // An open end reaches the first / last moment (never past the other end).
    let hi = asked_to.unwrap_or_else(|| {
        let last = latest.unwrap_or_else(|| Utc::now().date_naive());
        asked_from.map_or(last, |lo| last.max(lo))
    });
    let lo = asked_from.unwrap_or_else(|| earliest.unwrap_or(hi).min(hi));
    if lo > hi {
        return Err(OpsError::invalid(format!("the recap's start ({lo}) is after its end ({hi})")));
    }
    // A from/to that is exactly one calendar year reads as that year.
    let whole_year = lo.year() == hi.year() && (lo.month(), lo.day()) == (1, 1) && (hi.month(), hi.day()) == (12, 31);
    Ok(Span { from: lo, to: hi, year: whole_year.then_some(lo.year()) })
}

fn no_versions() -> OpsError {
    OpsError::not_found_with(
        "saved versions",
        "a recap is made from the versions every finished render keeps; make a few videos first",
    )
}

/// A recap length as a caller writes it: seconds (`"60"`, `"90s"`) or
/// minutes and seconds (`"1m30s"`, `"1m 30s"`, `"2m"`).
pub fn parse_length(s: &str) -> Result<f64, OpsError> {
    let bad = || OpsError::invalid_with_hint(format!("not a length: {s:?}"), "use seconds (60) or 1m30s");
    let t = s.trim().to_ascii_lowercase().replace(' ', "");
    if let Ok(secs) = t.parse::<f64>() {
        return (secs.is_finite() && secs > 0.0).then_some(secs).ok_or_else(bad);
    }
    let (mins, rest) = match t.split_once('m') {
        Some((m, r)) => (m.parse::<u32>().map_err(|_| bad())?, r),
        None => (0, t.as_str()),
    };
    let secs = match rest.strip_suffix('s') {
        Some(n) => n.parse::<f64>().map_err(|_| bad())?,
        None if rest.is_empty() => 0.0,
        None => return Err(bad()),
    };
    let total = mins as f64 * 60.0 + secs;
    (total.is_finite() && total > 0.0).then_some(total).ok_or_else(bad)
}

// ---------- Buckets ----------

/// What a recap's buckets are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Grouping {
    Month,
    Trip,
}

/// Months when the moments span at least [`MIN_MONTHS`] of them, else trips.
pub(crate) fn grouping_for(moments: &[Moment]) -> Grouping {
    let months: BTreeSet<(i32, u32)> =
        moments.iter().map(|m| (m.recorded_at.year(), m.recorded_at.month())).collect();
    if months.len() >= MIN_MONTHS { Grouping::Month } else { Grouping::Trip }
}

/// The moments of one month or trip, best first.
#[derive(Debug, Clone)]
pub(crate) struct Bucket {
    pub key: String,
    pub label: String,
    pub first: DateTime<Utc>,
    pub moments: Vec<Moment>,
}

/// "March", or "March 2026" when the recap spans more than one year.
fn month_label(t: &DateTime<Utc>, with_year: bool) -> String {
    t.format(if with_year { "%B %Y" } else { "%B" }).to_string()
}

/// Group moments into buckets, in the order they were filmed (a trip
/// counts from its first moment).
pub(crate) fn bucket(moments: &[Moment], grouping: Grouping, with_year: bool) -> Vec<Bucket> {
    let mut map: BTreeMap<String, Bucket> = BTreeMap::new();
    for m in moments {
        let (key, label) = match grouping {
            Grouping::Month => (m.recorded_at.format("%Y-%m").to_string(), month_label(&m.recorded_at, with_year)),
            Grouping::Trip => (m.trip.to_lowercase(), m.trip.clone()),
        };
        let b = map.entry(key.clone()).or_insert_with(|| Bucket {
            key,
            label,
            first: m.recorded_at,
            moments: Vec::new(),
        });
        b.first = b.first.min(m.recorded_at);
        b.moments.push(m.clone());
    }
    let mut out: Vec<Bucket> = map.into_values().collect();
    for b in &mut out {
        b.moments.sort_by(|x, y| y.rank().total_cmp(&x.rank()).then(x.recorded_at.cmp(&y.recorded_at)));
    }
    out.sort_by(|a, b| a.first.cmp(&b.first).then(a.key.cmp(&b.key)));
    out
}

/// Whether a window the user dropped (`[[selection.exclude]]`, matched as
/// the pipeline matches it: the file name or a suffix of it, any letter
/// case) covers some of a moment.
pub(crate) fn dropped(e: &SelectionExclude, m: &Moment) -> bool {
    let file = e.file.trim().to_ascii_lowercase();
    let name = m.path.rsplit(['/', '\\']).next().unwrap_or("").to_ascii_lowercase();
    !file.is_empty()
        && (name == file || name.ends_with(&file))
        && m.end_secs.min(e.end.as_secs_f64()) - m.start_secs.max(e.start.as_secs_f64()) > 0.0
}

/// A deterministic draw in `[0, 1)` for one moment under one seed.
fn moment_noise(seed: u64, m: &Moment) -> f32 {
    // FNV-1a of the moment's file and start, as the draw's index.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in m.path.bytes().chain(((m.start_secs * 1000.0).round() as i64).to_le_bytes()) {
        h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    vibecut::rng::coin_f32(seed, h as usize)
}

/// Re-rank a bucket for a shuffle: each moment's rank lifted by a seeded
/// draw of up to [`SHUFFLE_SPREAD`], so another of its strong moments
/// can lead. The same seed always gives the same order.
pub(crate) fn shuffle_bucket(b: &mut Bucket, seed: u64) {
    let key = |m: &Moment| m.rank() + SHUFFLE_SPREAD * moment_noise(seed, m);
    b.moments.sort_by(|x, y| key(y).total_cmp(&key(x)).then(x.recorded_at.cmp(&y.recorded_at)));
}

// ---------- Length budget ----------
//
// A moment's length is the time it is on screen, cut to cut. That is what
// the beat lock works in: it snaps each clip's planned length to the
// nearest beat and reads the crossfade's overlap on top. So a beat-locked
// recap lasts about the title card plus its moments' spans, and spans in
// whole beats of the song leave the lock nothing to stretch. Without music
// nothing adds the overlap, so each range carries it itself.

/// How many moments a `length_secs` recap wants, and the most it can hold.
pub(crate) fn moment_counts(length_secs: f64) -> (usize, usize) {
    let body = (length_secs - TITLE_SECS).max(MOMENT_MIN_SECS);
    let ideal = (body / MOMENT_IDEAL_SECS).round().max(1.0) as usize;
    let max = (body / MOMENT_MIN_SECS).floor().max(1.0) as usize;
    (ideal, max.max(ideal))
}

/// Seconds each of `n` moments is on screen so the recap lasts
/// `length_secs` with its title card, within the moment bounds.
pub(crate) fn moment_secs(length_secs: f64, n: usize) -> f64 {
    ((length_secs - TITLE_SECS) / n.max(1) as f64).clamp(MOMENT_MIN_SECS, MOMENT_MAX_SECS)
}

/// `n` on-screen spans in whole beats, adding up as close to `total`
/// seconds as whole beats allow: the spare beats spread evenly along the
/// recap, every span within the moment bounds.
pub(crate) fn beat_spans(total: f64, n: usize, beat: f64) -> Vec<f64> {
    if n == 0 || !(beat > 0.0) {
        return vec![total / n.max(1) as f64; n];
    }
    let lo = ((MOMENT_MIN_SECS / beat).ceil() as u64).max(1);
    let hi = ((MOMENT_MAX_SECS / beat).floor() as u64).max(lo);
    let n64 = n as u64;
    let want = ((total / beat).round().max(0.0) as u64).clamp(lo * n64, hi * n64);
    let (k, spare) = (want / n64, want % n64);
    (0..n64).map(|i| (k + (i + 1) * spare / n64 - i * spare / n64) as f64 * beat).collect()
}

/// Cut every moment's window to its span (plus `overlap`, the crossfade a
/// range carries when nothing beat-locks it) and re-total the recap.
fn fit_windows(plan: &mut RecapPlan, spans: &[f64], overlap: f64) {
    for (p, &span) in plan.moments.iter_mut().zip(spans) {
        let (lo, hi) = window(&p.moment, span + overlap);
        p.clip_start_secs = lo;
        p.clip_end_secs = hi;
    }
    separate_overlaps(&mut plan.moments);
    let shown: f64 = plan.moments.iter().map(|p| (p.clip_end_secs - p.clip_start_secs - overlap).max(0.0)).sum();
    plan.estimated_secs = if plan.moments.is_empty() { 0.0 } else { round_ms(TITLE_SECS + shown) };
}

/// Size the moments for the song the recap got: whole beats of it
/// (`beat`, when known), or — without music — ranges that carry their own
/// crossfades.
pub(crate) fn fit_to_song(plan: &mut RecapPlan, music: bool, beat: Option<f64>) {
    let n = plan.moments.len();
    let span = plan.moment_secs;
    match (music, beat) {
        (true, Some(b)) => {
            let spans = beat_spans(span * n as f64, n, b);
            plan.moment_secs = round_ms(spans.iter().sum::<f64>() / n.max(1) as f64);
            fit_windows(plan, &spans, 0.0);
        }
        (true, None) => fit_windows(plan, &vec![span; n], 0.0),
        (false, _) => {
            let seam = plan.seam();
            fit_windows(plan, &vec![span; n], seam)
        }
    }
}

/// Which moments make the recap, as `(bucket, moment)` indices: each
/// bucket's best (only the strongest buckets when more than `max` have
/// any), then one more per bucket in turn — the bucket with the fewest
/// picks first, ties to the stronger next moment — up to `ideal` moments
/// or until the buckets run out.
pub(crate) fn choose(buckets: &[Bucket], ideal: usize, max: usize) -> Vec<(usize, usize)> {
    let mut kept: Vec<usize> = (0..buckets.len()).filter(|&b| !buckets[b].moments.is_empty()).collect();
    if kept.len() > max {
        kept.sort_by(|&a, &b| buckets[b].moments[0].rank().total_cmp(&buckets[a].moments[0].rank()).then(a.cmp(&b)));
        kept.truncate(max);
        kept.sort_unstable();
    }
    let mut taken = vec![0usize; buckets.len()];
    let mut picks: Vec<(usize, usize)> = kept.iter().map(|&b| (b, 0)).collect();
    for &b in &kept {
        taken[b] = 1;
    }
    while picks.len() < ideal {
        let next = kept.iter().copied().filter(|&b| taken[b] < buckets[b].moments.len()).min_by(|&a, &b| {
            taken[a]
                .cmp(&taken[b])
                .then(buckets[b].moments[taken[b]].rank().total_cmp(&buckets[a].moments[taken[a]].rank()))
                .then(a.cmp(&b))
        });
        let Some(b) = next else { break };
        picks.push((b, taken[b]));
        taken[b] += 1;
    }
    picks
}

fn round_ms(secs: f64) -> f64 {
    (secs * 1000.0).round() / 1000.0
}

/// The `len`-second window centred on a moment, kept inside its file.
pub(crate) fn window(m: &Moment, len: f64) -> (f64, f64) {
    let centre = (m.start_secs + m.end_secs) / 2.0;
    let (mut lo, mut hi) = (centre - len / 2.0, centre + len / 2.0);
    let file = m.file_secs.filter(|f| *f > 0.0);
    if let Some(f) = file {
        if hi > f {
            lo -= hi - f;
            hi = f;
        }
    }
    if lo < 0.0 {
        hi -= lo;
        lo = 0.0;
    }
    if let Some(f) = file {
        hi = hi.min(f);
    }
    (round_ms(lo), round_ms(hi))
}

// ---------- The plan ----------

/// A moment as the recap plays it.
#[derive(Debug, Clone, Serialize)]
pub struct PlannedMoment {
    #[serde(flatten)]
    pub moment: Moment,
    /// What the recap plays, seconds into the file.
    pub clip_start_secs: f64,
    pub clip_end_secs: f64,
    /// The month or trip it stands for.
    pub bucket: String,
    /// The lower third over it: its bucket's label, on the bucket's first moment.
    pub caption: Option<String>,
    #[serde(skip)]
    bucket_index: usize,
}

/// One month or trip of the period, and how much of it the recap uses.
#[derive(Debug, Clone, Serialize)]
pub struct BucketInfo {
    pub key: String,
    pub label: String,
    #[serde(serialize_with = "ser_time")]
    pub first_recorded: DateTime<Utc>,
    /// Moments it has.
    pub available: usize,
    /// Moments the recap plays (0: left out, the length had no room).
    pub chosen: usize,
    pub trips: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PeriodInfo {
    pub from: String,
    pub to: String,
    pub year: Option<i32>,
}

/// What a recap will play, before anything is written.
#[derive(Debug, Clone, Serialize)]
pub struct RecapPlan {
    pub period: PeriodInfo,
    pub title: String,
    /// The title card's second line: "12 moments · 5 months".
    pub subtitle: String,
    pub grouping: Grouping,
    /// Every month or trip of the period with moments, in order.
    pub buckets: Vec<BucketInfo>,
    /// What plays, in order.
    pub moments: Vec<PlannedMoment>,
    pub length_secs: f64,
    pub moment_secs: f64,
    /// Title card plus every moment, crossfades overlapping (before the
    /// beat lock moves each cut onto a beat).
    pub estimated_secs: f64,
    /// At least [`MIN_BUCKETS`] months or trips have moments in the recap.
    pub eligible: bool,
    pub min_buckets: usize,
    /// Kept versions read.
    pub versions: usize,
    /// Things worth telling the user (missing footage, unreadable manifests).
    pub notes: Vec<String>,
    /// The shuffle this draw came from; `None`: each month's best.
    pub shuffle: Option<u64>,
    #[serde(skip)]
    span: Span,
    /// The windows the user dropped, carried into the config.
    #[serde(skip)]
    exclude: Vec<SelectionExclude>,
    /// The `[transition]` carried into the config; `None`: the recap's own.
    #[serde(skip)]
    transition: Option<Transition>,
}

/// What the caller asks for.
#[derive(Debug, Clone, Default)]
pub struct RecapArgs {
    pub year: Option<i32>,
    /// `2026`, `2026-03` or `2026-03-14`; open ends reach the first / last moment.
    pub from: Option<String>,
    pub to: Option<String>,
    /// Target length; default [`DEFAULT_LENGTH_SECS`], kept within 20 s – 5 min.
    pub length_secs: Option<f64>,
    /// Title card text; default "2026 in moments" / "March – June 2026".
    pub title: Option<String>,
    /// Vibe whose music profile picks the song; default [`DEFAULT_VIBE`].
    pub vibe: Option<String>,
    /// Song file; `None` lets Vibecut pick, `"none"` for no music.
    pub soundtrack: Option<String>,
    pub seed: u64,
    /// Windows the user dropped from the recap on screen (its
    /// `[[selection.exclude]]`): no moment they cover plays, and the new
    /// config keeps them, so making it again never brings one back.
    pub exclude: Vec<SelectionExclude>,
    /// `Some(seed)`: another draw of each month's strong moments (the
    /// Studio's Shuffle); `None` plays each month's best.
    pub shuffle: Option<u64>,
    /// The `[transition]` of the recap on screen — crossfades and speed
    /// ramps the Studio's Calmer / More energy changed — for the new one
    /// to keep; `None`: 300 ms crossfades, no ramps.
    pub transition: Option<Transition>,
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Plan a recap of `collection` for `args`.
pub(crate) fn plan(collection: &Collection, args: &RecapArgs) -> Result<RecapPlan, OpsError> {
    let span = resolve_period(args.year, args.from.as_deref(), args.to.as_deref(), &collection.moments)?;
    // At least a millisecond: the config writes them to the millisecond.
    let exclude: Vec<SelectionExclude> = args
        .exclude
        .iter()
        .filter(|e| e.end.saturating_sub(e.start) >= std::time::Duration::from_millis(1))
        .cloned()
        .collect();
    let moments: Vec<Moment> = collection
        .moments
        .iter()
        .filter(|m| span.contains(&m.recorded_at) && !exclude.iter().any(|e| dropped(e, m)))
        .cloned()
        .collect();
    let grouping = grouping_for(&moments);
    let mut buckets = bucket(&moments, grouping, span.from.year() != span.to.year());
    if let Some(seed) = args.shuffle {
        for b in &mut buckets {
            shuffle_bucket(b, seed);
        }
    }
    let length = args.length_secs.filter(|l| l.is_finite()).unwrap_or(DEFAULT_LENGTH_SECS).clamp(MIN_LENGTH_SECS, MAX_LENGTH_SECS);
    let (ideal, max) = moment_counts(length);
    let picks = choose(&buckets, ideal, max);
    let len = round_ms(moment_secs(length, picks.len()));

    let mut planned: Vec<PlannedMoment> = picks
        .iter()
        .map(|&(b, i)| {
            let m = &buckets[b].moments[i];
            let (lo, hi) = window(m, len);
            PlannedMoment {
                moment: m.clone(),
                clip_start_secs: lo,
                clip_end_secs: hi,
                bucket: buckets[b].label.clone(),
                caption: None,
                bucket_index: b,
            }
        })
        .collect();
    // In the order they were filmed; a trip's moments stay together.
    planned.sort_by(|a, b| {
        let by_bucket = match grouping {
            Grouping::Month => std::cmp::Ordering::Equal,
            Grouping::Trip => a.bucket_index.cmp(&b.bucket_index),
        };
        by_bucket.then(a.moment.recorded_at.cmp(&b.moment.recorded_at)).then(a.clip_start_secs.total_cmp(&b.clip_start_secs))
    });
    separate_overlaps(&mut planned);
    let mut captioned = HashSet::new();
    for p in &mut planned {
        if captioned.insert(p.bucket_index) {
            p.caption = Some(p.bucket.clone());
        }
    }

    let mut chosen = vec![0usize; buckets.len()];
    for p in &planned {
        chosen[p.bucket_index] += 1;
    }
    let bucket_info: Vec<BucketInfo> = buckets
        .iter()
        .zip(&chosen)
        .map(|(b, &n)| BucketInfo {
            key: b.key.clone(),
            label: b.label.clone(),
            first_recorded: b.first,
            available: b.moments.len(),
            chosen: n,
            trips: b.moments.iter().map(|m| m.trip.clone()).collect::<BTreeSet<_>>().into_iter().collect(),
        })
        .collect();
    let used = chosen.iter().filter(|&&n| n > 0).count();
    // As the beat lock will play it (see the length budget above); making
    // the recap re-fits the moments to the song it gets.
    let estimated = if planned.is_empty() {
        0.0
    } else {
        TITLE_SECS + planned.iter().map(|p| p.clip_end_secs - p.clip_start_secs).sum::<f64>()
    };
    let unit = match grouping {
        Grouping::Month => ("month", "months"),
        Grouping::Trip => ("trip", "trips"),
    };

    let mut notes = Vec::new();
    let left_out = bucket_info.iter().filter(|b| b.chosen == 0).count();
    if left_out > 0 {
        notes.push(format!(
            "{} left out to keep it about {}: make it longer to fit every one.",
            plural(left_out, unit.0, unit.1),
            clock(length)
        ));
    }
    if collection.missing_footage > 0 {
        notes.push(format!(
            "{} skipped: their footage is no longer there.",
            plural(collection.missing_footage, "picked moment", "picked moments")
        ));
    }
    if !collection.unreadable.is_empty() {
        notes.push(format!("{} couldn't be read.", plural(collection.unreadable.len(), "saved version", "saved versions")));
    }
    if moments.iter().any(|m| m.dated_by == DatedBy::FileTime) {
        notes.push("Some clips carry no recording date; they're dated by when the file was saved.".into());
    }

    Ok(RecapPlan {
        period: PeriodInfo { from: span.from.to_string(), to: span.to.to_string(), year: span.year },
        title: args.title.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string).unwrap_or_else(|| span.title()),
        subtitle: format!("{} · {}", plural(planned.len(), "moment", "moments"), plural(used, unit.0, unit.1)),
        grouping,
        buckets: bucket_info,
        moments: planned,
        length_secs: length,
        moment_secs: len,
        estimated_secs: round_ms(estimated),
        eligible: used >= MIN_BUCKETS,
        min_buckets: MIN_BUCKETS,
        versions: collection.versions,
        notes,
        shuffle: args.shuffle,
        span,
        exclude,
        transition: args.transition.clone(),
    })
}

/// Windows widened around two moments of one file may meet: split them
/// where their centres' midpoint falls, so no footage plays twice.
fn separate_overlaps(planned: &mut [PlannedMoment]) {
    let mut by_file: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, p) in planned.iter().enumerate() {
        by_file.entry(p.moment.path.as_str()).or_default().push(i);
    }
    let groups: Vec<Vec<usize>> = by_file.into_values().filter(|v| v.len() > 1).collect();
    for mut idx in groups {
        idx.sort_by(|&a, &b| planned[a].clip_start_secs.total_cmp(&planned[b].clip_start_secs));
        for w in idx.windows(2) {
            let (a, b) = (w[0], w[1]);
            if planned[a].clip_end_secs > planned[b].clip_start_secs {
                let centre = |p: &PlannedMoment| (p.moment.start_secs + p.moment.end_secs) / 2.0;
                let cut = round_ms(((centre(&planned[a]) + centre(&planned[b])) / 2.0).clamp(
                    planned[b].clip_start_secs,
                    planned[a].clip_end_secs,
                ));
                planned[a].clip_end_secs = cut;
                planned[b].clip_start_secs = cut;
            }
        }
    }
}

/// Seconds → "1:00".
fn clock(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

// ---------- The config ----------

/// A TOML basic string, escaped.
fn q(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// Seconds as a humantime string the config reads back exactly: "12s 400ms".
fn dur(secs: f64) -> String {
    let ms = (secs.max(0.0) * 1000.0).round() as u64;
    match (ms / 1000, ms % 1000) {
        (0, r) => format!("{r}ms"),
        (s, 0) => format!("{s}s"),
        (s, r) => format!("{s}s {r}ms"),
    }
}

/// The recap as an ordinary project config: its moments as
/// `ranges_only` `[[input.ranges]]` in playing order (captions on each
/// month's first), a title card, the song, beat-locked cuts.
pub(crate) fn config_toml(plan: &RecapPlan, output_path: &str, seed: u64, soundtrack: Option<&ProposedSoundtrack>) -> String {
    let unit = match plan.grouping {
        Grouping::Month => "month",
        Grouping::Trip => "trip",
    };
    let mut t = String::new();
    t.push_str(&format!(
        "# {} — a recap Vibecut made from {} you kept.\n\
         # Each [[input.ranges]] is one moment, in the order they were filmed; a\n\
         # caption names the {unit} it opens. Make the recap again to rebuild this\n\
         # file from your latest versions.\n",
        plan.title,
        plural(plan.versions, "saved version", "saved versions"),
    ));
    t.push_str("schema_version = 1\n\n[project]\n");
    t.push_str(&format!("name            = {}\n", q(&plan.title)));
    t.push_str(&format!("target_duration = \"{}s\"\n", plan.estimated_secs.round().max(1.0)));
    t.push_str(&format!("fps             = 30\nseed            = {seed}\n"));
    t.push_str("fade_in         = \"300ms\"\nfade_out        = \"1500ms\"\n\n");

    t.push_str("[[outputs]]\n");
    t.push_str(&format!("path          = {}\n", q(output_path)));
    t.push_str("resolution    = \"1920x1080\"\nencoder       = \"auto\"\ncrf           = 20\naudio_bitrate = \"192k\"\n\n");

    t.push_str("[input]\n# Exactly these moments, in this order: the picker adds none.\nranges_only = true\n");
    let mut files = BTreeSet::new();
    for p in &plan.moments {
        if files.insert(p.moment.path.as_str()) {
            t.push_str(&format!("\n[[input.videos]]\npath = {}\n", q(&p.moment.path)));
        }
    }
    for p in &plan.moments {
        let date = p.moment.recorded_at.format("%Y-%m-%d");
        t.push_str(&format!("\n# {date} · {}\n[[input.ranges]]\n", p.moment.trip.replace('\n', " ")));
        t.push_str(&format!("file    = {}\n", q(&p.moment.path)));
        t.push_str(&format!("start   = \"{}\"\nend     = \"{}\"\n", dur(p.clip_start_secs), dur(p.clip_end_secs)));
        if let Some(c) = &p.caption {
            t.push_str(&format!("caption = {}\n", q(c)));
        }
    }
    t.push('\n');

    match soundtrack {
        Some(s) => {
            for line in s.note().lines() {
                t.push_str(&format!("# {}\n", line.trim()));
            }
            t.push_str("[soundtrack]\n");
            t.push_str(&format!("file       = {}\n", q(&s.path)));
            t.push_str("beat_align = true\nduck_count = 2\nducking_db = -18.0\nfade_out   = \"2s\"\n");
            t.push_str("# A burst would cut in moments of its own.\nbeat_burst = \"off\"\n\n");
        }
        None => t.push_str("# No soundtrack: the moments keep their own sound.\n\n"),
    }

    t.push_str("[intro]\nduration = \"3s\"\nfade_in  = \"300ms\"\nfade_out = \"300ms\"\nmode     = \"card\"\nbg_color = \"#0a0a0a\"\n\n");
    t.push_str("[[intro.layers]]\nkind     = \"text\"\n");
    t.push_str(&format!("content  = {}\n", q(&plan.title)));
    t.push_str("size     = 104\ncolor    = \"#FFFFFF\"\nposition = \"center\"\ny_offset = -34\n\n");
    t.push_str("[[intro.layers]]\nkind     = \"text\"\n");
    t.push_str(&format!("content  = {}\n", q(&plan.subtitle)));
    t.push_str("size     = 40\ncolor    = \"#FFFFFFB3\"\nposition = \"center\"\ny_offset = 72\n\n");

    t.push_str("# The moments are already chosen: no footage analysis needed.\n[scoring]\n");
    t.push_str("motion_peaks    = 0.0\nsharpness_floor = 0.0\nexposure_floor  = 0.0\n\n");
    if !plan.exclude.is_empty() {
        t.push_str("# Moments you dropped: making the recap again leaves them out too.\n");
        for e in &plan.exclude {
            t.push_str(&format!(
                "[[selection.exclude]]\nfile  = {}\nstart = \"{}\"\nend   = \"{}\"\n\n",
                q(&e.file),
                dur(e.start.as_secs_f64()),
                dur(e.end.as_secs_f64())
            ));
        }
    }
    match plan.transition.as_ref().and_then(transition_section) {
        Some(section) => t.push_str(&section),
        None => {
            t.push_str("[transition]\ndefault  = \"fade\"\n");
            t.push_str(&format!("duration = \"{}\"\n", dur(SEAM_SECS)));
        }
    }
    t
}

/// A kept `[transition]` as its TOML section.
fn transition_section(tr: &Transition) -> Option<String> {
    #[derive(Serialize)]
    struct Section<'a> {
        transition: &'a Transition,
    }
    toml::to_string(&Section { transition: tr }).ok()
}

/// Parse and validate generated TOML: never write a config the pipeline
/// would reject.
fn checked(text: &str) -> Result<Config, OpsError> {
    let cfg: Config =
        toml::from_str(text).map_err(|e| OpsError::internal(format!("the recap wrote invalid TOML: {e}")))?;
    cfg.validate().map_err(|e| OpsError::internal(format!("the recap wrote a config that fails validation: {e:#}")))?;
    Ok(cfg)
}

// ---------- Ops ----------

/// Which years have moments, and whether each has enough for a recap.
#[derive(Debug, Clone, Serialize)]
pub struct YearSummary {
    pub year: i32,
    pub months: usize,
    pub trips: usize,
    pub moments: usize,
    pub eligible: bool,
}

pub(crate) fn years(moments: &[Moment]) -> Vec<YearSummary> {
    let mut by_year: BTreeMap<i32, Vec<&Moment>> = BTreeMap::new();
    for m in moments {
        by_year.entry(m.recorded_at.year()).or_default().push(m);
    }
    by_year
        .into_iter()
        .rev()
        .map(|(year, ms)| {
            let months = ms.iter().map(|m| m.recorded_at.month()).collect::<BTreeSet<_>>().len();
            let trips = ms.iter().map(|m| m.trip.to_lowercase()).collect::<BTreeSet<_>>().len();
            let buckets = if months >= MIN_MONTHS { months } else { trips };
            YearSummary { year, months, trips, moments: ms.len(), eligible: buckets >= MIN_BUCKETS }
        })
        .collect()
}

/// What `GET /api/recap` answers: the plan for the asked (or latest)
/// period, and every year's summary, so a UI can offer "Your 2026 recap".
#[derive(Debug, Clone, Serialize)]
pub struct RecapPreview {
    /// `None` when there are no saved versions at all.
    pub plan: Option<RecapPlan>,
    pub years: Vec<YearSummary>,
    pub versions: usize,
}

/// A recap written as a project config, ready to render.
#[derive(Debug, Clone, Serialize)]
pub struct RecapResult {
    pub config_path: String,
    pub config_toml: String,
    pub output_path: String,
    pub plan: RecapPlan,
    /// The song the config uses; `None` without music.
    pub soundtrack: Option<ProposedSoundtrack>,
    pub auto_pick: Option<AutoPick>,
    pub music_note: Option<String>,
    /// The vibe whose music profile chose the song.
    pub vibe: &'static str,
    pub vibe_name: &'static str,
    /// What the recap is, in a sentence.
    pub rationale: String,
}

impl RecapPlan {
    /// The crossfade into each clip: the config's `[transition]` duration
    /// (none with straight cuts).
    fn seam(&self) -> f64 {
        match &self.transition {
            Some(t) if t.hard_cut() => 0.0,
            Some(t) => t.duration.as_secs_f64(),
            None => SEAM_SECS,
        }
    }

    /// "The best moments of each month of 2026, in the order they were filmed."
    fn rationale(&self) -> String {
        let of = match self.period.year {
            Some(y) => y.to_string(),
            None => self.title.clone(),
        };
        let each = match self.grouping {
            Grouping::Month => "each month",
            Grouping::Trip => "each trip",
        };
        format!("The best moments of {each} of {of}, in the order they were filmed, cut to the beat.")
    }
}

pub(crate) fn preview_in(root: &Path, args: &RecapArgs) -> Result<RecapPreview, OpsError> {
    let collection = collect(root, &footage_on_disk);
    let plan = match plan(&collection, args) {
        Ok(p) => Some(p),
        Err(OpsError::NotFound { .. }) if collection.moments.is_empty() => None,
        Err(e) => return Err(e),
    };
    Ok(RecapPreview { plan, years: years(&collection.moments), versions: collection.versions })
}

/// A recap checked and planned, waiting for its song.
pub(crate) struct Prepared {
    vibe: &'static vibes::Vibe,
    /// The caller's song, checked: `None` = let Vibecut pick, `Some(None)` = no music.
    song: Option<Option<String>>,
    plan: RecapPlan,
    seed: u64,
}

/// Check the request and plan the recap: everything that can refuse it
/// happens here, before the music library is read.
pub(crate) fn prepare_in(root: &Path, args: &RecapArgs) -> Result<Prepared, OpsError> {
    let vibe_key = args.vibe.as_deref().map(str::trim).filter(|v| !v.is_empty()).unwrap_or(DEFAULT_VIBE);
    let vibe = vibes::get(vibe_key).ok_or_else(|| {
        OpsError::invalid_with_hint(format!("unknown vibe: {vibe_key:?}"), format!("available: {}", vibes::names().join(", ")))
    })?;
    let song = match propose::music_request(args.soundtrack.as_deref()) {
        propose::MusicRequest::Auto => None,
        propose::MusicRequest::Off => Some(None),
        propose::MusicRequest::File(p) => {
            Some(Some(crate::music::check_soundtrack_file(p)?.to_string_lossy().into_owned()))
        }
    };

    let collection = collect(root, &footage_on_disk);
    let plan = plan(&collection, args)?;
    if !plan.eligible {
        let used = plan.buckets.iter().filter(|b| b.chosen > 0).count();
        let unit = if plan.grouping == Grouping::Month { ("month", "months") } else { ("trip", "trips") };
        return Err(OpsError::invalid_with_hint(
            format!(
                "not enough for a recap of {}: moments from {}",
                plan.title,
                plural(used, unit.0, unit.1)
            ),
            format!(
                "a recap needs saved versions from at least {MIN_BUCKETS} months or {MIN_BUCKETS} trips; every finished render keeps one"
            ),
        ));
    }
    Ok(Prepared { vibe, song, plan, seed: args.seed })
}

/// Pick the song from `tracks` (the music library) and write the recap's
/// config under `root/recaps`.
pub(crate) fn write_in(root: &Path, prepared: Prepared, tracks: &[TrackInfo]) -> Result<RecapResult, OpsError> {
    let Prepared { vibe, song, mut plan, seed } = prepared;
    let request = match &song {
        None => propose::MusicRequest::Auto,
        Some(None) => propose::MusicRequest::Off,
        Some(Some(p)) => propose::MusicRequest::File(p),
    };
    let auto_pick = crate::music::auto_pick(tracks, vibe, plan.estimated_secs as f32, seed);
    let (soundtrack, music_note) = propose::decide_soundtrack(&request, auto_pick.as_ref(), tracks);
    // Whole beats of the song, when the library has analysed it.
    let beat = soundtrack
        .as_ref()
        .and_then(|s| tracks.iter().find(|t| t.path == s.path))
        .map(|t| t.analysis.beat_interval_secs as f64)
        .filter(|b| *b > 0.05);
    fit_to_song(&mut plan, soundtrack.is_some(), beat);

    let dir = root.join(RECAP_DIR);
    let name = format!("recap_{}", slugify(&plan.span.slug()));
    let output_path = dir.join("out").join(format!("{name}.mp4")).to_string_lossy().into_owned();
    let config_path = dir.join(format!("{name}.toml"));
    let toml = config_toml(&plan, &output_path, seed, soundtrack.as_ref());
    checked(&toml)?;
    std::fs::create_dir_all(dir.join("out"))
        .map_err(|e| OpsError::internal(format!("could not create {}: {e}", dir.display())))?;
    std::fs::write(&config_path, &toml)
        .map_err(|e| OpsError::internal(format!("could not write {}: {e}", config_path.display())))?;

    Ok(RecapResult {
        config_path: config_path.to_string_lossy().into_owned(),
        config_toml: toml,
        output_path,
        rationale: plan.rationale(),
        plan,
        soundtrack,
        auto_pick,
        music_note,
        vibe: vibe.key,
        vibe_name: vibe.name,
    })
}

/// What a recap of `args` would play — months or trips, moments, length —
/// without writing anything; plus every year's summary.
pub async fn preview(args: RecapArgs) -> Result<RecapPreview, OpsError> {
    tokio::task::spawn_blocking(move || preview_in(Path::new(PROJECTS_ROOT), &args))
        .await
        .map_err(|e| OpsError::internal(format!("recap preview panicked: {e}")))?
}

/// [`prepare_in`] then [`write_in`], for callers that have the library.
#[cfg(test)]
pub(crate) fn make_in(root: &Path, args: &RecapArgs, tracks: &[TrackInfo]) -> Result<RecapResult, OpsError> {
    write_in(root, prepare_in(root, args)?, tracks)
}

/// Plan the recap and write it as a project config in `/projects/recaps`
/// (the same period always writes the same file, so making it again
/// replaces it and its renders become its versions). Render it like any
/// config: `render::submit_async` with the returned `config_path`.
pub async fn make(args: RecapArgs) -> Result<RecapResult, OpsError> {
    let root = Path::new(PROJECTS_ROOT);
    let prepared = tokio::task::spawn_blocking(move || prepare_in(root, &args))
        .await
        .map_err(|e| OpsError::internal(format!("recap panicked: {e}")))??;
    // Only a recap that will be written reads (and maybe analyses) the library.
    let tracks = propose::library_tracks().await;
    tokio::task::spawn_blocking(move || write_in(root, prepared, &tracks))
        .await
        .map_err(|e| OpsError::internal(format!("recap panicked: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap()
    }

    fn moment(path: &str, start: f64, end: f64, score: f32, when: DateTime<Utc>, trip: &str) -> Moment {
        Moment {
            path: path.into(),
            start_secs: start,
            end_secs: end,
            file_secs: Some(60.0),
            recorded_at: when,
            dated_by: DatedBy::Footage,
            score,
            versions: 1,
            trip: trip.into(),
        }
    }

    fn manifest(json: serde_json::Value) -> KeptManifest {
        serde_json::from_value(json).unwrap()
    }

    fn there(_: &str) -> Option<FootageFile> {
        Some(FootageFile { modified: Some(at(2026, 11, 8)) })
    }

    // ---- collection ----

    #[test]
    fn each_version_gives_its_best_segments_by_score() {
        let m = manifest(serde_json::json!({
            "project_name": "Alps Ski Day · 2026-03-14",
            "sources": [{ "chapters": [
                { "path": "/f/alps/A.MP4", "duration_secs": 120.0, "creation_time": "2026-03-14T09:00:00.000000Z" },
            ]}],
            "segments": [
                { "chapter_path": "/f/alps/A.MP4", "in_chapter_start_secs": 0.0, "duration_secs": 3.0, "score": 0.2 },
                { "chapter_path": "/f/alps/A.MP4", "in_chapter_start_secs": 10.0, "duration_secs": 3.0, "score": 0.8 },
                { "chapter_path": "/f/alps/A.MP4", "in_chapter_start_secs": 20.0, "duration_secs": 3.0, "score": 0.4 },
                { "chapter_path": "/f/alps/A.MP4", "in_chapter_start_secs": 30.0, "duration_secs": 3.0, "score": 0.6 },
                { "chapter_path": "/f/alps/A.MP4", "in_chapter_start_secs": 40.0, "duration_secs": 3.0, "score": 0.3 },
                { "chapter_path": "/f/alps/photo.jpg", "in_chapter_start_secs": 0.0, "duration_secs": 4.0, "score": 0.9 },
                { "chapter_path": "/f/alps/A.MP4", "in_chapter_start_secs": 50.0, "duration_secs": 0.0, "score": 0.9 },
            ],
        }));
        let mut missing = 0;
        let got = version_moments(&m, "fallback", &there, &mut missing);
        let starts: Vec<f64> = got.iter().map(|m| m.start_secs).collect();
        // The top four; the photo and the empty segment never count.
        assert_eq!(starts, vec![10.0, 30.0, 20.0, 40.0]);
        assert_eq!(got[0].score, 1.0, "scored against the version's best");
        assert!((got[1].score - 0.75).abs() < 1e-6);
        assert_eq!(got[0].trip, "Alps Ski Day");
        assert_eq!(got[0].file_secs, Some(120.0));
        assert_eq!(got[0].recorded_at, Utc.with_ymd_and_hms(2026, 3, 14, 9, 0, 10).unwrap());
        assert_eq!(missing, 0);
    }

    /// A folded beat-burst is a render intermediate with a stand-in score
    /// of 1: it is neither a moment nor the version's best.
    #[test]
    fn a_folded_burst_is_not_footage() {
        let m = manifest(serde_json::json!({
            "sources": [{ "chapters": [{ "path": "/f/trip/GX010105.MP4", "duration_secs": 30.0 }] }],
            "segments": [
                { "source_id": "0105", "chapter_path": "/f/trip/GX010105.MP4", "in_chapter_start_secs": 9.9, "duration_secs": 2.9, "score": 0.5 },
                { "source_id": "burst:0105", "chapter_path": "/p/out/.vibecut_trip/beat_burst.mp4", "in_chapter_start_secs": 0.0, "duration_secs": 5.7, "score": 1.0 },
                // Not one of this version's footage files.
                { "source_id": "0999", "chapter_path": "/p/out/.vibecut_trip/seg_001.mp4", "in_chapter_start_secs": 0.0, "duration_secs": 3.0, "score": 0.9 },
                { "source_id": "0105", "chapter_path": "/f/trip/GX010105.MP4", "in_chapter_start_secs": 20.4, "duration_secs": 2.9, "score": 0.25 },
            ],
        }));
        let got = version_moments(&m, "trip", &there, &mut 0);
        let summary: Vec<(f64, f32)> = got.iter().map(|m| (m.start_secs, m.score)).collect();
        assert_eq!(summary, vec![(9.9, 1.0), (20.4, 0.5)]);
    }

    #[test]
    fn gone_footage_is_skipped_and_counted() {
        let m = manifest(serde_json::json!({
            "segments": [
                { "chapter_path": "/f/gone.MP4", "in_chapter_start_secs": 0.0, "duration_secs": 3.0, "score": 0.9 },
                { "chapter_path": "/f/here.MP4", "in_chapter_start_secs": 0.0, "duration_secs": 3.0, "score": 0.5 },
            ],
        }));
        let lookup = |p: &str| (p != "/f/gone.MP4").then_some(FootageFile { modified: Some(at(2026, 5, 1)) });
        let mut missing = 0;
        let got = version_moments(&m, "coastline_bike_ride", &lookup, &mut missing);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, "/f/here.MP4");
        assert_eq!(got[0].trip, "coastline_bike_ride", "no project name: the output's name");
        assert_eq!(missing, 1);
    }

    #[test]
    fn manifests_without_scores_still_count() {
        let m = manifest(serde_json::json!({
            "segments": [
                { "chapter_path": "/f/a.MP4", "in_chapter_start_secs": 0.0, "duration_secs": 3.0 },
                { "chapter_path": "/f/a.MP4", "in_chapter_start_secs": 5.0, "duration_secs": 3.0 },
            ],
        }));
        let got = version_moments(&m, "x", &there, &mut 0);
        assert_eq!(got.iter().map(|m| (m.start_secs, m.score)).collect::<Vec<_>>(), vec![(0.0, 0.5), (5.0, 0.5)]);
    }

    #[test]
    fn the_same_stretch_of_footage_is_one_moment_across_versions() {
        let when = at(2026, 3, 14);
        let got = dedup(vec![
            moment("/f/a.MP4", 10.0, 14.0, 0.7, when, "Alps"),
            // Another version of the project, a little later in the same take.
            moment("/f/a.MP4", 12.0, 15.0, 0.9, when, "Alps"),
            // A third, overlapping only the first.
            moment("/f/a.MP4", 8.0, 10.5, 0.4, when, "Alps"),
            // Touching is not overlapping.
            moment("/f/a.MP4", 15.0, 18.0, 0.5, when, "Alps"),
            // Same range, other file.
            moment("/f/b.MP4", 10.0, 14.0, 0.6, when, "Alps"),
        ]);
        let summary: Vec<(&str, f64, u32)> = got.iter().map(|m| (m.path.as_str(), m.start_secs, m.versions)).collect();
        assert_eq!(summary, vec![("/f/a.MP4", 12.0, 2), ("/f/b.MP4", 10.0, 1), ("/f/a.MP4", 15.0, 1), ("/f/a.MP4", 8.0, 1)]);
        // Picked again counts in its favour.
        assert!(got[0].rank() > got[0].score);
    }

    #[test]
    fn moments_are_dated_by_the_footage_not_the_render() {
        let saved = Some(at(2026, 11, 8));
        // The camera's clock, plus how far into the file.
        let (t, by) = date_moment(Some("2026-03-14T09:00:00.000000Z"), 75.5, saved).unwrap();
        assert_eq!((t, by), (Utc.with_ymd_and_hms(2026, 3, 14, 9, 1, 15).unwrap() + chrono::Duration::milliseconds(500), DatedBy::Footage));
        assert_eq!(date_moment(Some("2026-06-01T10:00:00+02:00"), 0.0, saved).unwrap().0, Utc.with_ymd_and_hms(2026, 6, 1, 8, 0, 0).unwrap());
        assert_eq!(date_moment(Some("2026-06-01 10:00:00"), 0.0, saved).unwrap().1, DatedBy::Footage);
        // No date, an unreadable one, or a never-set clock: the file's time.
        for bad in [None, Some(""), Some("yesterday"), Some("1904-01-01T00:00:00Z"), Some("1970-01-01T00:00:00Z")] {
            assert_eq!(date_moment(bad, 12.0, saved), Some((at(2026, 11, 8), DatedBy::FileTime)), "{bad:?}");
        }
        assert_eq!(date_moment(None, 0.0, None), None);
    }

    #[test]
    fn collect_reads_every_versions_folder_but_the_recaps() {
        let root = std::env::temp_dir().join(format!("vibecut_recap_collect_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let footage = root.join("footage");
        std::fs::create_dir_all(&footage).unwrap();
        let clip = footage.join("GX010001.MP4");
        std::fs::write(&clip, b"x").unwrap();
        let saved = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_772_000_000); // 2026-02-25
        std::fs::File::options().write(true).open(&clip).unwrap().set_modified(saved).unwrap();
        let clip = clip.to_string_lossy().into_owned();
        let write = |rel: &str, start: f64| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            let m = serde_json::json!({
                "project_name": "Snow day · 2026-02-25",
                "segments": [{ "chapter_path": clip.as_str(), "in_chapter_start_secs": start, "duration_secs": 2.0, "score": 0.5 }],
            });
            std::fs::write(p, m.to_string()).unwrap();
        };
        write("out/versions/snow__a.manifest.json", 1.0);
        write("out/versions/snow__b.manifest.json", 1.5); // the same moment again
        write("trips/2026/out/versions/x__c.manifest.json", 30.0);
        write("out/snow.manifest.json", 50.0); // the live output, not a kept version
        write("recaps/out/versions/recap_2026__d.manifest.json", 70.0);
        write(".vibecut_x/versions/y__e.manifest.json", 90.0);
        std::fs::write(root.join("out/versions/broken__f.manifest.json"), "{ not json").unwrap();

        let c = collect(&root, &footage_on_disk);
        assert_eq!(c.versions, 3);
        assert_eq!(c.unreadable.len(), 1);
        let got: Vec<(f64, u32, DatedBy)> = {
            let mut v: Vec<_> = c.moments.iter().map(|m| (m.start_secs, m.versions, m.dated_by)).collect();
            v.sort_by(|a, b| a.0.total_cmp(&b.0));
            v
        };
        assert_eq!(got, vec![(1.0, 2, DatedBy::FileTime), (30.0, 1, DatedBy::FileTime)]);
        assert_eq!(c.moments[0].recorded_at.date_naive(), NaiveDate::from_ymd_opt(2026, 2, 25).unwrap());
        assert_eq!(c.moments[0].trip, "Snow day");
        let _ = std::fs::remove_dir_all(&root);
    }

    // ---- period and buckets ----

    #[test]
    fn periods_read_years_months_and_days() {
        let ms = [moment("/a", 0.0, 1.0, 1.0, at(2025, 12, 30), "t"), moment("/b", 0.0, 1.0, 1.0, at(2026, 4, 2), "t")];
        let d = |y, m, dd| NaiveDate::from_ymd_opt(y, m, dd).unwrap();
        let s = resolve_period(Some(2025), None, None, &ms).unwrap();
        assert_eq!((s.from, s.to, s.year), (d(2025, 1, 1), d(2025, 12, 31), Some(2025)));
        // Nothing asked: the latest year with moments.
        assert_eq!(resolve_period(None, None, None, &ms).unwrap().year, Some(2026));
        let s = resolve_period(None, Some("2026-02"), Some("2026-06"), &ms).unwrap();
        assert_eq!((s.from, s.to, s.year), (d(2026, 2, 1), d(2026, 6, 30), None));
        assert_eq!(s.title(), "February – June 2026");
        assert_eq!(s.slug(), "2026-02_2026-06");
        let s = resolve_period(None, Some("2025-11-15"), None, &ms).unwrap();
        assert_eq!((s.from, s.to), (d(2025, 11, 15), d(2026, 4, 2)));
        assert_eq!(s.title(), "Nov 2025 – Apr 2026");
        assert_eq!(resolve_period(None, None, Some("2024-02"), &[]).unwrap().to, d(2024, 2, 29));
        assert_eq!(resolve_period(None, Some("2026-01"), Some("2026-12"), &ms).unwrap().year, Some(2026));
        assert_eq!(resolve_period(None, Some("2026-03"), Some("2026-03"), &ms).unwrap().title(), "March 2026");
        assert!(resolve_period(Some(2026), Some("2026-01"), None, &ms).is_err());
        assert!(resolve_period(None, Some("2026-13"), None, &ms).is_err());
        assert!(resolve_period(None, Some("March"), None, &ms).is_err());
        assert!(resolve_period(None, Some("2026-06"), Some("2026-02"), &ms).is_err());
        assert!(matches!(resolve_period(None, None, None, &[]), Err(OpsError::NotFound { .. })));
    }

    #[test]
    fn moments_bucket_by_month_or_by_trip() {
        let ms = vec![
            moment("/a", 0.0, 3.0, 0.5, at(2026, 6, 3), "Coast"),
            moment("/b", 0.0, 3.0, 0.9, at(2026, 3, 14), "Alps"),
            moment("/c", 0.0, 3.0, 0.4, at(2026, 3, 15), "Alps"),
            moment("/d", 0.0, 3.0, 0.7, at(2026, 9, 20), "Lake"),
        ];
        assert_eq!(grouping_for(&ms), Grouping::Month);
        let b = bucket(&ms, Grouping::Month, false);
        let labels: Vec<(&str, &str, usize)> = b.iter().map(|b| (b.key.as_str(), b.label.as_str(), b.moments.len())).collect();
        assert_eq!(labels, vec![("2026-03", "March", 2), ("2026-06", "June", 1), ("2026-09", "September", 1)]);
        assert_eq!(b[0].moments[0].path, "/b", "best first");
        assert_eq!(bucket(&ms, Grouping::Month, true)[0].label, "March 2026");

        // Two months only: by trip, in the order the trips happened.
        let short = vec![
            moment("/a", 0.0, 3.0, 0.5, at(2026, 6, 20), "Coast"),
            moment("/b", 0.0, 3.0, 0.9, at(2026, 6, 2), "Alps"),
            moment("/c", 0.0, 3.0, 0.4, at(2026, 7, 1), "lake"),
            moment("/d", 0.0, 3.0, 0.4, at(2026, 7, 2), "Lake"),
        ];
        assert_eq!(grouping_for(&short), Grouping::Trip);
        let b = bucket(&short, Grouping::Trip, false);
        assert_eq!(b.iter().map(|b| (b.label.as_str(), b.moments.len())).collect::<Vec<_>>(), vec![("Alps", 1), ("Coast", 1), ("lake", 2)]);
    }

    // ---- length budget ----

    #[test]
    fn the_length_budget_sizes_the_moments() {
        // 60 s: 57 s after the title, ~4 s on screen each.
        assert_eq!(moment_counts(60.0), (14, 22));
        assert_eq!(moment_counts(90.0), (22, 34));
        // Enough moments: they land the asked length exactly.
        for (len, n) in [(60.0, 14), (90.0, 22), (45.0, 11)] {
            let total = TITLE_SECS + n as f64 * moment_secs(len, n);
            assert!((total - len).abs() < 1e-9, "{len}: {total}");
        }
        // Few moments: each runs long, but never past the bound.
        assert_eq!(moment_secs(60.0, 3), MOMENT_MAX_SECS);
        assert_eq!(moment_secs(20.0, 40), MOMENT_MIN_SECS);
    }

    fn buckets_with(sizes: &[(usize, f32)]) -> Vec<Bucket> {
        sizes
            .iter()
            .enumerate()
            .map(|(b, &(n, best))| Bucket {
                key: format!("2026-{:02}", b + 1),
                label: format!("m{b}"),
                first: at(2026, b as u32 + 1, 1),
                moments: (0..n).map(|i| moment(&format!("/{b}"), i as f64 * 10.0, i as f64 * 10.0 + 3.0, best - i as f32 * 0.1, at(2026, b as u32 + 1, 1), "t")).collect(),
            })
            .collect()
    }

    #[test]
    fn every_month_gets_its_best_then_the_rest_goes_round() {
        let b = buckets_with(&[(3, 0.9), (1, 0.5), (4, 0.8)]);
        let picks = choose(&b, 6, 25);
        assert_eq!(picks, vec![(0, 0), (1, 0), (2, 0), (0, 1), (2, 1), (0, 2)]);
        // Runs out before the ideal count: everything there is.
        assert_eq!(choose(&b, 20, 25).len(), 8);
        // More months than fit: the strongest months, still in order.
        let b = buckets_with(&[(1, 0.3), (1, 0.9), (1, 0.5), (1, 0.8)]);
        assert_eq!(choose(&b, 2, 2), vec![(1, 0), (3, 0)]);
        assert!(choose(&[], 5, 5).is_empty());
    }

    #[test]
    fn windows_stay_inside_their_file() {
        let mut m = moment("/a", 10.0, 12.0, 1.0, at(2026, 1, 1), "t");
        assert_eq!(window(&m, 4.0), (9.0, 13.0));
        m.start_secs = 0.5;
        m.end_secs = 1.5;
        assert_eq!(window(&m, 4.0), (0.0, 4.0));
        m.start_secs = 58.0;
        m.end_secs = 60.0;
        assert_eq!(window(&m, 4.0), (56.0, 60.0));
        // A file shorter than the moment: all of it.
        m.file_secs = Some(2.5);
        m.start_secs = 0.0;
        m.end_secs = 2.5;
        assert_eq!(window(&m, 4.0), (0.0, 2.5));
    }

    fn collection(moments: Vec<Moment>) -> Collection {
        Collection { moments, versions: 7, ..Collection::default() }
    }

    fn year_args(len: f64) -> RecapArgs {
        RecapArgs { year: Some(2026), length_secs: Some(len), seed: 42, ..RecapArgs::default() }
    }

    #[test]
    fn a_plan_plays_each_months_moments_in_order_with_its_caption() {
        let ms = vec![
            moment("/f/coast.MP4", 20.0, 23.0, 0.6, at(2026, 6, 3), "Coast"),
            moment("/f/alps.MP4", 30.0, 33.0, 1.0, at(2026, 3, 14), "Alps"),
            moment("/f/alps.MP4", 5.0, 8.0, 0.8, at(2026, 3, 14) - chrono::Duration::minutes(5), "Alps"),
            moment("/f/lake.MP4", 0.0, 3.0, 0.9, at(2026, 9, 20), "Lake"),
            moment("/f/old.MP4", 0.0, 3.0, 1.0, at(2025, 9, 20), "Old"),
        ];
        let p = plan(&collection(ms), &year_args(30.0)).unwrap();
        assert_eq!(p.grouping, Grouping::Month);
        assert_eq!(p.title, "2026 in moments");
        assert!(p.eligible);
        // 30 s holds 7 ideal moments; there are 4 in 2026 (2025's is out).
        let order: Vec<(&str, f64, Option<&str>)> =
            p.moments.iter().map(|m| (m.moment.path.as_str(), m.moment.start_secs, m.caption.as_deref())).collect();
        assert_eq!(
            order,
            vec![
                ("/f/alps.MP4", 5.0, Some("March")),
                ("/f/alps.MP4", 30.0, None),
                ("/f/coast.MP4", 20.0, Some("June")),
                ("/f/lake.MP4", 0.0, Some("September")),
            ]
        );
        assert_eq!(p.subtitle, "4 moments · 3 months");
        assert_eq!(p.moment_secs, MOMENT_MAX_SECS);
        assert!(p.estimated_secs < p.length_secs, "few moments: shorter than asked");
        assert_eq!(p.buckets.iter().map(|b| (b.label.as_str(), b.chosen)).collect::<Vec<_>>(), vec![("March", 2), ("June", 1), ("September", 1)]);
    }

    #[test]
    fn widened_windows_on_one_file_never_replay_footage() {
        let ms = vec![
            moment("/f/a.MP4", 10.0, 12.0, 1.0, at(2026, 3, 1), "A"),
            moment("/f/a.MP4", 13.0, 15.0, 0.9, at(2026, 3, 1) + chrono::Duration::seconds(3), "A"),
            moment("/f/b.MP4", 0.0, 3.0, 0.9, at(2026, 4, 1), "B"),
            moment("/f/c.MP4", 0.0, 3.0, 0.9, at(2026, 5, 1), "C"),
        ];
        let p = plan(&collection(ms), &year_args(30.0)).unwrap();
        let a: Vec<(f64, f64)> =
            p.moments.iter().filter(|m| m.moment.path == "/f/a.MP4").map(|m| (m.clip_start_secs, m.clip_end_secs)).collect();
        assert_eq!(a.len(), 2);
        assert!(a[0].1 <= a[1].0, "{a:?}");
        assert_eq!(a[0].1, 12.5, "split halfway between the two moments");
    }

    #[test]
    fn a_long_recap_lands_near_its_length() {
        // Six months, plenty of moments each.
        let ms: Vec<Moment> = (1..=6)
            .flat_map(|mo| {
                (0..6).map(move |i| moment(&format!("/f/{mo}.MP4"), i as f64 * 8.0, i as f64 * 8.0 + 3.0, 0.5 + i as f32 * 0.05, at(2026, mo, 1 + i), "T"))
            })
            .collect();
        for len in [45.0, 60.0, 90.0] {
            let p = plan(&collection(ms.clone()), &year_args(len)).unwrap();
            assert!((p.estimated_secs - len).abs() < 0.05, "{len}: {}", p.estimated_secs);
            assert!(p.buckets.iter().all(|b| b.chosen >= 1));
            let spread = p.buckets.iter().map(|b| b.chosen).max().unwrap() - p.buckets.iter().map(|b| b.chosen).min().unwrap();
            assert!(spread <= 1, "shared out evenly");
            let mut sorted = p.moments.clone();
            sorted.sort_by(|a, b| a.moment.recorded_at.cmp(&b.moment.recorded_at));
            assert_eq!(sorted.iter().map(|m| m.clip_start_secs).collect::<Vec<_>>(), p.moments.iter().map(|m| m.clip_start_secs).collect::<Vec<_>>());
        }
        // Length is kept within bounds.
        assert_eq!(plan(&collection(ms.clone()), &year_args(5.0)).unwrap().length_secs, MIN_LENGTH_SECS);
        assert_eq!(plan(&collection(ms), &year_args(9999.0)).unwrap().length_secs, MAX_LENGTH_SECS);
    }

    /// The beat lock snaps every cut to the nearest beat: spans planned in
    /// whole beats of the song keep the recap at its length (an off-beat
    /// 4.1 s moment at 112 BPM came out 4.6 s, a 45 s recap 49.5 s).
    #[test]
    fn moments_fit_whole_beats_of_the_song() {
        let beat = 60.0 / 112.0;
        let spans = beat_spans(57.0, 14, beat);
        let beats: Vec<f64> = spans.iter().map(|s| s / beat).collect();
        assert!(beats.iter().all(|b| (b - b.round()).abs() < 1e-9), "{beats:?}");
        assert_eq!(beats.iter().sum::<f64>().round(), 106.0, "57 s is 106.4 beats");
        let (lo, hi) = (beats.iter().cloned().fold(f64::MAX, f64::min), beats.iter().cloned().fold(0.0, f64::max));
        assert!(hi - lo <= 1.0, "spread evenly: {beats:?}");
        // Bounds hold: never shorter or longer than a moment may be.
        assert_eq!(beat_spans(30.0, 2, 0.5), vec![6.0, 6.0]);
        assert_eq!(beat_spans(1.0, 2, 0.5), vec![2.5, 2.5]);

        let ms: Vec<Moment> = (1..=6)
            .flat_map(|mo| {
                (0..6).map(move |i| moment(&format!("/f/{mo}.MP4"), i as f64 * 8.0, i as f64 * 8.0 + 3.0, 0.5 + i as f32 * 0.05, at(2026, mo, 1 + i), "T"))
            })
            .collect();
        let mut p = plan(&collection(ms.clone()), &year_args(60.0)).unwrap();
        fit_to_song(&mut p, true, Some(beat));
        for m in &p.moments {
            let b = (m.clip_end_secs - m.clip_start_secs) / beat;
            assert!((b - b.round()).abs() < 0.01, "{b} beats");
        }
        assert!((p.estimated_secs - 60.0).abs() <= beat / 2.0, "{}", p.estimated_secs);

        // No music: each range carries its incoming crossfade, the length holds.
        let mut p = plan(&collection(ms), &year_args(60.0)).unwrap();
        let span = p.moment_secs;
        fit_to_song(&mut p, false, None);
        assert!(p.moments.iter().all(|m| ((m.clip_end_secs - m.clip_start_secs) - (span + SEAM_SECS)).abs() < 1e-6));
        assert!((p.estimated_secs - 60.0).abs() < 0.05, "{}", p.estimated_secs);
    }

    #[test]
    fn a_few_months_recap_by_trip() {
        let ms = vec![
            moment("/f/a.MP4", 0.0, 3.0, 1.0, at(2026, 11, 8), "Alps Ski Day"),
            moment("/f/b.MP4", 0.0, 3.0, 1.0, at(2026, 11, 8) + chrono::Duration::hours(2), "Coastline"),
            moment("/f/c.MP4", 0.0, 3.0, 1.0, at(2026, 11, 7), "trip"),
        ];
        let p = plan(&collection(ms.clone()), &year_args(30.0)).unwrap();
        assert_eq!(p.grouping, Grouping::Trip);
        assert!(p.eligible);
        assert_eq!(p.subtitle, "3 moments · 3 trips");
        let caps: Vec<&str> = p.moments.iter().filter_map(|m| m.caption.as_deref()).collect();
        assert_eq!(caps, vec!["trip", "Alps Ski Day", "Coastline"]);
        // Two trips are not enough.
        let p = plan(&collection(ms[..2].to_vec()), &year_args(30.0)).unwrap();
        assert!(!p.eligible);
    }

    // ---- the config ----

    fn write_footage(root: &Path, rel: &str) -> String {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"x").unwrap();
        p.to_string_lossy().into_owned()
    }

    fn keep_version(root: &Path, name: &str, project: &str, path: &str, when: &str, starts: &[f64]) {
        let p = root.join("out/versions").join(format!("{name}.manifest.json"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let segs: Vec<serde_json::Value> = starts
            .iter()
            .enumerate()
            .map(|(i, s)| serde_json::json!({ "chapter_path": path, "in_chapter_start_secs": s, "duration_secs": 3.0, "score": 1.0 - i as f64 * 0.1 }))
            .collect();
        let m = serde_json::json!({
            "project_name": project,
            "sources": [{ "chapters": [{ "path": path, "duration_secs": 40.0, "creation_time": when }] }],
            "segments": segs,
        });
        std::fs::write(p, m.to_string()).unwrap();
    }

    #[test]
    fn the_recap_is_an_ordinary_config_the_pipeline_loads() {
        let root = std::env::temp_dir().join(format!("vibecut_recap_make_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // Same file name in two folders: the ranges must name them apart.
        let alps = write_footage(&root, "footage/Alps Ski Day's/GX010201.MP4");
        let coast = write_footage(&root, "footage/Coast/GX010201.MP4");
        let lake = write_footage(&root, "footage/Lake/GX010101.MP4");
        keep_version(&root, "alps__1", "Alps \"Ski\" Day · 2026-03-14", &alps, "2026-03-14T09:00:00Z", &[4.0, 20.0]);
        keep_version(&root, "alps__2", "Alps \"Ski\" Day · 2026-03-14", &alps, "2026-03-14T09:00:00Z", &[21.0]);
        keep_version(&root, "coast__1", "Coast · 2026-06-02", &coast, "2026-06-02T17:30:00Z", &[10.0]);
        keep_version(&root, "lake__1", "Lake", &lake, "2026-09-20T08:00:00Z", &[0.0, 30.0]);

        let r = make_in(&root, &RecapArgs { year: Some(2026), length_secs: Some(45.0), seed: 7, ..RecapArgs::default() }, &[]).unwrap();
        assert!(r.config_path.ends_with("recap_2026.toml"), "{}", r.config_path);
        assert!(r.output_path.ends_with("recap_2026.mp4") && r.output_path.contains(RECAP_DIR));
        assert!(r.soundtrack.is_none() && r.music_note.as_deref().unwrap().contains("library is empty"));
        assert_eq!(r.vibe, DEFAULT_VIBE);

        // The project's own loader takes it as written.
        let cfg = Config::load(camino::Utf8Path::new(&r.config_path)).unwrap();
        assert!(cfg.input.ranges_only);
        assert!(cfg.input.folder.is_none());
        assert_eq!(cfg.input.videos.len(), 3);
        assert_eq!(cfg.project.name, "2026 in moments");
        assert_eq!(cfg.project.seed, Some(7));
        assert_eq!(cfg.outputs[0].path.as_str(), r.output_path);
        // March (two versions picked 20–23 / 21–24: one moment), June, September.
        let ranges: Vec<(&str, Option<&str>)> =
            cfg.input.ranges.iter().map(|x| (x.file.as_str(), x.caption.as_deref())).collect();
        assert_eq!(
            ranges,
            vec![
                (alps.as_str(), Some("March")),
                (alps.as_str(), None),
                (coast.as_str(), Some("June")),
                (lake.as_str(), Some("September")),
                (lake.as_str(), None),
            ]
        );
        for (x, p) in cfg.input.ranges.iter().zip(&r.plan.moments) {
            assert!((x.start.as_secs_f64() - p.clip_start_secs).abs() < 1e-9);
            assert!((x.end.as_secs_f64() - p.clip_end_secs).abs() < 1e-9);
            assert!(x.end.as_secs_f64() <= 40.0);
        }
        assert!(r.plan.moments[1].moment.versions == 2);
        // Each range names exactly its own file, though the names match.
        assert!(cfg.input.ranges[2].matches_file(camino::Utf8Path::new(&coast)));
        assert!(!cfg.input.ranges[2].matches_file(camino::Utf8Path::new(&alps)));
        let intro = cfg.intro.as_ref().unwrap();
        let texts: Vec<String> = intro
            .layers
            .iter()
            .filter_map(|l| match l {
                vibecut::project_config::CardLayer::Text(t) => Some(t.content.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["2026 in moments".to_string(), "5 moments · 3 months".to_string()]);
        assert!(cfg.soundtrack.is_none());
        assert_eq!(cfg.scoring.motion_peaks, 0.0);

        // A song: beat-locked, no burst.
        let song = ProposedSoundtrack { path: "/projects/music/a \"b\".mp3".into(), title: "A b".into(), picked_by: "vibecut", reason: Some("It fits.".into()) };
        let text = config_toml(&r.plan, &r.output_path, 7, Some(&song));
        let cfg = checked(&text).unwrap();
        let snd = cfg.soundtrack.unwrap();
        assert!(snd.beat_align);
        assert_eq!(snd.beat_burst, vibecut::project_config::BeatBurstMode::Off);
        assert_eq!(snd.files[0].as_str(), "/projects/music/a \"b\".mp3");
        assert!(text.contains("# Picked for you: A b. It fits."), "{text}");

        // Not enough months or trips: refused, nothing written.
        let err = make_in(&root, &RecapArgs { from: Some("2026-03".into()), to: Some("2026-06".into()), ..RecapArgs::default() }, &[]).unwrap_err();
        assert!(matches!(err, OpsError::InvalidInput { .. }), "{err}");
        assert!(!root.join(RECAP_DIR).join("recap_2026-03_2026-06.toml").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn lengths_read_as_seconds_or_minutes() {
        for (s, secs) in [("60", 60.0), ("45.5", 45.5), ("90s", 90.0), ("1m30s", 90.0), ("1m 30s", 90.0), ("2m", 120.0), (" 2M ", 120.0)] {
            assert_eq!(parse_length(s).unwrap(), secs, "{s}");
        }
        for s in ["", "soon", "0", "-5", "1h", "m", "1m30", "NaN", "inf"] {
            assert!(parse_length(s).is_err(), "{s}");
        }
    }

    #[test]
    fn durations_read_back_exactly() {
        assert_eq!(dur(12.4), "12s 400ms");
        assert_eq!(dur(0.3), "300ms");
        assert_eq!(dur(75.0), "75s");
        for secs in [0.001, 3.25, 12.4, 59.999, 61.5] {
            let d = humantime_parse(&dur(secs));
            assert!((d - secs).abs() < 1e-9, "{secs}");
        }
    }

    /// Read a duration through the config's own range field.
    fn humantime_parse(s: &str) -> f64 {
        let r: vibecut::project_config::InputRange =
            toml::from_str(&format!("file = \"x\"\nstart = \"{s}\"\nend = \"9999s\"\n")).unwrap();
        r.start.as_secs_f64()
    }

    fn excl(file: &str, start: f64, end: f64) -> SelectionExclude {
        SelectionExclude {
            file: file.into(),
            start: std::time::Duration::from_secs_f64(start),
            end: std::time::Duration::from_secs_f64(end),
        }
    }

    #[test]
    fn a_dropped_window_matches_like_the_pipelines_exclude() {
        let m = moment("/f/Alps/GX010201.MP4", 10.0, 13.0, 1.0, at(2026, 3, 1), "A");
        assert!(dropped(&excl("GX010201.MP4", 12.5, 16.0), &m));
        assert!(dropped(&excl("gx010201.mp4", 9.0, 10.5), &m), "any letter case");
        assert!(dropped(&excl("0201.MP4", 11.0, 12.0), &m), "a file-name suffix");
        assert!(!dropped(&excl("GX010201.MP4", 13.0, 16.0), &m), "touching is not covering");
        assert!(!dropped(&excl("GX010202.MP4", 10.0, 13.0), &m));
        assert!(!dropped(&excl("", 10.0, 13.0), &m));
    }

    /// "Drop this moment" on a recap writes an exclude; making the recap
    /// again (longer, another mix) must not bring the moment back, and must
    /// keep the exclude for the next time.
    #[test]
    fn dropped_moments_stay_out_when_the_recap_is_made_again() {
        let ms = vec![
            moment("/f/alps.MP4", 30.0, 33.0, 1.0, at(2026, 3, 14), "Alps"),
            moment("/f/alps.MP4", 5.0, 8.0, 0.8, at(2026, 3, 14) - chrono::Duration::minutes(5), "Alps"),
            moment("/f/coast.MP4", 20.0, 23.0, 0.6, at(2026, 6, 3), "Coast"),
            moment("/f/lake.MP4", 0.0, 3.0, 0.9, at(2026, 9, 20), "Lake"),
        ];
        // 20 s holds 4 moments' worth only at its minimum: one per month.
        let args = |exclude: Vec<SelectionExclude>| RecapArgs { exclude, ..year_args(20.0) };
        let before = plan(&collection(ms.clone()), &args(vec![])).unwrap();
        let march: Vec<f64> = before.moments.iter().filter(|m| m.bucket == "March").map(|m| m.moment.start_secs).collect();
        assert!(march.contains(&30.0), "{march:?}");

        // The window the drop wrote, padded around the clip that played.
        let p = before.moments.iter().find(|m| m.moment.start_secs == 30.0).unwrap();
        let drop = excl("alps.MP4", p.clip_start_secs - 0.5, p.clip_end_secs + 0.5);
        let after = plan(&collection(ms), &args(vec![drop, excl("x.MP4", 3.0, 3.0)])).unwrap();
        let starts: Vec<(&str, f64)> = after.moments.iter().map(|m| (m.moment.path.as_str(), m.moment.start_secs)).collect();
        assert!(!starts.contains(&("/f/alps.MP4", 30.0)), "{starts:?}");
        assert!(starts.contains(&("/f/alps.MP4", 5.0)), "March still has a moment: {starts:?}");
        assert!(after.eligible);

        // Written into the config (the empty window is not), which loads.
        let text = config_toml(&after, "/projects/recaps/out/recap_2026.mp4", 42, None);
        let cfg = checked(&text).unwrap();
        assert_eq!(cfg.selection.exclude.len(), 1, "{text}");
        assert_eq!(cfg.selection.exclude[0].file, "alps.MP4");
        assert!((cfg.selection.exclude[0].end.as_secs_f64() - (p.clip_end_secs + 0.5)).abs() < 1e-3);
        assert!(!config_toml(&before, "/x.mp4", 42, None).contains("selection.exclude"));
    }

    /// Calmer / More energy change the recap's `[transition]`; making it
    /// again (longer, shuffled) must keep that, not reset it.
    #[test]
    fn a_remake_keeps_the_transition_it_is_given() {
        let ms: Vec<Moment> = (1..=3).map(|mo| moment(&format!("/f/{mo}.MP4"), 10.0, 13.0, 1.0, at(2026, mo, 1), "T")).collect();
        let punchier = Transition {
            duration: std::time::Duration::from_millis(200),
            speed_ramp_probability: 0.5,
            speed_ramp_factor: 3.5,
            palette: vec!["fade".into(), "wipeleft".into()],
            ..Transition::default()
        };
        let mut p = plan(&collection(ms.clone()), &RecapArgs { transition: Some(punchier.clone()), ..year_args(30.0) }).unwrap();
        // Without music each window carries the kept crossfade.
        let span = p.moment_secs;
        fit_to_song(&mut p, false, None);
        assert!(p.moments.iter().all(|m| ((m.clip_end_secs - m.clip_start_secs) - (span + 0.2)).abs() < 1e-6));
        let cfg = checked(&config_toml(&p, "/x.mp4", 42, None)).unwrap();
        assert_eq!(cfg.transition.duration, std::time::Duration::from_millis(200));
        assert_eq!(cfg.transition.speed_ramp_probability, 0.5);
        assert_eq!(cfg.transition.speed_ramp_factor, 3.5);
        assert_eq!(cfg.transition.palette, punchier.palette);

        // Straight cuts: no overlap to carry.
        let cut = Transition { default: "cut".into(), ..Transition::default() };
        let mut p = plan(&collection(ms.clone()), &RecapArgs { transition: Some(cut), ..year_args(30.0) }).unwrap();
        let span = p.moment_secs;
        fit_to_song(&mut p, false, None);
        assert!(p.moments.iter().all(|m| ((m.clip_end_secs - m.clip_start_secs) - span).abs() < 1e-6));
        assert!(checked(&config_toml(&p, "/x.mp4", 42, None)).unwrap().transition.hard_cut());

        // Nothing given: the recap's own 300 ms crossfades.
        let p = plan(&collection(ms), &year_args(30.0)).unwrap();
        let cfg = checked(&config_toml(&p, "/x.mp4", 42, None)).unwrap();
        assert_eq!(cfg.transition.duration, std::time::Duration::from_millis(300));
        assert_eq!(cfg.transition.speed_ramp_probability, 0.0);
    }

    /// The Studio's Shuffle: another draw of each month's strong moments,
    /// the same for the same seed, still every month in filming order.
    #[test]
    fn a_shuffle_draws_other_strong_moments() {
        // Four strong moments a month, and one weak one (the last).
        let scores = [1.0, 0.95, 0.9, 0.85, 0.3];
        let ms: Vec<Moment> = (1..=4)
            .flat_map(|mo| {
                scores.iter().enumerate().map(move |(i, &s)| {
                    moment(&format!("/f/{mo}.MP4"), i as f64 * 10.0, i as f64 * 10.0 + 3.0, s, at(2026, mo, 1 + i as u32), "T")
                })
            })
            .collect();
        let picks = |shuffle: Option<u64>| {
            let p = plan(&collection(ms.clone()), &RecapArgs { shuffle, ..year_args(20.0) }).unwrap();
            assert_eq!(p.shuffle, shuffle);
            assert!(p.buckets.iter().all(|b| b.chosen >= 1), "every month still plays");
            let order: Vec<_> = p.moments.iter().map(|m| m.moment.recorded_at).collect();
            assert!(order.windows(2).all(|w| w[0] <= w[1]), "in filming order");
            p.moments.iter().map(|m| (m.moment.path.clone(), m.moment.start_secs)).collect::<Vec<_>>()
        };
        let best = picks(None);
        assert!(best.iter().all(|(_, s)| *s == 0.0), "unshuffled: each month's best: {best:?}");
        assert_eq!(picks(Some(7)), picks(Some(7)), "a seed always draws the same");
        let drawn: Vec<_> = (1..=8).map(|seed| picks(Some(seed))).collect();
        assert!(drawn.iter().any(|d| *d != best), "some shuffle draws other moments");
        assert!(drawn.windows(2).any(|w| w[0] != w[1]), "seeds draw differently");
        // Never a weak one: no lift carries 0.3 past the month's best.
        assert!(drawn.iter().flatten().all(|(_, s)| *s != 40.0), "{drawn:?}");
    }

    #[test]
    fn years_say_which_have_enough_for_a_recap() {
        let ms = vec![
            moment("/a", 0.0, 1.0, 1.0, at(2026, 1, 5), "A"),
            moment("/b", 0.0, 1.0, 1.0, at(2026, 5, 5), "A"),
            moment("/c", 0.0, 1.0, 1.0, at(2026, 8, 5), "B"),
            moment("/d", 0.0, 1.0, 1.0, at(2025, 7, 5), "C"),
            moment("/e", 0.0, 1.0, 1.0, at(2025, 7, 6), "D"),
        ];
        let y = years(&ms);
        let got: Vec<(i32, usize, usize, bool)> = y.iter().map(|y| (y.year, y.months, y.trips, y.eligible)).collect();
        assert_eq!(got, vec![(2026, 3, 2, true), (2025, 1, 2, false)]);
    }
}
