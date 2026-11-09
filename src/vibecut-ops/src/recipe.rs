//! Style codes ("recipes"): how a video was made — not what it was made
//! of — as a short code anyone can paste to get the same style on their
//! own footage.
//!
//! # What a recipe holds
//!
//! The creative choices of a project config, never its footage:
//!
//! - the **vibe** it starts from (see [`crate::vibes`]);
//! - the **music**: the clips' own sound, "let Vibecut pick", or the song
//!   itself, named by its [track id](track_id) together with its title,
//!   artist and tempo, so a library without that exact file finds the
//!   same song by name, or else one at a similar tempo;
//! - every **style setting** that differs from the vibe's own: `[project]`
//!   length, frame rate, seed and fades; `[soundtrack]` (all but the
//!   files: beat alignment, ducking, beat-burst, fade-out); the
//!   `[intro]` / `[outro]` cards with their text layers; `[scoring]`;
//!   `[selection]` (all but `exclude` and `source_weights`);
//!   `[transition]`; `[multi_angle]` (all but `groups`); `[grading]` (all
//!   but `lut`); and every `[[outputs]]` entry (all but `path`, `encoder`
//!   and `encoder_preset`).
//!
//! Left out: the footage (`[input]`), the title, every path (outputs,
//! LUTs, image layers), per-clip choices (exclusions, source weights,
//! angle groups) and machine-specific encoder settings. Card text keeps
//! its style but not the trip: text equal to the project's title becomes
//! `{title}`, dates (`YYYY-MM-DD`) become `{date}`, the trip folder's name
//! alone or before `" · "` becomes `{trip}`, and applying the recipe fills
//! them in for the new footage. Other card text (a sign-off, a channel
//! name) is shared as written.
//!
//! # The code
//!
//! `VC1-` + URL-safe base64 of a small binary payload and a 24-bit
//! checksum. The payload is canonical — settings sorted by path, values
//! in their shortest form — so the same style always gives the same code.
//! Settings are stored as differences from the vibe's style as it was
//! when format 1 was defined ([`frozen_baseline`], `recipe_baselines_v1.json`),
//! so a later change to a vibe template doesn't change what an old code
//! means. Paths and values are compressed with two small dictionaries
//! ([`SEGMENTS`], [`VALUES`]); a generic compressor wouldn't pay for
//! itself on payloads this size. An untouched vibe with a song is about
//! 50 characters; see the `code_lengths` test for a report.
//!
//! The checksum catches typos, truncation and tampering by accident; it
//! is not a signature. A code from a newer format (`VC2-…`) is refused
//! with a message saying so rather than misread.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use vibecut::project_config::Config;

use crate::error::OpsError;
use crate::music::{self, AutoPick, TrackInfo};
use crate::paths::{resolve_footage, resolve_projects, slugify, PROJECTS_ROOT};
use crate::propose::{self, ProposeResult, ProposedSoundtrack};
use crate::vibes::{self, SoundtrackSub, Substitutions, Vibe};

/// Every code starts with this, the format version and a dash.
pub const CODE_PREFIX: &str = "VC";
/// The code format this build writes and reads.
pub const FORMAT_VERSION: u32 = 1;
/// The seed a recipe assumes when it doesn't set one (propose's default).
const DEFAULT_SEED: u64 = 42;
/// Longest song title / artist a code carries, in bytes.
const TITLE_MAX: usize = 48;
const ARTIST_MAX: usize = 32;
/// Most songs a recipe names.
const MAX_SONGS: usize = 8;
/// A song within this fraction of the original's tempo (or of half or
/// double it) counts as "a similar tempo".
const TEMPO_BAND: f32 = 0.08;

/// Vibe ids in codes, by position. Append only.
const VIBE_IDS: [&str; 6] =
    ["kid_reel", "family_chronicle", "drone_cinematic", "action_highlight", "documentary", "memory_keeper"];

/// Path segments (config keys) by position. Append only — a code names
/// a key by its index here. At most 224 entries.
pub const SEGMENTS: &[&str] = &[
    "project", "target_duration", "fps", "seed", "fade_in", "fade_out",
    "soundtrack", "beat_align", "energy_drives_pacing", "ducking_db", "duck_count", "duck_min_spacing",
    "duck_ramp", "duck_min_dur", "duck_max_dur", "beat_burst", "beat_burst_min_beats", "beat_burst_max_beats",
    "vad_speech_weight",
    "intro", "outro", "duration", "mode", "bg_color", "layers", "kind", "content", "font", "size", "color",
    "position", "y_offset", "x_offset", "appear_at", "disappear_at", "background", "padding",
    "scoring", "hilight_tags", "accel_peaks", "gps_speed", "faces", "motion_peaks", "sharpness_floor",
    "exposure_floor", "diversity_per_source",
    "selection", "min_clip", "max_clip", "order", "max_per_source", "variety",
    "transition", "default", "palette", "speed_ramp_probability", "speed_ramp_duration", "speed_ramp_factor",
    "speed_ramp_flash",
    "multi_angle", "enabled", "sync", "min_hold",
    "grading", "stabilize",
    "outputs", "resolution", "crf", "preset", "audio_bitrate", "fit", "max_bitrate",
    "gyro", "landings",
];

/// Common string values by position. Append only; at most 128 entries.
pub const VALUES: &[&str] = &[
    "fade", "cut", "none", "dissolve", "fadeblack", "fadewhite", "wipeleft", "wiperight", "slideleft",
    "slideright", "smoothleft", "smoothright", "circleopen", "circleclose", "radial", "pixelize",
    "card", "overlay", "text",
    "center", "top-left", "top-right", "top-center", "bottom-left", "bottom-right", "bottom-center",
    "chronological", "energy_arc", "strict_chronological",
    "auto", "on", "off", "creation_time", "audio", "manual",
    "blur", "crop", "pad",
    "master", "youtube", "reels", "tiktok", "shorts", "square", "whatsapp",
    "1920x1080", "3840x2160", "2560x1440", "1280x720", "1080x1920", "1080x1080", "720x1280",
    "128k", "160k", "192k", "256k", "320k",
    "#FFFFFF", "#000000", "#ffffff",
    "{title}", "{trip} · {date}", "{trip}",
];

// ---------- the recipe ----------

/// One step along a config path: a key or an array index.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Seg {
    Key(String),
    Index(usize),
}

impl fmt::Display for Seg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Seg::Key(k) => f.write_str(k),
            Seg::Index(i) => write!(f, "{i}"),
        }
    }
}

/// `transition.duration`, `outputs.1.preset`.
pub fn path_string(path: &[Seg]) -> String {
    path.iter().map(Seg::to_string).collect::<Vec<_>>().join(".")
}

/// What a setting does at its path.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Set the value there (a whole subtree for something the vibe lacks).
    Set(Value),
    /// Remove the key.
    Remove,
    /// Resize the array to this many elements; later settings fill new ones.
    Count(usize),
}

/// One difference from the vibe's style.
#[derive(Debug, Clone, PartialEq)]
pub struct Setting {
    pub path: Vec<Seg>,
    pub op: Op,
}

impl Serialize for Setting {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = Map::new();
        m.insert("path".into(), Value::String(path_string(&self.path)));
        match &self.op {
            Op::Set(v) => m.insert("value".into(), v.clone()),
            Op::Remove => m.insert("remove".into(), Value::Bool(true)),
            Op::Count(n) => m.insert("count".into(), Value::from(*n)),
        };
        Value::Object(m).serialize(s)
    }
}

/// The music a recipe asks for.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Music {
    /// No music: the clips keep their own sound.
    Original,
    /// A library song Vibecut picks for the vibe and length.
    Auto,
    /// These songs, in order (a multi-file soundtrack names several).
    Songs { songs: Vec<SongRef> },
}

/// A song as a code names it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SongRef {
    /// The file's [`track_id`] (12 hex digits); `None` when it couldn't be read.
    #[serde(serialize_with = "ser_track_id")]
    pub id: Option<u64>,
    pub title: String,
    pub artist: Option<String>,
    /// Detected tempo, to 0.1 BPM.
    pub bpm: Option<f32>,
}

fn ser_track_id<S: serde::Serializer>(id: &Option<u64>, s: S) -> Result<S::Ok, S::Error> {
    match id {
        Some(id) => s.serialize_str(&format!("{id:012x}")),
        None => s.serialize_none(),
    }
}

/// A style: a vibe, its music and what differs from the vibe's own style.
#[derive(Debug, Clone, PartialEq)]
pub struct Recipe {
    pub vibe: String,
    pub music: Music,
    /// Sorted by path; see [`canonical_settings`].
    pub settings: Vec<Setting>,
}

/// Sort settings by path and keep the last of any duplicates.
pub fn canonical_settings(mut settings: Vec<Setting>) -> Vec<Setting> {
    settings.reverse();
    settings.sort_by(|a, b| a.path.cmp(&b.path));
    settings.dedup_by(|a, b| a.path == b.path);
    settings
}

// ---------- errors ----------

/// Why a code can't be read.
#[derive(Debug, Clone, PartialEq)]
pub enum CodeError {
    Empty,
    NotACode,
    UnsupportedVersion(u32),
    Damaged(&'static str),
    Changed,
    UnknownVibe,
}

const COPY_AGAIN: &str = "copy the whole code again — it's one line starting with VC1-";

impl CodeError {
    fn hint(&self) -> &'static str {
        match self {
            CodeError::Empty | CodeError::NotACode => "style codes look like VC1-… — copy the whole code",
            CodeError::UnsupportedVersion(_) | CodeError::UnknownVibe => {
                "update Vibecut to use it, or ask for a code from this version"
            }
            CodeError::Damaged(_) | CodeError::Changed => COPY_AGAIN,
        }
    }
}

impl fmt::Display for CodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodeError::Empty => f.write_str("paste a style code"),
            CodeError::NotACode => f.write_str("that isn't a Vibecut style code"),
            CodeError::UnsupportedVersion(v) if *v > FORMAT_VERSION => write!(
                f,
                "this style code was made by a newer Vibecut (style format {v}); this version reads format {FORMAT_VERSION}"
            ),
            CodeError::UnsupportedVersion(v) => write!(f, "unknown style code format {v}"),
            CodeError::Damaged(why) => write!(f, "this style code is damaged ({why})"),
            CodeError::Changed => f.write_str("this style code was changed or cut short: its checksum doesn't match"),
            CodeError::UnknownVibe => f.write_str("this style code uses a vibe this version of Vibecut doesn't have"),
        }
    }
}

impl From<CodeError> for OpsError {
    fn from(e: CodeError) -> Self {
        OpsError::invalid_with_hint(e.to_string(), e.hint())
    }
}

// ---------- encoding ----------

const T_NULL: u8 = 0x00;
const T_FALSE: u8 = 0x01;
const T_TRUE: u8 = 0x02;
const T_REMOVE: u8 = 0x03;
const T_COUNT: u8 = 0x04;
const T_UINT: u8 = 0x05;
const T_F64: u8 = 0x06;
/// `0x07..=0x0D`: a decimal `m / 10^k` for k = 0..=6 (zig-zag varint m).
const T_DEC: u8 = 0x07;
const T_NEGINT: u8 = 0x0E;
const T_STR: u8 = 0x10;
/// A duration in its canonical humantime spelling, in milliseconds…
const T_DUR_MS: u8 = 0x11;
/// …or in tenths of a second when it is a whole number of them.
const T_DUR_DS: u8 = 0x12;
const T_ARRAY: u8 = 0x14;
const T_OBJECT: u8 = 0x15;
/// `0x40..=0x7F`: the integers 0..=63.
const T_SMALL: u8 = 0x40;
/// `0x80..=0xFF`: [`VALUES`] by index.
const T_DICT: u8 = 0x80;

/// Segment tokens: below `SEG_INDEX` a [`SEGMENTS`] index; then array
/// indices 0..=15; then a varint index; then a literal key.
const SEG_INDEX: u8 = 0xE0;
const SEG_INDEX_VAR: u8 = 0xF0;
const SEG_LITERAL: u8 = 0xFF;

const MUSIC_ORIGINAL: u8 = 0;
const MUSIC_AUTO: u8 = 1;
const MUSIC_SONGS: u8 = 2;
/// Vibe index meaning "the vibe key follows as text".
const VIBE_LITERAL: u8 = 63;
/// Nested values deeper than this are refused when decoding.
const MAX_DEPTH: usize = 16;

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(u: u64) -> i64 {
    ((u >> 1) as i64) ^ -((u & 1) as i64)
}

fn encode_seg(seg: &Seg, out: &mut Vec<u8>) {
    match seg {
        Seg::Key(k) => match SEGMENTS.iter().position(|s| s == k) {
            Some(i) => out.push(i as u8),
            None => {
                out.push(SEG_LITERAL);
                put_str(out, k);
            }
        },
        Seg::Index(i) if *i < 16 => out.push(SEG_INDEX | *i as u8),
        Seg::Index(i) => {
            out.push(SEG_INDEX_VAR);
            put_varint(out, *i as u64);
        }
    }
}

/// `f` as `m / 10^k` with the smallest k, when that is exact — or, for a
/// value that came from an `f32` (every float in a config does), when it
/// reads back as the same `f32`.
fn decimal_of(f: f64) -> Option<(u8, i64)> {
    let from_f32 = (f as f32) as f64 == f;
    for k in 0..=6u8 {
        let m = (f * 10f64.powi(k as i32)).round();
        if !m.is_finite() || m.abs() >= 9.0e15 {
            return None;
        }
        let back = m / 10f64.powi(k as i32);
        if back == f || (from_f32 && back as f32 == f as f32) {
            return Some((k, m as i64));
        }
    }
    None
}

fn encode_number(n: &Number, out: &mut Vec<u8>) {
    if let Some(u) = n.as_u64() {
        if u < 64 {
            out.push(T_SMALL | u as u8);
        } else {
            out.push(T_UINT);
            put_varint(out, u);
        }
    } else if let Some(i) = n.as_i64() {
        // Negative (non-negative ones are u64 above).
        out.push(T_NEGINT);
        put_varint(out, (-(i + 1)) as u64);
    } else {
        let f = n.as_f64().unwrap_or(0.0);
        match decimal_of(f) {
            Some((k, m)) => {
                out.push(T_DEC + k);
                put_varint(out, zigzag(m));
            }
            None => {
                out.push(T_F64);
                out.extend_from_slice(&f.to_le_bytes());
            }
        }
    }
}

fn encode_str(s: &str, out: &mut Vec<u8>) {
    if let Some(i) = VALUES.iter().position(|v| *v == s) {
        out.push(T_DICT | i as u8);
    } else if let Some(ms) = parse_canonical_ms(s) {
        if ms % 100 == 0 {
            out.push(T_DUR_DS);
            put_varint(out, ms / 100);
        } else {
            out.push(T_DUR_MS);
            put_varint(out, ms);
        }
    } else {
        out.push(T_STR);
        put_str(out, s);
    }
}

fn encode_value(v: &Value, out: &mut Vec<u8>) {
    match v {
        Value::Null => out.push(T_NULL),
        Value::Bool(b) => out.push(if *b { T_TRUE } else { T_FALSE }),
        Value::Number(n) => encode_number(n, out),
        Value::String(s) => encode_str(s, out),
        Value::Array(a) => {
            out.push(T_ARRAY);
            put_varint(out, a.len() as u64);
            a.iter().for_each(|x| encode_value(x, out));
        }
        Value::Object(m) => {
            // A null member is left out: every optional config key reads a
            // missing value as null (`None`), and new subtrees (an end card,
            // another output) are mostly unset options.
            out.push(T_OBJECT);
            let keys: BTreeSet<&String> = m.iter().filter(|(_, v)| !v.is_null()).map(|(k, _)| k).collect();
            put_varint(out, keys.len() as u64);
            for k in keys {
                encode_seg(&Seg::Key(k.clone()), out);
                encode_value(&m[k], out);
            }
        }
    }
}

fn payload(r: &Recipe) -> Vec<u8> {
    let mut out = Vec::new();
    let vibe = VIBE_IDS.iter().position(|v| *v == r.vibe);
    let music = match r.music {
        Music::Original => MUSIC_ORIGINAL,
        Music::Auto => MUSIC_AUTO,
        Music::Songs { .. } => MUSIC_SONGS,
    };
    out.push(vibe.map(|i| i as u8).unwrap_or(VIBE_LITERAL) | (music << 6));
    if vibe.is_none() {
        put_str(&mut out, &r.vibe);
    }
    if let Music::Songs { songs } = &r.music {
        put_varint(&mut out, songs.len() as u64);
        for s in songs {
            out.extend_from_slice(&s.id.unwrap_or(0).to_be_bytes()[2..]);
            put_varint(&mut out, s.bpm.map(|b| (b * 10.0).round().max(1.0) as u64).unwrap_or(0));
            put_str(&mut out, &s.title);
            put_str(&mut out, s.artist.as_deref().unwrap_or(""));
        }
    }
    let settings = canonical_settings(r.settings.clone());
    let mut prev: &[Seg] = &[];
    for s in &settings {
        let shared = prev.iter().zip(&s.path).take_while(|(a, b)| a == b).count().min(15);
        let new = s.path.len() - shared;
        assert!(new >= 1 && new <= 15, "config paths are at most 15 keys deep");
        out.push(((shared as u8) << 4) | new as u8);
        s.path[shared..].iter().for_each(|seg| encode_seg(seg, &mut out));
        match &s.op {
            Op::Set(v) => encode_value(v, &mut out),
            Op::Remove => out.push(T_REMOVE),
            Op::Count(n) => {
                out.push(T_COUNT);
                put_varint(&mut out, *n as u64);
            }
        }
        prev = &s.path;
    }
    out
}

/// FNV-1a over `VC<version>` and the payload, low 24 bits.
fn checksum(version: u32, payload: &[u8]) -> [u8; 3] {
    let mut h: u32 = 0x811c_9dc5;
    for b in format!("{CODE_PREFIX}{version}").bytes().chain(payload.iter().copied()) {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    [(h >> 16) as u8, (h >> 8) as u8, h as u8]
}

/// The share code for `r`. Canonical: equal recipes give equal codes.
pub fn encode(r: &Recipe) -> String {
    let mut body = payload(r);
    let sum = checksum(FORMAT_VERSION, &body);
    body.extend_from_slice(&sum);
    format!("{CODE_PREFIX}{FORMAT_VERSION}-{}", URL_SAFE_NO_PAD.encode(body))
}

// ---------- decoding ----------

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

const TRUNCATED: CodeError = CodeError::Damaged("it ends too early");

impl<'a> Reader<'a> {
    fn done(&self) -> bool {
        self.i >= self.b.len()
    }
    fn remaining(&self) -> usize {
        self.b.len() - self.i
    }
    fn byte(&mut self) -> Result<u8, CodeError> {
        let b = *self.b.get(self.i).ok_or(TRUNCATED)?;
        self.i += 1;
        Ok(b)
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], CodeError> {
        if n > self.remaining() {
            return Err(TRUNCATED);
        }
        let s = &self.b[self.i..self.i + n];
        self.i += n;
        Ok(s)
    }
    fn varint(&mut self) -> Result<u64, CodeError> {
        let mut v: u64 = 0;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            v |= ((b & 0x7F) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(CodeError::Damaged("a number is too long"))
    }
    /// A count of things that each take at least one byte.
    fn count(&mut self) -> Result<usize, CodeError> {
        let n = self.varint()?;
        if n > self.remaining() as u64 {
            return Err(TRUNCATED);
        }
        Ok(n as usize)
    }
    fn str(&mut self) -> Result<String, CodeError> {
        let n = self.count()?;
        let b = self.bytes(n)?;
        String::from_utf8(b.to_vec()).map_err(|_| CodeError::Damaged("its text isn't UTF-8"))
    }
    fn seg(&mut self) -> Result<Seg, CodeError> {
        let b = self.byte()?;
        Ok(match b {
            SEG_LITERAL => Seg::Key(self.str()?),
            SEG_INDEX_VAR => Seg::Index(self.count()?),
            b if b >= SEG_INDEX => Seg::Index((b - SEG_INDEX) as usize),
            b => Seg::Key(SEGMENTS.get(b as usize).ok_or(CodeError::Damaged("it names an unknown setting"))?.to_string()),
        })
    }
    fn value(&mut self, depth: usize) -> Result<Value, CodeError> {
        let tag = self.byte()?;
        self.value_with(tag, depth)
    }
    fn value_with(&mut self, tag: u8, depth: usize) -> Result<Value, CodeError> {
        if depth > MAX_DEPTH {
            return Err(CodeError::Damaged("it nests too deep"));
        }
        let bad = CodeError::Damaged("it holds an unknown kind of value");
        Ok(match tag {
            T_NULL => Value::Null,
            T_FALSE => Value::Bool(false),
            T_TRUE => Value::Bool(true),
            T_UINT => Value::from(self.varint()?),
            T_NEGINT => {
                let v = self.varint()?;
                if v > i64::MAX as u64 {
                    return Err(CodeError::Damaged("a number is out of range"));
                }
                Value::from(-(v as i64) - 1)
            }
            T_F64 => {
                let b: [u8; 8] = self.bytes(8)?.try_into().map_err(|_| TRUNCATED)?;
                Number::from_f64(f64::from_le_bytes(b)).map(Value::Number).ok_or(bad)?
            }
            t if (T_DEC..T_DEC + 7).contains(&t) => {
                let m = unzigzag(self.varint()?);
                let f = m as f64 / 10f64.powi((t - T_DEC) as i32);
                Number::from_f64(f).map(Value::Number).ok_or(bad)?
            }
            T_STR => Value::String(self.str()?),
            T_DUR_MS => Value::String(fmt_ms(self.duration_ms(1)?)),
            T_DUR_DS => Value::String(fmt_ms(self.duration_ms(100)?)),
            T_ARRAY => {
                let n = self.count()?;
                Value::Array((0..n).map(|_| self.value(depth + 1)).collect::<Result<_, _>>()?)
            }
            T_OBJECT => {
                let n = self.count()?;
                let mut m = Map::new();
                for _ in 0..n {
                    let Seg::Key(k) = self.seg()? else {
                        return Err(CodeError::Damaged("an object has an index for a key"));
                    };
                    let v = self.value(depth + 1)?;
                    m.insert(k, v);
                }
                Value::Object(m)
            }
            t if (T_SMALL..T_DICT).contains(&t) => Value::from(t - T_SMALL),
            t if t >= T_DICT => {
                Value::String(VALUES.get((t - T_DICT) as usize).ok_or(CodeError::Damaged("it names an unknown value"))?.to_string())
            }
            _ => return Err(bad),
        })
    }
    fn duration_ms(&mut self, unit: u64) -> Result<u64, CodeError> {
        self.varint()?
            .checked_mul(unit)
            .filter(|ms| *ms < DAY_MS)
            .ok_or(CodeError::Damaged("a duration is out of range"))
    }
}

/// Read a code. Whitespace, line breaks and surrounding quotes (a chat
/// app's wrapping) are ignored; anything else must be exactly right.
pub fn decode(code: &str) -> Result<Recipe, CodeError> {
    let cleaned: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    let cleaned = cleaned.trim_matches(|c| matches!(c, '"' | '\'' | '`' | '“' | '”' | '‘' | '’' | '<' | '>'));
    if cleaned.is_empty() {
        return Err(CodeError::Empty);
    }
    if !cleaned.get(..2).is_some_and(|p| p.eq_ignore_ascii_case(CODE_PREFIX)) {
        return Err(CodeError::NotACode);
    }
    let (version, data) = cleaned[2..].split_once('-').ok_or(CodeError::NotACode)?;
    let version: u32 = version.parse().map_err(|_| CodeError::NotACode)?;
    if version != FORMAT_VERSION {
        return Err(CodeError::UnsupportedVersion(version));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(data)
        .map_err(|_| CodeError::Damaged("it has characters a style code never contains"))?;
    if bytes.len() < 4 {
        return Err(CodeError::Changed);
    }
    let (body, sum) = bytes.split_at(bytes.len() - 3);
    if checksum(version, body) != sum {
        return Err(CodeError::Changed);
    }
    parse_payload(body)
}

fn parse_payload(body: &[u8]) -> Result<Recipe, CodeError> {
    let mut r = Reader { b: body, i: 0 };
    let head = r.byte()?;
    let vibe = match head & 0x3F {
        VIBE_LITERAL => r.str()?,
        i => VIBE_IDS.get(i as usize).ok_or(CodeError::UnknownVibe)?.to_string(),
    };
    if vibes::get(&vibe).is_none() {
        return Err(CodeError::UnknownVibe);
    }
    let music = match head >> 6 {
        MUSIC_ORIGINAL => Music::Original,
        MUSIC_AUTO => Music::Auto,
        MUSIC_SONGS => {
            let n = r.count()?;
            if n == 0 || n > MAX_SONGS {
                return Err(CodeError::Damaged("it names too many songs"));
            }
            let mut songs = Vec::with_capacity(n);
            for _ in 0..n {
                let mut id = [0u8; 8];
                id[2..].copy_from_slice(r.bytes(6)?);
                let id = u64::from_be_bytes(id);
                let bpm10 = r.varint()?;
                let title = r.str()?;
                let artist = r.str()?;
                songs.push(SongRef {
                    id: (id != 0).then_some(id),
                    title,
                    artist: (!artist.is_empty()).then_some(artist),
                    bpm: (bpm10 > 0 && bpm10 < 10_000).then(|| bpm10 as f32 / 10.0),
                });
            }
            Music::Songs { songs }
        }
        _ => return Err(CodeError::Damaged("it asks for an unknown kind of music")),
    };
    let mut settings: Vec<Setting> = Vec::new();
    while !r.done() {
        let h = r.byte()?;
        let (shared, new) = ((h >> 4) as usize, (h & 0x0F) as usize);
        let prev = settings.last().map(|s| s.path.as_slice()).unwrap_or(&[]);
        if shared > prev.len() || new == 0 {
            return Err(CodeError::Damaged("its settings are out of order"));
        }
        let mut path = prev[..shared].to_vec();
        for _ in 0..new {
            path.push(r.seg()?);
        }
        if path.as_slice() <= prev {
            return Err(CodeError::Damaged("its settings are out of order"));
        }
        let op = match r.byte()? {
            T_REMOVE => Op::Remove,
            T_COUNT => Op::Count(r.count()?),
            tag => Op::Set(r.value_with(tag, 1)?),
        };
        settings.push(Setting { path, op });
    }
    Ok(Recipe { vibe, music, settings })
}

// ---------- durations ----------

const DAY_MS: u64 = 86_400_000;

/// `humantime::format_duration` for a whole number of milliseconds under
/// a day — how a config serializes its durations ("1s 500ms", "1m 30s").
fn fmt_ms(ms: u64) -> String {
    if ms == 0 {
        return "0s".into();
    }
    let parts = [(ms / 3_600_000, "h"), (ms / 60_000 % 60, "m"), (ms / 1000 % 60, "s"), (ms % 1000, "ms")];
    parts.iter().filter(|(v, _)| *v > 0).map(|(v, u)| format!("{v}{u}")).collect::<Vec<_>>().join(" ")
}

/// Milliseconds in a humantime-style duration ("1m 30s", "400ms",
/// "2.5s"); sub-millisecond parts are dropped.
fn parse_loose_ms(s: &str) -> Option<u64> {
    let mut total = 0f64;
    let mut any = false;
    for tok in s.split_whitespace() {
        let split = tok.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
        let n: f64 = tok[..split].parse().ok()?;
        let unit = match &tok[split..] {
            "h" | "hr" | "hours" | "hour" => 3_600_000.0,
            "m" | "min" | "mins" | "minutes" | "minute" => 60_000.0,
            "s" | "sec" | "secs" | "seconds" | "second" => 1000.0,
            "ms" | "msec" => 1.0,
            "us" | "usec" => 0.001,
            "ns" | "nsec" => 0.000_001,
            _ => return None,
        };
        total += n * unit;
        any = true;
    }
    (any && total.is_finite()).then_some(total.floor() as u64)
}

/// The milliseconds of `s` when `s` is exactly how a config writes that
/// duration — so the code can carry the number and give back the string.
fn parse_canonical_ms(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 24 || s.contains('.') {
        return None;
    }
    let ms = parse_loose_ms(s)?;
    (ms < DAY_MS && fmt_ms(ms) == s).then_some(ms)
}

// ---------- the style of a config ----------

/// The config sections a recipe covers, each with the keys it leaves out.
const SECTIONS: &[(&str, &[&str])] = &[
    ("project", &["name", "temp_dir", "models_dir", "quality"]),
    ("soundtrack", &["files"]),
    ("intro", &[]),
    ("outro", &[]),
    ("scoring", &[]),
    ("selection", &["exclude", "source_weights"]),
    ("transition", &[]),
    ("multi_angle", &["groups"]),
    ("grading", &["lut"]),
];
/// Keys of each `[[outputs]]` entry a recipe leaves out.
const OUTPUT_DENY: &[&str] = &["path", "encoder", "encoder_preset"];

/// What card text placeholders stand for.
#[derive(Debug, Clone, Copy)]
pub struct CardText<'a> {
    pub title: &'a str,
    pub trip: &'a str,
    pub date: &'a str,
}

/// The style part of a config (as JSON): the [`SECTIONS`] and outputs
/// without their footage-specific keys, image layers dropped from the
/// cards. Card text stays as written.
pub fn strip_to_style(cfg: &Value) -> Value {
    let mut out = Map::new();
    for (section, deny) in SECTIONS {
        let mut v = cfg.get(*section).cloned().unwrap_or(Value::Null);
        if let Value::Object(m) = &mut v {
            for d in *deny {
                m.remove(*d);
            }
            if let Some(Value::Array(layers)) = m.get_mut("layers") {
                layers.retain(|l| l.get("kind").and_then(Value::as_str) == Some("text"));
                // A font path is this machine's: share the file name.
                for l in layers.iter_mut() {
                    if let Some(Value::String(f)) = l.get_mut("font") {
                        if let Some(name) = f.rsplit(['/', '\\']).next().filter(|n| n.len() < f.len()) {
                            *f = name.to_string();
                        }
                    }
                }
            }
        }
        out.insert(section.to_string(), v);
    }
    let outputs: Vec<Value> = cfg
        .get("outputs")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|o| {
                    let mut o = o.clone();
                    if let Value::Object(m) = &mut o {
                        OUTPUT_DENY.iter().for_each(|d| {
                            m.remove(*d);
                        });
                    }
                    o
                })
                .collect()
        })
        .unwrap_or_default();
    out.insert("outputs".into(), Value::Array(outputs));
    Value::Object(out)
}

/// Each card text layer's `content`, through `f`.
fn map_card_text(style: &mut Value, f: impl Fn(&str) -> String) {
    for card in ["intro", "outro"] {
        let Some(Value::Array(layers)) = style.get_mut(card).and_then(|c| c.get_mut("layers")) else {
            continue;
        };
        for l in layers {
            if let Some(Value::String(c)) = l.get_mut("content") {
                *c = f(c);
            }
        }
    }
}

fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
}

/// Every `YYYY-MM-DD` in `s` replaced by `{date}`.
fn template_dates(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s.is_char_boundary(i) && s.get(i..i + 10).is_some_and(is_iso_date) {
            out.push_str("{date}");
            i += 10;
        } else {
            let c = s[i..].chars().next().expect("on a char boundary");
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// Card text with the trip taken out: text that is the title becomes
/// `{title}`, dates become `{date}`, and the trip folder's name — alone,
/// or before `" · "` as in propose's default title `"<trip> · <date>"` —
/// becomes `{trip}`. Anything else is shared as written (a title inside
/// longer text isn't hunted for: a short title would match inside words).
/// The title wins: a default title `"<trip> · <date>"` shares as
/// `{title}`, so the recipient's own title (by default the same pattern)
/// goes on the card.
fn templatize(content: &str, t: &CardText<'_>) -> String {
    if !t.title.is_empty() && content == t.title {
        return "{title}".into();
    }
    let s = template_dates(content);
    if !t.trip.is_empty() {
        if s == t.trip {
            return "{trip}".into();
        }
        if let Some(rest) = s.strip_prefix(t.trip).and_then(|r| r.strip_prefix(" · ")) {
            return format!("{{trip}} · {rest}");
        }
    }
    s
}

/// The placeholders in card text, filled in for new footage.
fn fill_template(content: &str, t: &CardText<'_>) -> String {
    content.replace("{title}", t.title).replace("{trip}", t.trip).replace("{date}", t.date)
}

/// The trip a config's footage folder names (its last component).
fn trip_label(cfg: &Config) -> String {
    cfg.input
        .folder
        .as_ref()
        .and_then(|f| f.as_str().trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next())
        .unwrap_or("")
        .to_string()
}

/// A config's style, ready to share: [`strip_to_style`] with its card
/// text [templatized](templatize).
pub fn style_of(cfg: &Config) -> Result<Value, OpsError> {
    let json = serde_json::to_value(cfg).map_err(|e| OpsError::internal(format!("could not read the config: {e}")))?;
    let mut style = strip_to_style(&json);
    let trip = trip_label(cfg);
    let text = CardText { title: &cfg.project.name, trip: &trip, date: "" };
    map_card_text(&mut style, |c| templatize(c, &text));
    Ok(style)
}

// ---------- vibe baselines ----------

/// The six vibes' styles as format 1 defined them (with a soundtrack).
const BASELINES_V1: &str = include_str!("recipe_baselines_v1.json");

fn frozen_baselines() -> &'static Map<String, Value> {
    static B: OnceLock<Map<String, Value>> = OnceLock::new();
    B.get_or_init(|| match serde_json::from_str(BASELINES_V1) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    })
}

/// A vibe's style as format 1 froze it: what a code's settings are
/// differences from. Without music the soundtrack section is empty.
///
/// The frozen file holds every key the schema had when format 1 shipped
/// (a test fails when a template has a key it lacks, until format 1
/// ships the file is regenerated instead). Keys added to the schema
/// after that (say a new scoring signal) take the vibe's current
/// template value as a fallback, so an untouched new key never shows up
/// as a setting and an old code, which can't mention it, gets today's
/// default. Keys the frozen file does have keep their format-1 value, so
/// what an existing code means never changes.
pub fn frozen_baseline(vibe: &str, with_music: bool) -> Result<Value, OpsError> {
    let mut v = frozen_baselines()
        .get(vibe)
        .cloned()
        .ok_or_else(|| OpsError::internal(format!("no format-1 baseline for vibe {vibe:?}")))?;
    if let Some(current) = current_baseline_cached(vibe) {
        fill_missing(&mut v, &current);
    }
    if !with_music {
        v["soundtrack"] = Value::Null;
    }
    Ok(v)
}

/// Adds every object key of `from` that `into` lacks (recursively).
/// Arrays are filled element by element, pairing elements by position
/// (a new key on a card's text layers reaches each frozen layer from the
/// template's layer at the same index); elements only one side has, and
/// existing scalars in `into`, are left as they are.
fn fill_missing(into: &mut Value, from: &Value) {
    match (into, from) {
        (Value::Object(i), Value::Object(f)) => {
            for (k, fv) in f {
                match i.get_mut(k) {
                    Some(iv) => fill_missing(iv, fv),
                    None => {
                        i.insert(k.clone(), fv.clone());
                    }
                }
            }
        }
        (Value::Array(i), Value::Array(f)) => {
            for (iv, fv) in i.iter_mut().zip(f) {
                fill_missing(iv, fv);
            }
        }
        _ => {}
    }
}

/// [`current_baseline`] per vibe, computed once.
fn current_baseline_cached(vibe: &str) -> Option<Value> {
    static C: OnceLock<Mutex<HashMap<String, Option<Value>>>> = OnceLock::new();
    let cache = C.get_or_init(|| Mutex::new(HashMap::new()));
    let mut g = cache.lock().unwrap_or_else(|e| e.into_inner());
    g.entry(vibe.to_string())
        .or_insert_with(|| vibes::get(vibe).and_then(|v| current_baseline(v).ok()))
        .clone()
}

/// A vibe's style as its template renders today (what format 1 froze,
/// plus any keys added since; the tests compare the two).
fn current_baseline(vibe: &Vibe) -> Result<Value, OpsError> {
    let (_, cfg) = propose::render_checked(
        vibe,
        &Substitutions {
            name: "{title}",
            title: "{title}",
            folder: "/footage/recipe",
            output_path: "/projects/out/recipe.mp4",
            target_duration: vibe.default_target_duration,
            seed: DEFAULT_SEED,
            soundtrack: Some(SoundtrackSub { file: "/projects/music/recipe.mp3", note: "" }),
        },
    )?;
    style_of(&cfg)
}

// ---------- diff and apply ----------

fn diff(base: &Value, target: &Value, path: &mut Vec<Seg>, out: &mut Vec<Setting>) {
    match (base, target) {
        (Value::Object(b), Value::Object(t)) => {
            let keys: BTreeSet<&String> = b.keys().chain(t.keys()).collect();
            for k in keys {
                path.push(Seg::Key(k.clone()));
                match (b.get(k), t.get(k)) {
                    (Some(bv), Some(tv)) => diff(bv, tv, path, out),
                    (None, Some(tv)) => out.push(Setting { path: path.clone(), op: Op::Set(tv.clone()) }),
                    (Some(_), None) => out.push(Setting { path: path.clone(), op: Op::Remove }),
                    (None, None) => {}
                }
                path.pop();
            }
        }
        (Value::Array(b), Value::Array(t)) => {
            if b.len() != t.len() {
                out.push(Setting { path: path.clone(), op: Op::Count(t.len()) });
            }
            for (i, tv) in t.iter().enumerate() {
                path.push(Seg::Index(i));
                match b.get(i) {
                    Some(bv) => diff(bv, tv, path, out),
                    None => out.push(Setting { path: path.clone(), op: Op::Set(tv.clone()) }),
                }
                path.pop();
            }
        }
        _ if base == target => {}
        _ => out.push(Setting { path: path.clone(), op: Op::Set(target.clone()) }),
    }
}

/// The settings that turn `base` into `target`, canonical.
pub fn diff_styles(base: &Value, target: &Value) -> Vec<Setting> {
    let mut out = Vec::new();
    diff(base, target, &mut Vec::new(), &mut out);
    canonical_settings(out)
}

fn apply_setting(root: &mut Value, s: &Setting) -> Result<(), String> {
    let missing = || format!("{} isn't a style setting here", path_string(&s.path));
    let (last, parents) = s.path.split_last().ok_or_else(missing)?;
    let mut cur = root;
    for seg in parents {
        cur = match seg {
            Seg::Key(k) => cur.get_mut(k.as_str()),
            Seg::Index(i) => cur.get_mut(*i),
        }
        .ok_or_else(missing)?;
    }
    match (last, &s.op) {
        (Seg::Key(k), Op::Set(v)) => {
            cur.as_object_mut().ok_or_else(missing)?.insert(k.clone(), v.clone());
        }
        (Seg::Key(k), Op::Remove) => {
            cur.as_object_mut().ok_or_else(missing)?.remove(k);
        }
        (Seg::Index(i), Op::Set(v)) => {
            *cur.as_array_mut().and_then(|a| a.get_mut(*i)).ok_or_else(missing)? = v.clone();
        }
        (seg, Op::Count(n)) => {
            let target = match seg {
                Seg::Key(k) => cur.get_mut(k.as_str()),
                Seg::Index(i) => cur.get_mut(*i),
            };
            target.and_then(Value::as_array_mut).ok_or_else(missing)?.resize(*n, Value::Null);
        }
        (Seg::Index(_), Op::Remove) => return Err(missing()),
    }
    Ok(())
}

fn vibe_of(r: &Recipe) -> Result<&'static Vibe, OpsError> {
    vibes::get(&r.vibe).ok_or_else(|| CodeError::UnknownVibe.into())
}

/// The full style a recipe stands for: its vibe's frozen style with the
/// settings applied, footage-specific keys stripped again (a hand-made
/// code can't smuggle in a path).
pub fn decoded_style(r: &Recipe) -> Result<Value, OpsError> {
    vibe_of(r)?;
    let mut v = frozen_baseline(&r.vibe, r.music != Music::Original)?;
    for s in &r.settings {
        apply_setting(&mut v, s).map_err(|e| {
            OpsError::invalid_with_hint(
                format!("this style code doesn't fit this version of Vibecut: {e}"),
                CodeError::UnsupportedVersion(FORMAT_VERSION + 1).hint(),
            )
        })?;
    }
    let style = strip_to_style(&v);
    check_style_values(&style).map_err(|e| OpsError::invalid(format!("this style code isn't valid: {e}")))?;
    Ok(style)
}

/// Every string a style sets that ends up in a render, against what it
/// may be: colours `#RRGGBB[AA]` or a colour name, transitions from the
/// xfade list (or `cut` / `none`), anchors from the known set, a font as
/// a bare file or family name, paddings and bitrates as numbers. A code
/// is anyone's text; none of it may carry anything else into ffmpeg.
pub fn check_style_values(style: &Value) -> Result<(), String> {
    use vibecut::video_renderer::{is_valid_color, is_xfade_transition, POSITIONS};
    fn str_at<'a>(v: &'a Value, key: &str, at: &str) -> Result<Option<&'a str>, String> {
        match v.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => Err(format!("{at}.{key} must be text")),
        }
    }
    let check = |v: &Value, key: &str, at: &str, ok: &dyn Fn(&str) -> bool, what: &str| -> Result<(), String> {
        match str_at(v, key, at)? {
            Some(s) if !ok(s) => Err(format!("{at}.{key} {s:?} is not {what}")),
            _ => Ok(()),
        }
    };
    let colour = |s: &str| is_valid_color(s);
    let position = |s: &str| POSITIONS.contains(&s);
    let transition = |s: &str| is_xfade_transition(s) || s == "cut" || s == "none";
    let font = |s: &str| {
        !s.trim().is_empty()
            && s.len() <= 128
            && !s.contains("..")
            && !s.chars().any(|c| c == '/' || c == '\\' || c.is_control())
    };
    let padding = |s: &str| {
        let n = s.strip_suffix("px").unwrap_or(s).trim();
        !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
    };
    let bitrate = |s: &str| {
        s == "none"
            || s == "off"
            || (s.bytes().all(|b| b.is_ascii_digit() || b == b'.' || b"kKmM".contains(&b))
                && vibecut::output_format::parse_bitrate(s).is_some())
    };
    let resolution = |s: &str| vibecut::output_format::parse_resolution(s).is_some() && !s.contains(' ');

    for card in ["intro", "outro"] {
        let Some(c) = style.get(card).filter(|c| c.is_object()) else { continue };
        check(c, "bg_color", card, &colour, "a colour")?;
        for (i, layer) in c.get("layers").and_then(Value::as_array).into_iter().flatten().enumerate() {
            let at = format!("{card}.layers.{i}");
            check(layer, "color", &at, &colour, "a colour")?;
            check(layer, "position", &at, &position, "a position")?;
            check(layer, "font", &at, &font, "a font name (a bare file or family name)")?;
            if let Some(b) = layer.get("background").filter(|b| b.is_object()) {
                let at = format!("{at}.background");
                check(b, "color", &at, &colour, "a colour")?;
                check(b, "padding", &at, &padding, "a padding in pixels")?;
            }
        }
    }
    if let Some(t) = style.get("transition").filter(|t| t.is_object()) {
        check(t, "default", "transition", &transition, "a transition")?;
        if let Some(p) = t.get("palette") {
            let list = p.as_array().ok_or("transition.palette must be a list")?;
            for (i, v) in list.iter().enumerate() {
                let s = v.as_str().ok_or_else(|| format!("transition.palette.{i} must be text"))?;
                if !transition(s) {
                    return Err(format!("transition.palette.{i} {s:?} is not a transition"));
                }
            }
        }
    }
    for (i, o) in style.get("outputs").and_then(Value::as_array).into_iter().flatten().enumerate() {
        let at = format!("outputs.{i}");
        check(o, "audio_bitrate", &at, &bitrate, "a bitrate")?;
        check(o, "max_bitrate", &at, &bitrate, "a bitrate")?;
        check(o, "resolution", &at, &resolution, "a resolution")?;
    }
    Ok(())
}

/// The recipe for `cfg`'s style. `vibe`: the vibe it was made from, when
/// known; otherwise the one whose style it is closest to (shortest code).
/// Without a `[soundtrack]` the music is always [`Music::Original`].
pub fn recipe_from_config(cfg: &Config, vibe: Option<&str>, music: Music) -> Result<Recipe, OpsError> {
    let style = style_of(cfg)?;
    let music = if cfg.soundtrack.is_none() { Music::Original } else { music };
    let with_music = music != Music::Original;
    let candidates: Vec<&str> = match vibe {
        Some(k) => vec![propose::get_vibe(k)?.key],
        None => VIBE_IDS.to_vec(),
    };
    let mut best: Option<(usize, Recipe)> = None;
    for key in candidates {
        let base = frozen_baseline(key, with_music)?;
        let recipe = Recipe { vibe: key.to_string(), music: music.clone(), settings: diff_styles(&base, &style) };
        let len = payload(&recipe).len();
        if best.as_ref().is_none_or(|(l, _)| len < *l) {
            best = Some((len, recipe));
        }
    }
    Ok(best.expect("at least one vibe").1)
}

/// Where a recipe is applied.
#[derive(Debug, Clone, Copy)]
pub struct Destination<'a> {
    /// Footage folder (as written into `[input]`).
    pub folder: &'a str,
    pub output_path: &'a str,
    pub text: CardText<'a>,
}

/// A second output's file: `<stem>.<preset>.mp4`, or `<stem>.<n>.mp4`.
fn extra_output_path(first: &str, i: usize, preset: Option<&str>, used: &[String]) -> String {
    let stem = first.strip_suffix(".mp4").unwrap_or(first);
    let named = preset.map(|p| format!("{stem}.{}.mp4", slugify(p)));
    match named {
        Some(p) if !used.contains(&p) => p,
        _ => format!("{stem}.{}.mp4", i + 1),
    }
}

/// Write `style` over a freshly rendered config: every style key the
/// style has replaces the config's; footage-specific keys stay. Without
/// a soundtrack in `full` (no music here), the style's soundtrack
/// settings have nothing to go on.
fn merge_style(full: &mut Value, style: &Value, dest: &Destination<'_>) {
    for (section, _) in SECTIONS {
        let s = style.get(*section).cloned().unwrap_or(Value::Null);
        let slot = &mut full[*section];
        match s {
            Value::Object(m) if slot.is_object() => {
                let o = slot.as_object_mut().expect("object");
                for (k, v) in m {
                    o.insert(k, v);
                }
            }
            _ if *section == "soundtrack" => {}
            other => *slot = other,
        }
    }
    map_card_text(full, |c| fill_template(c, &dest.text));
    let first = full["outputs"][0]["path"].as_str().unwrap_or(dest.output_path).to_string();
    if let Some(outs) = style.get("outputs").and_then(Value::as_array).filter(|a| !a.is_empty()) {
        let mut used: Vec<String> = Vec::new();
        let merged: Vec<Value> = outs
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let mut o = o.clone();
                let path = if i == 0 {
                    first.clone()
                } else {
                    extra_output_path(&first, i, o.get("preset").and_then(Value::as_str), &used)
                };
                used.push(path.clone());
                if let Value::Object(m) = &mut o {
                    m.insert("path".into(), Value::String(path));
                }
                o
            })
            .collect();
        full["outputs"] = Value::Array(merged);
    }
}

/// The config a recipe makes for `dest`: the vibe rendered for that
/// footage (exactly as propose does) with the recipe's style written over
/// it. `files`: the soundtrack (empty: no music).
pub fn build_config(r: &Recipe, dest: &Destination<'_>, files: &[String]) -> Result<Config, OpsError> {
    let vibe = vibe_of(r)?;
    let style = decoded_style(r)?;
    let (_, base) = propose::render_checked(
        vibe,
        &Substitutions {
            name: dest.text.title,
            title: dest.text.title,
            folder: dest.folder,
            output_path: dest.output_path,
            target_duration: vibe.default_target_duration,
            seed: DEFAULT_SEED,
            soundtrack: files.first().map(|f| SoundtrackSub { file: f, note: "" }),
        },
    )?;
    let mut full = serde_json::to_value(&base).map_err(|e| OpsError::internal(e.to_string()))?;
    if files.len() > 1 {
        full["soundtrack"]["files"] = Value::from(files.to_vec());
    }
    merge_style(&mut full, &style, dest);
    let cfg: Config = serde_json::from_value(full).map_err(|e| {
        OpsError::invalid_with_hint(
            format!("this style code sets something this version of Vibecut can't use: {e}"),
            CodeError::UnsupportedVersion(FORMAT_VERSION + 1).hint(),
        )
    })?;
    cfg.validate().map_err(|e| OpsError::invalid(format!("this style code makes a config that doesn't validate: {e}")))?;
    Ok(cfg)
}

// ---------- music ----------

/// How the songs a recipe names were found in a library.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MusicMatch {
    /// `same_song` (the same file), `same_title` (matched by title and
    /// artist), `similar_tempo` (not there; a library song at about its
    /// tempo), `vibecut_pick` (Vibecut picked for the vibe),
    /// `original_sound` (the style has no music) or `no_music` (the
    /// library is empty).
    pub how: &'static str,
    /// The files the config uses; empty for no music.
    pub files: Vec<String>,
    pub title: Option<String>,
    /// In plain words, for the user.
    pub note: String,
}

impl MusicMatch {
    /// The style's own song, rather than a stand-in.
    fn is_the_song(&self) -> bool {
        matches!(self.how, "same_song" | "same_title")
    }
}

/// Lowercase letters and digits only.
fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// `s` cut to at most `max` bytes, on a character boundary.
fn clip(s: &str, max: usize) -> String {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].trim_end().to_string()
}

fn same_title(song: &SongRef, t: &TrackInfo) -> bool {
    let title_ok = !norm(&song.title).is_empty() && norm(&song.title) == norm(&clip(&t.title, TITLE_MAX));
    let artist_ok = match (&song.artist, &t.artist) {
        (Some(a), Some(b)) => norm(a) == norm(&clip(b, ARTIST_MAX)),
        _ => true,
    };
    title_ok && artist_ok
}

fn in_tempo_band(bpm: f32, target: f32) -> bool {
    target > 0.0 && [bpm, bpm * 2.0, bpm / 2.0].iter().any(|b| (b - target).abs() <= target * TEMPO_BAND)
}

fn no_music_note(prefix: &str) -> String {
    format!(
        "{prefix}Your music library is empty, so this edit keeps the clips' own sound. Put songs in {} and Vibecut uses them.",
        music::own_music_dir().display()
    )
}

/// Find a recipe's music in `tracks`: the same file (by [`track_id`]),
/// else the same title and artist, else — for a song that isn't there — a
/// library song within [`TEMPO_BAND`] of its tempo, else Vibecut's pick
/// for the vibe. `id_of` gives a library track's id (it reads the file).
pub fn resolve_music(
    music: &Music,
    tracks: &[TrackInfo],
    id_of: &dyn Fn(&TrackInfo) -> Option<u64>,
    vibe: &Vibe,
    target_secs: f32,
    seed: u64,
) -> MusicMatch {
    let pick = |from: &[TrackInfo]| music::auto_pick(from, vibe, target_secs, seed);
    match music {
        Music::Original => MusicMatch {
            how: "original_sound",
            files: vec![],
            title: None,
            note: "This style keeps the clips' own sound — no music.".into(),
        },
        Music::Auto => match pick(tracks) {
            Some(p) => MusicMatch {
                how: "vibecut_pick",
                note: format!("Vibecut picks a song for the vibe: {}. {}", p.title, p.reason),
                files: vec![p.path],
                title: Some(p.title),
            },
            None => MusicMatch { how: "no_music", files: vec![], title: None, note: no_music_note("") },
        },
        Music::Songs { songs } => {
            let mut found: Vec<(&TrackInfo, bool)> = Vec::new();
            for s in songs {
                let by_id = s.id.and_then(|id| tracks.iter().find(|t| id_of(t) == Some(id)));
                match by_id {
                    Some(t) => found.push((t, true)),
                    None => {
                        if let Some(t) = tracks.iter().find(|t| same_title(s, t)) {
                            found.push((t, false));
                        }
                    }
                }
            }
            if !found.is_empty() {
                let exact = found.iter().all(|(_, by_id)| *by_id);
                let titles: Vec<&str> = found.iter().map(|(t, _)| t.title.as_str()).collect();
                let mut note = if exact {
                    format!("Same song as the original: {}.", titles.join(", "))
                } else {
                    format!("The same song from your library, found by its title: {}.", titles.join(", "))
                };
                if found.len() < songs.len() {
                    note.push_str(&format!(
                        " {} of its {} songs aren't in your library, so it plays without them.",
                        songs.len() - found.len(),
                        songs.len()
                    ));
                }
                return MusicMatch {
                    how: if exact { "same_song" } else { "same_title" },
                    files: found.iter().map(|(t, _)| t.path.clone()).collect(),
                    title: Some(titles.join(", ")),
                    note,
                };
            }
            let first = &songs[0];
            let missing = format!("\u{201c}{}\u{201d} isn't in your music library", first.title);
            if tracks.is_empty() {
                return MusicMatch { how: "no_music", files: vec![], title: None, note: no_music_note(&format!("{missing}. ")) };
            }
            let band: Vec<TrackInfo> = match first.bpm {
                Some(bpm) => tracks.iter().filter(|t| in_tempo_band(t.analysis.bpm, bpm)).cloned().collect(),
                None => Vec::new(),
            };
            if let (Some(p), Some(bpm)) = (pick(&band), first.bpm) {
                let got = band.iter().find(|t| t.path == p.path).map(|t| t.analysis.bpm).unwrap_or(bpm);
                return MusicMatch {
                    how: "similar_tempo",
                    note: format!(
                        "{missing}, so Vibecut picked {} — {:.0} BPM, close to its {:.0} BPM.",
                        p.title, got, bpm
                    ),
                    files: vec![p.path],
                    title: Some(p.title),
                };
            }
            match pick(tracks) {
                Some(p) => MusicMatch {
                    how: "vibecut_pick",
                    note: format!(
                        "{missing}{}, so Vibecut picked {} for the vibe.",
                        if first.bpm.is_some() { " and nothing there has its tempo" } else { "" },
                        p.title
                    ),
                    files: vec![p.path],
                    title: Some(p.title),
                },
                None => MusicMatch { how: "no_music", files: vec![], title: None, note: no_music_note("") },
            }
        }
    }
}

const ID_HEAD_BYTES: usize = 256 * 1024;
const ID_MASK: u64 = 0xFFFF_FFFF_FFFF;

/// FNV-1a 64 over the file length and its first bytes, 48 bits, never 0.
fn fingerprint(len: u64, head: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in len.to_le_bytes().iter().chain(head) {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    match h & ID_MASK {
        0 => 1,
        id => id,
    }
}

/// A track's id: a 48-bit fingerprint of the file's size and first
/// 256 KiB. The same file has the same id on every machine, whatever its
/// path or name; a re-encoded copy doesn't (the title matches it then).
/// Cached per path, size and modification time.
pub fn track_id(path: &Path) -> Option<u64> {
    type Key = (PathBuf, u64, u64);
    static CACHE: OnceLock<Mutex<HashMap<Key, u64>>> = OnceLock::new();
    let meta = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let key = (path.to_path_buf(), meta.len(), mtime);
    let cache = CACHE.get_or_init(Default::default);
    if let Some(id) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Some(*id);
    }
    let mut head = Vec::with_capacity(ID_HEAD_BYTES.min(meta.len() as usize));
    std::fs::File::open(path).ok()?.take(ID_HEAD_BYTES as u64).read_to_end(&mut head).ok()?;
    let id = fingerprint(meta.len(), &head);
    cache.lock().unwrap_or_else(|e| e.into_inner()).insert(key, id);
    Some(id)
}

// ---------- summary ----------

/// One thing a style sets, in words: `Length: 30 s`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Chip {
    pub label: &'static str,
    pub value: String,
}

/// A style in words, for a preview before using it.
#[derive(Debug, Clone, Serialize)]
pub struct StyleSummary {
    pub vibe: &'static str,
    pub vibe_name: &'static str,
    /// As a config writes it ("30s").
    pub target_duration: String,
    pub target_secs: f32,
    pub seed: u64,
    pub music: Music,
    /// What it sets, label by label.
    pub chips: Vec<Chip>,
    /// What differs from the vibe's own style.
    pub settings: Vec<Setting>,
}

/// "30 s", "1 min 30 s", "3 min".
fn length_label(ms: u64) -> String {
    let s = (ms as f64 / 1000.0).round() as u64;
    match (s / 60, s % 60) {
        (0, r) => format!("{r} s"),
        (m, 0) => format!("{m} min"),
        (m, r) => format!("{m} min {r} s"),
    }
}

/// "300 ms", "1.5 s", "4 s".
fn secs_label(ms: u64) -> String {
    if ms < 1000 {
        return format!("{ms} ms");
    }
    let s = format!("{:.2}", ms as f64 / 1000.0);
    format!("{} s", s.trim_end_matches('0').trim_end_matches('.'))
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn song_label(s: &SongRef) -> String {
    let mut out = s.title.clone();
    if let Some(a) = &s.artist {
        out.push_str(&format!(" — {a}"));
    }
    if let Some(b) = s.bpm {
        out.push_str(&format!(" · {b:.0} BPM"));
    }
    out
}

fn chips(r: &Recipe, vibe: &Vibe, style: &Value) -> Vec<Chip> {
    let get = |p: &str| style.pointer(p);
    let ms = |p: &str| get(p).and_then(Value::as_str).and_then(parse_loose_ms);
    let num = |p: &str| get(p).and_then(Value::as_f64);
    let text = |p: &str| get(p).and_then(Value::as_str).unwrap_or("");
    let mut out = vec![
        Chip { label: "Vibe", value: vibe.name.to_string() },
        Chip { label: "Length", value: ms("/project/target_duration").map(length_label).unwrap_or_default() },
        Chip {
            label: "Song",
            value: match &r.music {
                Music::Original => "Original sound".into(),
                Music::Auto => "Vibecut picks".into(),
                Music::Songs { songs } => songs.iter().map(song_label).collect::<Vec<_>>().join(" + "),
            },
        },
    ];
    let name = text("/transition/default");
    let dur = ms("/transition/duration").unwrap_or(0);
    let cuts = if name == "cut" || name == "none" || dur == 0 {
        "Hard cuts".to_string()
    } else if name == "fade" {
        format!("Crossfades · {}", secs_label(dur))
    } else {
        format!("{} · {}", capitalize(name), secs_label(dur))
    };
    out.push(Chip { label: "Cuts", value: cuts });
    let ramp = num("/transition/speed_ramp_probability").unwrap_or(0.0);
    out.push(Chip {
        label: "Speed ramps",
        value: if ramp > 0.0 {
            format!("{:.0}% of cuts · {:.1}×", ramp * 100.0, num("/transition/speed_ramp_factor").unwrap_or(1.0))
        } else {
            "None".into()
        },
    });
    let order = match text("/selection/order") {
        "energy_arc" => "energy arc",
        "strict_chronological" => "strictly by the clock",
        _ => "in order",
    };
    out.push(Chip {
        label: "Clips",
        value: format!(
            "{}–{} · {order}",
            secs_label(ms("/selection/min_clip").unwrap_or(0)).trim_end_matches(" s"),
            secs_label(ms("/selection/max_clip").unwrap_or(0))
        ),
    });
    if r.music != Music::Original {
        out.push(Chip { label: "Beat burst", value: capitalize(text("/soundtrack/beat_burst")) });
        let ducks = get("/soundtrack/duck_count").and_then(Value::as_u64).unwrap_or(0);
        out.push(Chip {
            label: "Ducking",
            value: if ducks == 0 {
                "Off".into()
            } else {
                format!("{ducks}× to {:.0} dB", num("/soundtrack/ducking_db").unwrap_or(0.0))
            },
        });
    }
    let card = |p: &str| -> Option<String> {
        let c = get(p).filter(|c| c.is_object())?;
        let dur = secs_label(c.get("duration").and_then(Value::as_str).and_then(parse_loose_ms).unwrap_or(0));
        let first = c.pointer("/layers/0/content").and_then(Value::as_str);
        let words = match first {
            None | Some("{title}") => String::new(),
            // Placeholder text reads as what fills it in, not as braces.
            Some(t) if t.contains("{title}") || t.contains("{trip}") || t.contains("{date}") => format!(
                " · {}",
                t.replace("{title}", "your title").replace("{trip}", "trip name").replace("{date}", "date")
            ),
            Some(t) => format!(" · \u{201c}{t}\u{201d}"),
        };
        Some(match c.get("mode").and_then(Value::as_str) {
            Some("overlay") => format!("Over the clip · {dur}{words}"),
            _ => format!("{} · {dur}{words}", c.get("bg_color").and_then(Value::as_str).unwrap_or("")),
        })
    };
    out.push(Chip { label: "Title card", value: card("/intro").unwrap_or_else(|| "None".into()) });
    if let Some(end) = card("/outro") {
        out.push(Chip { label: "End card", value: end });
    }
    let formats: Vec<String> = get("/outputs")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|o| match (o.get("preset").and_then(Value::as_str), o.get("resolution").and_then(Value::as_str)) {
                    (Some(p), _) => capitalize(p),
                    (None, Some(r)) => r.to_string(),
                    _ => "?".into(),
                })
                .collect()
        })
        .unwrap_or_default();
    out.push(Chip { label: "Formats", value: formats.join(" + ") });
    out.push(Chip {
        label: "Fades",
        value: format!(
            "{} in · {} out",
            secs_label(ms("/project/fade_in").unwrap_or(0)),
            secs_label(ms("/project/fade_out").unwrap_or(0))
        ),
    });
    if get("/grading/stabilize").and_then(Value::as_bool) == Some(true) {
        out.push(Chip { label: "Stabilized", value: "Yes".into() });
    }
    out
}

/// A recipe in words.
pub fn summarize(r: &Recipe) -> Result<StyleSummary, OpsError> {
    let vibe = vibe_of(r)?;
    let style = decoded_style(r)?;
    let target_duration = style.pointer("/project/target_duration").and_then(Value::as_str).unwrap_or("").to_string();
    Ok(StyleSummary {
        vibe: vibe.key,
        vibe_name: vibe.name,
        target_secs: parse_loose_ms(&target_duration).unwrap_or(0) as f32 / 1000.0,
        target_duration,
        seed: style.pointer("/project/seed").and_then(Value::as_u64).unwrap_or(DEFAULT_SEED),
        music: r.music.clone(),
        chips: chips(r, vibe, &style),
        settings: r.settings.clone(),
    })
}

// ---------- operations ----------

/// What `share_style` makes.
#[derive(Debug, Clone, Serialize)]
pub struct ShareResult {
    pub code: String,
    /// The code's length, in characters.
    pub chars: usize,
    pub style: StyleSummary,
}

pub struct ShareArgs<'a> {
    /// The project config to share the style of.
    pub config_path: &'a str,
    /// A render's manifest: the tempo and song credit it recorded fill in
    /// a song the music library doesn't know.
    pub manifest_path: Option<&'a str>,
    /// The vibe the project was made from; `None` = the closest one.
    pub vibe: Option<&'a str>,
    /// Share "let Vibecut pick" instead of the song itself.
    pub let_vibecut_pick: bool,
}

fn read_manifest(path: &str) -> Result<Value, OpsError> {
    let p = resolve_projects(path);
    let text = std::fs::read_to_string(&p).map_err(|_| OpsError::not_found_with("manifest", p.display().to_string()))?;
    serde_json::from_str(&text).map_err(|e| OpsError::invalid(format!("manifest {} isn't valid JSON: {e}", p.display())))
}

/// The songs a config uses, as a code names them: from the music library
/// when it has them, else from the render's credit and the file itself.
fn songs_for(files: &[PathBuf], tracks: &[TrackInfo], manifest: Option<&Value>) -> Vec<SongRef> {
    let credits: Vec<&Value> = manifest.and_then(|m| m.get("soundtrack")).and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
    let manifest_bpm = manifest.and_then(|m| m.pointer("/beat_sync/tempo_bpm")).and_then(Value::as_f64);
    files
        .iter()
        .take(MAX_SONGS)
        .map(|f| {
            let (title, artist, bpm) = match tracks.iter().find(|t| Path::new(&t.path) == f) {
                Some(t) => (t.title.clone(), t.artist.clone(), Some(t.analysis.bpm as f64)),
                None => {
                    let credit = credits.iter().find(|c| c.get("file").and_then(Value::as_str).map(Path::new) == Some(f.as_path()));
                    let field = |k: &str| credit.and_then(|c| c.get(k)).and_then(Value::as_str).map(str::to_string);
                    let (meta, _) = vibecut::track_meta::load_for_file(f, &mut Vec::new());
                    let title = field("title").or(meta.title).unwrap_or_else(|| vibecut::track_meta::title_from_file_name(f));
                    (title, field("artist").or(meta.artist), if files.len() == 1 { manifest_bpm } else { None })
                }
            };
            SongRef {
                id: track_id(f),
                title: clip(&title, TITLE_MAX),
                artist: artist.map(|a| clip(&a, ARTIST_MAX)).filter(|a| !a.is_empty()),
                bpm: bpm.filter(|b| *b > 0.0 && *b < 1000.0).map(|b| ((b * 10.0).round() / 10.0) as f32),
            }
        })
        .collect()
}

/// A style code for a project's config.
pub async fn share_style(args: ShareArgs<'_>) -> Result<ShareResult, OpsError> {
    let cfg = crate::config_io::read_config_structured(args.config_path).await?;
    let manifest = args.manifest_path.filter(|p| !p.trim().is_empty()).map(read_manifest).transpose()?;
    let music = match &cfg.soundtrack {
        None => Music::Original,
        Some(_) if args.let_vibecut_pick => Music::Auto,
        Some(snd) => {
            let tracks = propose::library_tracks().await;
            let files: Vec<PathBuf> = snd.files.iter().map(|f| f.clone().into_std_path_buf()).collect();
            let songs = tokio::task::spawn_blocking(move || songs_for(&files, &tracks, manifest.as_ref()))
                .await
                .map_err(|e| OpsError::internal(format!("song lookup panicked: {e}")))?;
            if songs.is_empty() {
                Music::Auto
            } else {
                Music::Songs { songs }
            }
        }
    };
    let recipe = recipe_from_config(&cfg, args.vibe.filter(|v| !v.trim().is_empty()), music)?;
    let code = encode(&recipe);
    Ok(ShareResult { chars: code.chars().count(), code, style: summarize(&recipe)? })
}

/// What a code would make, before making it.
#[derive(Debug, Clone, Serialize)]
pub struct PreviewResult {
    pub code: String,
    pub style: StyleSummary,
    /// The music it would get from this library.
    pub music_match: MusicMatch,
}

/// The library's ids, read off disk on a blocking thread.
async fn match_music(r: &Recipe, style: &StyleSummary, tracks: Vec<TrackInfo>) -> Result<(MusicMatch, Option<AutoPick>), OpsError> {
    let vibe = vibe_of(r)?;
    let music = r.music.clone();
    let (secs, seed) = (style.target_secs, style.seed);
    tokio::task::spawn_blocking(move || {
        let m = resolve_music(&music, &tracks, &|t| track_id(Path::new(&t.path)), vibe, secs, seed);
        (m, music::auto_pick(&tracks, vibe, secs, seed))
    })
    .await
    .map_err(|e| OpsError::internal(format!("music lookup panicked: {e}")))
}

/// Read a code (or a saved style's name) and say what it would make,
/// including which song it would get here.
pub async fn preview_style(code_or_name: &str) -> Result<PreviewResult, OpsError> {
    let code = resolve_code(code_or_name)?;
    let recipe = decode(&code)?;
    let style = summarize(&recipe)?;
    let (music_match, _) = match_music(&recipe, &style, propose::library_tracks().await).await?;
    Ok(PreviewResult { code: encode(&recipe), style, music_match })
}

pub struct ApplyArgs<'a> {
    /// A style code, or the name of a saved style.
    pub code: &'a str,
    /// Footage folder (same convention as propose).
    pub folder: &'a str,
    /// Intro-card title; default `"<folder> · YYYY-MM-DD"`.
    pub title: Option<&'a str>,
}

/// A proposal made from a style code: propose's result plus the style.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    #[serde(flatten)]
    pub proposal: ProposeResult,
    pub code: String,
    pub style: StyleSummary,
    pub music_match: MusicMatch,
}

/// Make a proposal for `folder` in a shared style: a config written to
/// `/projects` exactly like propose's, ready for the normal render.
pub async fn apply_style(args: ApplyArgs<'_>) -> Result<ApplyResult, OpsError> {
    apply_in(Path::new(PROJECTS_ROOT), args, propose::library_tracks().await).await
}

fn wrap_comment(text: &str) -> String {
    let mut out = String::new();
    let mut line = String::new();
    for w in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + w.chars().count() > 76 {
            out.push_str(&format!("# {line}\n"));
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(w);
    }
    if !line.is_empty() {
        out.push_str(&format!("# {line}\n"));
    }
    out
}

async fn apply_in(projects_root: &Path, args: ApplyArgs<'_>, tracks: Vec<TrackInfo>) -> Result<ApplyResult, OpsError> {
    let code = resolve_code(args.code)?;
    let recipe = decode(&code)?;
    let code = encode(&recipe);
    let folder = resolve_footage(args.folder);
    if !folder.exists() {
        return Err(OpsError::not_found_with("footage folder", folder.to_string_lossy().into_owned()));
    }
    crate::browse::check_footage_folder(&folder)?;
    let vibe = vibe_of(&recipe)?;
    let style = summarize(&recipe)?;
    let trip = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "edit".into());
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let title = args
        .title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{trip} · {today}"));
    let basename = slugify(&trip);
    let output_path = format!("{}/out/{basename}.mp4", projects_root.display());
    let folder_str = folder.to_string_lossy().into_owned();
    let (music_match, auto_pick) = match_music(&recipe, &style, tracks).await?;
    let dest = Destination {
        folder: &folder_str,
        output_path: &output_path,
        text: CardText { title: &title, trip: &trip, date: &today },
    };
    let cfg = build_config(&recipe, &dest, &music_match.files)?;
    let body = toml::to_string_pretty(&cfg).map_err(|e| OpsError::internal(format!("could not write the config: {e}")))?;
    let mut toml_text = format!("# Made from a shared style code: {code}\n");
    toml_text.push_str(&wrap_comment(&format!(
        "{} · {} · {} setting{} differ from the vibe. Music: {}",
        vibe.name,
        length_label((style.target_secs * 1000.0) as u64),
        style.settings.len(),
        if style.settings.len() == 1 { "" } else { "s" },
        music_match.note
    )));
    toml_text.push('\n');
    toml_text.push_str(&body);
    // The backstop propose has too: never write a config the pipeline rejects.
    toml::from_str::<Config>(&toml_text)
        .map_err(|e| OpsError::internal(format!("the style rendered invalid TOML: {e}")))?
        .validate()
        .map_err(|e| OpsError::internal(format!("the style rendered a config that fails validation: {e}")))?;

    let _ = std::fs::create_dir_all(projects_root.join("out"));
    let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string();
    let config_path = projects_root.join(format!("proposal_{basename}_{ts}.toml"));
    std::fs::write(&config_path, &toml_text)
        .map_err(|e| OpsError::internal(format!("could not write {}: {e}", config_path.display())))?;

    let soundtrack = music_match.files.first().map(|p| ProposedSoundtrack {
        path: p.clone(),
        title: music_match.title.clone().unwrap_or_default(),
        picked_by: if music_match.is_the_song() { "you" } else { "vibecut" },
        reason: Some(music_match.note.clone()),
    });
    Ok(ApplyResult {
        proposal: ProposeResult {
            vibe: vibe.key,
            vibe_name: vibe.name,
            rationale: vibe.rationale,
            config_path: config_path.to_string_lossy().into_owned(),
            config_toml: toml_text,
            output_path,
            music_note: soundtrack.is_none().then(|| music_match.note.clone()),
            soundtrack,
            auto_pick,
        },
        code,
        style,
        music_match,
    })
}

// ---------- saved styles ----------

/// Where named styles are kept: `/projects/recipes/<name>.json`.
pub fn recipes_dir() -> PathBuf {
    Path::new(PROJECTS_ROOT).join("recipes")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedFile {
    name: String,
    code: String,
    #[serde(default)]
    created_at: Option<String>,
}

/// One saved style.
#[derive(Debug, Clone, Serialize)]
pub struct SavedStyle {
    pub name: String,
    pub code: String,
    pub file: String,
    pub created_at: Option<String>,
    pub style: StyleSummary,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SavedStyles {
    /// Newest first.
    pub styles: Vec<SavedStyle>,
    /// Files that couldn't be read, or hold a code this version can't.
    pub warnings: Vec<String>,
    pub dir: String,
}

/// File name for a style name: its slug, plus a short hash when the slug
/// drops characters (so "Été" and "Ete" don't overwrite each other).
/// The file a saved style's name goes in. Names are matched without
/// regard to case, so the stem is too. A name the slug spells back
/// exactly (lower-case letters, digits, `-` and single spaces, which
/// become `_`) is its slug; any other name (`"my_style"`, `"my  style"`,
/// `"Été"`) gets a hash of the name after its slug, so two names that
/// slug the same never share a file.
fn file_stem_for(name: &str) -> String {
    let key = name.to_lowercase();
    let slug = slugify(&key);
    if slug.replace('_', " ") == key {
        return slug;
    }
    let h = fingerprint(key.len() as u64, key.as_bytes());
    format!("{slug}_{:06x}", h & 0xFF_FFFF)
}

fn save_in(dir: &Path, name: &str, code: &str) -> Result<SavedStyle, OpsError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(OpsError::invalid("a style's name must be 1 to 60 characters"));
    }
    let recipe = decode(code)?;
    let code = encode(&recipe);
    let style = summarize(&recipe)?;
    std::fs::create_dir_all(dir).map_err(|e| OpsError::internal(format!("could not create {}: {e}", dir.display())))?;
    let path = dir.join(format!("{}.json", file_stem_for(name)));
    // Replacing is for the same name only; a different name in this file
    // (a hash clash) is never overwritten.
    if let Some(other) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<SavedFile>(&t).ok())
        .filter(|f| f.name.trim().to_lowercase() != name.to_lowercase())
    {
        return Err(OpsError::invalid(format!(
            "a style saved as {:?} is stored where {name:?} would go; pick another name",
            other.name
        )));
    }
    let created_at = Some(chrono::Utc::now().to_rfc3339());
    let saved = SavedFile { name: name.to_string(), code: code.clone(), created_at: created_at.clone() };
    let text = serde_json::to_string_pretty(&saved).map_err(|e| OpsError::internal(e.to_string()))?;
    std::fs::write(&path, text).map_err(|e| OpsError::internal(format!("could not write {}: {e}", path.display())))?;
    Ok(SavedStyle { name: name.to_string(), code, file: path.to_string_lossy().into_owned(), created_at, style })
}

fn list_in(dir: &Path) -> SavedStyles {
    let mut out = SavedStyles { dir: dir.to_string_lossy().into_owned(), ..Default::default() };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut files: Vec<PathBuf> =
        entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
    files.sort();
    for p in files {
        let read = std::fs::read_to_string(&p)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<SavedFile>(&t).map_err(|e| e.to_string()));
        let saved = match read {
            Ok(s) => s,
            Err(e) => {
                out.warnings.push(format!("{}: not a saved style ({e}); skipped", p.display()));
                continue;
            }
        };
        match decode(&saved.code).map_err(OpsError::from).and_then(|r| summarize(&r)) {
            Ok(style) => out.styles.push(SavedStyle {
                name: saved.name,
                code: saved.code,
                file: p.to_string_lossy().into_owned(),
                created_at: saved.created_at,
                style,
            }),
            Err(e) => out.warnings.push(format!("{}: {e}; skipped", p.display())),
        }
    }
    out.styles.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Save a code under a name (replacing a style saved under the same name).
pub async fn save_style(name: &str, code: &str) -> Result<SavedStyle, OpsError> {
    save_in(&recipes_dir(), name, code)
}

/// Every saved style, newest first.
pub async fn list_styles() -> SavedStyles {
    list_in(&recipes_dir())
}

fn looks_like_code(s: &str) -> bool {
    let s = s.trim().trim_start_matches(|c| matches!(c, '"' | '\'' | '`' | '“' | '‘'));
    s.get(..2).is_some_and(|p| p.eq_ignore_ascii_case(CODE_PREFIX)) && s[2..].starts_with(|c: char| c.is_ascii_digit())
}

fn resolve_code_in(dir: &Path, code_or_name: &str) -> Result<String, OpsError> {
    if looks_like_code(code_or_name) || code_or_name.trim().is_empty() {
        return Ok(code_or_name.to_string());
    }
    let wanted = code_or_name.trim();
    list_in(dir)
        .styles
        .into_iter()
        .find(|s| s.name.eq_ignore_ascii_case(wanted))
        .map(|s| s.code)
        .ok_or_else(|| OpsError::not_found_with("saved style", wanted.to_string()))
}

/// A code as given, or the code of the saved style with that name.
pub fn resolve_code(code_or_name: &str) -> Result<String, OpsError> {
    resolve_code_in(&recipes_dir(), code_or_name)
}

#[cfg(test)]
mod tests;
