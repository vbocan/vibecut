//! Read/write raw TOML config text — the file-level counterpart of
//! [`crate::feedback::apply_feedback_to_file`], for callers (the web
//! Config editor) that want to hand-edit the TOML directly instead of
//! going through a structured [`crate::feedback::FeedbackAction`].

use serde::Serialize;
use vibecut::project_config::Config;

use crate::error::OpsError;
use crate::paths::resolve_projects;

#[derive(Debug, Clone, Serialize)]
pub struct ConfigText {
    pub config_path: String,
    pub config_toml: String,
}

/// Read a config file's raw TOML text — no parsing, just the bytes on
/// disk as a string. Used to refresh a UI's editor after a structured
/// mutation (`apply_feedback`) changed the file out from under it.
pub async fn read_config_toml(config_path: &str) -> Result<ConfigText, OpsError> {
    let path = resolve_projects(config_path);
    if !path.exists() {
        return Err(OpsError::not_found_with(
            "config",
            path.to_string_lossy().into_owned(),
        ));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| OpsError::internal(format!("could not read {}: {e}", path.display())))?;
    Ok(ConfigText {
        config_path: path.to_string_lossy().into_owned(),
        config_toml: text,
    })
}

/// Parse `toml_text` as a [`Config`], validate it, and write it to
/// `config_path` — the same load-shape guarantee
/// `apply_feedback_to_file` gives structured mutations, extended to
/// hand-edited TOML: the pipeline never sees a config it would
/// reject, whether the edit came from a feedback action or a human
/// typing directly into a textarea.
pub async fn write_config_toml(config_path: &str, toml_text: &str) -> Result<ConfigText, OpsError> {
    let cfg: Config = toml::from_str(toml_text)
        .map_err(|e| OpsError::invalid(format!("invalid TOML: {e}")))?;
    cfg.validate()
        .map_err(|e| OpsError::invalid(format!("config does not validate: {e:#}")))?;

    let path = resolve_projects(config_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&path, toml_text)
        .map_err(|e| OpsError::internal(format!("could not write {}: {e}", path.display())))?;

    Ok(ConfigText {
        config_path: path.to_string_lossy().into_owned(),
        config_toml: toml_text.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_toml() -> String {
        r#"
schema_version = 1
[project]
name = "x"
target_duration = "10s"
fps = 30
[input]
folder = "."
[[outputs]]
path = "out.mp4"
resolution = "1920x1080"
"#
        .to_string()
    }

    #[tokio::test]
    async fn write_then_read_round_trips() {
        let dir = std::env::temp_dir().join(format!("sv_config_io_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cfg.toml");
        let toml_text = valid_toml();

        let written = write_config_toml(&path.to_string_lossy(), &toml_text)
            .await
            .expect("valid config should write");
        assert_eq!(written.config_toml, toml_text);

        let read = read_config_toml(&path.to_string_lossy())
            .await
            .expect("just-written config should read back");
        assert_eq!(read.config_toml, toml_text);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn write_rejects_unparseable_toml() {
        let dir = std::env::temp_dir().join(format!("sv_config_io_bad_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cfg.toml");

        let err = write_config_toml(&path.to_string_lossy(), "this is not [ toml")
            .await
            .unwrap_err();
        assert!(matches!(err, OpsError::InvalidInput { .. }));
        assert!(!path.exists(), "invalid TOML must not be written to disk");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn write_rejects_parseable_but_invalid_config() {
        let dir = std::env::temp_dir().join(format!("sv_config_io_invalid_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cfg.toml");

        // Parses fine as TOML, but `outputs` is empty — Config::validate rejects it.
        let bad = r#"
schema_version = 1
[project]
name = "x"
target_duration = "10s"
fps = 30
[input]
folder = "."
"#;
        let err = write_config_toml(&path.to_string_lossy(), bad).await.unwrap_err();
        assert!(matches!(err, OpsError::InvalidInput { .. }));
        assert!(!path.exists(), "config that fails validate() must not be written");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn read_missing_config_is_not_found() {
        let err = read_config_toml("/nonexistent/path/cfg.toml").await.unwrap_err();
        assert!(matches!(err, OpsError::NotFound { .. }));
    }
}
