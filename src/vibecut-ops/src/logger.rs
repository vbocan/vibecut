//! Session logger for MCP, extended so
//! `vibecut-web` writes to the identical substrate instead of
//! re-deriving its own. Originally lived in `vibecut-mcp`; moved
//! here once the web transport needed the same "what happened, when"
//! record for its session-history panel — nothing about it was ever
//! MCP-specific, it just recorded whichever tool/route called it.
//!
//! Every call records a one-line JSONL entry under
//! `/projects/sessions/<container_iso>.jsonl`. The file is the
//! "substrate the paper needs": concrete, replayable, deterministic.
//!
//! Each entry is small and human-readable — args + a curated result
//! excerpt + elapsed time. Heavy payloads (image bytes, full TOML
//! content) are dropped. The contract is "enough to replay this call
//! and compare outputs."
//!
//! The logger is best-effort: a missing `/projects/sessions` directory
//! or a write failure logs a warning via `tracing` but never fails the
//! calling operation. Research substrate must not break user flow.

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use serde::Serialize;
use serde_json::json;

use crate::error::OpsError;
use crate::paths::PROJECTS_ROOT;

/// Where session logs land inside the container.
const SESSIONS_DIR: &str = "/projects/sessions";

/// One-line JSONL record describing a single tool call.
///
/// The shape is deliberately lean: `tool`, `args` (as the LLM passed
/// them), `result_excerpt` (a curated subset of the tool's response —
/// the fields we care about for replay determinism), `elapsed_ms`.
#[derive(Debug, Clone, Serialize)]
pub struct CallRecord<'a> {
    /// ISO-8601 UTC timestamp of when the call STARTED.
    pub ts: String,
    /// Schema marker so future log readers can branch.
    pub event: &'static str,
    /// Tool name as registered by `#[tool]`.
    pub tool: &'a str,
    /// Args the LLM passed. Same JSON shape as the JsonSchema variant.
    pub args: serde_json::Value,
    /// Curated excerpt of the tool's response. See `record_excerpt`
    /// helpers in `lib.rs` for what each tool extracts. Bounded.
    pub result_excerpt: serde_json::Value,
    /// Wall-clock duration of the call.
    pub elapsed_ms: u64,
    /// Whether the tool ran to completion. `false` for errors so the
    /// replay harness can distinguish drift from outright failure.
    pub ok: bool,
}

/// Best-effort, thread-safe, append-only JSONL session logger.
#[derive(Clone)]
pub struct SessionLogger {
    inner: Arc<LoggerInner>,
}

struct LoggerInner {
    /// `Some(file)` if open; `None` if logging is disabled or the
    /// open() failed. Wrapped in a Mutex so multiple in-flight tool
    /// calls serialize at the file boundary (writes are < 4 KB so
    /// contention is negligible).
    file: Mutex<Option<File>>,
    /// Path of the log file if we managed to open one. Preserved so
    /// `path()` can surface it (handy for tests and the response
    /// metadata of the very first tool call in a session).
    #[allow(dead_code)] // exposed via SessionLogger::path; future "where's my log?" tool
    path: Option<PathBuf>,
}

impl SessionLogger {
    /// Open a fresh log file under `/projects/sessions/`. Filename is
    /// `<iso8601>_<7-char-random>.jsonl`, derived from the wall-clock
    /// at construction so multiple containers / restarts don't
    /// collide. Falls back to a disabled logger (silently) if the
    /// directory or file can't be created — research substrate is
    /// best-effort, never blocking.
    ///
    /// `component` / `version` identify which transport is writing
    /// (`"vibecut-mcp"` / `"vibecut-web"`, each passing its own
    /// `env!("CARGO_PKG_VERSION")`) so a session-history view spanning
    /// both an LLM conversation and direct browser use can tell them
    /// apart. Kept as parameters rather than baked in here since this
    /// crate doesn't know which binary is calling it.
    pub fn open(component: &str, version: &str) -> Self {
        let now = Utc::now();
        let stamp = now.format("%Y%m%d_%H%M%S").to_string();
        // 7-char random suffix from the UNIX-nanos low bits — enough to
        // disambiguate containers started in the same second without
        // pulling in a uuid dep.
        let nanos = now.timestamp_nanos_opt().unwrap_or(0) as u64;
        let suffix: String = format!("{:07x}", nanos & 0xFFFFFFF);
        let filename = format!("{stamp}_{suffix}.jsonl");

        let dir = PathBuf::from(SESSIONS_DIR);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(
                "session logger: could not create {}: {e}; logging disabled",
                dir.display()
            );
            return Self::disabled();
        }
        let path = dir.join(&filename);
        let file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(
                    "session logger: could not open {}: {e}; logging disabled",
                    path.display()
                );
                return Self::disabled();
            }
        };
        let logger = Self {
            inner: Arc::new(LoggerInner {
                file: Mutex::new(Some(file)),
                path: Some(path.clone()),
            }),
        };
        // Write a session_start header so replay tooling can find the
        // boundary and pick up environment context.
        let header = json!({
            "ts": now.to_rfc3339(),
            "event": "session_start",
            "logger_path": path.to_string_lossy(),
            "component": component,
            "component_version": version,
            "projects_root": PROJECTS_ROOT,
        });
        let _ = logger.write_raw(&header);
        tracing::info!("session log: {}", path.display());
        logger
    }

    /// Logger that swallows everything. Used when the caller has
    /// explicitly opted out, or when /projects/sessions can't be
    /// created.
    pub fn disabled() -> Self {
        Self {
            inner: Arc::new(LoggerInner {
                file: Mutex::new(None),
                path: None,
            }),
        }
    }

    /// Path of the active log file, or `None` if disabled.
    #[allow(dead_code)] // public API for a future "where's my log?" tool
    pub fn path(&self) -> Option<&std::path::Path> {
        self.inner.path.as_deref()
    }

    /// Record one tool call. Best-effort: write failures go to
    /// `tracing` but do not propagate to the caller. The tool call
    /// itself has already finished — we don't want a flaky write
    /// to surface as a tool error.
    pub fn record(
        &self,
        tool: &str,
        args: serde_json::Value,
        result_excerpt: serde_json::Value,
        elapsed_ms: u64,
        ok: bool,
    ) {
        let rec = CallRecord {
            ts: Utc::now().to_rfc3339(),
            event: "tool_call",
            tool,
            args,
            result_excerpt,
            elapsed_ms,
            ok,
        };
        if let Err(e) = self.write_raw(&rec) {
            tracing::warn!("session logger: write failed: {e}");
        }
    }

    fn write_raw<T: Serialize>(&self, value: &T) -> std::io::Result<()> {
        let mut guard = self.inner.file.lock().expect("session logger mutex poisoned");
        let Some(file) = guard.as_mut() else {
            return Ok(()); // Disabled — silent no-op.
        };
        let mut bytes = serde_json::to_vec(value)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        bytes.push(b'\n');
        file.write_all(&bytes)?;
        // Flush each entry so a crash doesn't lose the in-flight call.
        // Throughput cost is irrelevant at our scale (a few writes per minute).
        file.flush()?;
        Ok(())
    }
}

impl Default for SessionLogger {
    fn default() -> Self {
        Self::disabled()
    }
}

/// One entry in [`list_sessions`] — enough for a history panel to show
/// a browsable list without reading every file's contents up front.
#[derive(Debug, Clone, Serialize)]
pub struct SessionFileInfo {
    pub filename: String,
    pub path: String,
    pub size_bytes: u64,
    /// Best-effort filesystem mtime, RFC-3339. `None` if the platform
    /// can't report it (rare) — the filename's own timestamp prefix
    /// is the fallback a caller should sort by either way.
    pub modified: Option<String>,
}

/// List session JSONL files under `/projects/sessions/`, newest first
/// by filename (the `<iso8601>_<suffix>.jsonl` naming already sorts
/// chronologically as a plain string). Returns an empty list — not an
/// error — when the directory doesn't exist yet, since "no sessions
/// recorded" is the normal state for a container nobody has used.
pub fn list_sessions() -> Result<Vec<SessionFileInfo>, OpsError> {
    list_sessions_in(std::path::Path::new(SESSIONS_DIR))
}

/// [`list_sessions`]'s actual logic, taking the directory as a
/// parameter so tests exercise real filesystem behavior (found files,
/// missing directory, non-`.jsonl` files ignored) against a temp dir
/// instead of the hardcoded `/projects/sessions` — which, unlike
/// `/footage` elsewhere in this workspace, this crate's *own* tests
/// can end up creating: a live `vibecut-web`/`vibecut-mcp` run
/// on the same host during manual testing writes real session logs
/// there, so "the well-known path doesn't exist" stops being a safe
/// assumption for a unit test the moment anyone has actually run the
/// server locally.
fn list_sessions_in(dir: &std::path::Path) -> Result<Vec<SessionFileInfo>, OpsError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(dir)
        .map_err(|e| OpsError::internal(format!("could not read {}: {e}", dir.display())))?;

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let meta = entry.metadata().ok();
        let size_bytes = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let modified = meta
            .and_then(|m| m.modified().ok())
            .map(|t| chrono::DateTime::<Utc>::from(t).to_rfc3339());
        out.push(SessionFileInfo {
            filename: path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: path.to_string_lossy().into_owned(),
            size_bytes,
            modified,
        });
    }
    out.sort_by(|a, b| b.filename.cmp(&a.filename));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_logger_swallows_records() {
        let l = SessionLogger::disabled();
        l.record(
            "noop",
            json!({"x": 1}),
            json!({"y": 2}),
            42,
            true,
        );
        assert!(l.path().is_none());
    }

    #[test]
    fn record_serialises_to_jsonl_shape() {
        // We can't write to /projects/sessions in unit tests (the dir
        // doesn't exist on the host), but we can exercise the JSON shape.
        let rec = CallRecord {
            ts: "2026-05-10T12:00:00Z".into(),
            event: "tool_call",
            tool: "render",
            args: json!({"config_path": "/projects/x.toml"}),
            result_excerpt: json!({"ok": true, "n_segments": 28}),
            elapsed_ms: 30000,
            ok: true,
        };
        let s = serde_json::to_string(&rec).unwrap();
        // One line, parseable.
        assert!(!s.contains('\n'));
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["tool"], "render");
        assert_eq!(v["ok"], true);
        assert_eq!(v["result_excerpt"]["n_segments"], 28);
    }

    /// Exercises `list_sessions_in` against a real temp directory
    /// rather than the hardcoded `/projects/sessions` — see that
    /// function's doc comment for why: unlike `/footage` elsewhere in
    /// this workspace, this crate's own tests (and any manual
    /// `vibecut-web`/`vibecut-mcp` run on the same host) can
    /// make `/projects/sessions` really exist, so asserting against
    /// the well-known path isn't hermetic here.
    #[test]
    fn list_sessions_empty_when_dir_missing() {
        let dir = std::env::temp_dir().join(format!("sv_logger_missing_{}", std::process::id()));
        // Deliberately not created.
        let sessions = list_sessions_in(&dir).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn list_sessions_finds_jsonl_and_ignores_other_files() {
        let dir = std::env::temp_dir().join(format!("sv_logger_list_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("20260101_000000_aaa.jsonl"), "{}\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "ignore me").unwrap();

        let sessions = list_sessions_in(&dir).unwrap();
        assert_eq!(sessions.len(), 1, "expected only the .jsonl file: {sessions:?}");
        assert_eq!(sessions[0].filename, "20260101_000000_aaa.jsonl");
        assert!(sessions[0].size_bytes > 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
