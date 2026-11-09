//! Generate a starting TOML config from a curated vibe + footage folder.
//! Transport-free counterpart of the old `propose_vibe_impl`.
//!
//! Music is on by default: without an explicit `soundtrack` the proposal
//! takes the library song [`music::auto_pick`] chooses for the vibe, so
//! every first render has music, and says why in the result and in a
//! comment above `[soundtrack]`.

use std::path::Path;

use serde::Serialize;
use vibecut::project_config::Config;

use crate::error::OpsError;
use crate::music::{self, AutoPick, TrackInfo};
use crate::paths::{resolve_footage, slugify, PROJECTS_ROOT};
use crate::vibes;

#[derive(Debug, Clone, Serialize)]
pub struct ProposeResult {
    pub vibe: &'static str,
    pub vibe_name: &'static str,
    pub rationale: &'static str,
    pub config_path: String,
    pub config_toml: String,
    pub output_path: String,
    /// The song the config uses; `None` when it has no `[soundtrack]`.
    pub soundtrack: Option<ProposedSoundtrack>,
    /// The library song Vibecut picks for this vibe, length and seed —
    /// what "let Vibecut pick" means, even when the caller chose its own.
    pub auto_pick: Option<AutoPick>,
    /// Why there's no music, when there isn't.
    pub music_note: Option<String>,
}

/// The soundtrack written into a proposal.
#[derive(Debug, Clone, Serialize)]
pub struct ProposedSoundtrack {
    pub path: String,
    pub title: String,
    /// `"vibecut"` (auto-picked) or `"you"` (passed in `soundtrack`).
    pub picked_by: &'static str,
    /// Why it was chosen (auto picks).
    pub reason: Option<String>,
}

pub struct ProposeArgs<'a> {
    /// Footage folder (same convention as `discover_sources`).
    pub folder: &'a str,
    /// Which curated vibe to start from.
    pub vibe: &'a str,
    /// Optional intro-card title. Defaults to `"<folder> · YYYY-MM-DD"`.
    pub title: Option<&'a str>,
    /// Target output duration as a humantime string. Falls back to the
    /// vibe's own default when omitted.
    pub target_duration: Option<&'a str>,
    /// Picker seed for reproducibility.
    pub seed: u64,
    /// Soundtrack file. `None` = let Vibecut pick from the music library;
    /// `"none"` (or empty) = no music, the clips keep their own sound.
    pub soundtrack: Option<&'a str>,
}

/// What the caller asked for, music-wise.
#[derive(Debug, PartialEq)]
pub(crate) enum MusicRequest<'a> {
    Auto,
    Off,
    File(&'a str),
}

pub(crate) fn music_request(s: Option<&str>) -> MusicRequest<'_> {
    match s.map(str::trim) {
        None => MusicRequest::Auto,
        Some(v) if v.is_empty() || v.eq_ignore_ascii_case("none") => MusicRequest::Off,
        Some(v) => MusicRequest::File(v),
    }
}

/// Render `vibe` and check the pipeline would accept the result.
pub(crate) fn render_checked(vibe: &vibes::Vibe, subs: &vibes::Substitutions<'_>) -> Result<(String, Config), OpsError> {
    let toml = vibe.render(subs);
    // Never write a config the pipeline would reject — same rule
    // `config_io::write_config_toml` applies to hand-edited TOML,
    // extended to the vibe-rendered starting point. `render()` escapes
    // substituted values (see `vibes::toml_escape`), but this is the
    // backstop: if a future template edit or a substituted value we
    // haven't thought of ever breaks the TOML, the caller gets a clear
    // error here instead of a file on disk that only fails later, on
    // whatever the next tool call happens to be.
    let cfg: Config = toml::from_str(&toml)
        .map_err(|e| OpsError::internal(format!("vibe {} rendered invalid TOML: {e}", vibe.key)))?;
    cfg.validate().map_err(|e| {
        OpsError::internal(format!("vibe {} rendered a config that fails validation: {e:#}", vibe.key))
    })?;
    Ok((toml, cfg))
}

pub(crate) fn get_vibe(key: &str) -> Result<&'static vibes::Vibe, OpsError> {
    vibes::get(key).ok_or_else(|| {
        OpsError::invalid_with_hint(
            format!("unknown vibe: {key:?}"),
            format!("available: {}", vibes::names().join(", ")),
        )
    })
}

/// Target length in seconds exactly as a proposal would read it: the
/// vibe's template parsed with the given (or default) `target_duration`.
fn target_secs(vibe: &vibes::Vibe, target_duration: &str) -> Result<f32, OpsError> {
    let (_, cfg) = render_checked(
        vibe,
        &vibes::Substitutions {
            name: "probe",
            title: "probe",
            folder: "/footage",
            output_path: "/projects/out/probe.mp4",
            target_duration,
            seed: 0,
            soundtrack: None,
        },
    )
    .map_err(|e| OpsError::invalid(format!("target_duration {target_duration:?}: {e}")))?;
    Ok(cfg.project.target_duration.as_secs_f32())
}

impl ProposedSoundtrack {
    /// The comment written above `[soundtrack]`.
    pub(crate) fn note(&self) -> String {
        match &self.reason {
            Some(r) => format!("Picked for you: {}. {r}", self.title),
            None => format!("Your pick: {}.", self.title),
        }
    }
}

/// The proposal's song and, when there is none, why: the caller's file,
/// no music, or — by default — the library's auto pick.
pub(crate) fn decide_soundtrack(
    request: &MusicRequest<'_>,
    auto_pick: Option<&AutoPick>,
    tracks: &[TrackInfo],
) -> (Option<ProposedSoundtrack>, Option<String>) {
    match *request {
        MusicRequest::Off => (None, Some("No music requested: the edit keeps the clips' own sound.".to_string())),
        MusicRequest::File(p) => {
            let title = tracks.iter().find(|t| t.path == p).map(|t| t.title.clone()).unwrap_or_else(|| {
                let (meta, _) = vibecut::track_meta::load_for_file(Path::new(p), &mut Vec::new());
                meta.title.unwrap_or_else(|| vibecut::track_meta::title_from_file_name(Path::new(p)))
            });
            (Some(ProposedSoundtrack { path: p.to_string(), title, picked_by: "you", reason: None }), None)
        }
        MusicRequest::Auto => match auto_pick {
            Some(pick) => (
                Some(ProposedSoundtrack {
                    path: pick.path.clone(),
                    title: pick.title.clone(),
                    picked_by: "vibecut",
                    reason: Some(pick.reason.clone()),
                }),
                None,
            ),
            None => (
                None,
                Some(format!(
                    "Your music library is empty, so this edit keeps the clips' own sound. Put songs in {} and Vibecut picks one for every new video.",
                    music::own_music_dir().display()
                )),
            ),
        },
    }
}

pub(crate) async fn library_tracks() -> Vec<TrackInfo> {
    match music::list_tracks().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "music library unavailable; proposing without a soundtrack");
            Vec::new()
        }
    }
}

/// The song a proposal for `vibe` / `target_duration` / `seed` would
/// auto-pick — what `GET /api/music/pick` shows as "Picked for you"
/// before anything is proposed.
pub async fn preview_pick(
    vibe: &str,
    target_duration: Option<&str>,
    seed: u64,
) -> Result<Option<AutoPick>, OpsError> {
    let vibe = get_vibe(vibe)?;
    let secs = target_secs(vibe, target_duration.unwrap_or(vibe.default_target_duration))?;
    Ok(music::auto_pick(&library_tracks().await, vibe, secs, seed))
}

pub async fn propose_vibe(args: ProposeArgs<'_>) -> Result<ProposeResult, OpsError> {
    let target_folder = resolve_footage(args.folder);
    if !target_folder.exists() {
        return Err(OpsError::not_found_with(
            "footage folder",
            target_folder.to_string_lossy().into_owned(),
        ));
    }
    crate::browse::check_footage_folder(&target_folder)?;

    let vibe = get_vibe(args.vibe)?;
    // The seed is written into the proposal's TOML, whose integers are
    // signed 64-bit.
    if args.seed > vibecut::project_config::MAX_SEED {
        return Err(OpsError::invalid(format!(
            "seed {} is too large: a seed is at most {} (it has to fit a TOML integer)",
            args.seed,
            vibecut::project_config::MAX_SEED
        )));
    }

    let folder_label = target_folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "edit".to_string());
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let final_title = args
        .title
        .map(str::to_string)
        .unwrap_or_else(|| format!("{folder_label} · {today}"));
    // Fall back to the vibe's own default when the caller doesn't
    // provide one — drone_cinematic naturally wants a longer hold,
    // action_highlight wants a shorter punch, etc.
    let target_duration = args
        .target_duration
        .map(str::to_string)
        .unwrap_or_else(|| vibe.default_target_duration.to_string());
    let output_basename = slugify(&folder_label);
    let output_path = format!("{PROJECTS_ROOT}/out/{output_basename}.mp4");
    let project_name = final_title.clone();
    let folder = target_folder.to_string_lossy();

    // Only audio in the music or projects folders: the path goes into the
    // config and its metadata is read, so it must not reach anywhere else.
    let checked;
    let mut request = music_request(args.soundtrack);
    if let MusicRequest::File(p) = request {
        checked = music::check_soundtrack_file(p)?.to_string_lossy().into_owned();
        request = MusicRequest::File(&checked);
    }

    let subs = |soundtrack| vibes::Substitutions {
        name: &project_name,
        title: &final_title,
        folder: &folder,
        output_path: &output_path,
        target_duration: &target_duration,
        seed: args.seed,
        soundtrack,
    };
    let (base_toml, base_cfg) = render_checked(vibe, &subs(None))?;
    let target_secs = base_cfg.project.target_duration.as_secs_f32();

    let tracks = library_tracks().await;
    let auto_pick = music::auto_pick(&tracks, vibe, target_secs, args.seed);
    let (soundtrack, music_note) = decide_soundtrack(&request, auto_pick.as_ref(), &tracks);

    let toml = match &soundtrack {
        None => base_toml,
        Some(s) => {
            let note = s.note();
            render_checked(vibe, &subs(Some(vibes::SoundtrackSub { file: &s.path, note: &note })))?.0
        }
    };

    let projects_root = Path::new(PROJECTS_ROOT);
    let _ = std::fs::create_dir_all(projects_root.join("out"));
    let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string();
    let config_path = projects_root.join(format!("proposal_{output_basename}_{ts}.toml"));
    std::fs::write(&config_path, &toml)
        .map_err(|e| OpsError::internal(format!("could not write {}: {e}", config_path.display())))?;

    Ok(ProposeResult {
        vibe: vibe.key,
        vibe_name: vibe.name,
        rationale: vibe.rationale,
        config_path: config_path.to_string_lossy().into_owned(),
        config_toml: toml,
        output_path,
        soundtrack,
        auto_pick,
        music_note,
    })
}

/// List the curated vibe catalog: key, name, rationale, default target
/// duration.
pub fn list_vibes() -> Vec<serde_json::Value> {
    vibes::catalog()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn music_requests_parse() {
        assert_eq!(music_request(None), MusicRequest::Auto);
        assert_eq!(music_request(Some("none")), MusicRequest::Off);
        assert_eq!(music_request(Some(" NONE ")), MusicRequest::Off);
        assert_eq!(music_request(Some("")), MusicRequest::Off);
        assert_eq!(music_request(Some("/m/a.mp3")), MusicRequest::File("/m/a.mp3"));
    }

    #[test]
    fn target_secs_reads_the_template_and_falls_back_to_the_vibe() {
        let v = vibes::get("action_highlight").unwrap();
        assert_eq!(target_secs(v, "30s").unwrap(), 30.0);
        assert_eq!(target_secs(v, "1m30s").unwrap(), 90.0);
        assert_eq!(target_secs(v, v.default_target_duration).unwrap(), 90.0);
        assert!(target_secs(v, "soon").is_err());
    }

    fn night_drive() -> AutoPick {
        AutoPick {
            path: "/projects/music/Night_Drive.mp3".into(),
            title: "Night Drive".into(),
            reason: "132 BPM sits in Kid reel's 100–135 BPM range.".into(),
            score: 0.9,
            close_matches: 1,
            covers_length: true,
            loops_cleanly: true,
        }
    }

    /// No `soundtrack` in the request: the proposal's config gets the
    /// auto pick, live, with the vibe's settings and the reason above it.
    #[test]
    fn without_a_soundtrack_the_proposal_takes_the_auto_pick() {
        let pick = night_drive();
        let (s, note) = decide_soundtrack(&MusicRequest::Auto, Some(&pick), &[]);
        let s = s.expect("music on by default");
        assert_eq!((s.path.as_str(), s.picked_by), (pick.path.as_str(), "vibecut"));
        assert_eq!(s.reason.as_deref(), Some(pick.reason.as_str()));
        assert!(note.is_none());
        assert_eq!(s.note(), "Picked for you: Night Drive. 132 BPM sits in Kid reel's 100–135 BPM range.");

        let v = vibes::get("kid_reel").unwrap();
        let note = s.note();
        let (toml, cfg) = render_checked(
            v,
            &vibes::Substitutions {
                name: "p",
                title: "p",
                folder: "/footage/x",
                output_path: "/projects/out/p.mp4",
                target_duration: "30s",
                seed: 42,
                soundtrack: Some(vibes::SoundtrackSub { file: &s.path, note: &note }),
            },
        )
        .unwrap();
        let snd = cfg.soundtrack.expect("live [soundtrack]");
        assert_eq!(snd.files[0].as_str(), pick.path);
        assert_eq!(snd.duck_count, 3, "the vibe's own soundtrack settings");
        assert!(toml.contains("# Picked for you: Night Drive."), "{toml}");
    }

    #[test]
    fn no_music_only_when_asked_or_the_library_is_empty() {
        let pick = night_drive();
        let (s, note) = decide_soundtrack(&MusicRequest::Auto, None, &[]);
        assert!(s.is_none());
        let note = note.unwrap();
        assert!(note.contains("library is empty") && note.contains(&*music::own_music_dir().to_string_lossy()), "{note}");

        let (s, note) = decide_soundtrack(&MusicRequest::Off, Some(&pick), &[]);
        assert!(s.is_none() && note.unwrap().contains("No music requested"));

        // The caller's own file wins over the pick, titled from its name.
        let (s, note) = decide_soundtrack(&MusicRequest::File("/m/golden_hour-drift.mp3"), Some(&pick), &[]);
        let s = s.unwrap();
        assert_eq!((s.title.as_str(), s.picked_by, s.reason.as_deref()), ("Golden hour drift", "you", None));
        assert_eq!(s.note(), "Your pick: Golden hour drift.");
        assert!(note.is_none());
    }

    #[tokio::test]
    async fn preview_pick_rejects_unknown_vibes() {
        assert!(preview_pick("no_such_vibe", None, 1).await.is_err());
    }

    /// A seed a TOML file can't hold is the caller's mistake (400), said
    /// before anything is written — not "rendered invalid TOML" (500).
    #[tokio::test]
    async fn a_seed_past_a_toml_integer_is_refused() {
        let folder = std::env::temp_dir();
        let r = propose_vibe(ProposeArgs {
            folder: &folder.to_string_lossy(),
            vibe: "kid_reel",
            title: None,
            target_duration: None,
            seed: vibecut::project_config::MAX_SEED + 1,
            soundtrack: Some("none"),
        })
        .await;
        match r {
            Err(OpsError::InvalidInput { message, .. }) => assert!(message.contains("seed"), "{message}"),
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }
}
