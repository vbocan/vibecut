//! Vibecut — programmatic library surface.
//!
//! The vibecut crate compiles two artifacts that share this code:
//! a CLI binary (`src/main.rs`) and this library. The MCP server crate
//! (`vibecut-mcp`) consumes the library directly so it can call
//! `media_probe::discover`, `Config::load`, etc. without subprocess
//! overhead — the goal of the Python→Rust migration.
//!
//! Stable surface (callers can rely on these): every `pub mod` below.
//! Internal helpers used only by the CLI binary stay private to
//! `main.rs`. When the MCP server (or another consumer) needs
//! something internal, promote it here intentionally.

pub mod audio_analysis;
pub mod beat_burst;
pub mod beat_lock;
pub mod beat_sync;
pub mod benchmark;
pub mod cancel;
pub mod disk_cache;
pub mod fonts;
#[cfg(test)]
mod test_env;
pub mod rng;
pub mod gopro_metadata;
pub mod media_probe;
pub mod ml_engine;
pub mod moment_signals;
pub mod multi_angle;
pub mod output_timeline;
pub mod output_format;
pub mod progress;
pub mod project_config;
pub mod selection_planner;
pub mod selection_scoring;
pub mod staging;
pub mod track_meta;
pub mod user_settings;
pub mod video_renderer;
pub mod visual_analysis;

// The pipeline orchestrator (formerly in `main.rs`), moved into the
// library so the MCP server's `render` tool can drive it in-process —
// same code path the CLI uses, no subprocess overhead, no
// stdout-parsing fragility.
pub mod run_pipeline;
