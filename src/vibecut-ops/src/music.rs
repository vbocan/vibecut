//! Music library: every track the server can offer as a soundtrack,
//! analysed once so a UI can browse by tempo, energy and mood and swap
//! songs without anyone typing a file path.
//!
//! Tracks come from `/projects/music` (the user's own music) plus any
//! directories listed in `VIBECUT_MUSIC_DIRS` (a bundled or shared
//! library; `:`-separated, `;` also accepted). Each track is run through
//! the same analysis the pipeline uses (fitted beat grid, energy curve,
//! loud region), so what the library shows is exactly what a render will
//! see. Results are cached in memory, keyed by path + size + mtime.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::Serialize;

use crate::error::OpsError;
use crate::paths::PROJECTS_ROOT;

/// Audio file extensions the library picks up.
const AUDIO_EXTENSIONS: [&str; 7] = ["mp3", "wav", "m4a", "aac", "flac", "ogg", "opus"];
/// How deep to look below each music directory.
const MAX_DEPTH: usize = 4;
/// Bins in the per-track energy sketch (enough for a waveform card).
const ENERGY_BINS: usize = 48;

/// One analysed track.
#[derive(Debug, Clone, Serialize)]
pub struct TrackInfo {
    /// Absolute server path: pass to the `different_music` feedback
    /// action or to `GET /api/music/file` to stream it.
    pub path: String,
    /// Title from the file name ("summer_drive-124bpm.mp3" → "Summer drive 124bpm").
    pub title: String,
    /// `"yours"` for /projects/music, `"library"` for VIBECUT_MUSIC_DIRS.
    pub collection: &'static str,
    pub duration_secs: f32,
    pub bpm: f32,
    pub beat_interval_secs: f32,
    /// First beat of the fitted grid.
    pub first_beat_secs: Option<f32>,
    /// True when the fitted beat grid is tight (machine-timed music);
    /// `None` when no grid could be fitted.
    pub grid_tight: Option<bool>,
    /// How clearly the beat pulses (≥ 1.8 drives beat-bursts).
    pub pulse_strength: f32,
    /// Normalised loudness sketch, `ENERGY_BINS` values in 0..1.
    pub energy: Vec<f32>,
    pub energy_mean: f32,
    /// Heuristic mood from tempo and energy: chill / warm / upbeat / driving.
    pub mood: &'static str,
    /// The loud section the smart loop repeats, seconds.
    pub loud_region: (f32, f32),
}

type CacheKey = (PathBuf, u64, u64);

fn cache() -> &'static Mutex<HashMap<CacheKey, TrackInfo>> {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, TrackInfo>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Music directories, user's own first.
pub fn music_dirs() -> Vec<(PathBuf, &'static str)> {
    let mut dirs = vec![(Path::new(PROJECTS_ROOT).join("music"), "yours")];
    if let Ok(extra) = std::env::var("VIBECUT_MUSIC_DIRS") {
        // OS path-list syntax (';' on Windows, ':' elsewhere), absolute
        // entries only, so a fragment never becomes a cwd-relative root.
        for d in std::env::split_paths(&extra).filter(|d| d.is_absolute()) {
            dirs.push((d, "library"));
        }
    }
    dirs
}

/// True when `path` is an audio file inside one of [`music_dirs`]
/// (after resolving symlinks and `..`), so it may be streamed.
pub fn is_library_file(path: &Path) -> bool {
    let Ok(canon) = path.canonicalize() else {
        return false;
    };
    let is_audio = canon
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()));
    is_audio
        && music_dirs()
            .iter()
            .filter_map(|(d, _)| d.canonicalize().ok())
            .any(|d| canon.starts_with(d))
}

/// List and analyse every track. New or changed files are analysed on
/// a blocking thread (a fraction of a second each); the rest come from
/// the cache.
pub async fn list_tracks() -> Result<Vec<TrackInfo>, OpsError> {
    tokio::task::spawn_blocking(list_tracks_blocking)
        .await
        .map_err(|e| OpsError::internal(format!("music scan panicked: {e}")))
}

fn list_tracks_blocking() -> Vec<TrackInfo> {
    let mut tracks = Vec::new();
    for (dir, collection) in music_dirs() {
        let mut files = Vec::new();
        collect_audio(&dir, 0, &mut files);
        files.sort();
        for f in files {
            match track_info(&f, collection) {
                Ok(t) => tracks.push(t),
                Err(e) => tracing::warn!(path = %f.display(), error = %e, "skipping unreadable track"),
            }
        }
    }
    tracks
}

fn collect_audio(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        if p.is_dir() {
            collect_audio(&p, depth + 1, out);
        } else if p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        {
            out.push(p);
        }
    }
}

fn track_info(path: &Path, collection: &'static str) -> anyhow::Result<TrackInfo> {
    let meta = std::fs::metadata(path)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let key = (path.to_path_buf(), meta.len(), mtime);
    if let Some(hit) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Ok(hit.clone());
    }
    let utf8 = camino::Utf8Path::from_path(path)
        .ok_or_else(|| anyhow::anyhow!("non-UTF-8 path {}", path.display()))?;
    let a = vibecut::audio_analysis::analyze_file(utf8)?;
    let energy = sketch(&a.energy_curve, ENERGY_BINS);
    let energy_mean = if a.energy_curve.is_empty() {
        0.0
    } else {
        a.energy_curve.iter().sum::<f32>() / a.energy_curve.len() as f32
    };
    let info = TrackInfo {
        path: path.to_string_lossy().into_owned(),
        title: title_from(path),
        collection,
        duration_secs: a.duration_secs,
        bpm: a.tempo_bpm,
        beat_interval_secs: a.beat_interval_secs(),
        first_beat_secs: a.beats.first().copied(),
        grid_tight: a.beat_grid.map(|g| g.is_tight()),
        pulse_strength: a.pulse_strength,
        energy,
        energy_mean,
        mood: mood(a.tempo_bpm, energy_mean),
        loud_region: a.detect_loud_region(0.80),
    };
    cache().lock().unwrap_or_else(|e| e.into_inner()).insert(key, info.clone());
    Ok(info)
}

/// Max-pool `curve` into `bins` values, normalised to the loudest bin.
fn sketch(curve: &[f32], bins: usize) -> Vec<f32> {
    if curve.is_empty() {
        return vec![0.0; bins];
    }
    let mut out: Vec<f32> = (0..bins)
        .map(|b| {
            let lo = b * curve.len() / bins;
            let hi = ((b + 1) * curve.len() / bins).max(lo + 1).min(curve.len());
            curve[lo..hi].iter().cloned().fold(0.0, f32::max)
        })
        .collect();
    let max = out.iter().cloned().fold(0.0_f32, f32::max);
    if max > 0.0 {
        out.iter_mut().for_each(|v| *v /= max);
    }
    out
}

/// Tempo × energy → a mood word people pick music by. A heuristic, not
/// a classifier: slow and sparse reads chill, fast and dense reads driving.
fn mood(bpm: f32, energy_mean: f32) -> &'static str {
    match (bpm, energy_mean) {
        (b, e) if b >= 118.0 && e >= 0.45 => "driving",
        (b, _) if b >= 105.0 => "upbeat",
        (_, e) if e >= 0.5 => "warm",
        _ => "chill",
    }
}

/// "summer_drive-124bpm.mp3" → "Summer drive 124bpm".
fn title_from(path: &Path) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_read_like_titles() {
        assert_eq!(title_from(Path::new("/m/summer_drive-124bpm.mp3")), "Summer drive 124bpm");
        assert_eq!(title_from(Path::new("/m/__.wav")), "Untitled");
    }

    #[test]
    fn sketch_max_pools_and_normalises() {
        let s = sketch(&[0.0, 0.5, 0.25, 1.0, 0.2, 0.1], 3);
        assert_eq!(s, vec![0.5, 1.0, 0.2]);
        assert_eq!(sketch(&[], 4), vec![0.0; 4]);
    }

    #[test]
    fn moods_follow_tempo_and_energy() {
        assert_eq!(mood(128.0, 0.6), "driving");
        assert_eq!(mood(110.0, 0.2), "upbeat");
        assert_eq!(mood(90.0, 0.7), "warm");
        assert_eq!(mood(80.0, 0.2), "chill");
    }
}
