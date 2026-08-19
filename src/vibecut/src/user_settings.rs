//! Per-user persistent settings.
//!
//! Stores tuning values that are derived from local hardware (e.g. the
//! optimal `visual_workers` count produced by `vibecut benchmark`).
//! Lives at:
//!   - Windows: `%APPDATA%\vibecut\settings.toml`
//!   - macOS / Linux: `~/.config/vibecut/settings.toml`
//!
//! Resolution priority (highest first) for `visual_workers`:
//!   1. `VIBECUT_VISUAL_WORKERS` environment variable.
//!   2. `visual_workers` in the user settings file.
//!   3. Hardcoded conservative default (`DEFAULT_VISUAL_WORKERS`).
//!
//! Missing or malformed settings are silently treated as "no override" — the
//! file is opt-in tuning, not required configuration.

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// Default conservative `visual_workers` cap when no benchmark has been run.
/// Tuned to be safe on a 1-NVDEC laptop GPU + HDD source. Can be overridden
/// via the env var or the settings file.
pub const DEFAULT_VISUAL_WORKERS: usize = 4;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct UserSettings {
    /// Optimal worker count for the per-source ffmpeg fan-out in visual
    /// analysis. Written by `vibecut benchmark --apply`.
    pub visual_workers: Option<u32>,
    /// ISO-8601 timestamp of the most recent benchmark run that produced
    /// `visual_workers`. Informational; not consumed.
    pub benchmark_run_at: Option<String>,
    /// Brief note about the machine the benchmark ran on (e.g. ffmpeg-detected
    /// `cuda_hwaccel`). Helps users notice when they've moved to a new box.
    pub benchmark_machine: Option<String>,
}

/// Resolve the settings file path. Returns `None` only when both
/// `APPDATA` (Windows) and `HOME` (Unix) are unset, which is exotic enough
/// that we skip persistence in that case.
pub fn settings_path() -> Option<Utf8PathBuf> {
    let base: Utf8PathBuf = if cfg!(windows) {
        let app = std::env::var("APPDATA").ok()?;
        Utf8PathBuf::from(app).join("vibecut")
    } else if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        Utf8PathBuf::from(xdg).join("vibecut")
    } else {
        let home = std::env::var("HOME").ok()?;
        Utf8PathBuf::from(home).join(".config").join("vibecut")
    };
    Some(base.join("settings.toml"))
}

/// Read settings from disk. A missing file is treated as `Default` (so
/// callers don't have to special-case "no file yet"). Malformed contents
/// produce a warning trace but still return defaults — we don't want a
/// stray edit to nuke runs.
pub fn load() -> UserSettings {
    let Some(path) = settings_path() else {
        return UserSettings::default();
    };
    let text = match std::fs::read_to_string(path.as_std_path()) {
        Ok(s) => s,
        Err(_) => return UserSettings::default(),
    };
    match toml::from_str(&text) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(path = %path, error = %e, "user settings parse failed; using defaults");
            UserSettings::default()
        }
    }
}

/// Write settings to disk, creating the parent directory if needed. Returns
/// the path written so callers can show it to the user.
pub fn save(settings: &UserSettings) -> Result<Utf8PathBuf> {
    let path = settings_path()
        .context("could not resolve user settings directory (APPDATA / HOME unset?)")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent.as_std_path())
            .with_context(|| format!("creating settings dir {parent}"))?;
    }
    let body = toml::to_string_pretty(settings).context("serializing settings")?;
    std::fs::write(path.as_std_path(), body).with_context(|| format!("writing {path}"))?;
    Ok(path)
}

/// Resolve the effective worker count using the documented priority:
/// env var > settings file > default. Logs the source so users can see
/// which knob was used.
pub fn resolve_visual_workers() -> usize {
    if let Ok(s) = std::env::var("VIBECUT_VISUAL_WORKERS") {
        if let Ok(n) = s.trim().parse::<usize>() {
            if n >= 1 {
                tracing::info!(workers = n, source = "env", "visual workers");
                return n;
            }
        }
    }
    let settings = load();
    if let Some(n) = settings.visual_workers {
        let n = n as usize;
        if n >= 1 {
            tracing::info!(workers = n, source = "settings file", "visual workers");
            return n;
        }
    }
    tracing::info!(
        workers = DEFAULT_VISUAL_WORKERS,
        source = "default",
        "visual workers (run `vibecut benchmark --apply` to tune)"
    );
    DEFAULT_VISUAL_WORKERS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_settings() {
        let s = UserSettings {
            visual_workers: Some(6),
            benchmark_run_at: Some("2026-05-07T13:30:00Z".into()),
            benchmark_machine: Some("CUDA decode + NVENC".into()),
        };
        let body = toml::to_string_pretty(&s).unwrap();
        let s2: UserSettings = toml::from_str(&body).unwrap();
        assert_eq!(s.visual_workers, s2.visual_workers);
        assert_eq!(s.benchmark_run_at, s2.benchmark_run_at);
        assert_eq!(s.benchmark_machine, s2.benchmark_machine);
    }

    #[test]
    fn unknown_field_is_rejected() {
        let bad = r#"
            visual_workers = 4
            mystery_field = "boo"
        "#;
        assert!(toml::from_str::<UserSettings>(bad).is_err());
    }
}
