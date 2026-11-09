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
//! Three more steer the picker for the next render:
//!
//! - [`FeedbackAction::MoreOf`] / [`FeedbackAction::LessOf`] — double or
//!   halve a source's `[[selection.source_weights]]` entry
//! - [`FeedbackAction::DropSegment`] — "drop the bit at M:SS", via a
//!   `[[selection.exclude]]` entry for what the manifest's timeline says
//!   played there

use std::time::Duration;

use anyhow::{anyhow, Result};
use camino::Utf8PathBuf;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use vibecut::project_config::{
    BeatBurstMode, Config, SelectionExclude, Soundtrack, SourceWeight,
};
use vibecut::selection_planner::same_source_id;

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

    /// "more of [source X]". Doubles each requested source's
    /// `[[selection.source_weights]]` entry (unset counts as 1.0), up
    /// to 8.0. The picker multiplies the source's score curve by its
    /// weight before composing picks, so a boosted source wins more
    /// candidate slots in the next render. Repeating it keeps boosting;
    /// it exactly undoes one "less of".
    MoreOf {
        /// Source IDs to boost. For GoPro chapters these are the
        /// trailing-4-digit ids (e.g. `["0320", "0321"]`).
        source_ids: Vec<String>,
    },

    /// "less of [source X]". Halves each requested source's
    /// `[[selection.source_weights]]` entry (unset counts as 1.0),
    /// down to 0.125 — never 0, which would ban the source outright.
    /// The source still competes, just at a fraction of its score, so
    /// it shows up less. Repeating it keeps reducing; it exactly undoes
    /// one "more of".
    LessOf {
        /// Source IDs to show less of.
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
        FeedbackAction::MoreOf { source_ids } => {
            Ok(apply_source_weight(cfg, source_ids, WeightStep::More))
        }
        FeedbackAction::LessOf { source_ids } => {
            Ok(apply_source_weight(cfg, source_ids, WeightStep::Less))
        }
    }
}

/// One "more of" multiplies a source's weight by this; one "less of"
/// divides by it, so the two undo each other exactly.
const SOURCE_WEIGHT_STEP: f32 = 2.0;
/// "less of" stops here: the picker multiplies the source's score curve
/// by its weight and treats an all-zero curve as "never pick this
/// source", so the floor stays above 0. At 1/8 the source still fills
/// slots other sources can't, but rarely wins one outright.
const SOURCE_WEIGHT_FLOOR: f32 = 0.125;
/// "more of" stops here, three steps up as the floor is three down (the
/// picker accepts up to 10).
const SOURCE_WEIGHT_CAP: f32 = 8.0;

#[derive(Debug, Clone, Copy)]
enum WeightStep {
    More,
    Less,
}

/// The weight one more/less step moves `current` to. Never crosses
/// its own direction: "less" never raises a weight already below the
/// floor (a hand-set 0 stays 0), "more" never lowers one already above
/// the cap.
fn stepped_weight(current: f32, step: WeightStep) -> f32 {
    let current = if current.is_finite() { current.max(0.0) } else { 1.0 };
    match step {
        WeightStep::Less => current.min((current / SOURCE_WEIGHT_STEP).max(SOURCE_WEIGHT_FLOOR)),
        WeightStep::More => current
            .max((current.max(SOURCE_WEIGHT_FLOOR) * SOURCE_WEIGHT_STEP).min(SOURCE_WEIGHT_CAP)),
    }
}

fn apply_source_weight(
    cfg: &mut Config,
    source_ids: &[String],
    step: WeightStep,
) -> Vec<ChangeNote> {
    let mut notes = Vec::new();
    for sid in source_ids {
        // The picker reads the last entry for a source, so that's the
        // current weight; unset means 1.0. Ids match ignoring letter
        // case, as the picker matches them (`same_source_id`): "less of
        // clip dji_0001" finds an entry written `DJI_0001`.
        let existing = cfg
            .selection
            .source_weights
            .iter()
            .rev()
            .find(|w| same_source_id(&w.source_id, sid));
        let current = existing.map(|w| w.weight);
        // Keep the key as the config already spells it; a new one as asked.
        let key = existing.map_or_else(|| sid.clone(), |w| w.source_id.clone());
        let before = current
            .map(|w| format!("{w:.3}"))
            .unwrap_or_else(|| "(unset; default 1.0)".to_string());
        let weight = stepped_weight(current.unwrap_or(1.0), step);
        // Replace any prior entries for the same source, so repeated
        // MoreOf / LessOf calls leave exactly one entry.
        cfg.selection
            .source_weights
            .retain(|w| !same_source_id(&w.source_id, sid));
        cfg.selection.source_weights.push(SourceWeight {
            source_id: key.clone(),
            weight,
        });
        notes.push(note(
            &format!("selection.source_weights[{key}]"),
            &before,
            &format!("{weight:.3}"),
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
    // clip can afford, or renders start failing. A hard-cut project has no
    // crossfade to lengthen: giving a zero `duration` 200 ms would silently
    // switch it to crossfades.
    let before = cfg.transition.duration;
    let after = (before + Duration::from_millis(200)).min(calmer_crossfade_cap(cfg));
    if !cfg.transition.hard_cut() && after > before {
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

    // Shorter crossfades, down to 200 ms. Hard-cut projects are already as
    // punchy as cuts get: raising a zero `duration` to the floor would turn
    // their straight cuts into crossfades.
    let before = cfg.transition.duration;
    let target = before.saturating_sub(Duration::from_millis(100));
    let after = target.max(Duration::from_millis(200));
    if !cfg.transition.hard_cut() && before != after {
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
    // The same rule as the music routes and propose's `soundtrack`: an
    // audio file in the music library or the projects folder (after
    // resolving `..` and symlinks; a relative path is taken from the
    // projects folder), never an arbitrary file on the server.
    let checked = crate::music::check_soundtrack_file(new_path)?;
    let new_path = checked.to_str().ok_or_else(|| anyhow!("non-UTF-8 soundtrack path"))?;
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

    // What played at `at_seconds`: from the manifest's exact output
    // timeline when it has one (one entry per clip and per beat-burst cut,
    // with the real source window), else the old estimate for manifests
    // written before the timeline existed.
    let DropTarget { chapter_path, in_chapter_start, source_secs, seg_index } =
        match drop_target_from_timeline(&manifest, at_seconds) {
            Some(hit) => hit?,
            None => drop_target_by_summed_durations(&manifest, at_seconds)?,
        };
    let chapter_path = chapter_path.as_str();
    let segment_dur = source_secs;
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

    let mut notes = vec![
        ChangeNote {
            field: "selection.exclude".into(),
            before: format!("(no exclude entries for {basename})"),
            after: format!(
                "added: file={:?} start={:.2}s end={:.2}s (segment #{} at output_t={:.2}s)",
                basename, lo, hi, seg_index, at_seconds
            ),
        },
    ];
    // An `[[input.ranges]]` moment plays whatever the picker excludes:
    // dropping one removes its range. (Not a pick that merely overlaps one.)
    let forced = cfg.input.ranges_only
        || manifest
            .pointer(&format!("/segments/{seg_index}/reason"))
            .and_then(|r| r.as_str())
            .is_some_and(|r| r.starts_with(FORCED_RANGE_REASON));
    if forced {
        let window = (in_chapter_start as f64, (in_chapter_start + segment_dur) as f64);
        notes.extend(remove_dropped_range(cfg, chapter_path, window));
    }
    Ok(notes)
}

/// How the pipeline's manifest marks a segment injected from
/// `[[input.ranges]]` (`run_pipeline`: "forced via [[input.ranges]] 2.00s..6.00s").
const FORCED_RANGE_REASON: &str = "forced via [[input.ranges]]";

/// Remove the `[[input.ranges]]` entry a dropped clip played — on its
/// file, the one its source window overlaps most — and say so. In an
/// `input.ranges_only` config, whose list is the playing order, a caption
/// the range opened (a recap's month) passes to the next range, unless
/// that one opens its own.
fn remove_dropped_range(cfg: &mut Config, chapter_path: &str, (lo, hi): (f64, f64)) -> Option<ChangeNote> {
    let path = camino::Utf8Path::new(chapter_path);
    let (i, _) = cfg
        .input
        .ranges
        .iter()
        .enumerate()
        .filter(|(_, r)| r.matches_file(path))
        .map(|(i, r)| (i, hi.min(r.end.as_secs_f64()) - lo.max(r.start.as_secs_f64())))
        .filter(|(_, overlap)| *overlap > 0.0)
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    let before = cfg.input.ranges.len();
    let r = cfg.input.ranges.remove(i);
    if cfg.input.ranges_only {
        if let (Some(caption), Some(next)) = (&r.caption, cfg.input.ranges.get_mut(i)) {
            next.caption.get_or_insert_with(|| caption.clone());
        }
    }
    Some(ChangeNote {
        field: "input.ranges".into(),
        before: format!("{before} range(s)"),
        after: format!(
            "removed: file={:?} start={} end={}",
            r.file,
            fmt_duration(r.start),
            fmt_duration(r.end)
        ),
    })
}

/// The source window that played at an output time.
struct DropTarget {
    chapter_path: String,
    in_chapter_start: f32,
    /// Seconds of source the clip read (its length plus any speed-ramp
    /// tail).
    source_secs: f32,
    seg_index: usize,
}

/// Look `t` up in the manifest's exact `timeline` (written by
/// `vibecut::output_timeline`): the clip whose visible span — cut in to
/// cut out — covers it. A folded beat-burst has one entry per burst cut,
/// so a drop inside the burst excludes that cut's own footage. `None`
/// when the manifest has no timeline.
fn drop_target_from_timeline(
    manifest: &serde_json::Value,
    t: f32,
) -> Option<Result<DropTarget>> {
    let timeline = manifest.get("timeline").filter(|v| !v.is_null())?;
    let clips = timeline.get("clips")?.as_array()?;
    let t = t as f64;
    let num = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_f64());
    let hit = clips.iter().find(|c| {
        matches!(
            (num(c, "cut_in_secs"), num(c, "cut_out_secs")),
            (Some(a), Some(b)) if t >= a && t < b
        )
    });
    let Some(c) = hit else {
        let cut = |k: &str| timeline.get(k).and_then(|card| num(card, "cut_secs"));
        let what = if cut("intro").is_some_and(|c| t < c) {
            "the intro card".to_string()
        } else if cut("outro").is_some_and(|c| t >= c) {
            "the outro card".to_string()
        } else {
            format!(
                "past the end of the video ({:.2}s)",
                num(timeline, "duration_secs").unwrap_or(0.0)
            )
        };
        return Some(Err(anyhow!(
            "output time {t:.2}s is {what}, not a clip — nothing to drop"
        )));
    };
    let field = |k: &str| num(c, k).ok_or_else(|| anyhow!("timeline clip has no {k}"));
    Some((|| {
        let chapter_path = c
            .get("source_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("timeline clip has no source_path"))?
            .to_string();
        let a = field("source_in_secs")?;
        let b = field("source_out_secs")?;
        Ok(DropTarget {
            chapter_path,
            in_chapter_start: a as f32,
            source_secs: (b - a).max(0.0) as f32,
            seg_index: c.get("segment_index").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        })
    })())
}

/// The estimate for manifests without a `timeline`: segments laid end to
/// end by `duration_secs` from 0, ignoring crossfades, speed ramps and the
/// intro card. Kept so a drop on an old render still resolves the segment
/// the UI shows for it (the UI makes the same estimate for such manifests).
fn drop_target_by_summed_durations(
    manifest: &serde_json::Value,
    at_seconds: f32,
) -> Result<DropTarget> {
    let segments = manifest
        .get("segments")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("manifest has no `segments` array"))?;
    let mut output_offset = 0.0_f32;
    let mut found: Option<(usize, &serde_json::Value)> = None;
    for (i, s) in segments.iter().enumerate() {
        let dur = s.get("duration_secs").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let end = output_offset + dur;
        if at_seconds >= output_offset && at_seconds < end {
            found = Some((i, s));
            break;
        }
        output_offset = end;
    }
    let (seg_index, seg) = found.ok_or_else(|| {
        anyhow!(
            "no segment covers output time {at_seconds:.2}s (manifest has {} segments)",
            segments.len()
        )
    })?;
    let chapter_path = seg
        .get("chapter_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("segment has no chapter_path"))?
        .to_string();
    let in_chapter_start = seg
        .get("in_chapter_start_secs")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| anyhow!("segment has no in_chapter_start_secs"))?
        as f32;
    let source_secs = seg
        .get("duration_secs")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| anyhow!("segment has no duration_secs"))?
        as f32;
    Ok(DropTarget { chapter_path, in_chapter_start, source_secs, seg_index })
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

    // An action that refused its input with an `OpsError` (a soundtrack
    // outside the library is invalid, a missing one not found) keeps that
    // kind; anything else is an invalid action.
    let notes = apply(&mut cfg, action).map_err(|e| match e.downcast::<OpsError>() {
        Ok(ops) => ops,
        Err(e) => OpsError::invalid(format!("feedback action failed: {e:#}")),
    })?;

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
    fn punchier_and_calmer_keep_a_hard_cut_project_on_cuts() {
        // `duration = "0s"` with a crossfade name is hard-cut mode. Punchier
        // raised it to its 200 ms floor and calmer added 200 ms, so either
        // tap silently switched the next render to crossfades.
        for action in [FeedbackAction::Punchier, FeedbackAction::Calmer] {
            let mut cfg = base_config();
            cfg.transition.default = "fade".into();
            cfg.transition.duration = Duration::ZERO;
            assert!(cfg.transition.hard_cut());
            let notes = apply(&mut cfg, &action).unwrap();
            assert_eq!(cfg.transition.duration, Duration::ZERO, "{action:?}");
            assert!(cfg.transition.hard_cut(), "{action:?}");
            assert!(notes.iter().all(|n| n.field != "transition.duration"), "{action:?}");
            // The rest of the action still applies.
            assert!(notes.iter().any(|n| n.field == "transition.speed_ramp_probability"), "{action:?}");
        }
        // `default = "cut"`: the duration has no effect, so it isn't touched
        // (and no change is reported) either.
        for action in [FeedbackAction::Punchier, FeedbackAction::Calmer] {
            let mut cfg = base_config();
            cfg.transition.default = "cut".into();
            let before = cfg.transition.duration;
            let notes = apply(&mut cfg, &action).unwrap();
            assert_eq!(cfg.transition.duration, before, "{action:?}");
            assert!(notes.iter().all(|n| n.field != "transition.duration"), "{action:?}");
        }
    }

    #[test]
    fn punchier_shortens_the_crossfade_to_its_floor() {
        let mut cfg = base_config();
        cfg.transition.duration = Duration::from_millis(400);
        apply(&mut cfg, &FeedbackAction::Punchier).unwrap();
        assert_eq!(cfg.transition.duration, Duration::from_millis(300));
        apply(&mut cfg, &FeedbackAction::Punchier).unwrap();
        apply(&mut cfg, &FeedbackAction::Punchier).unwrap();
        assert_eq!(cfg.transition.duration, Duration::from_millis(200));
    }

    #[test]
    fn punchier_caps_at_one() {
        let mut cfg = base_config();
        cfg.transition.speed_ramp_probability = 0.95;
        apply(&mut cfg, &FeedbackAction::Punchier).unwrap();
        assert!((cfg.transition.speed_ramp_probability - 1.0).abs() < 1e-4);
    }

    /// A music library for these tests (`music::use_roots`, as the web
    /// crate's tests do), holding the songs the music tests pick.
    fn test_library() -> &'static std::path::Path {
        static LIB: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        LIB.get_or_init(|| {
            let root = std::env::temp_dir().join(format!("vibecut_feedback_music_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let music = root.join("music");
            std::fs::create_dir_all(&music).unwrap();
            for song in ["new_track.mp3", "intro.wav", "x.mp3"] {
                std::fs::write(music.join(song), b"not really audio").unwrap();
            }
            std::fs::write(root.join("secret.mp3"), b"outside the library").unwrap();
            crate::music::use_roots(crate::music::LibraryRoots { own: music, extra: vec![], cache_dir: None });
            root
        })
    }

    fn library_song(name: &str) -> String {
        test_library().join("music").join(name).to_string_lossy().into_owned()
    }

    #[test]
    fn different_music_swaps_existing_soundtrack() {
        let mut cfg = base_config();
        // base_config()'s fixture has a [soundtrack] section.
        assert!(cfg.soundtrack.is_some());
        let new_path = &library_song("new_track.mp3");
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
        let new_path = &library_song("intro.wav");
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

    /// "different music" follows the music routes' rule: only an audio
    /// file in the library or the projects folder goes into the config.
    #[test]
    fn different_music_refuses_files_outside_the_library() {
        let lib = test_library();
        let outside = lib.join("secret.mp3").to_string_lossy().into_owned();
        let climb = lib.join("music/../secret.mp3").to_string_lossy().into_owned();
        let not_audio = lib.join("music/new_track.txt").to_string_lossy().into_owned();
        for (path, invalid) in [
            ("/etc/passwd", true),
            ("/etc/nonexistent.mp3", true),
            (outside.as_str(), true),
            (climb.as_str(), true),
            (not_audio.as_str(), true),
            // Inside a root, but not there.
            ("/projects/music/missing.mp3", false),
        ] {
            let mut cfg = base_config();
            let before = cfg.soundtrack.as_ref().unwrap().files.clone();
            let err = apply(&mut cfg, &FeedbackAction::DifferentMusic { path: path.into() })
                .expect_err(path)
                .downcast::<OpsError>()
                .unwrap_or_else(|e| panic!("{path}: not an OpsError: {e:#}"));
            if invalid {
                assert!(matches!(err, OpsError::InvalidInput { .. }), "{path}: {err}");
            } else {
                assert!(matches!(err, OpsError::NotFound { .. }), "{path}: {err}");
            }
            assert_eq!(cfg.soundtrack.unwrap().files, before, "{path} changed the config");
        }
    }

    /// Through the file op too: the refusal keeps its kind and the config
    /// on disk is left alone; a library song is written.
    #[tokio::test]
    async fn apply_feedback_to_file_checks_the_song() {
        let dir = test_library().join("cfg");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.toml");
        let original = toml::to_string_pretty(&base_config()).unwrap();
        std::fs::write(&path, &original).unwrap();
        let p = path.to_string_lossy().into_owned();
        let bad = FeedbackAction::DifferentMusic { path: "/etc/passwd".into() };
        let err = apply_feedback_to_file(&p, &bad).await.expect_err("refused");
        assert!(matches!(err, OpsError::InvalidInput { .. }), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let song = library_song("x.mp3");
        let ok = apply_feedback_to_file(&p, &FeedbackAction::DifferentMusic { path: song.clone() }).await.unwrap();
        assert_eq!(ok.changes[0].after, song);
        assert!(std::fs::read_to_string(&path).unwrap().contains(&song));
    }

    /// "less of clip dji_0001" (the tell-it grammar lowercases what the
    /// user types) steps the entry the config already has for `DJI_0001`,
    /// keeping its key as written, and leaves one entry.
    #[test]
    fn source_weights_match_ids_ignoring_case_and_keep_the_written_key() {
        let mut cfg = base_config();
        cfg.selection.source_weights = vec![SourceWeight { source_id: "DJI_0001".into(), weight: 2.0 }];
        let notes = apply(&mut cfg, &FeedbackAction::LessOf { source_ids: vec!["dji_0001".into()] }).unwrap();
        assert_eq!(cfg.selection.source_weights.len(), 1, "{:?}", cfg.selection.source_weights);
        assert_eq!(cfg.selection.source_weights[0].source_id, "DJI_0001");
        assert!((cfg.selection.source_weights[0].weight - 1.0).abs() < 1e-6);
        assert_eq!(notes[0].field, "selection.source_weights[DJI_0001]");
        assert_eq!(notes[0].before, "2.000");
        // And back up, typed another way.
        apply(&mut cfg, &FeedbackAction::MoreOf { source_ids: vec!["Dji_0001".into()] }).unwrap();
        assert_eq!(cfg.selection.source_weights.len(), 1);
        assert_eq!(cfg.selection.source_weights[0].source_id, "DJI_0001");
        assert!((cfg.selection.source_weights[0].weight - 2.0).abs() < 1e-6);
        // A source with no entry yet gets one as asked; the pipeline
        // matches it to the source ignoring case too.
        apply(&mut cfg, &FeedbackAction::LessOf { source_ids: vec!["img_1234".into()] }).unwrap();
        assert_eq!(cfg.selection.source_weights.len(), 2);
        assert_eq!(cfg.selection.source_weights[1].source_id, "img_1234");
        assert!((cfg.selection.source_weights[1].weight - 0.5).abs() < 1e-6);
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
                path: library_song("x.mp3"),
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
        assert!((w0318.weight - 2.0).abs() < 1e-4);
        cfg.validate().unwrap();
    }

    fn weight_of(cfg: &Config, sid: &str) -> f32 {
        let entries: Vec<_> =
            cfg.selection.source_weights.iter().filter(|w| w.source_id == sid).collect();
        assert_eq!(entries.len(), 1, "exactly one entry for {sid}");
        entries[0].weight
    }

    #[test]
    fn less_of_reduces_the_weight_but_never_bans_the_source() {
        // Weight 0 zeroes the source's score curve and the planner never
        // picks it again; "less of" must leave it in the running.
        let mut cfg = base_config();
        let less = FeedbackAction::LessOf { source_ids: vec!["0320".into()] };
        let mut seen = Vec::new();
        for _ in 0..5 {
            apply(&mut cfg, &less).unwrap();
            seen.push(weight_of(&cfg, "0320"));
        }
        assert_eq!(seen, vec![0.5, 0.25, 0.125, 0.125, 0.125]);
        assert!(seen.iter().all(|&w| w > 0.0));
        cfg.validate().unwrap();
    }

    #[test]
    fn more_of_keeps_boosting_up_to_the_cap() {
        let mut cfg = base_config();
        let more = FeedbackAction::MoreOf { source_ids: vec!["0318".into()] };
        let mut seen = Vec::new();
        for _ in 0..5 {
            apply(&mut cfg, &more).unwrap();
            seen.push(weight_of(&cfg, "0318"));
        }
        assert_eq!(seen, vec![2.0, 4.0, 8.0, 8.0, 8.0]);
    }

    #[test]
    fn more_of_then_less_of_overrides_not_duplicates() {
        let mut cfg = base_config();
        let more = FeedbackAction::MoreOf { source_ids: vec!["0318".into()] };
        let less = FeedbackAction::LessOf { source_ids: vec!["0318".into()] };
        // One step each way cancels out, with ONE entry left behind.
        apply(&mut cfg, &more).unwrap();
        apply(&mut cfg, &less).unwrap();
        assert_eq!(weight_of(&cfg, "0318"), 1.0);
        apply(&mut cfg, &less).unwrap();
        apply(&mut cfg, &more).unwrap();
        assert_eq!(weight_of(&cfg, "0318"), 1.0);
    }

    #[test]
    fn weight_steps_never_move_the_wrong_way() {
        // A hand-set 0 ("never use this source") stays 0 under "less of";
        // "more of" brings it back into play.
        assert_eq!(stepped_weight(0.0, WeightStep::Less), 0.0);
        assert_eq!(stepped_weight(0.0, WeightStep::More), 0.25);
        // Hand-set weights past the bounds aren't pulled back.
        assert_eq!(stepped_weight(0.05, WeightStep::Less), 0.05);
        assert_eq!(stepped_weight(10.0, WeightStep::More), 10.0);
        assert_eq!(stepped_weight(10.0, WeightStep::Less), 5.0);
    }

    /// A manifest whose timeline disagrees with the summed-durations
    /// estimate: a 3 s intro card, crossfades, and a folded beat-burst.
    fn timeline_manifest() -> serde_json::Value {
        serde_json::json!({
            "segments": [
                { "chapter_path": "E:/footage/GH010320.MP4",
                  "in_chapter_start_secs": 4.0, "duration_secs": 3.0 },
                { "chapter_path": "/work/.vibecut_x/beat_burst.mp4",
                  "in_chapter_start_secs": 0.0, "duration_secs": 2.0 },
                { "chapter_path": "E:/footage/GH010322.MP4",
                  "in_chapter_start_secs": 20.0, "duration_secs": 3.0 },
            ],
            "timeline": {
                "fps": 30, "frames": 300, "duration_secs": 10.0,
                "intro": { "start_frame": 0, "end_frame": 90,
                           "output_start_secs": 0.0, "output_end_secs": 3.0,
                           "cut_secs": 2.8, "transition_secs": 0.4 },
                "clips": [
                    { "segment_index": 0, "source_id": "0320",
                      "source_path": "E:/footage/GH010320.MP4",
                      "source_in_secs": 4.0, "source_out_secs": 7.4, "speed": 1.0,
                      "ramp": { "output_start_secs": 5.4, "factor": 3.0 },
                      "start_frame": 78, "end_frame": 168,
                      "output_start_secs": 2.6, "output_end_secs": 5.6,
                      "cut_in_secs": 2.8, "cut_out_secs": 5.55,
                      "transition_in_secs": 0.4, "transition_out_secs": 0.1 },
                    { "segment_index": 1, "burst_part": 0, "source_id": "0304",
                      "source_path": "E:/footage/GX010304.MP4",
                      "source_in_secs": 8.0, "source_out_secs": 9.0, "speed": 1.0,
                      "start_frame": 165, "end_frame": 195,
                      "output_start_secs": 5.5, "output_end_secs": 6.5,
                      "cut_in_secs": 5.55, "cut_out_secs": 6.5,
                      "transition_in_secs": 0.1, "transition_out_secs": 0.0 },
                    { "segment_index": 1, "burst_part": 1, "source_id": "0301",
                      "source_path": "E:/footage/GX010301.MP4",
                      "source_in_secs": 3.0, "source_out_secs": 4.0, "speed": 1.0,
                      "start_frame": 195, "end_frame": 225,
                      "output_start_secs": 6.5, "output_end_secs": 7.5,
                      "cut_in_secs": 6.5, "cut_out_secs": 7.3,
                      "transition_in_secs": 0.0, "transition_out_secs": 0.4 },
                    { "segment_index": 2, "source_id": "0322",
                      "source_path": "E:/footage/GH010322.MP4",
                      "source_in_secs": 20.0, "source_out_secs": 23.0, "speed": 1.0,
                      "start_frame": 213, "end_frame": 300,
                      "output_start_secs": 7.1, "output_end_secs": 10.0,
                      "cut_in_secs": 7.3, "cut_out_secs": 10.0,
                      "transition_in_secs": 0.4, "transition_out_secs": 0.0 },
                ],
            },
        })
    }

    fn drop_at(manifest: &serde_json::Value, name: &str, t: f32) -> Result<Config> {
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest_path = dir.join("manifest.json");
        std::fs::write(&manifest_path, serde_json::to_string(manifest).unwrap()).unwrap();
        let mut cfg = base_config();
        let r = apply(
            &mut cfg,
            &FeedbackAction::DropSegment {
                at_seconds: t,
                manifest_path: Some(manifest_path.to_string_lossy().into()),
            },
        );
        let _ = std::fs::remove_file(&manifest_path);
        let _ = std::fs::remove_dir(&dir);
        r.map(|_| cfg)
    }

    #[test]
    fn drop_segment_reads_the_exact_timeline() {
        // 4.0 s: summing durations from 0 says segment 1 (3–5 s); the clip
        // really on screen is segment 0 (cuts at 2.8 and 5.55 s), whose
        // window includes its speed-ramp tail.
        let cfg = drop_at(&timeline_manifest(), "sv_drop_timeline", 4.0).unwrap();
        let added = cfg.selection.exclude.last().unwrap();
        assert_eq!(added.file, "GH010320.MP4");
        assert!((added.start.as_secs_f32() - 3.5).abs() < 1e-3);
        assert!((added.end.as_secs_f32() - 7.9).abs() < 1e-3);
        cfg.validate().unwrap();
    }

    #[test]
    fn drop_segment_inside_the_burst_drops_that_burst_cut() {
        // Not the workspace's folded beat_burst.mp4 — the cut's own footage.
        let cfg = drop_at(&timeline_manifest(), "sv_drop_burst", 6.8).unwrap();
        let added = cfg.selection.exclude.last().unwrap();
        assert_eq!(added.file, "GX010301.MP4");
        assert!((added.start.as_secs_f32() - 2.5).abs() < 1e-3);
        assert!((added.end.as_secs_f32() - 4.5).abs() < 1e-3);
    }

    #[test]
    fn drop_segment_reads_the_timeline_the_engine_writes() {
        // The reader above walks raw JSON; build the timeline with the
        // engine's own code so a renamed field can't slip past.
        use vibecut::output_timeline::{ChainItem, ChainLayout, ClipSource, OutputTimeline};
        let clip = |i: usize, path: &str, start: f64| ClipSource {
            segment_index: i,
            source_id: format!("s{i}"),
            source_path: path.into(),
            source_in_secs: start,
            duration_secs: 2.0,
            ramp: None,
        };
        let layout = ChainLayout::new(&[3.0, 2.0, 2.0], &[0.4, 0.4], 30);
        let items = vec![
            ChainItem::IntroCard,
            ChainItem::Clip(clip(0, "E:/footage/GX010320.MP4", 4.0)),
            ChainItem::Clip(clip(1, "E:/footage/GX010322.MP4", 20.0)),
        ];
        let timeline = OutputTimeline::build(&layout, &items);
        let manifest = serde_json::json!({
            "segments": [
                { "chapter_path": "E:/footage/GX010320.MP4", "in_chapter_start_secs": 4.0, "duration_secs": 2.0 },
                { "chapter_path": "E:/footage/GX010322.MP4", "in_chapter_start_secs": 20.0, "duration_secs": 2.0 },
            ],
            "timeline": serde_json::to_value(&timeline).unwrap(),
        });
        // 4.6 s: past both segments by summed durations (0–4 s); on screen
        // it's the second clip (cuts at 4.4 and 6.2 s).
        let cfg = drop_at(&manifest, "sv_drop_engine_timeline", 4.6).unwrap();
        let added = cfg.selection.exclude.last().unwrap();
        assert_eq!(added.file, "GX010322.MP4");
        assert!((added.start.as_secs_f32() - 19.5).abs() < 1e-3);
        assert!((added.end.as_secs_f32() - 22.5).abs() < 1e-3);
    }

    /// A recap-shaped manifest: three clips that play `[[input.ranges]]`,
    /// laid out by the engine's own timeline code.
    fn ranges_manifest(reasons: [&str; 3]) -> serde_json::Value {
        use vibecut::output_timeline::{ChainItem, ChainLayout, ClipSource, OutputTimeline};
        let parts = [("/f/alps/GX010201.MP4", 4.0), ("/f/alps/GX010201.MP4", 20.0), ("/f/coast/GX010301.MP4", 8.0)];
        let layout = ChainLayout::new(&[3.0, 4.0, 4.0, 4.0], &[0.3, 0.3, 0.3], 30);
        let mut items = vec![ChainItem::IntroCard];
        items.extend(parts.iter().enumerate().map(|(i, &(path, start))| {
            ChainItem::Clip(ClipSource {
                segment_index: i,
                source_id: format!("s{i}"),
                source_path: path.into(),
                source_in_secs: start,
                duration_secs: 4.0,
                ramp: None,
            })
        }));
        let segments: Vec<serde_json::Value> = parts
            .iter()
            .zip(reasons)
            .map(|(&(path, start), reason)| {
                serde_json::json!({ "chapter_path": path, "in_chapter_start_secs": start, "duration_secs": 4.0, "reason": reason })
            })
            .collect();
        serde_json::json!({
            "segments": segments,
            "timeline": serde_json::to_value(OutputTimeline::build(&layout, &items)).unwrap(),
        })
    }

    fn with_ranges(cfg: &mut Config, ranges_only: bool) {
        let range = |file: &str, start: u64, caption: Option<&str>| vibecut::project_config::InputRange {
            file: file.into(),
            start: Duration::from_secs(start),
            end: Duration::from_secs(start + 4),
            caption: caption.map(str::to_string),
        };
        cfg.input.ranges_only = ranges_only;
        cfg.input.ranges = vec![
            range("/f/alps/GX010201.MP4", 4, Some("March")),
            range("/f/alps/GX010201.MP4", 20, None),
            range("/f/coast/GX010301.MP4", 8, Some("June")),
        ];
    }

    fn drop_in(cfg: &mut Config, manifest: &serde_json::Value, name: &str, t: f32) -> Result<Vec<ChangeNote>> {
        let dir = std::env::temp_dir().join(format!("{name}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("manifest.json");
        std::fs::write(&p, manifest.to_string()).unwrap();
        let r = apply(cfg, &FeedbackAction::DropSegment { at_seconds: t, manifest_path: Some(p.to_string_lossy().into()) });
        let _ = std::fs::remove_dir_all(&dir);
        r
    }

    fn range_summary(cfg: &Config) -> Vec<(u64, Option<&str>)> {
        cfg.input.ranges.iter().map(|r| (r.start.as_secs(), r.caption.as_deref())).collect()
    }

    /// A recap plays exactly its ranges: an exclude alone would leave the
    /// dropped moment in. Dropping removes its range; the month's caption
    /// moves on to the month's next moment.
    #[test]
    fn dropping_a_recap_moment_removes_its_range() {
        let manifest = ranges_manifest(["forced via [[input.ranges]] 4.00s..8.00s"; 3]);
        let mut cfg = base_config();
        with_ranges(&mut cfg, true);
        // Intro 0–3 s, then 4 s clips: the first plays to about 6.85 s.
        let notes = drop_in(&mut cfg, &manifest, "sv_drop_recap", 4.0).unwrap();
        assert_eq!(range_summary(&cfg), vec![(20, Some("March")), (8, Some("June"))]);
        assert!(notes.iter().any(|n| n.field == "input.ranges" && n.after.contains("GX010201")), "{notes:?}");
        assert_eq!(cfg.selection.exclude.last().unwrap().file, "GX010201.MP4");
        cfg.validate().unwrap();

        // The month's only moment: its caption goes with it.
        let mut cfg = base_config();
        with_ranges(&mut cfg, true);
        cfg.input.ranges.remove(1);
        let manifest = {
            let mut m = ranges_manifest(["forced via [[input.ranges]]"; 3]);
            // Same layout; the second clip now plays the coast range.
            m["timeline"]["clips"][1]["source_path"] = "/f/coast/GX010301.MP4".into();
            m["timeline"]["clips"][1]["source_in_secs"] = 8.0.into();
            m["timeline"]["clips"][1]["source_out_secs"] = 12.3.into();
            m
        };
        drop_in(&mut cfg, &manifest, "sv_drop_recap_only", 4.0).unwrap();
        assert_eq!(range_summary(&cfg), vec![(8, Some("June"))]);

        // The last one: a ranges_only config can't be left empty.
        drop_in(&mut cfg, &manifest, "sv_drop_recap_last", 9.0).unwrap();
        assert!(cfg.input.ranges.is_empty());
        assert!(cfg.validate().unwrap_err().to_string().contains("ranges_only"));
    }

    /// In an ordinary project a range is a must-include on top of the
    /// picks: dropping it removes it, dropping a pick that happens to
    /// overlap it does not.
    #[test]
    fn dropping_a_pick_keeps_the_range_it_overlaps() {
        let manifest = ranges_manifest([
            "score=0.91 hilight+accel",
            "forced via [[input.ranges]] 20.00s..24.00s",
            "score=0.40 hilight+accel",
        ]);
        let mut cfg = base_config();
        with_ranges(&mut cfg, false);
        let notes = drop_in(&mut cfg, &manifest, "sv_drop_pick", 4.0).unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(cfg.input.ranges.len(), 3);
        // The forced one (second clip, about 6.85–10.55 s).
        drop_in(&mut cfg, &manifest, "sv_drop_forced", 8.0).unwrap();
        assert_eq!(range_summary(&cfg), vec![(4, Some("March")), (8, Some("June"))]);
        assert_eq!(cfg.selection.exclude.len(), 2);
    }

    #[test]
    fn drop_segment_on_the_intro_card_drops_nothing() {
        let e = drop_at(&timeline_manifest(), "sv_drop_intro", 1.0).unwrap_err();
        assert!(e.to_string().contains("intro card"), "{e}");
        let e = drop_at(&timeline_manifest(), "sv_drop_past", 12.0).unwrap_err();
        assert!(e.to_string().contains("past the end"), "{e}");
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
