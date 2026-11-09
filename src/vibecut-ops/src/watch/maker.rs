//! Turning a settled folder into a reel: propose a project with the
//! defaults (the vibe and length the app would pick for this footage, the
//! auto-picked song), render it as an ordinary job, grab a poster frame.
//!
//! [`ReelMaker`] is the seam: the watcher only talks to it, so tests run
//! the whole watch → queue → render → notify flow with a stand-in, and
//! [`OpsReelMaker`] does it for real through the same ops the web UI uses.

use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::notify::reel_duration;
use super::state::WatchEntry;
use crate::jobs::JobRegistry;
use crate::paths::{slugify, PROJECTS_ROOT};
use crate::{config_io, discover, frames, propose, render, vibes};

/// A project written for a watched folder.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub config_path: String,
    /// What the UI needs to open it: the propose result without its TOML,
    /// plus `title` and `target_duration`.
    pub summary: Value,
}

/// Makes a reel from a watched folder.
pub trait ReelMaker: Send + Sync + 'static {
    /// Write a project for the entry's footage.
    fn propose(&self, entry: &WatchEntry) -> impl Future<Output = Result<Proposal, String>> + Send;
    /// Start rendering it as a normal job in the watcher's job registry;
    /// returns the job id.
    fn submit(&self, config_path: &str) -> impl Future<Output = Result<String, String>> + Send;
    /// Save a frame of the finished reel (`render`: the job's result) and
    /// return its path. `None` is fine: the reel just has no poster.
    fn poster(&self, render: &Value) -> impl Future<Output = Option<String>> + Send;
}

/// The vibe the app picks for footage nobody chose one for (the Create
/// screen's default): no sound anywhere reads as drone footage,
/// high-frame-rate clips as action, everything else as a family day.
pub fn auto_vibe(sources: &[Value]) -> &'static str {
    let has_audio = |s: &Value| s.get("has_audio").and_then(Value::as_bool).unwrap_or(false);
    let fast = |s: &Value| {
        s.get("fps").and_then(Value::as_array).is_some_and(|f| f.iter().filter_map(Value::as_f64).any(|f| f >= 100.0))
    };
    if !sources.is_empty() && sources.iter().all(|s| !has_audio(s)) {
        "drone_cinematic"
    } else if sources.iter().any(fast) {
        "action_highlight"
    } else {
        "family_chronicle"
    }
}

/// The length the app picks by default: about a tenth of the footage,
/// 30–90 s, in whole 5 s.
pub fn auto_length_secs(total_secs: f64) -> u32 {
    ((total_secs * 0.1).clamp(30.0, 90.0) / 5.0).round() as u32 * 5
}

/// Where in the reel to grab its poster: the middle of the clip the picker
/// scored highest (from the manifest's exact timeline), else a third of
/// the way in, past any intro card.
pub fn poster_time(manifest: &Value) -> Option<f64> {
    let timeline = manifest.get("timeline");
    let clips = timeline.and_then(|t| t.get("clips")).and_then(Value::as_array);
    let segments = manifest.get("segments").and_then(Value::as_array);
    let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64);
    let score = |c: &Value| {
        c.get("segment_index")
            .and_then(Value::as_u64)
            .and_then(|i| segments?.get(i as usize))
            .and_then(|s| num(s, "score"))
            .unwrap_or(0.0)
    };
    let best = clips.into_iter().flatten().fold(None::<(&Value, f64)>, |best, c| {
        let (Some(a), Some(b)) = (num(c, "cut_in_secs"), num(c, "cut_out_secs")) else {
            return best;
        };
        if b <= a {
            return best;
        }
        let s = score(c);
        match best {
            Some((_, top)) if top >= s => best,
            _ => Some((c, s)),
        }
    });
    if let Some((c, _)) = best {
        return Some((num(c, "cut_in_secs")? + num(c, "cut_out_secs")?) / 2.0);
    }
    let duration = timeline.and_then(|t| num(t, "duration_secs")).or_else(|| num(manifest, "actual_duration_secs"))?;
    (duration > 0.0).then_some(duration * 0.35)
}

/// `<video>.poster.jpg` next to the video.
pub fn poster_path_for(video: &str) -> String {
    let p = Path::new(video);
    p.with_extension("poster.jpg").to_string_lossy().into_owned()
}

/// `<dir>/<base>.mp4`, or `<base>_2.mp4`, `_3`, … when that is taken: a
/// watched folder never overwrites another project's video.
pub fn unique_output(dir: &Path, base: &str) -> PathBuf {
    let taken = |p: &Path| p.exists() || p.with_extension("manifest.json").exists();
    let first = dir.join(format!("{base}.mp4"));
    if !taken(&first) {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{base}_{n}.mp4")))
        .find(|p| !taken(p))
        .expect("an unused name")
}

/// Point the proposal's output at `new` instead of `old` (the quoted path
/// in `[[outputs]]`).
pub fn rewrite_output(toml: &str, old: &str, new: &str) -> Result<String, String> {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let from = quote(old);
    if !toml.contains(&from) {
        return Err(format!("the proposal doesn't name its output {old}"));
    }
    Ok(toml.replace(&from, &quote(new)))
}

/// The project (`/projects/*.toml`, top level) already made from this
/// footage, if any — the watcher leaves those folders alone. Matches
/// `[input].folder` against the unit's path and its footage folder;
/// relative folders are read from `footage_root` and `projects_root`.
/// `ours` are the projects the watcher wrote itself: they never count, or
/// a card mounted at the same place would only ever make one reel.
pub fn existing_project(
    projects_root: &Path,
    footage_root: &Path,
    unit_paths: &[&Path],
    ours: &HashSet<PathBuf>,
) -> Option<String> {
    let mut configs: Vec<PathBuf> = std::fs::read_dir(projects_root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("toml")) && p.is_file())
        .filter(|p| !ours.contains(p))
        .collect();
    configs.sort();
    configs.into_iter().find_map(|cfg| {
        let text = std::fs::read_to_string(&cfg).ok()?;
        let doc: toml::Value = toml::from_str(&text).ok()?;
        let folder = doc.get("input")?.get("folder")?.as_str()?;
        let folder = Path::new(folder);
        let candidates: Vec<PathBuf> = if folder.is_absolute() || folder.to_string_lossy().starts_with('/') {
            vec![folder.to_path_buf()]
        } else {
            vec![footage_root.join(folder), projects_root.join(folder)]
        };
        candidates
            .iter()
            .any(|c| unit_paths.contains(&c.as_path()))
            .then(|| cfg.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
    })
}

/// [`ReelMaker`] over the real ops: discover → propose → render job →
/// poster, all under `/footage` and `/projects`.
#[derive(Clone)]
pub struct OpsReelMaker {
    pub jobs: JobRegistry,
    /// `VIBECUT_WATCH_VIBE`; `None`: picked from the footage.
    pub vibe: Option<String>,
    /// `VIBECUT_WATCH_LENGTH`; `None`: picked from the footage.
    pub target_duration: Option<String>,
}

impl ReelMaker for OpsReelMaker {
    async fn propose(&self, entry: &WatchEntry) -> Result<Proposal, String> {
        let found = discover::discover_sources(&entry.folder).await.map_err(|e| e.to_string())?;
        if found.n_sources == 0 {
            return Err("No clips Vibecut can read in this folder.".into());
        }
        let vibe = self.vibe.clone().unwrap_or_else(|| auto_vibe(&found.sources).to_string());
        let length = self
            .target_duration
            .clone()
            .unwrap_or_else(|| format!("{}s", auto_length_secs(found.total_secs)));
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let title = format!("{} · {today}", entry.name);
        let r = propose::propose_vibe(propose::ProposeArgs {
            folder: &entry.folder,
            vibe: &vibe,
            title: Some(&title),
            target_duration: Some(&length),
            seed: 42,
            soundtrack: None,
        })
        .await
        .map_err(|e| e.to_string())?;

        // Named after the folder (a card: after the card and the day), and
        // never over another project's video.
        let base = match entry.kind {
            super::UnitKind::Folder => slugify(&entry.name),
            super::UnitKind::Card => slugify(&format!("{} {today}", entry.name)),
        };
        let output = unique_output(&Path::new(PROJECTS_ROOT).join("out"), &base).to_string_lossy().into_owned();
        if output != r.output_path {
            let written = match rewrite_output(&r.config_toml, &r.output_path, &output) {
                Ok(toml) => config_io::write_config_toml(&r.config_path, &toml).await.map_err(|e| e.to_string()),
                Err(e) => Err(e),
            };
            if let Err(e) = written {
                // Not left behind: the watcher only knows its own projects
                // by the proposals it stored, and this one never will be.
                let _ = tokio::fs::remove_file(&r.config_path).await;
                return Err(e);
            }
        }
        let summary = json!({
            "vibe": r.vibe,
            "vibe_name": r.vibe_name,
            "rationale": r.rationale,
            "config_path": r.config_path,
            "output_path": output,
            "soundtrack": r.soundtrack,
            "auto_pick": r.auto_pick,
            "music_note": r.music_note,
            "title": title,
            "target_duration": length,
        });
        Ok(Proposal { config_path: r.config_path, summary })
    }

    async fn submit(&self, config_path: &str) -> Result<String, String> {
        render::submit_async(&self.jobs, config_path).await.map_err(|e| e.to_string())
    }

    async fn poster(&self, render: &Value) -> Option<String> {
        let s = |k: &str| render.get(k).and_then(Value::as_str).filter(|v| !v.is_empty());
        let video = s("version_path").or(s("output_path"))?.to_string();
        let manifest = s("version_manifest_path")
            .or(s("manifest_path"))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str::<Value>(&t).ok());
        let t = manifest
            .as_ref()
            .and_then(poster_time)
            .or_else(|| reel_duration(render).map(|d| d * 0.35))
            .unwrap_or(1.0);
        let jpeg = match frames::extract_frame_jpeg(Path::new(&video), t as f32, 640, None).await {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(error = %e, "watched reel: no poster frame");
                return None;
            }
        };
        let out = poster_path_for(&video);
        tokio::fs::write(&out, jpeg).await.ok()?;
        Some(out)
    }
}

/// A vibe key from the environment, if it names a real vibe.
pub fn checked_vibe(v: Option<String>) -> Option<String> {
    let v = v?.trim().to_string();
    if v.is_empty() {
        return None;
    }
    if vibes::get(&v).is_some() {
        Some(v)
    } else {
        tracing::warn!(vibe = %v, available = %vibes::names().join(", "), "VIBECUT_WATCH_VIBE is not a vibe; picking from the footage");
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(audio: bool, fps: &[f64]) -> Value {
        json!({ "has_audio": audio, "fps": fps })
    }

    #[test]
    fn the_default_vibe_and_length_match_the_create_screen() {
        assert_eq!(auto_vibe(&[src(false, &[30.0]), src(false, &[60.0])]), "drone_cinematic");
        assert_eq!(auto_vibe(&[src(true, &[30.0]), src(true, &[120.0])]), "action_highlight");
        assert_eq!(auto_vibe(&[src(true, &[30.0]), src(false, &[25.0])]), "family_chronicle");
        assert_eq!(auto_vibe(&[]), "family_chronicle");
        assert_eq!(auto_length_secs(60.0), 30);
        assert_eq!(auto_length_secs(523.0), 50);
        assert_eq!(auto_length_secs(5000.0), 90);
    }

    #[test]
    fn the_poster_is_the_best_clip_else_a_third_in() {
        let m = json!({
            "segments": [{ "score": 0.4 }, { "score": 2.5 }, { "score": 1.0 }],
            "timeline": { "duration_secs": 30.0, "clips": [
                { "segment_index": 0, "cut_in_secs": 3.0, "cut_out_secs": 6.0 },
                { "segment_index": 1, "cut_in_secs": 6.0, "cut_out_secs": 10.0 },
                { "segment_index": 2, "cut_in_secs": 10.0, "cut_out_secs": 12.0 }
            ] }
        });
        assert_eq!(poster_time(&m), Some(8.0));
        // No scores: the first clip.
        let m = json!({ "timeline": { "duration_secs": 30.0, "clips": [
            { "segment_index": 0, "cut_in_secs": 3.0, "cut_out_secs": 5.0 },
            { "segment_index": 1, "cut_in_secs": 5.0, "cut_out_secs": 9.0 }
        ] } });
        assert_eq!(poster_time(&m), Some(4.0));
        // No timeline (older manifest): a third of the way in.
        assert_eq!(poster_time(&json!({ "actual_duration_secs": 20.0 })), Some(7.0));
        assert_eq!(poster_time(&json!({})), None);
        assert_eq!(
            poster_path_for("/projects/out/versions/a__t_job_1.mp4"),
            "/projects/out/versions/a__t_job_1.poster.jpg"
        );
    }

    #[test]
    fn outputs_never_overwrite_another_video() {
        let dir = std::env::temp_dir().join(format!("vibecut_watch_out_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_output(&dir, "trip"), dir.join("trip.mp4"));
        std::fs::write(dir.join("trip.mp4"), b"x").unwrap();
        // Only the manifest left (output deleted): still taken.
        std::fs::write(dir.join("trip_2.manifest.json"), b"{}").unwrap();
        assert_eq!(unique_output(&dir, "trip"), dir.join("trip_3.mp4"));
        let toml = "[[outputs]]\npath = \"/projects/out/dcim.mp4\"\nresolution = \"1920x1080\"\n";
        let out = rewrite_output(toml, "/projects/out/dcim.mp4", "/projects/out/sd_card_2026-11-09.mp4").unwrap();
        assert!(out.contains("path = \"/projects/out/sd_card_2026-11-09.mp4\""), "{out}");
        assert!(rewrite_output(toml, "/projects/out/other.mp4", "/x.mp4").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folders_that_already_have_a_project_are_found() {
        let dir = std::env::temp_dir().join(format!("vibecut_watch_projects_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let cfg = |folder: &str| format!("schema_version = 1\n[input]\nfolder = \"{folder}\"\n");
        std::fs::write(dir.join("proposal_alps_1.toml"), cfg("/footage/Alps Ski Day")).unwrap();
        std::fs::write(dir.join("relative.toml"), cfg("Coastline/")).unwrap();
        std::fs::write(dir.join("broken.toml"), "not toml [").unwrap();
        // Only the top level counts.
        std::fs::write(dir.join("sub/deep.toml"), cfg("/footage/Deep")).unwrap();
        let footage = Path::new("/footage");
        let none = HashSet::new();
        let alps = Path::new("/footage/Alps Ski Day");
        assert_eq!(existing_project(&dir, footage, &[alps], &none).as_deref(), Some("proposal_alps_1.toml"));
        let coast = Path::new("/footage/Coastline");
        assert_eq!(existing_project(&dir, footage, &[coast], &none).as_deref(), Some("relative.toml"));
        assert_eq!(existing_project(&dir, footage, &[Path::new("/footage/Deep")], &none), None);
        // A card: its root or its DCIM.
        let card = Path::new("/cards/A");
        let dcim = Path::new("/cards/A/DCIM");
        std::fs::write(dir.join("card.toml"), cfg("/cards/A/DCIM")).unwrap();
        assert_eq!(existing_project(&dir, footage, &[card, dcim], &none).as_deref(), Some("card.toml"));
        assert_eq!(existing_project(&dir.join("missing"), footage, &[alps], &none), None);
        // The watcher's own projects don't count; anyone else's still do.
        let ours: HashSet<PathBuf> = [dir.join("card.toml")].into();
        assert_eq!(existing_project(&dir, footage, &[card, dcim], &ours), None);
        std::fs::write(dir.join("my_edit.toml"), cfg("/cards/A")).unwrap();
        assert_eq!(existing_project(&dir, footage, &[card, dcim], &ours).as_deref(), Some("my_edit.toml"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_vibes_are_checked() {
        assert_eq!(checked_vibe(Some(" kid_reel ".into())).as_deref(), Some("kid_reel"));
        assert_eq!(checked_vibe(Some("nope".into())), None);
        assert_eq!(checked_vibe(Some("".into())), None);
        assert_eq!(checked_vibe(None), None);
    }
}
