//! Read/write config files — both as raw TOML text (the file-level
//! counterpart of [`crate::feedback::apply_feedback_to_file`], for
//! callers that want to hand-edit the TOML directly) and as a
//! structured [`Config`] (for a guided/wizard editor that never shows
//! TOML syntax to the user at all). Both paths share the same
//! guarantee: the pipeline never sees a config it would reject,
//! whether the edit came from a feedback action, a human typing into a
//! textarea, or a human filling in form fields.
//!
//! Every `config_path` is confined to the `/projects` mount by
//! [`crate::paths::resolve_projects_checked`], and writes additionally
//! require a `.toml` extension — these are reachable from
//! unauthenticated REST and MCP callers.

use std::path::{Path, PathBuf};

use serde::Serialize;
// Re-exported so downstream transports (vibecut-web's structured config
// routes) can name the type without taking a direct dependency on
// `vibecut` themselves — this crate stays the one place that owns the
// pipeline dependency.
pub use vibecut::project_config::Config;

use crate::error::OpsError;
use crate::paths::{ensure_toml_extension, resolve_under};

#[derive(Debug, Clone, Serialize)]
pub struct ConfigText {
    pub config_path: String,
    pub config_toml: String,
}

/// Read a config file's raw TOML text — no parsing, just the bytes on
/// disk as a string. Used to refresh a UI's editor after a structured
/// mutation (`apply_feedback`) changed the file out from under it.
pub async fn read_config_toml(config_path: &str) -> Result<ConfigText, OpsError> {
    read_config_toml_in(&crate::paths::projects_root_for(config_path), config_path).await
}

async fn read_config_toml_in(root: &Path, config_path: &str) -> Result<ConfigText, OpsError> {
    let path = resolve_under(root, config_path)?;
    let text = read_existing(&path)?;
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
    write_config_toml_in(&crate::paths::projects_root_for(config_path), config_path, toml_text).await
}

async fn write_config_toml_in(
    root: &Path,
    config_path: &str,
    toml_text: &str,
) -> Result<ConfigText, OpsError> {
    let path = resolve_write_target(root, config_path)?;
    let cfg: Config = toml::from_str(toml_text)
        .map_err(|e| OpsError::invalid(format!("invalid TOML: {e}")))?;
    cfg.validate()
        .map_err(|e| OpsError::invalid(format!("config does not validate: {e:#}")))?;

    write_creating_parent(&path, toml_text)?;

    Ok(ConfigText {
        config_path: path.to_string_lossy().into_owned(),
        config_toml: toml_text.to_string(),
    })
}

/// Read a config file and parse it into a structured [`Config`] — the
/// guided editor's counterpart of [`read_config_toml`]. No TOML ever
/// reaches the frontend on this path.
pub async fn read_config_structured(config_path: &str) -> Result<Config, OpsError> {
    read_config_structured_in(&crate::paths::projects_root_for(config_path), config_path).await
}

async fn read_config_structured_in(root: &Path, config_path: &str) -> Result<Config, OpsError> {
    let path = resolve_under(root, config_path)?;
    let text = read_existing(&path)?;
    toml::from_str(&text).map_err(|e| OpsError::invalid(format!("invalid config: {e}")))
}

/// Validate `cfg`, serialize it back to TOML, and write it to
/// `config_path` — the same load-shape guarantee every other write
/// path gives, extended to a fully structured input. Returns the
/// config as actually persisted (post-validation, post-round-trip
/// through the serializer) so a caller can trust it as the new
/// canonical state without a separate re-read.
pub async fn write_config_structured(config_path: &str, cfg: &Config) -> Result<Config, OpsError> {
    write_config_structured_in(&crate::paths::projects_root_for(config_path), config_path, cfg).await
}

async fn write_config_structured_in(
    root: &Path,
    config_path: &str,
    cfg: &Config,
) -> Result<Config, OpsError> {
    let path = resolve_write_target(root, config_path)?;
    cfg.validate()
        .map_err(|e| OpsError::invalid(format!("config does not validate: {e:#}")))?;
    let toml_text = toml::to_string_pretty(cfg)
        .map_err(|e| OpsError::internal(format!("could not serialize config: {e}")))?;

    write_creating_parent(&path, &toml_text)?;

    Ok(cfg.clone())
}

/// Confine a write to the projects root and to `.toml` files, checked
/// before anything else so a rejected path never costs a parse.
fn resolve_write_target(root: &Path, config_path: &str) -> Result<PathBuf, OpsError> {
    let path = resolve_under(root, config_path)?;
    ensure_toml_extension(&path)?;
    Ok(path)
}

fn read_existing(path: &Path) -> Result<String, OpsError> {
    if !path.exists() {
        return Err(OpsError::not_found_with(
            "config",
            path.to_string_lossy().into_owned(),
        ));
    }
    std::fs::read_to_string(path)
        .map_err(|e| OpsError::internal(format!("could not read {}: {e}", path.display())))
}

fn write_creating_parent(path: &Path, text: &str) -> Result<(), OpsError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(path, text)
        .map_err(|e| OpsError::internal(format!("could not write {}: {e}", path.display())))
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

    /// A fresh, empty temp dir standing in for `/projects`.
    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sv_config_io_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn assert_invalid<T: std::fmt::Debug>(result: Result<T, OpsError>) {
        assert!(
            matches!(result, Err(OpsError::InvalidInput { .. })),
            "expected InvalidInput, got {result:?}"
        );
    }

    #[tokio::test]
    async fn write_then_read_round_trips() {
        let root = temp_root("round_trip");
        let toml_text = valid_toml();

        let written = write_config_toml_in(&root, "cfg.toml", &toml_text)
            .await
            .expect("valid config should write");
        assert_eq!(written.config_toml, toml_text);
        assert_eq!(PathBuf::from(&written.config_path), root.join("cfg.toml"));

        let read = read_config_toml_in(&root, "cfg.toml")
            .await
            .expect("just-written config should read back");
        assert_eq!(read.config_toml, toml_text);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn absolute_path_under_root_is_accepted() {
        let root = temp_root("abs_under");
        let abs = root.join("nested").join("cfg.toml");

        let written = write_config_toml_in(&root, &abs.to_string_lossy(), &valid_toml())
            .await
            .expect("absolute path under the root should write");
        assert_eq!(PathBuf::from(&written.config_path), abs);
        assert!(abs.exists());

        let read = read_config_toml_in(&root, &abs.to_string_lossy())
            .await
            .expect("absolute path under the root should read");
        assert_eq!(read.config_toml, valid_toml());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn write_rejects_unparseable_toml() {
        let root = temp_root("bad");
        let path = root.join("cfg.toml");

        assert_invalid(write_config_toml_in(&root, "cfg.toml", "this is not [ toml").await);
        assert!(!path.exists(), "invalid TOML must not be written to disk");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn write_rejects_parseable_but_invalid_config() {
        let root = temp_root("invalid");
        let path = root.join("cfg.toml");

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
        assert_invalid(write_config_toml_in(&root, "cfg.toml", bad).await);
        assert!(!path.exists(), "config that fails validate() must not be written");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn read_missing_config_is_not_found() {
        let root = temp_root("missing");
        let err = read_config_toml_in(&root, "cfg.toml").await.unwrap_err();
        assert!(matches!(err, OpsError::NotFound { .. }));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The motivating case: `GET /api/config?config_path=/proc/self/environ`
    /// used to return the server's environment, API keys included.
    #[tokio::test]
    async fn reads_outside_projects_are_rejected() {
        for p in [
            "/proc/self/environ",
            "/etc/passwd",
            "../etc/passwd",
            "..\\..\\etc\\passwd",
            "/projects/../etc/passwd",
        ] {
            assert_invalid(read_config_toml(p).await);
            assert_invalid(read_config_structured(p).await);
        }
    }

    #[tokio::test]
    async fn writes_outside_root_are_rejected_and_not_written() {
        let root = temp_root("escape_root");
        let outside = temp_root("escape_outside");
        let target = outside.join("cfg.toml");
        let outside_name = outside.file_name().unwrap().to_string_lossy().into_owned();
        let cfg = minimal_config();

        let attempts = [
            target.to_string_lossy().into_owned(),
            format!("../{outside_name}/cfg.toml"),
            format!("..\\{outside_name}\\cfg.toml"),
        ];
        for p in &attempts {
            assert_invalid(write_config_toml_in(&root, p, &valid_toml()).await);
            assert_invalid(write_config_structured_in(&root, p, &cfg).await);
        }
        assert!(!target.exists(), "a rejected write must not touch disk");

        // And through the real `/projects` entry points.
        assert_invalid(write_config_toml("/etc/vibecut.toml", &valid_toml()).await);
        assert_invalid(write_config_structured("/etc/vibecut.toml", &cfg).await);

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[tokio::test]
    async fn writes_require_toml_extension() {
        let root = temp_root("non_toml");
        let cfg = minimal_config();

        for p in ["out/render.mp4", "session.jsonl", "cfg"] {
            assert_invalid(write_config_toml_in(&root, p, &valid_toml()).await);
            assert_invalid(write_config_structured_in(&root, p, &cfg).await);
            assert!(!root.join(p).exists(), "{p} must not be written");
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    fn minimal_config() -> Config {
        toml::from_str(&valid_toml()).expect("valid_toml() must parse")
    }

    #[tokio::test]
    async fn structured_write_then_read_round_trips() {
        let root = temp_root("struct");
        let mut cfg = minimal_config();
        cfg.project.name = "structured edit".into();

        let written = write_config_structured_in(&root, "cfg.toml", &cfg)
            .await
            .expect("valid config should write");
        assert_eq!(written.project.name, "structured edit");

        let read = read_config_structured_in(&root, "cfg.toml")
            .await
            .expect("just-written config should read back");
        assert_eq!(read.project.name, "structured edit");
        assert_eq!(read.project.fps, cfg.project.fps);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn structured_write_rejects_invalid_config() {
        let root = temp_root("struct_invalid");
        let path = root.join("cfg.toml");

        let mut cfg = minimal_config();
        cfg.outputs.clear(); // Config::validate rejects an empty outputs list.

        assert_invalid(write_config_structured_in(&root, "cfg.toml", &cfg).await);
        assert!(!path.exists(), "config that fails validate() must not be written");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn structured_read_missing_config_is_not_found() {
        let root = temp_root("struct_missing");
        let err = read_config_structured_in(&root, "cfg.toml").await.unwrap_err();
        assert!(matches!(err, OpsError::NotFound { .. }));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn structured_read_rejects_unparseable_toml() {
        let root = temp_root("struct_bad");
        std::fs::write(root.join("cfg.toml"), "this is not [ toml").unwrap();

        assert_invalid(read_config_structured_in(&root, "cfg.toml").await);

        let _ = std::fs::remove_dir_all(&root);
    }
}
