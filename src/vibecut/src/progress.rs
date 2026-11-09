//! Stage-level render progress, so a UI can show what a long render is
//! doing instead of a bare "running".
//!
//! The pipeline calls [`report`] at every stage boundary and per item
//! inside the long stages (clips analysed, clips cut). The caller decides
//! where reports go by running the pipeline inside [`with_sink`]; with no
//! sink installed every report is a no-op, so the CLI is unaffected.
//! Work fanned out to other threads (the visual-analysis pool) reports
//! through a [`Reporter`] cloned from [`current`] before the fan-out.
//!
//! `overall` is a 0..1 estimate built from fixed per-stage weights (a
//! typical render's time split). It never goes backwards, even when a
//! stage is skipped or reports out of order from worker threads.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use serde::Serialize;

/// Render stages, in pipeline order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Finding and probing the footage files.
    Probing,
    /// Beat grid, tempo and energy of the soundtrack.
    Music,
    /// Telemetry and visual analysis of every source.
    Analysing,
    /// Scoring, picking, ordering and beat-locking the moments.
    Picking,
    /// Cutting and conforming each clip.
    Clips,
    /// Crossfading the clips (and cards) into one body.
    Joining,
    /// Laying the music under the body.
    Mixing,
    /// Transcoding extra outputs.
    Encoding,
    /// Post-render beat-sync check and manifest.
    Checking,
}

impl Stage {
    const ALL: [Stage; 9] = [
        Stage::Probing,
        Stage::Music,
        Stage::Analysing,
        Stage::Picking,
        Stage::Clips,
        Stage::Joining,
        Stage::Mixing,
        Stage::Encoding,
        Stage::Checking,
    ];

    /// Share of a typical render's wall-clock time (sums to 1).
    fn weight(self) -> f32 {
        match self {
            Stage::Probing => 0.04,
            Stage::Music => 0.03,
            Stage::Analysing => 0.30,
            Stage::Picking => 0.03,
            Stage::Clips => 0.35,
            Stage::Joining => 0.12,
            Stage::Mixing => 0.06,
            Stage::Encoding => 0.04,
            Stage::Checking => 0.03,
        }
    }

    /// Friendly, jargon-free description for people watching a render.
    pub fn label(self) -> &'static str {
        match self {
            Stage::Probing => "Finding your clips",
            Stage::Music => "Listening to the music",
            Stage::Analysing => "Watching your footage",
            Stage::Picking => "Picking the best moments",
            Stage::Clips => "Cutting the clips",
            Stage::Joining => "Stitching it together",
            Stage::Mixing => "Mixing the music",
            Stage::Encoding => "Encoding",
            Stage::Checking => "Checking the beat",
        }
    }

    fn start(self) -> f32 {
        Stage::ALL
            .iter()
            .take_while(|&&s| s != self)
            .map(|s| s.weight())
            .sum()
    }
}

/// One progress report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Progress {
    pub stage: Stage,
    /// [`Stage::label`], denormalised so clients don't need a table.
    pub label: &'static str,
    /// 0..1 within the stage.
    pub stage_fraction: f32,
    /// 0..1 for the whole render (weighted estimate, never decreasing).
    pub overall: f32,
    /// Optional detail, e.g. "clip 7 of 24".
    pub detail: Option<String>,
}

type Sink = dyn Fn(&Progress) + Send + Sync;

/// Cloneable handle that delivers reports to the installed sink.
#[derive(Clone)]
pub struct Reporter {
    sink: Arc<Sink>,
    high_water: Arc<Mutex<f32>>,
}

impl Reporter {
    pub fn report(&self, stage: Stage, stage_fraction: f32, detail: Option<String>) {
        let f = stage_fraction.clamp(0.0, 1.0);
        let raw = (stage.start() + f * stage.weight()).clamp(0.0, 1.0);
        // Deliver under the lock: parallel workers (the analysis stage
        // reports from rayon) must not overtake each other, or a stale
        // report would land after a newer one and the bar would step back.
        let mut hw = self.high_water.lock().unwrap_or_else(|e| e.into_inner());
        if raw < *hw {
            return;
        }
        *hw = raw;
        (self.sink)(&Progress { stage, label: stage.label(), stage_fraction: f, overall: raw, detail });
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Reporter>> = const { RefCell::new(None) };
}

/// Restores the previous sink when dropped, so a panic inside
/// [`with_sink`] can't leave a stale sink installed on a pooled thread.
struct Restore(Option<Reporter>);

impl Drop for Restore {
    fn drop(&mut self) {
        let prev = self.0.take();
        CURRENT.with(|c| *c.borrow_mut() = prev);
    }
}

/// Run `f` with `sink` receiving every progress report made on this
/// thread (and through [`Reporter`]s cloned from it).
pub fn with_sink<R>(sink: impl Fn(&Progress) + Send + Sync + 'static, f: impl FnOnce() -> R) -> R {
    let reporter = Reporter { sink: Arc::new(sink), high_water: Arc::new(Mutex::new(0.0)) };
    let prev = CURRENT.with(|c| c.replace(Some(reporter)));
    let _restore = Restore(prev);
    f()
}

/// The reporter installed on this thread, if any — clone it into work
/// that runs on other threads.
pub fn current() -> Option<Reporter> {
    CURRENT.with(|c| c.borrow().clone())
}

/// Report progress on this thread's sink (no-op without one).
pub fn report(stage: Stage, stage_fraction: f32, detail: Option<String>) {
    if let Some(r) = current() {
        r.report(stage, stage_fraction, detail);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_sum_to_one() {
        let total: f32 = Stage::ALL.iter().map(|s| s.weight()).sum();
        assert!((total - 1.0).abs() < 1e-5, "{total}");
    }

    #[test]
    fn reports_reach_the_sink_and_never_go_backwards() {
        let seen: Arc<Mutex<Vec<Progress>>> = Arc::default();
        let sink_seen = seen.clone();
        with_sink(
            move |p| sink_seen.lock().unwrap().push(p.clone()),
            || {
                report(Stage::Probing, 1.0, None);
                report(Stage::Clips, 0.5, Some("clip 3 of 6".into()));
                // A late report from an earlier stage is dropped, so neither
                // the bar nor the label steps back.
                report(Stage::Analysing, 0.9, None);
                let r = current().expect("reporter installed");
                std::thread::spawn(move || r.report(Stage::Checking, 1.0, None))
                    .join()
                    .unwrap();
            },
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[1].label, "Cutting the clips");
        assert_eq!(seen[1].detail.as_deref(), Some("clip 3 of 6"));
        assert!(seen.windows(2).all(|w| w[1].overall >= w[0].overall));
        assert!((seen[2].overall - 1.0).abs() < 1e-5);
        // Outside `with_sink`, reporting is a no-op.
        assert!(current().is_none());
    }
}
