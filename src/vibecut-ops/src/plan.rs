//! The edit before the encode: plan a project config — the picks, their
//! beat-locked lengths, the exact output timeline, the music's beats and a
//! frame to show per clip — without rendering it. Seconds on footage that
//! was analysed before; what a UI shows as a storyboard while the render
//! runs. Transport-free counterpart of `POST /api/plan` and the MCP
//! `plan_edit` tool.
//!
//! A render plans its config too, to the same edit. So that a storyboard
//! asked for while a render of that config runs doesn't plan it a second
//! time (on footage not analysed yet that means every source decoded
//! twice at once), each render registers here when it is submitted and
//! publishes its plan the moment it has one ([`RenderPlan`]); [`plan`]
//! waits for that instead of planning again. A plan is reused only for
//! the exact config text it was made from, and for [`REUSE_FOR`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use camino::Utf8PathBuf;
use tokio::sync::watch;

use crate::error::OpsError;
use crate::paths::resolve_projects;

/// How long a finished plan answers later requests for the same config
/// text. Footage added in the meantime shows up after this.
pub const REUSE_FOR: Duration = Duration::from_secs(10 * 60);

/// The longest a request waits for a render's plan before planning on
/// its own (a render on a huge card that is still analysing).
const WAIT_AT_MOST: Duration = Duration::from_secs(15 * 60);

/// One config's plan, as known here.
#[derive(Debug, Clone)]
enum Slot {
    /// A render of `text` was submitted and hasn't planned yet.
    Pending { text: String },
    /// The plan of `text`, made at `at`.
    Ready { text: String, plan: serde_json::Value, at: Instant, by: &'static str },
    /// The render stopped or failed before it planned.
    Gone,
}

/// Per config (canonical path): the latest slot.
static SLOTS: LazyLock<Mutex<HashMap<PathBuf, watch::Receiver<Slot>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn key(cfg: &Path) -> PathBuf {
    std::fs::canonicalize(cfg).unwrap_or_else(|_| cfg.to_path_buf())
}

fn read_text(cfg: &Path) -> String {
    std::fs::read_to_string(cfg).unwrap_or_default()
}

fn install(cfg: &Path, slot: Slot) -> watch::Sender<Slot> {
    let (tx, rx) = watch::channel(slot);
    SLOTS.lock().unwrap_or_else(|e| e.into_inner()).insert(key(cfg), rx);
    tx
}

/// A render's place to publish its plan. Made when the render is
/// submitted (so a storyboard request right after waits for it); dropped
/// without a plan — stopped, failed — it lets the waiters plan on their own.
pub struct RenderPlan {
    cfg: PathBuf,
    tx: watch::Sender<Slot>,
}

impl RenderPlan {
    /// Register a render of `cfg` (its config text as it is now).
    pub fn begin(cfg: &Path) -> Self {
        let text = read_text(cfg);
        RenderPlan { cfg: cfg.to_path_buf(), tx: install(cfg, Slot::Pending { text }) }
    }

    /// The render is starting: it reads the config as it is now, which may
    /// have changed while the job was queued.
    pub fn started(&self) {
        let text = read_text(&self.cfg);
        self.tx.send_if_modified(|s| match s {
            Slot::Pending { text: old } if *old != text => {
                *old = text;
                true
            }
            _ => false,
        });
    }

    /// The pipeline's plan hook: publishes the plan as JSON.
    pub fn hook(self) -> Box<dyn FnOnce(&vibecut::run_pipeline::PlanSummary) + Send> {
        Box::new(move |summary| {
            let text = match &*self.tx.borrow() {
                Slot::Pending { text } => text.clone(),
                _ => return,
            };
            if let Ok(plan) = serde_json::to_value(summary) {
                self.tx.send_replace(Slot::Ready { text, plan, at: Instant::now(), by: "render" });
            }
        })
    }
}

impl Drop for RenderPlan {
    fn drop(&mut self) {
        // Never planned: whoever waits plans on their own.
        self.tx.send_if_modified(|s| {
            if matches!(s, Slot::Pending { .. }) {
                *s = Slot::Gone;
                true
            } else {
                false
            }
        });
    }
}

/// What a request for `text` gets from `rx`: a plan to reuse (and who made
/// it), or `None` to plan on its own. Waits while a render of the same
/// text is planning it.
async fn reuse(mut rx: watch::Receiver<Slot>, text: &str) -> Option<(serde_json::Value, &'static str)> {
    let deadline = tokio::time::Instant::now() + WAIT_AT_MOST;
    loop {
        let slot = rx.borrow_and_update().clone();
        match slot {
            Slot::Ready { text: t, plan, at, by } if t == text && at.elapsed() < REUSE_FOR => return Some((plan, by)),
            Slot::Pending { text: t } if t == text => {
                match tokio::time::timeout_at(deadline, rx.changed()).await {
                    Ok(Ok(())) => continue,
                    // The render went away without a word, or is taking
                    // too long: plan here.
                    _ => return None,
                }
            }
            _ => return None,
        }
    }
}

/// Plan `config_path` (absolute, or relative to `/projects`): the
/// [`vibecut::run_pipeline::PlanSummary`] as JSON, plus `config_path`,
/// `planned_by` (`render` — a render of this config planned it; `plan` —
/// planned for this request, or an earlier one) and `elapsed_ms`.
pub async fn plan(config_path: &str) -> Result<serde_json::Value, OpsError> {
    let cfg = resolve_projects(config_path);
    if !cfg.exists() {
        return Err(OpsError::not_found_with("config", cfg.to_string_lossy().into_owned()));
    }
    let cfg_camino = Utf8PathBuf::from_path_buf(cfg.clone())
        .map_err(|_| OpsError::invalid(format!("non-UTF-8 config path: {}", cfg.display())))?;
    let t0 = Instant::now();
    let text = read_text(&cfg);
    let known = SLOTS.lock().unwrap_or_else(|e| e.into_inner()).get(&key(&cfg)).cloned();
    let (mut plan, by) = match match known {
        Some(rx) => reuse(rx, &text).await,
        None => None,
    } {
        Some(found) => found,
        None => {
            let summary = tokio::task::spawn_blocking(move || {
                vibecut::run_pipeline::plan_cmd(&cfg_camino, /*no_visual*/ false, /*no_cache*/ false)
            })
            .await
            .map_err(|e| OpsError::internal(format!("plan task panicked: {e}")))?
            .map_err(|e| OpsError::invalid(format!("{e:#}")))?;
            let plan = serde_json::to_value(&summary)
                .map_err(|e| OpsError::internal(format!("serializing the plan: {e}")))?;
            // Later requests for the same text reuse it — unless a render
            // registered meanwhile, whose own plan is the one to show.
            let mut slots = SLOTS.lock().unwrap_or_else(|e| e.into_inner());
            let busy = slots.get(&key(&cfg)).is_some_and(|rx| matches!(&*rx.borrow(), Slot::Pending { .. }));
            if !busy {
                let (_tx, rx) = watch::channel(Slot::Ready { text, plan: plan.clone(), at: Instant::now(), by: "plan" });
                slots.insert(key(&cfg), rx);
            }
            (plan, "plan")
        }
    };
    if let serde_json::Value::Object(map) = &mut plan {
        map.insert("config_path".into(), cfg.to_string_lossy().into_owned().into());
        map.insert("planned_by".into(), by.into());
        map.insert("elapsed_ms".into(), (t0.elapsed().as_millis() as u64).into());
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vibecut_plan_ops_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn summary(name: &str) -> vibecut::run_pipeline::PlanSummary {
        serde_json::from_value(serde_json::json!({
            "project_name": name,
            "fps": 30,
            "seed": 1,
            "target_duration_secs": 10.0,
            "duration_secs": 9.5,
            "beats": [0.5, 1.0],
            "segments": [],
            "timeline": { "fps": 30, "frames": 285, "duration_secs": 9.5, "clips": [] },
            "thumbnails": []
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn a_missing_config_is_not_found() {
        let e = plan("/nonexistent/vibecut/p.toml").await.unwrap_err();
        assert!(matches!(e, OpsError::NotFound { .. }), "{e:?}");
    }

    /// A storyboard asked for while a render of the same config plans it
    /// gets the render's plan — it waits rather than planning again. (The
    /// config here isn't even valid: planning it would fail.)
    #[tokio::test]
    async fn a_request_during_a_render_waits_for_the_renders_plan() {
        let d = tmp("wait");
        let cfg = d.join("p.toml");
        std::fs::write(&cfg, "not a config, so planning it would fail").unwrap();
        let render = RenderPlan::begin(&cfg);
        let path = cfg.to_string_lossy().into_owned();
        let asked = tokio::spawn(async move { plan(&path).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!asked.is_finished(), "it must wait for the render's plan");
        render.started();
        (render.hook())(&summary("from the render"));
        let got = asked.await.unwrap().unwrap();
        assert_eq!(got["project_name"], "from the render");
        assert_eq!(got["planned_by"], "render");
        assert_eq!(got["config_path"], cfg.to_string_lossy().as_ref());
        // Later requests for the same text reuse it at once.
        let again = plan(&cfg.to_string_lossy()).await.unwrap();
        assert_eq!(again["project_name"], "from the render");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A render that stops before it plans, or a config changed since,
    /// means planning here — never a plan of other settings.
    #[tokio::test]
    async fn a_stopped_render_or_an_edited_config_plans_afresh() {
        let d = tmp("afresh");
        let cfg = d.join("p.toml");
        std::fs::write(&cfg, "still not a config").unwrap();
        let render = RenderPlan::begin(&cfg);
        let path = cfg.to_string_lossy().into_owned();
        let asked = tokio::spawn(async move { plan(&path).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(render); // stopped before planning
        let e = asked.await.unwrap().unwrap_err();
        assert!(matches!(e, OpsError::InvalidInput { .. }), "planned on its own: {e:?}");

        // Published for one text, asked for another: not reused.
        let render = RenderPlan::begin(&cfg);
        (render.hook())(&summary("old settings"));
        std::fs::write(&cfg, "edited, and still not a config").unwrap();
        assert!(plan(&cfg.to_string_lossy()).await.is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
