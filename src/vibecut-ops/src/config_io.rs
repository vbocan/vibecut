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
    let toml_text = cfg
        .to_toml_pretty()
        .map_err(|e| OpsError::internal(format!("could not serialize config: {e}")))?;

    write_creating_parent(&path, &toml_text)?;

    Ok(cfg.clone())
}

/// One change to a config, by dotted key (`selection.max_clip`,
/// `intro.layers.0.content`; a number is an array index). `value: null`
/// removes the key, so the setting goes back to its built-in default.
#[derive(Debug, Clone, serde::Deserialize, Serialize)]
pub struct ConfigChange {
    pub key: String,
    #[serde(default)]
    pub value: serde_json::Value,
}

/// Apply `changes` to the config file in place, keeping everything else
/// as written: comments, key order, keys this server doesn't know. The
/// result must still load and validate, or nothing is written.
pub async fn patch_config(config_path: &str, changes: &[ConfigChange]) -> Result<ConfigText, OpsError> {
    patch_config_in(&crate::paths::projects_root_for(config_path), config_path, changes).await
}

async fn patch_config_in(root: &Path, config_path: &str, changes: &[ConfigChange]) -> Result<ConfigText, OpsError> {
    let path = resolve_write_target(root, config_path)?;
    let text = read_existing(&path)?;
    let new_text = patch_text(&text, changes)?;
    write_creating_parent(&path, &new_text)?;
    Ok(ConfigText { config_path: path.to_string_lossy().into_owned(), config_toml: new_text })
}

/// [`patch_config`] on the text alone.
pub fn patch_text(text: &str, changes: &[ConfigChange]) -> Result<String, OpsError> {
    use toml_edit::DocumentMut;
    let mut doc: DocumentMut =
        text.parse().map_err(|e| OpsError::invalid(format!("the config isn't valid TOML: {e}")))?;
    for c in changes {
        let segs: Vec<&str> = c.key.split('.').collect();
        if c.key.is_empty() || c.key.len() > 200 || segs.iter().any(|s| s.is_empty()) {
            return Err(OpsError::invalid(format!("not a setting: {:?}", c.key)));
        }
        let value = if c.value.is_null() {
            None
        } else {
            Some(json_to_toml(&c.value).ok_or_else(|| OpsError::invalid(format!("{} can't be set to {}", c.key, c.value)))?)
        };
        apply(doc.as_table_mut(), false, &segs, value, &c.key)?;
    }
    let out = doc.to_string();
    let cfg: vibecut::project_config::Config =
        toml::from_str(&out).map_err(|e| OpsError::invalid(format!("that change makes the config invalid: {e}")))?;
    cfg.validate().map_err(|e| OpsError::invalid(format!("that change makes the config invalid: {e:#}")))?;
    Ok(out)
}

/// Set (or with `None` remove) `segs` under `table`. `inline`: the table is
/// an inline one, so new tables inside it must be inline too.
fn apply(
    table: &mut dyn toml_edit::TableLike,
    inline: bool,
    segs: &[&str],
    value: Option<toml_edit::Value>,
    key: &str,
) -> Result<(), OpsError> {
    use toml_edit::{Item, Value};
    let bad = || OpsError::invalid(format!("not a setting: {key:?}"));
    let (seg, rest) = segs.split_first().ok_or_else(bad)?;
    if rest.is_empty() {
        match value {
            None => {
                table.remove(seg);
            }
            Some(mut v) => {
                // Keep the comment and spacing around a value that is replaced.
                if let Some(old) = table.get(seg).and_then(Item::as_value) {
                    *v.decor_mut() = old.decor().clone();
                }
                // A whole table (an `[intro]` card) or list of tables (its
                // `[[intro.layers]]`) is written as real tables, the way a
                // person writes them, unless it sits inside an inline one.
                table.insert(seg, if inline { Item::Value(v) } else { tableize(v) });
            }
        }
        return Ok(());
    }
    if let Ok(i) = rest[0].parse::<usize>() {
        let item = table.get_mut(seg).ok_or_else(bad)?;
        let tail = &rest[1..];
        return match item {
            Item::ArrayOfTables(a) => {
                if tail.is_empty() {
                    // The whole entry: replace it, add one at the end, or remove it.
                    let table = |v: Value| match tableize(v) {
                        Item::Table(t) => Some(t),
                        _ => None,
                    };
                    match value {
                        Some(v) if i < a.len() => {
                            let t = table(v).ok_or_else(bad)?;
                            *a.get_mut(i).ok_or_else(bad)? = t;
                        }
                        Some(v) if i == a.len() => a.push(table(v).ok_or_else(bad)?),
                        None if i < a.len() => a.remove(i),
                        _ => return Err(bad()),
                    }
                    return Ok(());
                }
                let t = a.get_mut(i).ok_or_else(bad)?;
                apply(t, false, tail, value, key)
            }
            Item::Value(Value::Array(arr)) => {
                if tail.is_empty() {
                    match value {
                        Some(v) if i < arr.len() => {
                            arr.replace(i, v);
                        }
                        Some(v) if i == arr.len() => arr.push(v),
                        None if i < arr.len() => {
                            arr.remove(i);
                        }
                        _ => return Err(bad()),
                    }
                    return Ok(());
                }
                let el = arr.get_mut(i).and_then(Value::as_inline_table_mut).ok_or_else(bad)?;
                apply(el, true, tail, value, key)
            }
            _ => Err(bad()),
        };
    }
    if table.get(seg).is_none() {
        if value.is_none() {
            return Ok(()); // nothing to remove
        }
        let fresh = if inline {
            Item::Value(Value::InlineTable(toml_edit::InlineTable::new()))
        } else {
            Item::Table(toml_edit::Table::new())
        };
        table.insert(seg, fresh);
    }
    let item = table.get_mut(seg).ok_or_else(bad)?;
    match item {
        Item::Table(t) => apply(t, false, rest, value, key),
        Item::Value(Value::InlineTable(t)) => apply(t, true, rest, value, key),
        _ => Err(bad()),
    }
}

/// An inline table as a `[table]`, and an array of inline tables as a
/// `[[table]]` list, all the way down; anything else stays a value.
fn tableize(v: toml_edit::Value) -> toml_edit::Item {
    use toml_edit::{ArrayOfTables, Item, Table, Value};
    fn table_of(t: toml_edit::InlineTable) -> Table {
        let mut out = Table::new();
        for (k, v) in t {
            out.insert(&k, tableize(v));
        }
        out
    }
    match v {
        Value::InlineTable(t) => Item::Table(table_of(t)),
        Value::Array(a) if !a.is_empty() && a.iter().all(Value::is_inline_table) => {
            let mut list = ArrayOfTables::new();
            for el in a {
                if let Value::InlineTable(t) = el {
                    list.push(table_of(t));
                }
            }
            Item::ArrayOfTables(list)
        }
        other => Item::Value(other),
    }
}

/// A JSON value as a TOML value: integers stay integers, floats are
/// written short, objects become inline tables. `null` inside is refused.
fn json_to_toml(v: &serde_json::Value) -> Option<toml_edit::Value> {
    use serde_json::Value as J;
    Some(match v {
        J::Bool(b) => (*b).into(),
        J::Number(n) => match n.as_i64() {
            Some(i) => i.into(),
            None => {
                let f = n.as_f64()?;
                let short = (f as f32).to_string().parse::<f64>().ok().filter(|s| (s - f).abs() < 1e-9).unwrap_or(f);
                short.into()
            }
        },
        J::String(s) => s.as_str().into(),
        J::Array(a) => {
            let mut arr = toml_edit::Array::new();
            for x in a {
                arr.push(json_to_toml(x)?);
            }
            toml_edit::Value::Array(arr)
        }
        J::Object(m) => {
            let mut t = toml_edit::InlineTable::new();
            for (k, x) in m {
                t.insert(k, json_to_toml(x)?);
            }
            toml_edit::Value::InlineTable(t)
        }
        J::Null => return None,
    })
}

#[cfg(test)]
mod patch_tests {
    use super::*;

    const BASE: &str = r#"schema_version = 1
# my note about this trip
[project]
name = "x"
target_duration = "30s"
fps = 30

[input]
folder = "/footage/x"

[selection]
max_clip = "7s"   # keep shots short-ish
variety = 0.05

[soundtrack]
files = ["/projects/music/a.mp3"]
ducking_db = -16.0

[intro]
duration = "3s"

[[intro.layers]]
kind = "text"
content = "Hello"

[[outputs]]
path = "/projects/out/x.mp4"
resolution = "1920x1080"
"#;

    fn ch(key: &str, value: serde_json::Value) -> ConfigChange {
        ConfigChange { key: key.into(), value }
    }

    #[test]
    fn patches_keep_comments_order_and_formatting() {
        let out = patch_text(
            BASE,
            &[
                ch("selection.max_clip", serde_json::json!("5s")),
                ch("soundtrack.ducking_db", serde_json::json!(-10.5)),
                ch("scoring.landings", serde_json::json!(1.0)),
                ch("intro.layers.0.content", serde_json::json!("Coastline")),
                ch("selection.variety", serde_json::Value::Null),
            ],
        )
        .unwrap();
        assert!(out.contains("# my note about this trip"), "{out}");
        assert!(out.contains("max_clip = \"5s\"   # keep shots short-ish"), "{out}");
        assert!(out.contains("ducking_db = -10.5"), "{out}");
        assert!(out.contains("landings = 1"), "{out}");
        assert!(out.contains("content = \"Coastline\""), "{out}");
        assert!(!out.contains("variety"), "{out}");
        assert!(out.find("[selection]").unwrap() < out.find("[soundtrack]").unwrap(), "order kept");
    }

    /// Titles: a whole card is added as an `[outro]` table with
    /// `[[outro.layers]]`, a text line is added, replaced and removed by
    /// index, and the card goes away again.
    #[test]
    fn cards_and_their_lines_are_added_and_removed_as_tables() {
        let card = serde_json::json!({
            "duration": "4s", "fade_in": "500ms", "fade_out": "1s", "mode": "card", "bg_color": "#101820",
            "layers": [{ "kind": "text", "content": "Thanks for watching", "size": 80, "color": "#ffffff",
                         "position": "center", "x_offset": 0, "y_offset": 0 }]
        });
        let out = patch_text(BASE, &[ch("outro", card)]).unwrap();
        assert!(out.contains("[outro]\n"), "{out}");
        assert!(out.contains("[[outro.layers]]\n"), "{out}");
        assert!(out.contains("content = \"Thanks for watching\""), "{out}");
        assert!(!out.contains("outro = {"), "not inline: {out}");

        let line = serde_json::json!({ "kind": "text", "content": "Summer 2026", "size": 48, "color": "#ffcc00",
                                        "position": "bottom-center", "x_offset": 0, "y_offset": -20 });
        let out = patch_text(&out, &[ch("outro.layers.1", line)]).unwrap();
        assert_eq!(out.matches("[[outro.layers]]").count(), 2, "{out}");
        assert!(out.contains("content = \"Summer 2026\""), "{out}");
        let out = patch_text(&out, &[ch("outro.layers.1.position", serde_json::json!("top-right"))]).unwrap();
        assert!(out.contains("position = \"top-right\""), "{out}");
        let out = patch_text(&out, &[ch("outro.layers.0", serde_json::Value::Null)]).unwrap();
        assert_eq!(out.matches("[[outro.layers]]").count(), 1, "{out}");
        assert!(!out.contains("Thanks for watching") && out.contains("Summer 2026"), "{out}");
        let out = patch_text(&out, &[ch("outro", serde_json::Value::Null)]).unwrap();
        assert!(!out.contains("outro"), "{out}");
        // The intro written by hand above is untouched throughout.
        assert!(out.contains("content = \"Hello\""), "{out}");
        // Adding past the end of the list is refused.
        assert!(patch_text(BASE, &[ch("intro.layers.5", serde_json::json!({ "kind": "text", "content": "x" }))]).is_err());
    }

    #[test]
    fn a_change_that_breaks_the_config_is_refused() {
        for (k, v) in [
            ("selection.max_clip", serde_json::json!("soon")),
            ("project.fps", serde_json::json!("fast")),
            ("intro.layers.5.content", serde_json::json!("x")),
            ("", serde_json::json!(1)),
            ("selection..max_clip", serde_json::json!("5s")),
            ("project.name.first", serde_json::json!("x")),
            ("selection.nonsense", serde_json::json!(1)),
        ] {
            assert!(matches!(patch_text(BASE, &[ch(k, v.clone())]), Err(OpsError::InvalidInput { .. })), "{k} = {v}");
        }
    }
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
