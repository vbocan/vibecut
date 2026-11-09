//! Soundtrack metadata: who made a track, under what licence, and the
//! credit line a post needs — read from optional JSON files that sit
//! next to the music, never from the audio itself.
//!
//! Two places are consulted, most specific first:
//!
//! 1. A sidecar next to the track: `<file>.json` (`song.mp3.json`) or
//!    `<stem>.json` (`song.json`).
//! 2. A `library.json` in the track's folder: folder-wide defaults plus a
//!    `tracks` list keyed by file name.
//!
//! ```json
//! {
//!   "licence": "CC-BY-4.0",
//!   "artist": "Example Artist",
//!   "tracks": [
//!     { "file": "song.mp3", "title": "Song", "source_url": "https://…", "bpm": 120 }
//!   ]
//! }
//! ```
//!
//! Every field is optional. A missing file means "no metadata"; an
//! unreadable or invalid one is ignored with a warning, so a typo in a
//! JSON file never hides a track or fails a render.
//!
//! The render writes what it finds into the manifest
//! ([`SoundtrackCredit`], including the file's SHA-256) so a false
//! Content ID claim can be disputed with the exact file and licence.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

/// Name of the per-folder metadata file.
pub const LIBRARY_FILE: &str = "library.json";

/// Metadata for one track, as written in a sidecar or `library.json`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct TrackMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// Licence as written: `CC0-1.0`, `CC-BY-4.0`, `Pixabay`,
    /// `YouTube-Audio-Library`, `custom`, … (see [`Licence::parse`]).
    #[serde(default, alias = "license", skip_serializing_if = "Option::is_none")]
    pub licence: Option<String>,
    /// Where the track was downloaded from.
    #[serde(default, alias = "url", alias = "source", skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// The exact credit line the licence or site asks for. When absent and
    /// the licence needs attribution, one is built (see [`TrackMeta::credit_line`]).
    #[serde(default, alias = "attribution", skip_serializing_if = "Option::is_none")]
    pub credit: Option<String>,
    /// Tempo the site declares (informational: renders use the detected beat grid).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpm: Option<f32>,
    /// Overrides whether the caption must credit the track (e.g. a YouTube
    /// Audio Library track marked "attribution required").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution_required: Option<bool>,
}

impl TrackMeta {
    /// Fill every field `self` leaves empty from `fallback`.
    fn or(mut self, fallback: &TrackMeta) -> TrackMeta {
        fn pick<T: Clone>(a: &mut Option<T>, b: &Option<T>) {
            if a.is_none() {
                a.clone_from(b);
            }
        }
        pick(&mut self.title, &fallback.title);
        pick(&mut self.artist, &fallback.artist);
        pick(&mut self.licence, &fallback.licence);
        pick(&mut self.source_url, &fallback.source_url);
        pick(&mut self.credit, &fallback.credit);
        pick(&mut self.bpm, &fallback.bpm);
        pick(&mut self.attribution_required, &fallback.attribution_required);
        self
    }

    /// Blank strings count as absent.
    fn cleaned(mut self) -> TrackMeta {
        for s in [
            &mut self.title,
            &mut self.artist,
            &mut self.licence,
            &mut self.source_url,
            &mut self.credit,
        ] {
            if s.as_deref().is_some_and(|v| v.trim().is_empty()) {
                *s = None;
            } else if let Some(v) = s.as_mut() {
                *v = v.trim().to_string();
            }
        }
        if self.bpm.is_some_and(|b| !b.is_finite() || b <= 0.0) {
            self.bpm = None;
        }
        self
    }

    pub fn licence(&self) -> Licence {
        Licence::parse(self.licence.as_deref())
    }

    /// Whether a public post must credit the track: the sidecar's explicit
    /// `attribution_required`, else the licence's rule (every CC BY
    /// variant), else "a credit line was supplied" for licences that
    /// don't settle it either way (YouTube Audio Library, custom, unknown).
    pub fn attribution_required(&self) -> bool {
        if let Some(explicit) = self.attribution_required {
            return explicit;
        }
        let kind = self.licence().kind;
        kind.requires_attribution()
            || (self.credit.is_some() && !matches!(kind, LicenceKind::Cc0 | LicenceKind::Pixabay))
    }

    /// The line a caption or description should carry: the supplied
    /// `credit` verbatim, else — when the licence needs attribution — a
    /// title / author / source / licence line in the form Creative Commons
    /// recommends. `None` when no credit is needed or known.
    pub fn credit_line(&self, fallback_title: &str) -> Option<String> {
        if let Some(c) = &self.credit {
            return Some(c.clone());
        }
        if !self.attribution_required() {
            return None;
        }
        let title = self.title.as_deref().unwrap_or(fallback_title);
        let mut line = format!("Music: \"{title}\"");
        if let Some(a) = &self.artist {
            line.push_str(&format!(" by {a}"));
        }
        if let Some(u) = &self.source_url {
            line.push_str(&format!(" ({u})"));
        }
        let lic = self.licence();
        if lic.kind != LicenceKind::Unknown {
            line.push_str(&format!(" — licensed under {}", lic.label));
            if let Some(url) = lic.url() {
                line.push_str(&format!(" ({url})"));
            }
        }
        Some(line)
    }
}

/// What a licence allows, as far as posting a video goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LicenceKind {
    /// No licence recorded.
    Unknown,
    /// CC0 / public domain.
    Cc0,
    /// CC BY: free use with credit.
    CcBy,
    /// CC BY-SA: credit, and adaptations share the licence.
    CcBySa,
    /// Any CC licence with NC: non-commercial only.
    CcByNc,
    /// CC BY-ND: no adaptations.
    CcByNd,
    /// The Pixabay Content License.
    Pixabay,
    /// YouTube Audio Library terms.
    YoutubeAudioLibrary,
    /// Anything else (a site's own terms, GPL, …).
    Custom,
}

impl LicenceKind {
    pub fn requires_attribution(self) -> bool {
        matches!(self, Self::CcBy | Self::CcBySa | Self::CcByNc | Self::CcByNd)
    }

    /// Licences the starter pack accepts (see MUSIC.md): CC0 and plain CC BY.
    pub fn starter_eligible(self) -> bool {
        matches!(self, Self::Cc0 | Self::CcBy)
    }

    /// Plain-language advice for someone about to post a video with it.
    pub fn note(self) -> &'static str {
        match self {
            Self::Unknown => "Fine for personal videos; check the site's terms before posting publicly.",
            Self::Cc0 => "Public domain (CC0): use it anywhere, no credit needed.",
            Self::CcBy => "Needs credit — Vibecut adds it to your caption.",
            Self::CcBySa => {
                "Needs credit — Vibecut adds it to your caption. Share-alike terms may also apply to your video; check before posting."
            }
            Self::CcByNc => {
                "Non-commercial only: fine for personal posts, not for monetised or sponsored videos. Needs credit — Vibecut adds it to your caption."
            }
            Self::CcByNd => {
                "No-derivatives licence: cutting it into a video may not be allowed. Check the terms before posting."
            }
            Self::Pixabay => {
                "Free to use, no credit needed. Some Pixabay tracks are registered with Content ID (the shield icon on the site), so YouTube may flag your video — the licence lets you dispute it."
            }
            Self::YoutubeAudioLibrary => {
                "Safe on YouTube. Terms on other platforms are unclear; check before posting elsewhere."
            }
            Self::Custom => "Custom licence: read its terms before posting publicly.",
        }
    }
}

/// A licence string, normalised.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Licence {
    /// Normalised id: `CC0-1.0`, `CC-BY-4.0`, `CC-BY-SA-3.0`, `Pixabay`,
    /// `YouTube-Audio-Library`, `unknown`, or the text as written for a
    /// custom licence.
    pub id: String,
    /// Human label: "CC BY 4.0", "Pixabay Content License", …
    pub label: String,
    pub kind: LicenceKind,
}

impl Licence {
    pub fn parse(raw: Option<&str>) -> Licence {
        let raw = raw.map(str::trim).unwrap_or("");
        let norm: String = raw
            .to_ascii_uppercase()
            .chars()
            .map(|c| if c == ' ' || c == '_' { '-' } else { c })
            .collect();
        let norm = norm.trim_matches('-');
        let lic = |id: &str, label: &str, kind| Licence { id: id.into(), label: label.into(), kind };
        if norm.is_empty() || norm == "UNKNOWN" || norm == "NONE" {
            return lic("unknown", "Unknown licence", LicenceKind::Unknown);
        }
        if norm.starts_with("CC0") || norm == "PUBLIC-DOMAIN" || norm == "PD" {
            return lic("CC0-1.0", "CC0 1.0", LicenceKind::Cc0);
        }
        if let Some(rest) = norm.strip_prefix("CC-BY").or_else(|| norm.strip_prefix("CCBY")) {
            let parts: Vec<&str> = rest.split('-').filter(|p| !p.is_empty()).collect();
            let version = parts.iter().find(|p| p.contains('.') || p.parse::<u32>().is_ok());
            let mods: Vec<&str> = parts
                .iter()
                .filter(|p| matches!(**p, "SA" | "NC" | "ND"))
                .copied()
                .collect();
            let unknown_part = parts
                .iter()
                .any(|p| Some(p) != version && !matches!(*p, "SA" | "NC" | "ND" | "INTERNATIONAL"));
            if !unknown_part {
                let kind = if mods.contains(&"NC") {
                    LicenceKind::CcByNc
                } else if mods.contains(&"ND") {
                    LicenceKind::CcByNd
                } else if mods.contains(&"SA") {
                    LicenceKind::CcBySa
                } else {
                    LicenceKind::CcBy
                };
                let suffix = mods.join("-");
                let mut id = String::from("CC-BY");
                let mut label = String::from("CC BY");
                if !suffix.is_empty() {
                    id.push_str(&format!("-{suffix}"));
                    label.push_str(&format!("-{suffix}"));
                }
                if let Some(v) = version {
                    let v = if v.contains('.') { v.to_string() } else { format!("{v}.0") };
                    id.push_str(&format!("-{v}"));
                    label.push_str(&format!(" {v}"));
                }
                return Licence { id, label, kind };
            }
        }
        if norm.starts_with("PIXABAY") {
            return lic("Pixabay", "Pixabay Content License", LicenceKind::Pixabay);
        }
        if norm.contains("YOUTUBE") || norm == "YTAL" {
            return lic("YouTube-Audio-Library", "YouTube Audio Library", LicenceKind::YoutubeAudioLibrary);
        }
        if norm == "CUSTOM" {
            return lic("custom", "Custom licence", LicenceKind::Custom);
        }
        Licence { id: raw.to_string(), label: raw.to_string(), kind: LicenceKind::Custom }
    }

    /// Deed URL for a versioned Creative Commons licence.
    pub fn url(&self) -> Option<String> {
        let path = match self.kind {
            LicenceKind::Cc0 => return Some("https://creativecommons.org/publicdomain/zero/1.0/".into()),
            LicenceKind::CcBy | LicenceKind::CcBySa | LicenceKind::CcByNc | LicenceKind::CcByNd => {
                self.id.strip_prefix("CC-")?.to_ascii_lowercase()
            }
            _ => return None,
        };
        // "by-sa-4.0" → "by-sa/4.0"; no version → no URL (never guess one).
        let (terms, version) = path.rsplit_once('-')?;
        version.contains('.').then(|| format!("https://creativecommons.org/licenses/{terms}/{version}/"))
    }
}

/// Where a track's metadata came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetaSource {
    /// `<file>.json` / `<stem>.json` next to the track.
    Sidecar,
    /// The folder's `library.json`.
    Library,
}

#[derive(Debug, Deserialize)]
struct LibraryFile {
    #[serde(flatten)]
    defaults: TrackMeta,
    #[serde(default)]
    tracks: Vec<LibraryEntry>,
}

#[derive(Debug, Deserialize)]
struct LibraryEntry {
    file: String,
    #[serde(flatten)]
    meta: TrackMeta,
}

/// One folder's `library.json`, parsed once and shared by its tracks.
#[derive(Debug, Clone, Default)]
pub struct FolderMeta {
    defaults: TrackMeta,
    /// File name → entry.
    tracks: HashMap<String, TrackMeta>,
    present: bool,
}

impl FolderMeta {
    /// Read `dir/library.json`. A missing file is normal (empty result);
    /// an unreadable or invalid one, or an entry naming a file that isn't
    /// there, adds a warning and is otherwise ignored.
    pub fn load(dir: &Path, warnings: &mut Vec<String>) -> FolderMeta {
        let path = dir.join(LIBRARY_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return FolderMeta::default(),
            Err(e) => {
                warnings.push(format!("{}: unreadable ({e}); ignored", path.display()));
                return FolderMeta::default();
            }
        };
        let lib: LibraryFile = match serde_json::from_str(&text) {
            Ok(l) => l,
            Err(e) => {
                warnings.push(format!("{}: not valid metadata JSON ({e}); ignored", path.display()));
                return FolderMeta::default();
            }
        };
        let mut tracks = HashMap::new();
        for entry in lib.tracks {
            if !dir.join(&entry.file).is_file() {
                warnings.push(format!(
                    "{}: lists {:?}, which isn't in that folder; entry ignored",
                    path.display(),
                    entry.file
                ));
                continue;
            }
            tracks.insert(entry.file, entry.meta.cleaned());
        }
        FolderMeta { defaults: lib.defaults.cleaned(), tracks, present: true }
    }

    /// Metadata for `track` (a file in this folder): its sidecar, then its
    /// `library.json` entry, then the folder defaults.
    pub fn for_track(&self, track: &Path, warnings: &mut Vec<String>) -> (TrackMeta, Option<MetaSource>) {
        let name = track.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let entry = self.tracks.get(name);
        let library = entry.cloned().unwrap_or_default().or(&self.defaults);
        match read_sidecar(track, warnings) {
            Some(side) => (side.or(&library), Some(MetaSource::Sidecar)),
            None if entry.is_some() || (self.present && library != TrackMeta::default()) => {
                (library, Some(MetaSource::Library))
            }
            None => (library, None),
        }
    }
}

/// Candidate sidecar paths for a track, most specific first.
fn sidecar_paths(track: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(name) = track.file_name().and_then(|n| n.to_str()) {
        out.push(track.with_file_name(format!("{name}.json")));
    }
    if track.extension().is_some() {
        let stem = track.with_extension("json");
        if stem.file_name().and_then(|n| n.to_str()) != Some(LIBRARY_FILE) {
            out.push(stem);
        }
    }
    out
}

fn read_sidecar(track: &Path, warnings: &mut Vec<String>) -> Option<TrackMeta> {
    for p in sidecar_paths(track) {
        let text = match std::fs::read_to_string(&p) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                warnings.push(format!("{}: unreadable ({e}); ignored", p.display()));
                continue;
            }
        };
        match serde_json::from_str::<TrackMeta>(&text) {
            Ok(m) => return Some(m.cleaned()),
            Err(e) => warnings.push(format!("{}: not valid metadata JSON ({e}); ignored", p.display())),
        }
    }
    None
}

/// Metadata for a single track (sidecar + its folder's `library.json`).
pub fn load_for_file(track: &Path, warnings: &mut Vec<String>) -> (TrackMeta, Option<MetaSource>) {
    let dir = track.parent().unwrap_or(Path::new("."));
    FolderMeta::load(dir, warnings).for_track(track, warnings)
}

/// Title from a file name: "summer_drive-124bpm.mp3" → "Summer drive 124bpm".
pub fn title_from_file_name(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Untitled");
    let words: String = stem
        .chars()
        .map(|c| if c == '_' || c == '-' { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Untitled".into(),
    }
}

/// The manifest's record of one soundtrack file: enough to credit it and
/// to prove which exact file a video used.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct SoundtrackCredit {
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub file: Utf8PathBuf,
    pub title: String,
    pub artist: Option<String>,
    /// Normalised licence id (`CC-BY-4.0`, `Pixabay`, `unknown`, …).
    pub licence: String,
    pub licence_label: String,
    pub source_url: Option<String>,
    /// Credit line for captions (see [`TrackMeta::credit_line`]).
    pub credit: Option<String>,
    pub attribution_required: bool,
    /// SHA-256 of the file as rendered, lowercase hex. `None` if unreadable.
    pub sha256: Option<String>,
    #[cfg_attr(feature = "ts-export", ts(type = "number"))]
    pub size_bytes: u64,
}

impl SoundtrackCredit {
    /// Build the record for `file`. Never fails: metadata problems are
    /// logged as warnings and leave fields empty.
    pub fn for_file(file: &Utf8Path) -> SoundtrackCredit {
        let mut warnings = Vec::new();
        let (meta, _) = load_for_file(file.as_std_path(), &mut warnings);
        for w in &warnings {
            tracing::warn!("soundtrack metadata: {w}");
        }
        let sha256 = match sha256_file(file.as_std_path()) {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::warn!(file = %file, error = %e, "could not hash soundtrack");
                None
            }
        };
        Self::from_meta(file, &meta, sha256)
    }

    pub fn from_meta(file: &Utf8Path, meta: &TrackMeta, sha256: Option<String>) -> SoundtrackCredit {
        let fallback = title_from_file_name(file.as_std_path());
        let lic = meta.licence();
        SoundtrackCredit {
            file: file.to_path_buf(),
            title: meta.title.clone().unwrap_or_else(|| fallback.clone()),
            artist: meta.artist.clone(),
            licence: lic.id,
            licence_label: lic.label,
            source_url: meta.source_url.clone(),
            credit: meta.credit_line(&fallback),
            attribution_required: meta.attribution_required(),
            sha256,
            size_bytes: std::fs::metadata(file.as_std_path()).map(|m| m.len()).unwrap_or(0),
        }
    }
}

/// Credits for every soundtrack file, in order.
pub fn soundtrack_credits(files: &[Utf8PathBuf]) -> Vec<SoundtrackCredit> {
    files.iter().map(|f| SoundtrackCredit::for_file(f)).collect()
}

// ---------- SHA-256 (FIPS 180-4) ----------
//
// Small and self-contained so the engine needs no hashing crate for one
// digest per render.

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Incremental SHA-256.
pub struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    block_len: usize,
    total_len: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Sha256 {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            block: [0; 64],
            block_len: 0,
            total_len: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);
        while !data.is_empty() {
            let take = (64 - self.block_len).min(data.len());
            self.block[self.block_len..self.block_len + take].copy_from_slice(&data[..take]);
            self.block_len += take;
            data = &data[take..];
            if self.block_len == 64 {
                let block = self.block;
                self.compress(&block);
                self.block_len = 0;
            }
        }
    }

    pub fn finish_hex(mut self) -> String {
        let bit_len = self.total_len.wrapping_mul(8);
        self.update(&[0x80]);
        while self.block_len != 56 {
            self.update(&[0]);
        }
        self.update(&bit_len.to_be_bytes());
        self.state.iter().map(|w| format!("{w:08x}")).collect()
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (i, c) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (s, v) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }
}

/// SHA-256 of a file's bytes, lowercase hex.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finish_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(data);
        h.finish_hex()
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("vibecut_track_meta_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn sha256_matches_fips_vectors() {
        assert_eq!(sha(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            sha(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // Split updates across block boundaries give the same digest.
        let data = vec![b'a'; 1_000_000];
        let mut h = Sha256::new();
        for chunk in data.chunks(997) {
            h.update(chunk);
        }
        assert_eq!(h.finish_hex(), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    }

    #[test]
    fn licences_normalise() {
        let p = |s: &str| Licence::parse(Some(s));
        assert_eq!(p("CC0-1.0").kind, LicenceKind::Cc0);
        assert_eq!(p("public domain").id, "CC0-1.0");
        assert_eq!(p("CC-BY-4.0"), Licence { id: "CC-BY-4.0".into(), label: "CC BY 4.0".into(), kind: LicenceKind::CcBy });
        assert_eq!(p("cc by 3.0").id, "CC-BY-3.0");
        assert_eq!(p("CC BY").id, "CC-BY");
        assert_eq!(p("CC-BY-SA-4.0").kind, LicenceKind::CcBySa);
        assert_eq!(p("CC BY-NC-SA 4.0").kind, LicenceKind::CcByNc);
        assert_eq!(p("CC-BY-ND-4.0").kind, LicenceKind::CcByNd);
        assert_eq!(p("Pixabay Content License").id, "Pixabay");
        assert_eq!(p("YouTube Audio Library").id, "YouTube-Audio-Library");
        assert_eq!(p("youtube-audio-library").kind, LicenceKind::YoutubeAudioLibrary);
        assert_eq!(p("custom").kind, LicenceKind::Custom);
        let gpl = p("GPL-3.0");
        assert_eq!((gpl.kind, gpl.id.as_str()), (LicenceKind::Custom, "GPL-3.0"));
        assert_eq!(Licence::parse(None).kind, LicenceKind::Unknown);
        assert_eq!(p("  ").kind, LicenceKind::Unknown);
        assert!(LicenceKind::Cc0.starter_eligible() && LicenceKind::CcBy.starter_eligible());
        assert!(!LicenceKind::CcBySa.starter_eligible() && !LicenceKind::CcByNc.starter_eligible());
        assert!(!LicenceKind::Unknown.starter_eligible() && !LicenceKind::Pixabay.starter_eligible());
    }

    #[test]
    fn cc_deed_urls_only_when_versioned() {
        assert_eq!(
            Licence::parse(Some("CC-BY-4.0")).url().as_deref(),
            Some("https://creativecommons.org/licenses/by/4.0/")
        );
        assert_eq!(
            Licence::parse(Some("CC-BY-SA-3.0")).url().as_deref(),
            Some("https://creativecommons.org/licenses/by-sa/3.0/")
        );
        assert_eq!(Licence::parse(Some("CC BY")).url(), None);
        assert_eq!(Licence::parse(Some("Pixabay")).url(), None);
    }

    #[test]
    fn credit_text_follows_the_licence() {
        let cc_by = TrackMeta {
            title: Some("Night Drive".into()),
            artist: Some("Ada".into()),
            licence: Some("CC-BY-4.0".into()),
            source_url: Some("https://example.org/night".into()),
            ..Default::default()
        };
        assert!(cc_by.attribution_required());
        assert_eq!(
            cc_by.credit_line("fallback").unwrap(),
            "Music: \"Night Drive\" by Ada (https://example.org/night) — licensed under CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/)"
        );
        // A supplied credit line is used verbatim.
        let given = TrackMeta { credit: Some("Song by Ada | example.org".into()), ..cc_by.clone() };
        assert_eq!(given.credit_line("x").unwrap(), "Song by Ada | example.org");
        // CC BY without a title or artist still credits the file's title.
        let bare = TrackMeta { licence: Some("CC BY".into()), ..Default::default() };
        assert_eq!(bare.credit_line("Golden hour").unwrap(), "Music: \"Golden hour\" — licensed under CC BY");
        // No credit needed: CC0, Pixabay, unknown.
        for l in ["CC0-1.0", "Pixabay"] {
            let m = TrackMeta { licence: Some(l.into()), ..Default::default() };
            assert!(!m.attribution_required(), "{l}");
            assert_eq!(m.credit_line("t"), None, "{l}");
        }
        assert_eq!(TrackMeta::default().credit_line("t"), None);
        // A YouTube Audio Library track that ships a credit line needs it.
        let ytal = TrackMeta {
            licence: Some("YouTube-Audio-Library".into()),
            credit: Some("Song by Ada".into()),
            ..Default::default()
        };
        assert!(ytal.attribution_required());
        // The explicit flag wins.
        let off = TrackMeta { attribution_required: Some(false), ..cc_by };
        assert!(!off.attribution_required());
    }

    #[test]
    fn sidecar_beats_library_entry_beats_folder_defaults() {
        let dir = tmp_dir("precedence");
        std::fs::write(dir.join("a.mp3"), b"x").unwrap();
        std::fs::write(dir.join("b.mp3"), b"y").unwrap();
        std::fs::write(dir.join("c.mp3"), b"z").unwrap();
        std::fs::write(
            dir.join(LIBRARY_FILE),
            r#"{ "licence": "CC0-1.0", "artist": "Folder Artist",
                 "tracks": [ { "file": "a.mp3", "title": "Lib A", "license": "CC-BY-4.0" },
                             { "file": "b.mp3", "title": "Lib B", "bpm": 120 } ] }"#,
        )
        .unwrap();
        std::fs::write(dir.join("a.mp3.json"), r#"{ "title": "Side A", "url": "https://x" }"#).unwrap();
        let mut w = Vec::new();
        let folder = FolderMeta::load(&dir, &mut w);
        let (a, src_a) = folder.for_track(&dir.join("a.mp3"), &mut w);
        assert_eq!(src_a, Some(MetaSource::Sidecar));
        assert_eq!(a.title.as_deref(), Some("Side A"));
        assert_eq!(a.licence.as_deref(), Some("CC-BY-4.0"), "library entry fills what the sidecar leaves out");
        assert_eq!(a.artist.as_deref(), Some("Folder Artist"), "folder default fills the rest");
        assert_eq!(a.source_url.as_deref(), Some("https://x"));
        let (b, src_b) = folder.for_track(&dir.join("b.mp3"), &mut w);
        assert_eq!(src_b, Some(MetaSource::Library));
        assert_eq!((b.title.as_deref(), b.bpm, b.licence.as_deref()), (Some("Lib B"), Some(120.0), Some("CC0-1.0")));
        let (c, src_c) = folder.for_track(&dir.join("c.mp3"), &mut w);
        assert_eq!(src_c, Some(MetaSource::Library), "folder defaults apply to unlisted tracks");
        assert_eq!(c.licence.as_deref(), Some("CC0-1.0"));
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn stem_sidecar_is_found() {
        let dir = tmp_dir("stem");
        std::fs::write(dir.join("song.mp3"), b"x").unwrap();
        std::fs::write(dir.join("song.json"), r#"{ "title": "Song", "licence": "Pixabay" }"#).unwrap();
        let mut w = Vec::new();
        let (m, src) = load_for_file(&dir.join("song.mp3"), &mut w);
        assert_eq!(src, Some(MetaSource::Sidecar));
        assert_eq!(m.licence().kind, LicenceKind::Pixabay);
        assert!(w.is_empty());
    }

    #[test]
    fn invalid_or_missing_files_are_ignored_with_a_warning() {
        let dir = tmp_dir("invalid");
        std::fs::write(dir.join("a.mp3"), b"x").unwrap();
        // No metadata at all: no warning, nothing found.
        let mut w = Vec::new();
        assert_eq!(load_for_file(&dir.join("a.mp3"), &mut w), (TrackMeta::default(), None));
        assert!(w.is_empty());

        // A broken sidecar falls back to library.json and warns.
        std::fs::write(dir.join("a.mp3.json"), "{ not json").unwrap();
        std::fs::write(
            dir.join(LIBRARY_FILE),
            r#"{ "tracks": [ { "file": "a.mp3", "title": "From library" },
                             { "file": "gone.mp3", "title": "Deleted" } ] }"#,
        )
        .unwrap();
        let mut w = Vec::new();
        let (m, src) = load_for_file(&dir.join("a.mp3"), &mut w);
        assert_eq!((m.title.as_deref(), src), (Some("From library"), Some(MetaSource::Library)));
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w.iter().any(|x| x.contains("a.mp3.json") && x.contains("not valid")), "{w:?}");
        assert!(w.iter().any(|x| x.contains("gone.mp3")), "{w:?}");

        // A broken library.json is ignored too; wrong field types count as invalid.
        std::fs::write(dir.join(LIBRARY_FILE), r#"{ "tracks": "nope" }"#).unwrap();
        std::fs::remove_file(dir.join("a.mp3.json")).unwrap();
        let mut w = Vec::new();
        assert_eq!(load_for_file(&dir.join("a.mp3"), &mut w), (TrackMeta::default(), None));
        assert!(w.len() == 1 && w[0].contains(LIBRARY_FILE), "{w:?}");
    }

    #[test]
    fn credit_record_hashes_the_file_and_names_the_track() {
        let dir = tmp_dir("credit");
        let song = dir.join("night_drive.mp3");
        std::fs::write(&song, b"abc").unwrap();
        std::fs::write(
            dir.join("night_drive.mp3.json"),
            r#"{ "title": "Night Drive", "artist": "Ada", "licence": "CC-BY-4.0" }"#,
        )
        .unwrap();
        let file = Utf8PathBuf::from_path_buf(song).unwrap();
        let c = SoundtrackCredit::for_file(&file);
        assert_eq!(c.title, "Night Drive");
        assert_eq!(c.artist.as_deref(), Some("Ada"));
        assert_eq!((c.licence.as_str(), c.licence_label.as_str()), ("CC-BY-4.0", "CC BY 4.0"));
        assert!(c.attribution_required);
        assert!(c.credit.as_deref().unwrap().starts_with("Music: \"Night Drive\" by Ada"));
        assert_eq!(c.sha256.as_deref(), Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
        assert_eq!(c.size_bytes, 3);

        // No metadata: the file name titles it, licence unknown, no credit.
        let plain = Utf8PathBuf::from_path_buf(dir.join("golden_hour-drift.mp3")).unwrap();
        std::fs::write(&plain, b"").unwrap();
        let c = SoundtrackCredit::for_file(&plain);
        assert_eq!((c.title.as_str(), c.licence.as_str(), c.credit), ("Golden hour drift", "unknown", None));
    }

    /// The manifest gains a `soundtrack` list; manifests without music
    /// serialise exactly as before.
    #[test]
    fn manifest_carries_soundtrack_credits() {
        use crate::selection_planner::{Manifest, MANIFEST_SCHEMA_VERSION};
        let credit = SoundtrackCredit::from_meta(
            Utf8Path::new("/music/song.mp3"),
            &TrackMeta { title: Some("Song".into()), licence: Some("CC0-1.0".into()), ..Default::default() },
            Some("ab".repeat(32)),
        );
        let mut m = Manifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            project_name: "p".into(),
            output: "/out/p.mp4".into(),
            target_duration_secs: 30.0,
            actual_duration_secs: 30.0,
            resolution: "1920x1080".into(),
            fps: 30,
            seed: Some(1),
            soundtrack: vec![credit],
            sources: vec![],
            segments: vec![],
            duck_windows: vec![],
            angle_groups: vec![],
            beat_sync: None,
            generator: "test".into(),
        };
        let v = serde_json::to_value(&m).unwrap();
        let s = &v["soundtrack"][0];
        assert_eq!(s["title"], "Song");
        assert_eq!(s["licence"], "CC0-1.0");
        assert_eq!(s["sha256"], "ab".repeat(32));
        assert_eq!(s["file"], "/music/song.mp3");
        assert_eq!(s["attribution_required"], false);
        let back: Manifest = serde_json::from_value(v).unwrap();
        assert_eq!(back.soundtrack.len(), 1);

        m.soundtrack.clear();
        let v = serde_json::to_value(&m).unwrap();
        assert!(v.get("soundtrack").is_none(), "no music, no field");
        // Older manifests (no field) still load.
        let old: Manifest = serde_json::from_value(v).unwrap();
        assert!(old.soundtrack.is_empty());
    }

    #[test]
    fn titles_from_file_names() {
        assert_eq!(title_from_file_name(Path::new("/m/summer_drive-124bpm.mp3")), "Summer drive 124bpm");
        assert_eq!(title_from_file_name(Path::new("/m/__.wav")), "Untitled");
    }
}
