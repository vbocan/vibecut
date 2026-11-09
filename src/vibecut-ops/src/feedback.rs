//! Feedback grammar v1.
//!
//! The user reacts to a render in plain language. The LLM client maps
//! that reaction onto one of the [`FeedbackAction`] variants here, and
//! [`apply`] mutates a [`Config`] deterministically so the next render
//! reflects the change.
//!
//! Five variants ship in v1, covering the patterns that don't need
//! picker-side support:
//!
//! - [`FeedbackAction::ScaleDuration`] — "shorter" / "longer"
//! - [`FeedbackAction::Calmer`] — "less aggressive" / "calmer"
//! - [`FeedbackAction::Punchier`] — "more energy" / "punchier"
//! - [`FeedbackAction::DifferentMusic`] — "different music"
//! - [`FeedbackAction::Reroll`] — "same but different"
//!
//! Three more (`MoreOf`, `LessOf`, `DropSegment`, `ExtendSegment`) need
//! a `[[selection.exclude]]` mechanism on the picker side that doesn't
//! exist yet.

use std::time::Duration;

use anyhow::{anyhow, Result};
use camino::Utf8PathBuf;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use vibecut::project_config::{
    BeatBurstMode, Config, SelectionExclude, Soundtrack, SourceWeight,
};

use crate::error::OpsError;
use crate::paths::resolve_projects;

/// One discrete pattern of feedback. Each variant is a self-contained
/// deterministic mutation against a [`Config`]; the LLM picks the
/// variant, the server applies it. Adding a new pattern means adding
/// a variant here.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export, tag = "kind", rename_all = "snake_case"))]
pub enum FeedbackAction {
    /// "shorter" / "longer". `factor` scales `[project].target_duration`.
    /// Typical values: `0.75` for "shorter" (-25 %), `1.25` for "longer"
    /// (+25 %), `0.5` for "much shorter", `2.0` for "much longer".
    /// Clamped to `[0.25, 4.0]`. Result is floored at 5 seconds.
    ScaleDuration {
        /// Multiplier applied to the current `target_duration`.
        factor: f32,
    },

    /// "calmer" / "less aggressive". Applies a coordinated set of
    /// mutations that collectively dial down the kinetic feel:
    /// drops `speed_ramp_probability` by 0.3 (floor 0.0); drops
    /// `speed_ramp_factor` by 0.5 (floor 1.5); lengthens the default
    /// xfade by 200 ms; drops `selection.variety` by 0.1 (floor 0.0);
    /// disables `beat_burst` if a soundtrack is configured.
    Calmer,

    /// "more energy" / "punchier". Inverse of [`Calmer`]. Raises
    /// `speed_ramp_probability` by 0.3 (cap 1.0); raises
    /// `speed_ramp_factor` by 0.5 (cap 4.0); shortens the xfade by
    /// 100 ms (floor 200 ms); enables `beat_burst = "on"` if a
    /// soundtrack is configured. When the previous render had
    /// `speed_ramp_probability == 0` (e.g. drone_cinematic), this
    /// activates the speed-ramp at a sensible default.
    Punchier,

    /// "different music". Replaces the soundtrack file. The visual
    /// analysis cache stays warm across this change so the next render
    /// is fast (no need to re-probe sources). If the project had no
    /// `[soundtrack]` section, one is synthesised with sensible
    /// defaults around the new file.
    DifferentMusic {
        /// Container path to the new soundtrack mp3/wav file
        /// (typically under `/projects/music/...`).
        path: String,
    },

    /// "same but different". Bumps `[project].seed` to a fresh value
    /// derived from the system clock + a SplitMix64 mix. Everything
    /// else stays untouched, so the picker explores a different
    /// neighbourhood of the same scoring landscape.
    Reroll,

    /// "more of [source X]". Appends a
    /// `[[selection.source_weights]]` entry per requested source_id
    /// with `weight = 1.5` (50 % boost). Picker multiplies the score
    /// curve before composing picks, so those sources win more
    /// candidate slots in the next render.
    MoreOf {
        /// Source IDs to boost. For GoPro chapters these are the
        /// trailing-4-digit ids (e.g. `["0320", "0321"]`).
        source_ids: Vec<String>,
    },

    /// "less of [source X]". Appends a
    /// `[[selection.source_weights]]` entry per requested source_id
    /// with `weight = 0.0` (effectively drops that source from picks).
    /// Use Reroll plus a few LessOf entries to refresh a cut while
    /// excluding specific sources.
    LessOf {
        /// Source IDs to drop.
        source_ids: Vec<String>,
    },

    /// "drop the bit at M:SS". Manifest-aware exclude.
    /// Consumes a manifest from a previous render to identify which
    /// segment landed at the user-referenced output time, then adds a
    /// `[[selection.exclude]]` entry covering that segment's
    /// source-window so the next render avoids it.
    ///
    /// The MCP server resolves the manifest path by convention
    /// (`<output>.manifest.json` next to the rendered mp4); the
    /// caller can override via `manifest_path`.
    DropSegment {
        /// Output-time in seconds the user pointed at. The action
        /// finds the segment whose output-position covers this t.
        at_seconds: f32,
        /// Optional explicit manifest path. If omitted, derived from
        /// the config's `[outputs].path`.
        manifest_path: Option<String>,
    },
}

/// A [`FeedbackAction`] pick plus the one-line rationale behind it —
/// the free-text-brief feature asks an LLM for both in a
/// single structured-output call. Defined here, wrapping
/// `FeedbackAction` directly, rather than composed in
/// `vibecut-web` from two separately-generated schemas: a
/// `schemars`-generated schema's internal `$ref`/`$defs` assume
/// they're resolved against their own document root, so nesting one
/// schema as a property's value inside a second, independently-built
/// schema doesn't reliably resolve those references. One `schema_for!`
/// call over this wrapper sidesteps the problem entirely.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct FeedbackPick {
    pub action: FeedbackAction,
    /// One sentence explaining the pick, shown to the user.
    pub rationale: String,
}

/// JSON Schema for [`FeedbackPick`], as a plain `serde_json::Value` —
/// used to build the Anthropic tool-use `input_schema` for the
/// free-text brief feature without `vibecut-web` needing its own
/// `schemars` dependency just to call `schema_for!` itself.
pub fn feedback_pick_json_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(FeedbackPick)).unwrap_or(serde_json::Value::Null)
}

/// One field's before / after — what the server tells the LLM about a
/// mutation it just performed. The LLM relays a human-friendly summary
/// back to the user.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct ChangeNote {
    /// Dotted path of the mutated field, e.g. `"project.target_duration"`.
    pub field: String,
    /// Stringified previous value.
    pub before: String,
    /// Stringified new value.
    pub after: String,
}

/// Apply `action` to `cfg` in place. Returns the list of fields that
/// actually changed (some no-op cases produce an empty list — e.g.
/// `Calmer` on a config that already has speed_ramp_probability = 0).
pub fn apply(cfg: &mut Config, action: &FeedbackAction) -> Result<Vec<ChangeNote>> {
    match action {
        FeedbackAction::ScaleDuration { factor } => apply_scale_duration(cfg, *factor),
        FeedbackAction::Calmer => Ok(apply_calmer(cfg)),
        FeedbackAction::Punchier => Ok(apply_punchier(cfg)),
        FeedbackAction::DifferentMusic { path } => apply_different_music(cfg, path),
        FeedbackAction::Reroll => Ok(apply_reroll(cfg)),
        FeedbackAction::DropSegment { at_seconds, manifest_path } => {
            apply_drop_segment(cfg, *at_seconds, manifest_path.as_deref())
        }
        FeedbackAction::MoreOf { source_ids } => Ok(apply_source_weight(cfg, source_ids, 1.5)),
        FeedbackAction::LessOf { source_ids } => Ok(apply_source_weight(cfg, source_ids, 0.0)),
    }
}

fn apply_source_weight(
    cfg: &mut Config,
    source_ids: &[String],
    weight: f32,
) -> Vec<ChangeNote> {
    let mut notes = Vec::new();
    for sid in source_ids {
        // Last-write-wins: drop any prior entry for the same source_id
        // before appending the new one, so calling MoreOf then LessOf
        // on the same source doesn't leave both entries fighting.
        let before = cfg
            .selection
            .source_weights
            .iter()
            .find(|w| w.source_id == *sid)
            .map(|w| format!("{:.2}", w.weight))
            .unwrap_or_else(|| "(unset; default 1.0)".to_string());
        cfg.selection
            .source_weights
            .retain(|w| w.source_id != *sid);
        cfg.selection.source_weights.push(SourceWeight {
            source_id: sid.clone(),
            weight,
        });
        notes.push(note(
            &format!("selection.source_weights[{sid}]"),
            &before,
            &format!("{weight:.2}"),
        ));
    }
    notes
}

// ---------- individual mutations ----------

fn apply_scale_duration(cfg: &mut Config, factor: f32) -> Result<Vec<ChangeNote>> {
    if !factor.is_finite() || factor <= 0.0 {
        return Err(anyhow!("scale factor must be positive and finite; got {factor}"));
    }
    let factor = factor.clamp(0.25, 4.0);
    let before = cfg.project.target_duration;
    let new_secs = (before.as_secs_f32() * factor).max(5.0);
    let after = Duration::from_secs_f32(new_secs);
    cfg.project.target_duration = after;
    Ok(vec![note(
        "project.target_duration",
        &fmt_duration(before),
        &fmt_duration(after),
    )])
}

fn apply_calmer(cfg: &mut Config) -> Vec<ChangeNote> {
    let mut notes = Vec::new();

    let before = cfg.transition.speed_ramp_probability;
    let after = (before - 0.3).max(0.0);
    if (before - after).abs() > f32::EPSILON {
        cfg.transition.speed_ramp_probability = after;
        notes.push(note(
            "transition.speed_ramp_probability",
            &before.to_string(),
            &after.to_string(),
        ));
    }

    let before = cfg.transition.speed_ramp_factor;
    let after = (before - 0.5).max(1.5);
    if (before - after).abs() > f32::EPSILON {
        cfg.transition.speed_ramp_factor = after;
        notes.push(note(
            "transition.speed_ramp_factor",
            &before.to_string(),
            &after.to_string(),
        ));
    }

    // Longer crossfades read calmer, but each one eats into both clips it
    // joins. Repeated "calmer" taps must not grow it past what the shortest
    // clip can afford, or renders start failing.
    let before = cfg.transition.duration;
    let after = (before + Duration::from_millis(200)).min(calmer_crossfade_cap(cfg));
    if after > before {
        cfg.transition.duration = after;
        notes.push(note(
            "transition.duration",
            &fmt_duration(before),
            &fmt_duration(after),
        ));
    }

    let before = cfg.selection.variety;
    let after = (before - 0.1).max(0.0);
    if (before - after).abs() > f32::EPSILON {
        cfg.selection.variety = after;
        notes.push(note(
            "selection.variety",
            &before.to_string(),
            &after.to_string(),
        ));
    }

    if let Some(snd) = cfg.soundtrack.as_mut() {
        let before = snd.beat_burst;
        if before != BeatBurstMode::Off {
            snd.beat_burst = BeatBurstMode::Off;
            notes.push(note(
                "soundtrack.beat_burst",
                &format!("{before:?}").to_lowercase(),
                "off",
            ));
        }
    }

    notes
}

fn apply_punchier(cfg: &mut Config) -> Vec<ChangeNote> {
    let mut notes = Vec::new();

    let before = cfg.transition.speed_ramp_probability;
    // If ramps were entirely off (calm vibe), kick to 0.5; otherwise nudge up.
    let after = if before <= 0.0 {
        0.5
    } else {
        (before + 0.3).min(1.0)
    };
    if (before - after).abs() > f32::EPSILON {
        cfg.transition.speed_ramp_probability = after;
        notes.push(note(
            "transition.speed_ramp_probability",
            &before.to_string(),
            &after.to_string(),
        ));
    }

    let before = cfg.transition.speed_ramp_factor;
    let after = if before < 1.5 {
        3.0
    } else {
        (before + 0.5).min(4.0)
    };
    if (before - after).abs() > f32::EPSILON {
        cfg.transition.speed_ramp_factor = after;
        notes.push(note(
            "transition.speed_ramp_factor",
            &before.to_string(),
            &after.to_string(),
        ));
    }

    let before = cfg.transition.duration;
    let target = before.saturating_sub(Duration::from_millis(100));
    let after = target.max(Duration::from_millis(200));
    if before != after {
        cfg.transition.duration = after;
        notes.push(note(
            "transition.duration",
            &fmt_duration(before),
            &fmt_duration(after),
        ));
    }

    if let Some(snd) = cfg.soundtrack.as_mut() {
        let before = snd.beat_burst;
        if before != BeatBurstMode::On {
            snd.beat_burst = BeatBurstMode::On;
            notes.push(note(
                "soundtrack.beat_burst",
                &format!("{before:?}").to_lowercase(),
                "on",
            ));
        }
    }

    notes
}

fn apply_different_music(cfg: &mut Config, new_path: &str) -> Result<Vec<ChangeNote>> {
    let new_pathbuf = Utf8PathBuf::from(new_path);
    let mut notes = Vec::new();

    match cfg.soundtrack.as_mut() {
        Some(snd) => {
            let before = snd
                .files
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            snd.files = vec![new_pathbuf];
            notes.push(note(
                "soundtrack.files",
                if before.is_empty() { "(none)" } else { &before },
                new_path,
            ));
        }
        None => {
            // Synthesise a default-shaped Soundtrack with just the
            // file set. Round-tripping through serde keeps us aligned
            // with whatever defaults the vibecut schema currently
            // declares — adding new soundtrack fields upstream
            // doesn't break us.
            let stub = format!(r#"file = "{}""#, new_path.replace('"', r#"\""#));
            let snd: Soundtrack = toml::from_str(&stub)?;
            cfg.soundtrack = Some(snd);
            notes.push(note(
                "soundtrack.files",
                "(no soundtrack section)",
                new_path,
            ));
        }
    }

    Ok(notes)
}

fn apply_drop_segment(
    cfg: &mut Config,
    at_seconds: f32,
    explicit_manifest_path: Option<&str>,
) -> Result<Vec<ChangeNote>> {
    if !at_seconds.is_finite() || at_seconds < 0.0 {
        return Err(anyhow!(
            "at_seconds must be a non-negative finite number; got {at_seconds}"
        ));
    }
    // Resolve the manifest path. Either explicit, or derived from
    // the config's first output: `<output_path>.manifest.json` —
    // matching the convention `manifest_path_for` uses in run_pipeline.
    let manifest_path = match explicit_manifest_path {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            let out = cfg
                .outputs
                .first()
                .ok_or_else(|| anyhow!("config has no [[outputs]] entry"))?;
            let mut p = out.path.as_std_path().to_path_buf();
            // Append ".manifest.json" to the basename. We don't use
            // `.with_extension()` because it would replace `.mp4`.
            let mut s = p.as_os_str().to_os_string();
            s.push(".manifest.json");
            p = std::path::PathBuf::from(&s);
            // Fallback: try `<stem>.manifest.json` (the convention
            // vibecut actually uses when there's a single output).
            if !p.exists() {
                if let Some(stem) = out.path.file_stem() {
                    if let Some(parent) = out.path.parent() {
                        p = parent
                            .as_std_path()
                            .join(format!("{stem}.manifest.json"));
                    }
                }
            }
            p
        }
    };

    let manifest_text = std::fs::read_to_string(&manifest_path).map_err(|e| {
        anyhow!(
            "could not read manifest {}: {e}",
            manifest_path.display()
        )
    })?;
    let manifest: serde_json::Value =
        serde_json::from_str(&manifest_text).map_err(|e| {
            anyhow!(
                "manifest {} is not valid JSON: {e}",
                manifest_path.display()
            )
        })?;

    // Find the segment whose output position covers `at_seconds`. The
    // manifest stores segments in output order with a per-segment
    // `duration_secs`. We sum durations to compute output positions
    // until we land on (or past) the target.
    let segments = manifest
        .get("segments")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("manifest has no `segments` array"))?;
    let intro_dur = manifest
        .get("intro_duration_secs")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as f32;

    let mut output_offset = intro_dur;
    let mut found: Option<&serde_json::Value> = None;
    let mut seg_index = 0usize;
    for (i, s) in segments.iter().enumerate() {
        let dur = s.get("duration_secs").and_then(|v| v.as_f64()).unwrap_or(0.0)
            as f32;
        let end = output_offset + dur;
        if at_seconds >= output_offset && at_seconds < end {
            found = Some(s);
            seg_index = i;
            break;
        }
        output_offset = end;
    }
    let seg = found.ok_or_else(|| {
        anyhow!(
            "no segment covers output time {at_seconds:.2}s (manifest has {} segments)",
            segments.len()
        )
    })?;

    // Pull the source-window we want to exclude.
    let chapter_path = seg
        .get("chapter_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("segment has no chapter_path"))?;
    let in_chapter_start = seg
        .get("in_chapter_start_secs")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| anyhow!("segment has no in_chapter_start_secs"))?
        as f32;
    let segment_dur = seg
        .get("duration_secs")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| anyhow!("segment has no duration_secs"))?
        as f32;
    let basename = std::path::Path::new(chapter_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(chapter_path)
        .to_string();

    // Pad the exclude window slightly so the picker can't snap right
    // back to the same neighbourhood — enough to skip past min_clip
    // worth of overlap. 0.5 s on each side is plenty.
    let pad = 0.5_f32;
    let lo = (in_chapter_start - pad).max(0.0);
    let hi = in_chapter_start + segment_dur + pad;

    cfg.selection.exclude.push(SelectionExclude {
        file: basename.clone(),
        start: Duration::from_secs_f32(lo),
        end: Duration::from_secs_f32(hi),
    });

    Ok(vec![
        ChangeNote {
            field: "selection.exclude".into(),
            before: format!("(no exclude entries for {basename})"),
            after: format!(
                "added: file={:?} start={:.2}s end={:.2}s (segment #{} at output_t={:.2}s)",
                basename, lo, hi, seg_index, at_seconds
            ),
        },
    ])
}

fn apply_reroll(cfg: &mut Config) -> Vec<ChangeNote> {
    let before = cfg.project.seed;
    let after = generate_random_seed();
    cfg.project.seed = Some(after);
    vec![note(
        "project.seed",
        &before
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(unset)".into()),
        &after.to_string(),
    )]
}

// ---------- helpers ----------

fn note(field: &str, before: &str, after: &str) -> ChangeNote {
    ChangeNote {
        field: field.into(),
        before: before.into(),
        after: after.into(),
    }
}

/// Pretty-print a `Duration` the way humantime does. We don't pull in
/// humantime here just for this — the manual format matches what the
/// vibecut TOML uses (e.g. "3m" / "750ms").
/// The longest crossfade "calmer" will set: 1.2 s, and never more than
/// 35% of the shortest clip the selection allows.
fn calmer_crossfade_cap(cfg: &Config) -> Duration {
    const ABS_CAP: Duration = Duration::from_millis(1200);
    let rel = cfg.selection.min_clip.mul_f64(0.35);
    ABS_CAP.min(rel).max(Duration::from_millis(100))
}

fn fmt_duration(d: Duration) -> String {
    let total_ms = d.as_millis();
    if total_ms == 0 {
        return "0s".into();
    }
    if total_ms % 1000 != 0 {
        return format!("{}ms", total_ms);
    }
    let total_secs = total_ms / 1000;
    if total_secs % 60 != 0 {
        return format!("{}s", total_secs);
    }
    let mins = total_secs / 60;
    if mins % 60 != 0 {
        return format!("{}m", mins);
    }
    let hours = mins / 60;
    format!("{}h", hours)
}

/// SplitMix64 mix of the system clock — same shape as the seed
/// generator used inside the vibecut binary. We keep our own copy
/// rather than expose theirs because the contract is stable (it just
/// has to be deterministic + reasonably uniform).
fn generate_random_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0xa1b2c3d4_e5f60718);
    let mut x = nanos;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^= x >> 31;
    x
}

// ---------- file-level orchestration ----------

/// Result of applying feedback to a config file on disk: the mutated
/// path, what changed, and whether the mutation was a no-op.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyFeedbackResult {
    pub config_path: String,
    pub changes: Vec<ChangeNote>,
}

/// Load the config at `config_path`, apply `action`, validate the
/// result, and write it back to the same path. This is the
/// file-on-disk counterpart of [`apply`] — the MCP `apply_feedback`
/// tool (and, eventually, the web UI's quick-feedback buttons) call
/// this directly instead of hand-rolling the load/mutate/validate/save
/// round-trip themselves.
pub async fn apply_feedback_to_file(
    config_path: &str,
    action: &FeedbackAction,
) -> Result<ApplyFeedbackResult, OpsError> {
    let cfg_path = resolve_projects(config_path);
    if !cfg_path.exists() {
        return Err(OpsError::not_found_with(
            "config",
            cfg_path.to_string_lossy().into_owned(),
        ));
    }
    let cfg_camino = Utf8PathBuf::from_path_buf(cfg_path.clone())
        .map_err(|_| OpsError::invalid(format!("non-UTF-8 config path: {}", cfg_path.display())))?;

    let mut cfg = Config::load(&cfg_camino)
        .map_err(|e| OpsError::internal(format!("could not load config: {e:#}")))?;

    let notes = apply(&mut cfg, action)
        .map_err(|e| OpsError::invalid(format!("feedback action failed: {e:#}")))?;

    // Re-serialise and validate before persisting — the caller gets a
    // clean error rather than discovering corruption only when
    // `render` blows up. Invalid-input, not internal: the mutation
    // itself is well-formed, it just produced a config the pipeline
    // would reject.
    cfg.validate()
        .map_err(|e| OpsError::invalid(format!("mutation produced invalid config: {e:#}")))?;

    let new_toml = toml::to_string_pretty(&cfg)
        .map_err(|e| OpsError::internal(format!("could not serialise mutated config: {e}")))?;
    std::fs::write(&cfg_path, &new_toml)
        .map_err(|e| OpsError::internal(format!("could not write {}: {e}", cfg_path.display())))?;

    Ok(ApplyFeedbackResult {
        config_path: cfg_path.to_string_lossy().into_owned(),
        changes: notes,
    })
}

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;

    /// The schema must be a usable Anthropic tool `input_schema`: an
    /// object type exposing both `action` and `rationale`, with
    /// `FeedbackAction`'s variants reachable (whether inlined or via
    /// `$defs` — schemars' exact layout isn't the contract here, "the
    /// LLM can construct a valid `pick_feedback_action` call from it"
    /// is).
    #[test]
    fn feedback_pick_schema_exposes_action_and_rationale() {
        let schema = feedback_pick_json_schema();
        assert_eq!(schema["type"], "object");
        let props = &schema["properties"];
        assert!(props.get("action").is_some(), "schema: {schema:#}");
        assert!(props.get("rationale").is_some(), "schema: {schema:#}");
    }

    /// A round-trip sanity check that the wrapper actually
    /// deserializes the shape an LLM tool-use response would send.
    #[test]
    fn feedback_pick_deserializes_from_tool_use_shape() {
        let value = serde_json::json!({
            "action": { "kind": "calmer" },
            "rationale": "the brief said \"make it chill\""
        });
        let pick: FeedbackPick = serde_json::from_value(value).unwrap();
        assert!(matches!(pick.action, FeedbackAction::Calmer));
        assert!(pick.rationale.contains("chill"));
    }

    fn base_config() -> Config {
        // A known-good config exercising the same shape as the kid_reel
        // template (multi-layer intro/outro, speed-ramp transition,
        // ducked soundtrack) — inlined rather than loaded from an
        // examples/ file, since this crate ships with no example
        // projects.
        let toml_str = r##"
schema_version = 1

[project]
name            = "Test Project"
target_duration = "5m"
fps             = 30
seed            = 42
fade_in         = "100ms"
fade_out        = "2000ms"

[[outputs]]
path          = "out/test_project.mp4"
resolution    = "1920x1080"
encoder       = "auto"
crf           = 20
audio_bitrate = "192k"

[input]
folder  = "footage/"
include = ["**/*.MP4"]

[soundtrack]
file              = "music/track.mp3"
beat_align        = true
duck_count        = 3
duck_min_spacing  = "20s"
duck_min_dur      = "4s"
duck_max_dur      = "6s"
ducking_db        = -18
fade_out          = "3000ms"
beat_burst        = "auto"

[selection]
min_clip       = "2s"
max_clip       = "5s"
max_per_source = 4

[transition]
default                = "fade"
duration               = "400ms"
speed_ramp_probability = 0.5
speed_ramp_duration    = "2500ms"
speed_ramp_factor      = 3.0
speed_ramp_flash       = true

[intro]
duration = "5.0s"
mode     = "card"
bg_color = "#1c1c2e"
fade_in  = "500ms"
fade_out = "500ms"

[[intro.layers]]
kind         = "text"
content      = "Test Title"
font         = "arialbd.ttf"
size         = 100
color        = "#FFE0A0"
position     = "center"
appear_at    = "0s"
disappear_at = "2.3s"

[[intro.layers]]
kind         = "text"
content      = "subtitle"
font         = "arial.ttf"
size         = 60
color        = "#FFFFFF"
position     = "center"
appear_at    = "2.6s"
disappear_at = "5.0s"

[outro]
duration = "5.0s"
mode     = "overlay"
fade_in  = "600ms"
fade_out = "1000ms"

[[outro.layers]]
kind       = "text"
content    = "credit"
font       = "arialbd.ttf"
size       = 64
color      = "#FFFFFF"
position   = "bottom-center"
background = { color = "#000000AA", padding = "20px" }

[grading]
stabilize = false
"##;
        toml::from_str(toml_str).expect("fixture must parse")
    }

    #[test]
    fn scale_duration_shorter() {
        let mut cfg = base_config();
        let before = cfg.project.target_duration;
        let notes = apply(
            &mut cfg,
            &FeedbackAction::ScaleDuration { factor: 0.75 },
        )
        .unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].field, "project.target_duration");
        // "5m" * 0.75 = 225s = 3m45s
        assert!(
            cfg.project.target_duration.as_secs_f32() < before.as_secs_f32(),
            "expected shorter duration"
        );
    }

    #[test]
    fn scale_duration_longer() {
        let mut cfg = base_config();
        let before = cfg.project.target_duration;
        let notes = apply(
            &mut cfg,
            &FeedbackAction::ScaleDuration { factor: 1.5 },
        )
        .unwrap();
        assert!(notes.len() == 1);
        assert!(cfg.project.target_duration > before);
    }

    #[test]
    fn scale_duration_clamps_extreme_factor() {
        let mut cfg = base_config();
        // factor 100 should clamp to 4.0; result still finite.
        apply(&mut cfg, &FeedbackAction::ScaleDuration { factor: 100.0 }).unwrap();
        assert!(cfg.project.target_duration.as_secs_f32() < 100_000.0);
    }

    #[test]
    fn scale_duration_floors_at_5_seconds() {
        let mut cfg = base_config();
        // factor 0.0001 would otherwise produce ~30 ms.
        apply(&mut cfg, &FeedbackAction::ScaleDuration { factor: 0.001 }).unwrap();
        assert!(cfg.project.target_duration.as_secs_f32() >= 5.0);
    }

    #[test]
    fn scale_duration_rejects_zero_factor() {
        let mut cfg = base_config();
        let r = apply(&mut cfg, &FeedbackAction::ScaleDuration { factor: 0.0 });
        assert!(r.is_err());
    }

    #[test]
    fn calmer_drops_ramp_probability() {
        let mut cfg = base_config();
        let before_prob = cfg.transition.speed_ramp_probability;
        // Set up a known starting point.
        cfg.transition.speed_ramp_probability = 0.6;
        let _ = apply(&mut cfg, &FeedbackAction::Calmer).unwrap();
        assert!((cfg.transition.speed_ramp_probability - 0.3).abs() < 1e-4);
        let _ = before_prob;
    }

    #[test]
    fn calmer_lengthens_xfade() {
        let mut cfg = base_config();
        let before = cfg.transition.duration;
        apply(&mut cfg, &FeedbackAction::Calmer).unwrap();
        assert_eq!(cfg.transition.duration, before + Duration::from_millis(200));
    }

    #[test]
    fn calmer_floors_at_zero() {
        let mut cfg = base_config();
        cfg.transition.speed_ramp_probability = 0.1;
        apply(&mut cfg, &FeedbackAction::Calmer).unwrap();
        assert_eq!(cfg.transition.speed_ramp_probability, 0.0);
    }

    #[test]
    fn punchier_kicks_zero_to_default() {
        let mut cfg = base_config();
        cfg.transition.speed_ramp_probability = 0.0;
        cfg.transition.speed_ramp_factor = 1.0;
        apply(&mut cfg, &FeedbackAction::Punchier).unwrap();
        assert!((cfg.transition.speed_ramp_probability - 0.5).abs() < 1e-4);
        assert!((cfg.transition.speed_ramp_factor - 3.0).abs() < 1e-4);
    }

    #[test]
    fn punchier_caps_at_one() {
        let mut cfg = base_config();
        cfg.transition.speed_ramp_probability = 0.95;
        apply(&mut cfg, &FeedbackAction::Punchier).unwrap();
        assert!((cfg.transition.speed_ramp_probability - 1.0).abs() < 1e-4);
    }

    #[test]
    fn different_music_swaps_existing_soundtrack() {
        let mut cfg = base_config();
        // base_config()'s fixture has a [soundtrack] section.
        assert!(cfg.soundtrack.is_some());
        let new_path = "/projects/music/new_track.mp3";
        let notes = apply(
            &mut cfg,
            &FeedbackAction::DifferentMusic {
                path: new_path.into(),
            },
        )
        .unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(cfg.soundtrack.as_ref().unwrap().files.len(), 1);
        assert_eq!(
            cfg.soundtrack.as_ref().unwrap().files[0].as_str(),
            new_path
        );
    }

    #[test]
    fn different_music_synthesises_when_no_soundtrack() {
        let mut cfg = base_config();
        cfg.soundtrack = None;
        let new_path = "/projects/music/intro.wav";
        apply(
            &mut cfg,
            &FeedbackAction::DifferentMusic {
                path: new_path.into(),
            },
        )
        .unwrap();
        let snd = cfg.soundtrack.expect("soundtrack should be created");
        assert_eq!(snd.files[0].as_str(), new_path);
        // Default-driven fields should be sensible.
        assert!(snd.beat_align);
    }

    #[test]
    fn reroll_changes_seed() {
        let mut cfg = base_config();
        cfg.project.seed = Some(42);
        apply(&mut cfg, &FeedbackAction::Reroll).unwrap();
        assert!(cfg.project.seed.is_some());
        assert_ne!(cfg.project.seed, Some(42));
    }

    #[test]
    fn reroll_produces_different_seeds_across_calls() {
        let mut cfg1 = base_config();
        let mut cfg2 = base_config();
        apply(&mut cfg1, &FeedbackAction::Reroll).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        apply(&mut cfg2, &FeedbackAction::Reroll).unwrap();
        assert_ne!(cfg1.project.seed, cfg2.project.seed);
    }

    /// After any mutation, the resulting Config should still pass
    /// `validate()` — i.e. we never produce a Config that vibecut
    /// would reject. (DropSegment is excluded here because it requires
    /// a manifest on disk; tested separately below.)
    #[test]
    fn every_action_leaves_valid_config() {
        let actions = vec![
            FeedbackAction::ScaleDuration { factor: 0.75 },
            FeedbackAction::ScaleDuration { factor: 1.5 },
            FeedbackAction::Calmer,
            FeedbackAction::Punchier,
            FeedbackAction::DifferentMusic {
                path: "/projects/music/x.mp3".into(),
            },
            FeedbackAction::Reroll,
            FeedbackAction::MoreOf {
                source_ids: vec!["0318".into()],
            },
            FeedbackAction::LessOf {
                source_ids: vec!["0319".into()],
            },
        ];
        for a in actions {
            let mut cfg = base_config();
            apply(&mut cfg, &a).unwrap_or_else(|e| panic!("{a:?} failed: {e}"));
            cfg.validate()
                .unwrap_or_else(|e| panic!("{a:?} produced invalid config: {e}"));
        }
    }

    #[test]
    fn more_of_appends_source_weight() {
        let mut cfg = base_config();
        let n_before = cfg.selection.source_weights.len();
        let notes = apply(
            &mut cfg,
            &FeedbackAction::MoreOf {
                source_ids: vec!["0318".into(), "0319".into()],
            },
        )
        .unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(cfg.selection.source_weights.len(), n_before + 2);
        let w0318 = cfg.selection.source_weights.iter()
            .find(|w| w.source_id == "0318").unwrap();
        assert!((w0318.weight - 1.5).abs() < 1e-4);
        cfg.validate().unwrap();
    }

    #[test]
    fn less_of_appends_zero_weight() {
        let mut cfg = base_config();
        let notes = apply(
            &mut cfg,
            &FeedbackAction::LessOf {
                source_ids: vec!["0320".into()],
            },
        )
        .unwrap();
        assert_eq!(notes.len(), 1);
        let w = cfg.selection.source_weights.iter()
            .find(|w| w.source_id == "0320").unwrap();
        assert_eq!(w.weight, 0.0);
        cfg.validate().unwrap();
    }

    #[test]
    fn more_of_then_less_of_overrides_not_duplicates() {
        let mut cfg = base_config();
        // First MoreOf.
        apply(
            &mut cfg,
            &FeedbackAction::MoreOf { source_ids: vec!["0318".into()] },
        )
        .unwrap();
        // Then LessOf on the same source.
        apply(
            &mut cfg,
            &FeedbackAction::LessOf { source_ids: vec!["0318".into()] },
        )
        .unwrap();
        // Should be ONE entry, with weight=0.0.
        let entries: Vec<_> = cfg
            .selection
            .source_weights
            .iter()
            .filter(|w| w.source_id == "0318")
            .collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].weight, 0.0);
    }

    #[test]
    fn drop_segment_appends_exclude_entry() {
        let mut cfg = base_config();
        // Synthesise a minimal manifest on a temp path.
        let dir = std::env::temp_dir().join("sv_drop_segment_test");
        std::fs::create_dir_all(&dir).unwrap();
        let manifest_path = dir.join("manifest.json");
        let manifest = serde_json::json!({
            "intro_duration_secs": 0.0,
            "segments": [
                { "chapter_path": "E:/footage/GH010320.MP4",
                  "in_chapter_start_secs": 4.0,
                  "duration_secs": 3.0 },
                { "chapter_path": "E:/footage/GH010321.MP4",
                  "in_chapter_start_secs": 12.5,
                  "duration_secs": 4.0 },
                { "chapter_path": "E:/footage/GH010322.MP4",
                  "in_chapter_start_secs": 20.0,
                  "duration_secs": 3.0 },
            ],
        });
        std::fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap())
            .unwrap();

        // Output time 5.0s lands inside segment[1] (3.0 < 5.0 < 7.0).
        // Segment[1] is in GH010321.MP4 at chapter time 12.5..16.5.
        let n_excludes_before = cfg.selection.exclude.len();
        let notes = apply(
            &mut cfg,
            &FeedbackAction::DropSegment {
                at_seconds: 5.0,
                manifest_path: Some(manifest_path.to_string_lossy().into()),
            },
        )
        .unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(cfg.selection.exclude.len(), n_excludes_before + 1);
        let added = cfg.selection.exclude.last().unwrap();
        assert_eq!(added.file, "GH010321.MP4");
        // Padded by 0.5 s on each side.
        assert!((added.start.as_secs_f32() - 12.0).abs() < 1e-3);
        assert!((added.end.as_secs_f32() - 17.0).abs() < 1e-3);
        cfg.validate()
            .expect("DropSegment must leave a valid config");

        let _ = std::fs::remove_file(&manifest_path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn drop_segment_rejects_out_of_range_time() {
        let mut cfg = base_config();
        let dir = std::env::temp_dir().join("sv_drop_segment_test_oor");
        std::fs::create_dir_all(&dir).unwrap();
        let manifest_path = dir.join("manifest.json");
        let manifest = serde_json::json!({
            "segments": [{
                "chapter_path": "E:/footage/X.MP4",
                "in_chapter_start_secs": 0.0,
                "duration_secs": 2.0,
            }],
        });
        std::fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap())
            .unwrap();
        // 100s is way past the manifest's coverage.
        let r = apply(
            &mut cfg,
            &FeedbackAction::DropSegment {
                at_seconds: 100.0,
                manifest_path: Some(manifest_path.to_string_lossy().into()),
            },
        );
        assert!(r.is_err());
        let _ = std::fs::remove_file(&manifest_path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn calmer_crossfade_stops_growing_at_the_cap() {
        let mut cfg = base_config();
        // min_clip = 2s → cap is min(1.2s, 0.7s) = 700ms.
        for _ in 0..20 {
            apply(&mut cfg, &FeedbackAction::Calmer).unwrap();
        }
        assert_eq!(cfg.transition.duration, Duration::from_millis(700));
        // Once capped, another tap reports no crossfade change.
        let notes = apply(&mut cfg, &FeedbackAction::Calmer).unwrap_or_default();
        assert!(notes.iter().all(|n| n.field != "transition.duration"));
    }

    #[test]
    fn fmt_duration_round_pretty() {
        assert_eq!(fmt_duration(Duration::from_secs(180)), "3m");
        assert_eq!(fmt_duration(Duration::from_secs(45)), "45s");
        assert_eq!(fmt_duration(Duration::from_millis(750)), "750ms");
        assert_eq!(fmt_duration(Duration::from_secs(3600)), "1h");
    }
}
