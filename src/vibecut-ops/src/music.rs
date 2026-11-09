//! Music library: every track the server can offer as a soundtrack,
//! analysed once so a UI can browse by tempo, energy and mood and swap
//! songs without anyone typing a file path.
//!
//! Tracks come from `/projects/music` (the user's own music) plus any
//! directories listed in `VIBECUT_MUSIC_DIRS` (a bundled or shared
//! library; `:`-separated, `;` also accepted). Each track is run through
//! the same analysis the pipeline uses (fitted beat grid, energy curve,
//! loud region), so what the library shows is exactly what a render will
//! see. Results are cached in memory and on disk
//! (`/projects/.vibecut-cache`), keyed by path + size + mtime, so a big
//! downloads folder is analysed once, not on every server start.
//!
//! Who made a track and under what licence comes from optional JSON next
//! to it (`vibecut::track_meta`: `<track>.json` or a folder
//! `library.json`). A `starter/` folder directly inside a music directory
//! holds the vetted starter pack: its tracks are listed as `"starter"`
//! when their licence is CC0 or CC BY (see MUSIC.md).
//!
//! [`auto_pick`] chooses a song for a vibe so every first render has
//! music; `propose` and `GET /api/music/pick` both call it, so the UI's
//! "Picked for you" and the proposed config can't disagree.
//!
//! Only one scan runs at a time: callers that arrive while one is in
//! flight wait for it and share its result, so a cold library (first
//! start, a batch of new downloads) is decoded once, not once per request.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use vibecut::disk_cache::{Cache, Scope};
use vibecut::track_meta::{FolderMeta, Licence, LicenceKind, MetaSource, TrackMeta};

use crate::error::OpsError;
use crate::paths::PROJECTS_ROOT;
use crate::vibes::Vibe;

/// Audio file extensions the library picks up.
const AUDIO_EXTENSIONS: [&str; 7] = ["mp3", "wav", "m4a", "aac", "flac", "ogg", "opus"];
/// How deep to look below each music directory.
const MAX_DEPTH: usize = 4;
/// Bins in the per-track energy sketch (enough for a waveform card).
const ENERGY_BINS: usize = 48;
/// Folder (directly inside a music directory) holding the starter pack.
pub const STARTER_DIR: &str = "starter";
/// On-disk analysis cache scope; bump the version when [`TrackAnalysis`]
/// or the analysis itself changes.
const CACHE_SCOPE: Scope = Scope("music");
const ANALYSIS_VERSION: u32 = 1;
/// Shortest section the smart loop can repeat without the seam being the
/// whole song (a few bars at any tempo).
const LOOP_MIN_SECS: f32 = 8.0;

/// What the analyser measured. Cached (memory + disk), so it never holds
/// anything that can change without the audio file changing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackAnalysis {
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
    /// The loud section the smart loop repeats, seconds.
    pub loud_region: (f32, f32),
}

/// A licence as the library reports it: normalised id, label, kind and
/// a plain-language note for someone about to post.
#[derive(Debug, Clone, Serialize)]
pub struct LicenceInfo {
    pub id: String,
    pub label: String,
    pub kind: LicenceKind,
    pub note: &'static str,
}

impl From<Licence> for LicenceInfo {
    fn from(l: Licence) -> Self {
        LicenceInfo { note: l.note(), id: l.id, label: l.label, kind: l.kind }
    }
}

/// One analysed track.
#[derive(Debug, Clone, Serialize)]
pub struct TrackInfo {
    /// Absolute server path: pass to the `different_music` feedback
    /// action, to `propose` as `soundtrack`, or to `GET /api/music/file`.
    pub path: String,
    /// Title from the metadata, else from the file name
    /// ("summer_drive-124bpm.mp3" → "Summer drive 124bpm").
    pub title: String,
    /// `"yours"` for /projects/music, `"library"` for VIBECUT_MUSIC_DIRS,
    /// `"starter"` for the vetted starter pack.
    pub collection: &'static str,
    pub artist: Option<String>,
    pub licence: LicenceInfo,
    /// A public post must credit the track (CC BY and friends).
    pub attribution_required: bool,
    /// Credit line for captions: the metadata's own, or one built from
    /// title / artist / source / licence when attribution is required.
    pub credit: Option<String>,
    pub source_url: Option<String>,
    /// Tempo the metadata declares (the detected `bpm` drives renders).
    pub declared_bpm: Option<f32>,
    /// Where the metadata came from: `"sidecar"`, `"library"` or `null`.
    pub metadata: Option<MetaSource>,
    #[serde(flatten)]
    pub analysis: TrackAnalysis,
    /// Heuristic mood from tempo and energy: chill / warm / upbeat / driving.
    pub mood: &'static str,
    /// Steady beat and a long enough loud section that the smart loop
    /// can repeat it without an audible seam.
    pub loops_cleanly: bool,
}

/// The whole library plus anything worth telling the user about it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Library {
    pub tracks: Vec<TrackInfo>,
    /// Metadata files that were unreadable or invalid, starter-pack tracks
    /// without an accepted licence, unreadable audio.
    pub warnings: Vec<String>,
}

type CacheKey = (PathBuf, u64, u64);

fn cache() -> &'static Mutex<HashMap<CacheKey, TrackAnalysis>> {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, TrackAnalysis>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Where the library lives, when not the default (`/projects/music` plus
/// `VIBECUT_MUSIC_DIRS`, analysis cache in `/projects/.vibecut-cache`).
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryRoots {
    /// The user's own folder: where downloads go.
    pub own: PathBuf,
    /// Further folders, listed as `"library"`.
    pub extra: Vec<PathBuf>,
    /// Folder for the on-disk analysis cache; `None` = memory only.
    pub cache_dir: Option<PathBuf>,
}

static ROOTS: RwLock<Option<LibraryRoots>> = RwLock::new(None);

/// Serve the library from `roots` instead of the default folders, for this
/// whole process. For tests of crates that serve the library (they must
/// never scan or write into a real `/projects`) and for embedders.
pub fn use_roots(roots: LibraryRoots) {
    *ROOTS.write().unwrap_or_else(|e| e.into_inner()) = Some(roots);
}

fn roots_override() -> Option<LibraryRoots> {
    ROOTS.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The analysis cache a scan should use: the override's folder, else
/// `/projects/.vibecut-cache` when `/projects` exists. This crate's own
/// tests never touch the default one; other crates' tests use [`use_roots`].
fn disk_cache() -> Option<Cache> {
    let open = |dir: PathBuf| {
        let dir = camino::Utf8PathBuf::from_path_buf(dir).ok()?;
        Cache::open(&dir, true)
            .map_err(|e| tracing::warn!(error = %e, "music analysis cache unavailable"))
            .ok()
    };
    if let Some(r) = roots_override() {
        return r.cache_dir.and_then(open);
    }
    static DEFAULT: OnceLock<Option<Cache>> = OnceLock::new();
    DEFAULT
        .get_or_init(|| {
            let root = Path::new(PROJECTS_ROOT);
            if cfg!(test) || !root.is_dir() {
                return None;
            }
            open(root.join(".vibecut-cache"))
        })
        .clone()
}

/// The user's own music folder (`/projects/music`): where to put downloads.
pub fn own_music_dir() -> PathBuf {
    match roots_override() {
        Some(r) => r.own,
        None => Path::new(PROJECTS_ROOT).join("music"),
    }
}

/// Music directories, user's own first, then the library folders, then
/// the folders the user picked on a drive ([`add_folder`]).
pub fn music_dirs() -> Vec<(PathBuf, &'static str)> {
    let mut dirs = if let Some(r) = roots_override() {
        let mut dirs = vec![(r.own, "yours")];
        dirs.extend(r.extra.into_iter().map(|d| (d, "library")));
        dirs
    } else {
        let mut dirs = vec![(own_music_dir(), "yours")];
        if let Ok(extra) = std::env::var("VIBECUT_MUSIC_DIRS") {
            // OS path-list syntax (';' on Windows, ':' elsewhere), absolute
            // entries only, so a fragment never becomes a cwd-relative root.
            for d in std::env::split_paths(&extra).filter(|d| d.is_absolute()) {
                dirs.push((d, "library"));
            }
        }
        dirs
    };
    for d in picked_folders() {
        if !dirs.iter().any(|(have, _)| *have == d) {
            dirs.push((d, "yours"));
        }
    }
    dirs
}

/// Where the picked music folders are kept: beside the user's own music
/// folder (`/projects/.vibecut-music-folders.json`).
fn picked_file() -> PathBuf {
    let own = own_music_dir();
    own.parent().unwrap_or(&own).join(".vibecut-music-folders.json")
}

/// Serialises read-modify-write of the picked folders file.
static PICKED_LOCK: Mutex<()> = Mutex::new(());

/// Music folders the user picked on a drive ("Browse your drives…" on the
/// song step), as container paths. Unreadable or odd entries are skipped.
pub fn picked_folders() -> Vec<PathBuf> {
    picked_in(&picked_file())
}

fn picked_in(file: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<String>>(&text)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| (p.starts_with('/') || Path::new(p).is_absolute()) && !Path::new(p).components().any(|c| matches!(c, Component::ParentDir)))
        .map(PathBuf::from)
        .collect()
}

fn save_picked(file: &Path, folders: &[PathBuf]) -> Result<(), OpsError> {
    let list: Vec<String> = folders.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let text = serde_json::to_string_pretty(&list).map_err(|e| OpsError::internal(format!("{e}")))?;
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, &file))
        .map_err(|e| OpsError::internal(format!("could not save the music folders to {}: {e}", file.display())))
}

/// What [`add_folder`] did.
#[derive(Debug, Clone, Serialize)]
pub struct AddedFolder {
    /// The folder as the server reads it.
    pub container_path: String,
    /// The folder as the user knows it (`D:\Music\Road trip`).
    pub path: String,
    /// It was already in the library (itself or a folder above it).
    pub already: bool,
}

/// Add a folder on a drive (a display path like `D:\Music` or a container
/// path, see [`crate::browse`]) to the music library. Its songs are
/// listed with the user's own, can be previewed, and renders may read
/// them (`paths::extra_read_roots` includes every music folder). A whole
/// drive, hidden folders and anything outside the browse roots are
/// refused.
pub async fn add_folder(path: &str) -> Result<AddedFolder, OpsError> {
    let path = path.to_string();
    tokio::task::spawn_blocking(move || {
        let (dir, display) = crate::browse::resolve_folder(&path)?;
        let _guard = PICKED_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let have: Vec<PathBuf> = music_dirs().into_iter().map(|(d, _)| d).collect();
        add_in(&picked_file(), &have, dir, display)
    })
    .await
    .map_err(|e| OpsError::internal(format!("music folder task panicked: {e}")))?
}

/// [`add_folder`] once the folder is resolved and checked: `file` keeps
/// the picked folders, `have` is every music folder already read.
fn add_in(file: &Path, have: &[PathBuf], dir: PathBuf, display: String) -> Result<AddedFolder, OpsError> {
    let canon = dir.canonicalize().unwrap_or_else(|_| dir.clone());
    let already = have.iter().filter_map(|d| d.canonicalize().ok()).any(|d| canon.starts_with(d));
    if !already {
        let mut picked = picked_in(file);
        // A folder inside the new one is now covered by it.
        picked.retain(|p| !p.canonicalize().is_ok_and(|c| c.starts_with(&canon)));
        picked.push(dir.clone());
        save_picked(file, &picked)?;
    }
    Ok(AddedFolder { container_path: dir.to_string_lossy().into_owned(), path: display, already })
}

/// A folder on a drive that likely holds the user's songs and isn't in
/// the library yet: someone's Downloads or Music folder.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SuggestedFolder {
    /// As the user knows it (`C:\Users\me\Downloads`): what [`add_folder`] takes.
    pub path: String,
    pub container_path: String,
    /// Songs in it and one level below (a lower bound past 500).
    pub n_songs: u32,
}

/// Folder names that hold songs: browsers save to Downloads.
const LIKELY_MUSIC: [&str; 2] = ["Downloads", "Music"];
/// Profile folders that are never a person's.
const NOT_PEOPLE: [&str; 4] = ["Public", "Default", "Default User", "All Users"];

/// Downloads and Music folders of every user on every drive (Windows
/// `Users\*`, Linux `home/*`, a macOS `/Users` mount's `*`) that hold
/// songs and aren't read already. For "Read my Downloads folder".
pub async fn suggested_folders() -> Vec<SuggestedFolder> {
    tokio::task::spawn_blocking(|| {
        let have: Vec<PathBuf> = music_dirs().into_iter().map(|(d, _)| d).collect();
        suggested_in(&crate::browse::roots(), &have)
    })
    .await
    .unwrap_or_default()
}

pub(crate) fn suggested_in(roots: &[crate::browse::BrowseRoot], have: &[PathBuf]) -> Vec<SuggestedFolder> {
    let have: Vec<PathBuf> = have.iter().filter_map(|d| d.canonicalize().ok()).collect();
    let mut out = Vec::new();
    for r in roots.iter().filter(|r| r.kind != crate::browse::RootKind::Footage) {
        // People's folders: `<root>/Users/<name>`, `<root>/home/<name>`,
        // or `<root>/<name>` when the root is a users folder itself.
        let mut people: Vec<Vec<String>> = Vec::new();
        for base in [Some("Users"), Some("home"), None] {
            let dir = match base {
                Some(b) => r.container.join(b),
                None => r.container.clone(),
            };
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten().take(200) {
                let name = e.file_name().to_string_lossy().into_owned();
                let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
                if !is_dir || crate::browse::is_hidden_name(&name) || NOT_PEOPLE.contains(&name.as_str()) {
                    continue;
                }
                let mut rel: Vec<String> = base.map(|b| vec![b.to_string()]).unwrap_or_default();
                rel.push(name);
                people.push(rel);
            }
        }
        for person in people {
            for sub in LIKELY_MUSIC {
                let mut rel = person.clone();
                rel.push(sub.to_string());
                let dir = rel.iter().fold(r.container.clone(), |p, c| p.join(c));
                let Ok(canon) = dir.canonicalize() else { continue };
                if !canon.is_dir() || have.iter().any(|h| canon.starts_with(h)) || out.iter().any(|s: &SuggestedFolder| Path::new(&s.container_path) == dir) {
                    continue;
                }
                let n = count_songs(&dir, 1, 500);
                if n > 0 {
                    out.push(SuggestedFolder {
                        path: r.display_of(&rel),
                        container_path: dir.to_string_lossy().into_owned(),
                        n_songs: n,
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| b.n_songs.cmp(&a.n_songs).then(a.path.cmp(&b.path)));
    out.truncate(6);
    out
}

/// Audio files in `dir` and up to `depth` levels below, stopping at `cap`.
fn count_songs(dir: &Path, depth: usize, cap: u32) -> u32 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut n = 0;
    for e in entries.flatten().take(5000) {
        if n >= cap {
            break;
        }
        let Ok(ft) = e.file_type() else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        if ft.is_file() && is_audio_name(&name) {
            n += 1;
        } else if ft.is_dir() && depth > 0 && !name.starts_with('.') {
            n += count_songs(&e.path(), depth - 1, cap - n);
        }
    }
    n.min(cap)
}

/// Take a picked folder out of the library (its songs stay on the drive).
/// `path` is the container path [`music_dirs`] lists, or its display path.
/// Returns whether it was there.
pub fn remove_folder(path: &str) -> Result<bool, OpsError> {
    let mapped = crate::browse::display_to_container(&crate::browse::roots(), path);
    let target = mapped.unwrap_or_else(|| PathBuf::from(path.trim()));
    let _guard = PICKED_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    remove_in(&picked_file(), &target)
}

fn remove_in(file: &Path, target: &Path) -> Result<bool, OpsError> {
    let mut picked = picked_in(file);
    let before = picked.len();
    picked.retain(|p| p != target);
    if picked.len() == before {
        return Ok(false);
    }
    save_picked(file, &picked)?;
    Ok(true)
}

/// True when `path` is an audio file inside one of [`music_dirs`]
/// (after resolving symlinks and `..`), so it may be streamed.
pub fn is_library_file(path: &Path) -> bool {
    let Ok(canon) = path.canonicalize() else {
        return false;
    };
    is_audio(&canon)
        && music_dirs()
            .iter()
            .filter_map(|(d, _)| d.canonicalize().ok())
            .any(|d| canon.starts_with(d))
}

/// An audio file name the library reads (by extension).
pub fn is_audio_name(name: &str) -> bool {
    !name.starts_with('.') && is_audio(Path::new(name))
}

fn is_audio(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Check a soundtrack file a caller named (propose's `soundtrack`): an
/// audio file inside the projects folder or a music directory, after
/// resolving `..` and symlinks. A relative path is taken from the
/// projects folder. Returns the path to write into the config.
///
/// A path outside those folders is refused the same way whether or not it
/// exists, so the check can't be used to probe the server's filesystem.
pub fn check_soundtrack_file(requested: &str) -> Result<PathBuf, OpsError> {
    let mut roots: Vec<PathBuf> = music_dirs().into_iter().map(|(d, _)| d).collect();
    roots.push(PathBuf::from(PROJECTS_ROOT));
    let requested = requested.trim();
    let path = if requested.starts_with('/') || Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        Path::new(PROJECTS_ROOT).join(requested)
    };
    soundtrack_file_in(&path, &roots)
}

fn soundtrack_file_in(path: &Path, roots: &[PathBuf]) -> Result<PathBuf, OpsError> {
    let refuse = || {
        OpsError::invalid_with_hint(
            format!("soundtrack must be an audio file in your music folder or projects folder: {:?}", path.display().to_string()),
            format!("put the song in {} and pick it from the library", own_music_dir().display()),
        )
    };
    let lexical_ok = path.is_absolute()
        && is_audio(path)
        && !path.components().any(|c| matches!(c, Component::ParentDir))
        && roots.iter().any(|r| path.starts_with(r));
    if !lexical_ok {
        return Err(refuse());
    }
    // Named inside a root: now it may be missing, or a link out of it.
    let canon = match path.canonicalize() {
        Ok(c) if c.is_file() => c,
        _ => return Err(OpsError::not_found_with("soundtrack file", path.display().to_string())),
    };
    if !roots.iter().filter_map(|r| r.canonicalize().ok()).any(|r| canon.starts_with(r)) || !is_audio(&canon) {
        return Err(refuse());
    }
    Ok(path.to_path_buf())
}

/// List and analyse every track. New or changed files are analysed on
/// a blocking thread (a fraction of a second each); the rest come from
/// the cache.
pub async fn list_tracks() -> Result<Vec<TrackInfo>, OpsError> {
    Ok(list_library().await?.tracks)
}

/// [`list_tracks`] plus the warnings the scan produced.
pub async fn list_library() -> Result<Library, OpsError> {
    scan_shared(music_dirs()).await
}

/// Start analysing the library in the background at server start, so the
/// first proposal doesn't wait for a cold library; a request that arrives
/// meanwhile joins this scan instead of starting its own.
pub fn warm_up() {
    tokio::spawn(async {
        match list_library().await {
            Ok(lib) => tracing::info!(tracks = lib.tracks.len(), "music library analysed"),
            Err(e) => tracing::warn!(error = %e, "music library warm-up failed"),
        }
    });
}

/// The most recent scan, kept so callers that queued behind it can share it.
struct LastScan {
    dirs: Vec<(PathBuf, &'static str)>,
    finished: Instant,
    lib: Library,
}

static SCAN: tokio::sync::Mutex<Option<LastScan>> = tokio::sync::Mutex::const_new(None);

/// Scan `dirs`, one scan at a time. A caller that arrives while a scan of
/// the same folders is running waits for it and gets its result rather
/// than starting a second scan over the same uncached files.
async fn scan_shared(dirs: Vec<(PathBuf, &'static str)>) -> Result<Library, OpsError> {
    let asked = Instant::now();
    let mut last = SCAN.lock().await;
    if let Some(prev) = last.as_ref() {
        if prev.finished >= asked && prev.dirs == dirs {
            return Ok(prev.lib.clone());
        }
    }
    let scan_dirs = dirs.clone();
    let lib = tokio::task::spawn_blocking(move || scan(&scan_dirs, disk_cache().as_ref()))
        .await
        .map_err(|e| OpsError::internal(format!("music scan panicked: {e}")))?;
    *last = Some(LastScan { dirs, finished: Instant::now(), lib: lib.clone() });
    Ok(lib)
}

fn scan(dirs: &[(PathBuf, &'static str)], disk_cache: Option<&Cache>) -> Library {
    let mut lib = Library::default();
    for (dir, collection) in dirs {
        let mut files = Vec::new();
        collect_audio(dir, 0, &mut files);
        files.sort();
        let mut folders: HashMap<PathBuf, FolderMeta> = HashMap::new();
        for f in files {
            let analysis = match analyse_with(&f, disk_cache) {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!(path = %f.display(), error = %e, "skipping unreadable track");
                    lib.warnings.push(format!("{}: can't read this audio file ({e}); skipped", f.display()));
                    continue;
                }
            };
            let parent = f.parent().unwrap_or(Path::new("")).to_path_buf();
            let folder = folders
                .entry(parent.clone())
                .or_insert_with(|| FolderMeta::load(&parent, &mut lib.warnings));
            let (meta, source) = folder.for_track(&f, &mut lib.warnings);
            let collection = collection_for(dir, &f, collection, &meta, &mut lib.warnings);
            lib.tracks.push(track_info(&f, collection, meta, source, analysis));
        }
    }
    for w in &lib.warnings {
        tracing::warn!("music library: {w}");
    }
    lib
}

/// `"starter"` for a track under `<root>/starter/` whose licence the
/// starter pack accepts (CC0 / CC BY); anything else keeps the root's
/// collection — with a warning when it sits in `starter/` unvetted.
fn collection_for(
    root: &Path,
    track: &Path,
    root_collection: &'static str,
    meta: &TrackMeta,
    warnings: &mut Vec<String>,
) -> &'static str {
    let in_starter = track
        .strip_prefix(root)
        .ok()
        .and_then(|rel| rel.components().next())
        .is_some_and(|c| c.as_os_str() == STARTER_DIR);
    if !in_starter {
        return root_collection;
    }
    let lic = meta.licence();
    if lic.kind.starter_eligible() {
        "starter"
    } else {
        warnings.push(format!(
            "{}: in the starter pack folder but its licence is {} — only CC0 and CC BY tracks are labelled \"Starter pack\"",
            track.display(),
            if lic.kind == LicenceKind::Unknown { "not recorded".to_string() } else { lic.label }
        ));
        root_collection
    }
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
        } else if is_audio(&p) {
            out.push(p);
        }
    }
}

/// How many times each file was actually decoded (tests: one scan at a time
/// means each new file is decoded once).
#[cfg(test)]
static DECODES: Mutex<Option<HashMap<PathBuf, usize>>> = Mutex::new(None);

/// Analyse one file, via the memory cache, then the disk cache.
fn analyse_with(path: &Path, disk_cache: Option<&Cache>) -> anyhow::Result<TrackAnalysis> {
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
    let disk = disk_cache.and_then(|c| {
        let k = Cache::key_for_file(utf8, &ANALYSIS_VERSION).ok()?;
        Some((c, k))
    });
    if let Some(hit) = disk.as_ref().and_then(|(c, k)| c.load::<TrackAnalysis>(k, CACHE_SCOPE)) {
        cache().lock().unwrap_or_else(|e| e.into_inner()).insert(key, hit.clone());
        return Ok(hit);
    }
    #[cfg(test)]
    {
        let mut d = DECODES.lock().unwrap_or_else(|e| e.into_inner());
        *d.get_or_insert_with(HashMap::new).entry(path.to_path_buf()).or_default() += 1;
    }
    let a = vibecut::audio_analysis::analyze_file(utf8)?;
    let energy_mean = if a.energy_curve.is_empty() {
        0.0
    } else {
        a.energy_curve.iter().sum::<f32>() / a.energy_curve.len() as f32
    };
    let analysis = TrackAnalysis {
        duration_secs: a.duration_secs,
        bpm: a.tempo_bpm,
        beat_interval_secs: a.beat_interval_secs(),
        first_beat_secs: a.beats.first().copied(),
        grid_tight: a.beat_grid.map(|g| g.is_tight()),
        pulse_strength: a.pulse_strength,
        energy: sketch(&a.energy_curve, ENERGY_BINS),
        energy_mean,
        loud_region: a.detect_loud_region(0.80),
    };
    if let Some((c, k)) = &disk {
        if let Err(e) = c.save(k, CACHE_SCOPE, &analysis) {
            tracing::debug!(error = %e, "could not write music analysis cache");
        }
    }
    cache().lock().unwrap_or_else(|e| e.into_inner()).insert(key, analysis.clone());
    Ok(analysis)
}

fn track_info(
    path: &Path,
    collection: &'static str,
    meta: TrackMeta,
    source: Option<MetaSource>,
    analysis: TrackAnalysis,
) -> TrackInfo {
    let file_title = title_from(path);
    TrackInfo {
        path: path.to_string_lossy().into_owned(),
        credit: meta.credit_line(&file_title),
        attribution_required: meta.attribution_required(),
        licence: meta.licence().into(),
        title: meta.title.unwrap_or(file_title),
        collection,
        artist: meta.artist,
        source_url: meta.source_url,
        declared_bpm: meta.bpm,
        metadata: source,
        mood: mood(analysis.bpm, analysis.energy_mean),
        loops_cleanly: loops_cleanly(&analysis),
        analysis,
    }
}

/// The engine's smart loop repeats the loud section on whole beats with a
/// beat-aligned crossfade (`run_pipeline::soundtrack_loop_plan`), so a
/// short track loops cleanly when its beat grid is tight and the section
/// it repeats spans at least [`LOOP_MIN_SECS`].
fn loops_cleanly(a: &TrackAnalysis) -> bool {
    let (start, end) = a.loud_region;
    let smart = start > 0.5 || end < a.duration_secs - 0.5;
    let span = if smart && end > start { end - start } else { a.duration_secs };
    a.grid_tight == Some(true) && span >= LOOP_MIN_SECS
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
    vibecut::track_meta::title_from_file_name(path)
}

// ---------- Auto-pick ----------

/// Score components' weights: tempo matters most, then whether the song
/// covers the cut, then energy; mood and beat clarity break ties.
const W_TEMPO: f32 = 0.35;
const W_LENGTH: f32 = 0.25;
const W_ENERGY: f32 = 0.20;
const W_MOOD: f32 = 0.10;
const W_BEAT: f32 = 0.10;
/// Tracks within this much of the best score count as equally good; the
/// seed chooses among them (at most [`MAX_CLOSE`]).
const NEAR_TIE: f32 = 0.04;
const MAX_CLOSE: usize = 3;

/// The server's song choice for a vibe, with the reason in plain words.
#[derive(Debug, Clone, Serialize)]
pub struct AutoPick {
    pub path: String,
    pub title: String,
    /// Why this track: tempo against the vibe's band, energy, length.
    pub reason: String,
    pub score: f32,
    /// How many tracks scored about as well (the seed chose among them).
    pub close_matches: usize,
    /// The track is at least as long as the cut.
    pub covers_length: bool,
    pub loops_cleanly: bool,
}

/// How a track's tempo relates to a vibe's band.
#[derive(Debug, Clone, Copy, PartialEq)]
enum TempoFit {
    InBand,
    /// Half or double this tempo is in the band.
    Octave(f32),
    Near,
    Off,
}

/// 1 inside `[lo, hi]`, falling linearly to 0 at `falloff` outside it.
fn band_fit(x: f32, (lo, hi): (f32, f32), falloff: f32) -> f32 {
    let d = if x < lo { lo - x } else if x > hi { x - hi } else { 0.0 };
    (1.0 - d / falloff).max(0.0)
}

fn tempo_fit(bpm: f32, band: (f32, f32)) -> (f32, TempoFit) {
    if !(bpm > 0.0) {
        return (0.0, TempoFit::Off);
    }
    let direct = band_fit(bpm, band, 25.0);
    let (oct_bpm, octave) = [bpm / 2.0, bpm * 2.0]
        .into_iter()
        .map(|b| (b, 0.75 * band_fit(b, band, 25.0)))
        .fold((0.0, 0.0_f32), |best, c| if c.1 > best.1 { c } else { best });
    if direct >= 1.0 {
        (1.0, TempoFit::InBand)
    } else if octave >= 0.75 && octave > direct {
        (octave, TempoFit::Octave(oct_bpm))
    } else if direct.max(octave) > 0.0 {
        (direct.max(octave), TempoFit::Near)
    } else {
        (0.0, TempoFit::Off)
    }
}

struct Scored<'a> {
    track: &'a TrackInfo,
    score: f32,
    tempo: TempoFit,
    covers: bool,
}

fn score<'a>(t: &'a TrackInfo, vibe: &Vibe, target_secs: f32) -> Scored<'a> {
    let a = &t.analysis;
    let (tempo, fit) = tempo_fit(a.bpm, vibe.music.bpm);
    let energy = band_fit(a.energy_mean, vibe.music.energy, 0.3);
    let mood = match vibe.music.moods.iter().position(|m| *m == t.mood) {
        Some(0) => 1.0,
        Some(1) => 0.7,
        Some(_) => 0.5,
        None => 0.2,
    };
    let covers = a.duration_secs + 0.5 >= target_secs;
    let length = if covers {
        1.0
    } else if t.loops_cleanly {
        0.75
    } else if target_secs > 0.0 {
        0.35 * (a.duration_secs / target_secs).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let beat = match a.grid_tight {
        Some(true) => 1.0,
        Some(false) => 0.6,
        None => 0.2,
    };
    let score = W_TEMPO * tempo + W_LENGTH * length + W_ENERGY * energy + W_MOOD * mood + W_BEAT * beat;
    Scored { track: t, score, tempo: fit, covers }
}

/// Pick the library track that best suits `vibe` for a `target_secs` cut.
///
/// Scores tempo against the vibe's band (half / double time counts),
/// mean energy against its energy band, mood, beat clarity, and length:
/// a track at least as long as the cut beats one that has to loop, and
/// one that loops cleanly (steady beat, long loud section) beats one that
/// doesn't. Tracks within [`NEAR_TIE`] of the best are equally good; the
/// `seed` chooses among them, so the same seed always gives the same song
/// and a new seed can give a different one. `None` for an empty library.
pub fn auto_pick(tracks: &[TrackInfo], vibe: &Vibe, target_secs: f32, seed: u64) -> Option<AutoPick> {
    let mut scored: Vec<Scored<'_>> = tracks.iter().map(|t| score(t, vibe, target_secs)).collect();
    scored.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.track.path.cmp(&b.track.path)));
    let best = scored.first()?.score;
    let close: Vec<&Scored<'_>> = scored.iter().take_while(|s| s.score >= best - NEAR_TIE).take(MAX_CLOSE).collect();
    let mix = vibe.key.bytes().fold(0xcbf29ce484222325_u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    let i = (vibecut::rng::splitmix64(seed ^ mix) % close.len() as u64) as usize;
    let s = close[i];
    Some(AutoPick {
        path: s.track.path.clone(),
        title: s.track.title.clone(),
        reason: reason(s, vibe, target_secs, close.len(), seed),
        score: s.score,
        close_matches: close.len(),
        covers_length: s.covers,
        loops_cleanly: s.track.loops_cleanly,
    })
}

/// Seconds → "3:12".
fn clock(secs: f32) -> String {
    let s = secs.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Seconds → "30 s" / "3 min" / "1 min 30 s".
fn span(secs: f32) -> String {
    let s = secs.max(0.0).round() as u64;
    match (s / 60, s % 60) {
        (0, r) => format!("{r} s"),
        (m, 0) => format!("{m} min"),
        (m, r) => format!("{m} min {r} s"),
    }
}

fn reason(s: &Scored<'_>, vibe: &Vibe, target_secs: f32, close: usize, seed: u64) -> String {
    let a = &s.track.analysis;
    let (lo, hi) = vibe.music.bpm;
    let band = format!("{}'s {lo:.0}–{hi:.0} BPM range", vibe.name);
    let bpm = a.bpm.round();
    let tempo = match s.tempo {
        TempoFit::InBand => format!("{bpm:.0} BPM sits in {band}"),
        TempoFit::Octave(felt) => format!("{bpm:.0} BPM feels like {:.0} BPM, inside {band}", felt.round()),
        TempoFit::Near => format!("{bpm:.0} BPM is close to {band}"),
        TempoFit::Off if a.bpm > 0.0 => format!("{bpm:.0} BPM is the nearest the library has to {band}"),
        TempoFit::Off => "it has no clear beat, but it's the best the library has".to_string(),
    };
    let feel = if vibe.music.moods.contains(&s.track.mood) {
        format!("its {} feel suits the vibe", s.track.mood)
    } else {
        format!("its {} feel is the closest match", s.track.mood)
    };
    let length = if s.covers {
        format!("at {} it covers the whole {} cut", clock(a.duration_secs), span(target_secs))
    } else if s.track.loops_cleanly {
        format!(
            "it's shorter than the {} cut ({}) but its steady beat loops cleanly",
            span(target_secs),
            clock(a.duration_secs)
        )
    } else {
        format!("it's shorter than the {} cut ({}) and will repeat", span(target_secs), clock(a.duration_secs))
    };
    let mut out = format!("{tempo}, {feel}, and {length}.");
    if close > 1 {
        out.push_str(&format!(" One of {close} close matches; seed {seed} chose this one."));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vibes;

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

    fn analysis(bpm: f32, energy_mean: f32, duration: f32, tight: bool) -> TrackAnalysis {
        TrackAnalysis {
            duration_secs: duration,
            bpm,
            beat_interval_secs: 60.0 / bpm,
            first_beat_secs: Some(0.1),
            grid_tight: Some(tight),
            pulse_strength: 3.0,
            energy: vec![energy_mean; 4],
            energy_mean,
            loud_region: (0.0, duration),
        }
    }

    fn track(path: &str, bpm: f32, energy_mean: f32, duration: f32, tight: bool) -> TrackInfo {
        let p = Path::new(path);
        track_info(p, "yours", TrackMeta::default(), None, analysis(bpm, energy_mean, duration, tight))
    }

    fn vibe(key: &str) -> &'static Vibe {
        vibes::get(key).unwrap()
    }

    #[test]
    fn auto_pick_follows_the_vibe() {
        let lib = vec![
            track("/m/night_drive.mp3", 132.0, 0.62, 200.0, true),
            track("/m/waves.mp3", 72.0, 0.22, 200.0, true),
            track("/m/golden.mp3", 98.0, 0.55, 200.0, true),
        ];
        let pick = |v: &str| auto_pick(&lib, vibe(v), 60.0, 42).unwrap();
        assert_eq!(pick("action_highlight").path, "/m/night_drive.mp3");
        assert_eq!(pick("drone_cinematic").path, "/m/waves.mp3");
        assert_eq!(pick("documentary").path, "/m/waves.mp3");
        assert_eq!(pick("family_chronicle").path, "/m/golden.mp3");
        let p = pick("action_highlight");
        assert!(p.reason.contains("132 BPM sits in Action highlight's 120–175 BPM range"), "{}", p.reason);
        assert!(p.reason.contains("driving feel suits the vibe"), "{}", p.reason);
        assert!(p.reason.contains("covers the whole 1 min cut"), "{}", p.reason);
        assert!(p.covers_length);
        assert_eq!(p.close_matches, 1);
    }

    #[test]
    fn auto_pick_counts_half_and_double_time() {
        // 66 BPM drives a 132 BPM edit fine (cut on every beat of the
        // double-time feel), so it beats a track far outside the band.
        let lib = vec![track("/m/slow.mp3", 66.0, 0.6, 300.0, true), track("/m/mid.mp3", 95.0, 0.6, 300.0, true)];
        let p = auto_pick(&lib, vibe("action_highlight"), 90.0, 1).unwrap();
        assert_eq!(p.path, "/m/slow.mp3");
        assert!(p.reason.contains("66 BPM feels like 132 BPM"), "{}", p.reason);
    }

    #[test]
    fn auto_pick_prefers_songs_that_cover_the_cut_then_clean_loops() {
        let long = track("/m/b_long.mp3", 130.0, 0.6, 180.0, true);
        let short_tight = track("/m/a_short_tight.mp3", 130.0, 0.6, 40.0, true);
        let short_loose = track("/m/c_short_loose.mp3", 130.0, 0.6, 40.0, false);
        assert!(short_tight.loops_cleanly && !short_loose.loops_cleanly);

        let lib = vec![short_tight.clone(), long.clone(), short_loose.clone()];
        let p = auto_pick(&lib, vibe("action_highlight"), 90.0, 7).unwrap();
        assert_eq!(p.path, "/m/b_long.mp3", "a song as long as the cut wins");

        // Nothing long enough: the one that loops cleanly wins, and says so.
        let lib = vec![short_loose, short_tight];
        let p = auto_pick(&lib, vibe("action_highlight"), 90.0, 7).unwrap();
        assert_eq!(p.path, "/m/a_short_tight.mp3");
        assert!(!p.covers_length && p.loops_cleanly);
        assert!(p.reason.contains("shorter than the 1 min 30 s cut (0:40) but its steady beat loops cleanly"), "{}", p.reason);

        // A short cut is covered by the short track too.
        let lib = vec![track("/m/x.mp3", 130.0, 0.6, 40.0, true)];
        assert!(auto_pick(&lib, vibe("action_highlight"), 30.0, 7).unwrap().covers_length);
    }

    #[test]
    fn loops_cleanly_needs_a_steady_beat_and_a_long_enough_section() {
        let mut a = analysis(120.0, 0.5, 60.0, true);
        assert!(loops_cleanly(&a));
        a.loud_region = (20.0, 26.0); // 6 s loud section: too short a loop
        assert!(!loops_cleanly(&a));
        a.loud_region = (10.0, 40.0);
        assert!(loops_cleanly(&a));
        a.grid_tight = None;
        assert!(!loops_cleanly(&a));
    }

    #[test]
    fn auto_pick_is_seeded_among_close_matches_only() {
        let lib = vec![
            track("/m/a.mp3", 128.0, 0.60, 200.0, true),
            track("/m/b.mp3", 129.0, 0.61, 210.0, true),
            track("/m/c.mp3", 130.0, 0.59, 220.0, true),
            track("/m/far.mp3", 70.0, 0.15, 30.0, false),
        ];
        let v = vibe("action_highlight");
        // Deterministic: the same seed always gives the same song.
        for seed in [0, 1, 42, 9_999] {
            assert_eq!(auto_pick(&lib, v, 60.0, seed).unwrap().path, auto_pick(&lib, v, 60.0, seed).unwrap().path);
        }
        // Varied: different seeds reach every close match, never the poor one.
        let mut seen = std::collections::BTreeSet::new();
        for seed in 0..64 {
            let p = auto_pick(&lib, v, 60.0, seed).unwrap();
            assert_eq!(p.close_matches, 3);
            assert!(p.reason.contains(&format!("seed {seed} chose this one")), "{}", p.reason);
            seen.insert(p.path);
        }
        assert_eq!(seen.into_iter().collect::<Vec<_>>(), vec!["/m/a.mp3", "/m/b.mp3", "/m/c.mp3"]);
        // Library order doesn't matter.
        let mut rev = lib.clone();
        rev.reverse();
        for seed in 0..16 {
            assert_eq!(auto_pick(&lib, v, 60.0, seed).unwrap().path, auto_pick(&rev, v, 60.0, seed).unwrap().path);
        }
        assert!(auto_pick(&[], v, 60.0, 1).is_none());
    }

    #[test]
    fn starter_folder_labels_only_vetted_licences() {
        let root = Path::new("/music");
        let cc0 = TrackMeta { licence: Some("CC0-1.0".into()), ..Default::default() };
        let by = TrackMeta { licence: Some("CC-BY-4.0".into()), ..Default::default() };
        let sa = TrackMeta { licence: Some("CC-BY-SA-4.0".into()), ..Default::default() };
        let mut w = Vec::new();
        let at = |p: &str| root.join(p);
        assert_eq!(collection_for(root, &at("starter/a.mp3"), "library", &cc0, &mut w), "starter");
        assert_eq!(collection_for(root, &at("starter/sub/b.mp3"), "library", &by, &mut w), "starter");
        assert_eq!(collection_for(root, &at("other/starter/c.mp3"), "library", &cc0, &mut w), "library");
        assert_eq!(collection_for(root, &at("d.mp3"), "yours", &by, &mut w), "yours");
        assert!(w.is_empty());
        assert_eq!(collection_for(root, &at("starter/e.mp3"), "library", &sa, &mut w), "library");
        assert_eq!(collection_for(root, &at("starter/f.mp3"), "library", &TrackMeta::default(), &mut w), "library");
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].contains("CC BY-SA 4.0") && w[1].contains("not recorded"), "{w:?}");
    }

    /// 6 s of 120 BPM clicks, mono 22.05 kHz 16-bit PCM WAV.
    fn write_click_wav(p: &Path) {
        let rate = 22_050u32;
        let n = rate as usize * 6;
        let mut pcm = vec![0i16; n];
        for beat in 0..12 {
            let s = beat * rate as usize / 2;
            for i in 0..400.min(n - s) {
                pcm[s + i] = ((i as f32 * 0.3).sin() * 20_000.0 * (1.0 - i as f32 / 400.0)) as i16;
            }
        }
        let mut bytes = Vec::new();
        let data_len = (n * 2) as u32;
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for v in pcm {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        std::fs::write(p, bytes).unwrap();
    }

    /// A second server start reads the analysis from disk instead of
    /// decoding the file again.
    #[test]
    fn analysis_is_cached_on_disk() {
        let root = std::env::temp_dir().join(format!("vibecut_music_cache_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let song = root.join("song.wav");
        write_click_wav(&song);
        let dir = camino::Utf8PathBuf::from_path_buf(root.join(".vibecut-cache")).unwrap();
        let disk = Cache::open(&dir, true).unwrap();

        let first = analyse_with(&song, Some(&disk)).unwrap();
        assert!((first.duration_secs - 6.0).abs() < 0.1, "{first:?}");
        let entries = std::fs::read_dir(dir.as_std_path()).unwrap().count();
        assert_eq!(entries, 1, "one cache entry written");

        // Forget the in-memory copy (a restart) and doctor the disk entry:
        // the next analysis must come from the disk cache.
        cache().lock().unwrap().retain(|k, _| k.0 != song);
        let key = Cache::key_for_file(camino::Utf8Path::from_path(&song).unwrap(), &ANALYSIS_VERSION).unwrap();
        let mut doctored = first.clone();
        doctored.bpm = 999.0;
        disk.save(&key, CACHE_SCOPE, &doctored).unwrap();
        assert_eq!(analyse_with(&song, Some(&disk)).unwrap().bpm, 999.0);
        // Still in memory after that: no disk read needed.
        assert_eq!(analyse_with(&song, None).unwrap().bpm, 999.0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// End to end over real files: metadata, starter labelling, warnings.
    /// Audio is a generated click track (needs ffmpeg, like the pipeline).
    #[test]
    fn scan_reads_metadata_and_labels_the_starter_pack() {
        let root = std::env::temp_dir().join(format!("vibecut_music_scan_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("starter")).unwrap();
        let wav = write_click_wav;
        wav(&root.join("mine.wav"));
        wav(&root.join("starter/open.wav"));
        wav(&root.join("starter/unvetted.wav"));
        std::fs::write(root.join("mine.wav.json"), r#"{ "title": "Mine", "licence": "CC-BY-4.0", "artist": "Me" }"#).unwrap();
        std::fs::write(
            root.join("starter").join(vibecut::track_meta::LIBRARY_FILE),
            r#"{ "tracks": [ { "file": "open.wav", "title": "Open", "licence": "CC0-1.0" } ] }"#,
        )
        .unwrap();
        std::fs::write(root.join("starter/unvetted.wav.json"), "{ broken").unwrap();

        let lib = scan(&[(root.clone(), "library")], None);
        let by_title: HashMap<&str, &TrackInfo> = lib.tracks.iter().map(|t| (t.title.as_str(), t)).collect();
        assert_eq!(lib.tracks.len(), 3, "{:?}", lib.tracks.iter().map(|t| &t.title).collect::<Vec<_>>());
        let mine = by_title["Mine"];
        assert_eq!((mine.collection, mine.licence.kind), ("library", LicenceKind::CcBy));
        assert!(mine.attribution_required);
        assert_eq!(mine.credit.as_deref(), Some("Music: \"Mine\" by Me — licensed under CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/)"));
        assert_eq!(mine.metadata, Some(MetaSource::Sidecar));
        assert!((mine.analysis.duration_secs - 6.0).abs() < 0.1);
        let open = by_title["Open"];
        assert_eq!((open.collection, open.metadata), ("starter", Some(MetaSource::Library)));
        let unvetted = by_title["Unvetted"];
        assert_eq!((unvetted.collection, unvetted.licence.kind), ("library", LicenceKind::Unknown));
        assert_eq!(lib.warnings.len(), 2, "{:?}", lib.warnings);
        assert!(lib.warnings.iter().any(|w| w.contains("unvetted.wav.json")), "{:?}", lib.warnings);
        assert!(lib.warnings.iter().any(|w| w.contains("starter pack folder")), "{:?}", lib.warnings);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The page load's listing and an early propose arrive together on a
    /// cold library: they share one scan, so each file is decoded once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_listings_share_one_scan() {
        let root = std::env::temp_dir().join(format!("vibecut_music_single_flight_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        write_click_wav(&root.join("a.wav"));
        write_click_wav(&root.join("b.wav"));
        let dirs = vec![(root.clone(), "yours")];
        let decodes = |f: &str| {
            let d = DECODES.lock().unwrap();
            d.as_ref().and_then(|m| m.get(&root.join(f)).copied()).unwrap_or(0)
        };

        let (x, y) = tokio::join!(scan_shared(dirs.clone()), scan_shared(dirs.clone()));
        assert_eq!((x.unwrap().tracks.len(), y.unwrap().tracks.len()), (2, 2));
        assert_eq!((decodes("a.wav"), decodes("b.wav")), (1, 1), "one decode per file");

        // A later listing ("Look again") scans afresh and finds new files,
        // without decoding the known ones again.
        write_click_wav(&root.join("c.wav"));
        let z = scan_shared(dirs).await.unwrap();
        assert_eq!(z.tracks.len(), 3);
        assert_eq!((decodes("a.wav"), decodes("c.wav")), (1, 1));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// propose's `soundtrack` may name only audio inside the projects or
    /// music folders, and the answer outside them never depends on whether
    /// the file exists.
    #[test]
    fn soundtrack_paths_must_be_audio_inside_the_library_or_projects() {
        let base = std::env::temp_dir().join(format!("vibecut_music_paths_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let projects = base.join("projects");
        let outside = base.join("elsewhere");
        std::fs::create_dir_all(projects.join("music")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(projects.join("music/song.mp3"), b"x").unwrap();
        std::fs::write(projects.join("notes.txt"), b"x").unwrap();
        std::fs::write(outside.join("secret.mp3"), b"x").unwrap();
        let roots = vec![projects.join("music"), projects.clone()];
        let check = |p: &Path| soundtrack_file_in(p, &roots);

        let song = projects.join("music/song.mp3");
        assert_eq!(check(&song).unwrap(), song);
        assert!(matches!(check(&projects.join("music/missing.mp3")), Err(OpsError::NotFound { .. })));
        for p in [
            outside.join("secret.mp3"),
            outside.join("missing.mp3"),
            projects.join("notes.txt"),
            projects.join("music/../../elsewhere/secret.mp3"),
            PathBuf::from("music/song.mp3"),
        ] {
            assert!(matches!(check(&p), Err(OpsError::InvalidInput { .. })), "{p:?}");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.join("secret.mp3"), projects.join("music/link.mp3")).unwrap();
            assert!(matches!(check(&projects.join("music/link.mp3")), Err(OpsError::InvalidInput { .. })));
        }
        // The real roots: system files are refused, existing or not.
        for p in ["/etc/passwd", "/proc/self/environ", "/etc/nonexistent.mp3"] {
            assert!(matches!(check_soundtrack_file(p), Err(OpsError::InvalidInput { .. })), "{p}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Picked folders are kept in their file, skip what the library
    /// already reads, absorb folders inside them, and can be removed.
    #[test]
    fn picked_music_folders_are_kept_and_removed() {
        let base = std::env::temp_dir().join(format!("vibecut_picked_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let own = base.join("music");
        let road = base.join("drive/Music/Road trip");
        let music = base.join("drive/Music");
        for d in [&own, &road, &own.join("sub")] {
            std::fs::create_dir_all(d).unwrap();
        }
        let file = base.join(".vibecut-music-folders.json");
        let have = vec![own.clone()];

        let a = add_in(&file, &have, road.clone(), r"D:\Music\Road trip".into()).unwrap();
        assert!(!a.already);
        assert_eq!(a.path, r"D:\Music\Road trip");
        assert_eq!(picked_in(&file), vec![road.clone()]);
        // Adding it again, or a folder inside the own one, changes nothing.
        assert!(add_in(&file, &[own.clone(), road.clone()], road.clone(), "x".into()).unwrap().already);
        assert!(add_in(&file, &have, own.join("sub"), "x".into()).unwrap().already);
        assert_eq!(picked_in(&file), vec![road.clone()]);
        // The folder above takes its place.
        add_in(&file, &have, music.clone(), "x".into()).unwrap();
        assert_eq!(picked_in(&file), vec![music.clone()]);

        assert!(!remove_in(&file, &road).unwrap());
        assert!(remove_in(&file, &music).unwrap());
        assert!(picked_in(&file).is_empty());

        // A hand-edited file: relative and `..` entries are ignored.
        std::fs::write(&file, r#"["relative/dir", "/a/../etc", "/ok/music"]"#).unwrap();
        assert_eq!(picked_in(&file), vec![PathBuf::from("/ok/music")]);
        std::fs::write(&file, "not json").unwrap();
        assert!(picked_in(&file).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn audio_names_are_counted_by_extension() {
        assert!(is_audio_name("Road.MP3") && is_audio_name("a.flac") && is_audio_name("b.opus"));
        assert!(!is_audio_name(".hidden.mp3") && !is_audio_name("clip.mp4") && !is_audio_name("notes.txt"));
    }

    /// People's Downloads / Music folders with songs are suggested once,
    /// biggest first; empty ones, shared profiles and folders the library
    /// already reads are not.
    #[test]
    fn downloads_and_music_folders_are_suggested() {
        let base = std::env::temp_dir().join(format!("vibecut_suggest_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let c = base.join("host/c");
        let touch = |p: PathBuf| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"x").unwrap();
        };
        touch(c.join("Users/me/Downloads/a.mp3"));
        touch(c.join("Users/me/Downloads/b.wav"));
        touch(c.join("Users/me/Downloads/setup.exe"));
        touch(c.join("Users/me/Music/Road trip/c.flac"));
        touch(c.join("Users/you/Downloads/notes.txt"));
        touch(c.join("Users/Public/Music/d.mp3"));
        touch(c.join("Users/them/Music/e.mp3"));
        let roots = crate::browse::roots_from(Some("auto"), &base.join("footage"), &base.join("host"));
        let have = vec![c.join("Users/them/Music")];
        let got = suggested_in(&roots, &have);
        let paths: Vec<(&str, u32)> = got.iter().map(|s| (s.path.as_str(), s.n_songs)).collect();
        assert_eq!(paths, vec![(r"C:\Users\me\Downloads", 2), (r"C:\Users\me\Music", 1)], "{got:?}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
