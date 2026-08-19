//! Generate a starting TOML config from a curated vibe + footage folder.
//! Transport-free counterpart of the old `propose_vibe_impl`.

use std::path::Path;

use serde::Serialize;
use vibecut::project_config::Config;

use crate::error::OpsError;
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
}

pub async fn propose_vibe(args: ProposeArgs<'_>) -> Result<ProposeResult, OpsError> {
    let target_folder = resolve_footage(args.folder);
    if !target_folder.exists() {
        return Err(OpsError::not_found_with(
            "footage folder",
            target_folder.to_string_lossy().into_owned(),
        ));
    }

    let vibe = vibes::get(args.vibe).ok_or_else(|| {
        OpsError::invalid_with_hint(
            format!("unknown vibe: {:?}", args.vibe),
            format!("available: {}", vibes::names().join(", ")),
        )
    })?;

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

    let toml = vibe.render(&vibes::Substitutions {
        name: &project_name,
        title: &final_title,
        folder: &target_folder.to_string_lossy(),
        output_path: &output_path,
        target_duration: &target_duration,
        seed: args.seed,
    });

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
    })
}

/// List the curated vibe catalog: key, name, rationale, default target
/// duration.
pub fn list_vibes() -> Vec<serde_json::Value> {
    vibes::catalog()
}
