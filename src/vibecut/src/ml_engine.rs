//! ONNX-Runtime-backed ML signals.
//!
//! Currently provides **Silero VAD** (voice activity detection) for
//! speech-driven duck-window selection. Architecture is signal-agnostic so
//! face detection, NIMA, etc. can slot in alongside VAD without re-plumbing
//! the planner.
//!
//! # Execution provider selection
//!
//! `MlEngine::new` first tries CUDA EP. If CUDA initialization fails (driver
//! mismatch, no GPU, etc.) the engine falls back transparently to CPU EP and
//! logs a warning. The runtime cost difference for Silero VAD is small (the
//! model is tiny — kernel-launch overhead nearly equals compute) but the
//! fallback is essential for portability.
//!
//! # Models
//!
//! `silero_vad.onnx` (≈2.3 MB, MIT-licensed, Silero VAD v6.2) comes from
//! <https://github.com/snakers4/silero-vad> (`src/silero_vad/data/`); the
//! Docker image bakes in the copy from the v6.2.3 release commit at
//! `/app/models`. [`ModelDirs`] finds model files: `[project].models_dir`,
//! then `$VIBECUT_MODELS_DIR`, then `/app/models`.
//!
//! The v5+ interface this engine drives: inputs `input` (f32 `[1, 64 + 512]`,
//! the previous window's last 64 samples prepended), `state` (f32
//! `[2, 1, 128]`), `sr` (i64, 16000); outputs `output` (speech probability)
//! and `stateN` (the next state). Upstream needs ONNX Runtime ≥ 1.16.1 for
//! it; the `ort` crate bundles 1.24.

use anyhow::{anyhow, Result};
use camino::{Utf8Path, Utf8PathBuf};
use ndarray::{Array1, Array2, Array3};
use ort::execution_providers::{CPUExecutionProvider, CUDAExecutionProvider};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use std::path::Path;

/// Convert any ort error to an anyhow error via `Display`.
fn ort_err<E: std::fmt::Display>(e: E) -> anyhow::Error {
    anyhow!("ort: {e}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpKind {
    Cuda,
    Cpu,
}

pub struct MlEngine {
    pub ep: EpKind,
    silero: Option<Session>,
}

impl MlEngine {
    /// Initialize ONNX Runtime, prefer CUDA, fall back to CPU on failure.
    /// `ort::init()` is idempotent so calling this multiple times is safe.
    pub fn new() -> Result<Self> {
        // ort::init().commit() returns bool indicating whether init happened
        // (false on subsequent calls); either is fine.
        let _ = ort::init().with_name("vibecut").commit();

        let ep = probe_ep();
        match ep {
            EpKind::Cuda => tracing::info!("ML engine: CUDA execution provider"),
            EpKind::Cpu => tracing::warn!("ML engine: falling back to CPU (CUDA unavailable)"),
        }
        Ok(Self { ep, silero: None })
    }

    #[allow(dead_code)] // diagnostic helper for callers / tests
    pub fn is_gpu(&self) -> bool {
        self.ep == EpKind::Cuda
    }

    pub fn ep_name(&self) -> &'static str {
        match self.ep {
            EpKind::Cuda => "CUDA",
            EpKind::Cpu => "CPU",
        }
    }

    /// Load the Silero VAD model from disk and bind it to the engine.
    /// Idempotent — safe to call multiple times.
    pub fn with_silero_vad(&mut self, path: &Utf8Path) -> Result<()> {
        if self.silero.is_some() {
            return Ok(());
        }
        let session = build_session(path.as_std_path(), self.ep)?;
        self.silero = Some(session);
        tracing::info!(model = %path, ep = self.ep_name(), "Silero VAD loaded");
        Ok(())
    }

    /// Run Silero VAD on 16 kHz mono PCM. Returns one speech-probability
    /// value per 32 ms (512-sample) window. The model is stateful (LSTM);
    /// hidden state is maintained internally across windows.
    pub fn vad(&mut self, pcm_16khz: &[f32]) -> Result<VadSeries> {
        let session = self
            .silero
            .as_mut()
            .ok_or_else(|| anyhow!("VAD model not loaded — call with_silero_vad first"))?;
        const SR: i64 = 16_000;
        let mut state: Array3<f32> = Array3::zeros((2, 1, 128));
        let mut probs: Vec<f32> = Vec::with_capacity(pcm_16khz.len() / VAD_WINDOW);

        for frame in vad_frames(pcm_16khz) {
            let input: Array2<f32> = Array2::from_shape_vec((1, frame.len()), frame)
                .map_err(|e| anyhow!("input shape: {e}"))?;
            let sr_arr: Array1<i64> = Array1::from_vec(vec![SR]);

            let outputs = session.run(ort::inputs! [
                "input" => Tensor::from_array(input).map_err(ort_err)?,
                "state" => Tensor::from_array(state.clone()).map_err(ort_err)?,
                "sr"    => Tensor::from_array(sr_arr).map_err(ort_err)?,
            ]).map_err(ort_err)?;

            let prob_view = outputs["output"].try_extract_array::<f32>().map_err(ort_err)?;
            let prob = prob_view.iter().next().copied().unwrap_or(0.0);
            probs.push(prob);

            let state_view = outputs["stateN"].try_extract_array::<f32>().map_err(ort_err)?;
            // Reshape new state back to (2, 1, 128). If shape mismatches we
            // leave the previous state in place (model error guard).
            let shape = state_view.shape().to_vec();
            if shape == [2usize, 1, 128] {
                if let Ok(arr) = state_view.to_owned().into_shape_with_order((2, 1, 128)) {
                    state = arr;
                }
            }
        }

        Ok(VadSeries {
            probs,
            window_samples: VAD_WINDOW as u32,
            sample_rate: SR as u32,
        })
    }
}

/// New samples per Silero VAD step at 16 kHz.
const VAD_WINDOW: usize = 512;
/// Samples of the previous window fed again in front of each step at 16 kHz.
/// Silero v5+ was trained this way (upstream's `OnnxWrapper` keeps a 64-sample
/// `_context`); without it the speech probabilities come out degraded.
const VAD_CONTEXT: usize = 64;

/// The model inputs for `pcm`: each full 512-sample window with the 64
/// samples before it prepended (zeros ahead of the first). A trailing
/// partial window is dropped.
fn vad_frames(pcm: &[f32]) -> impl Iterator<Item = Vec<f32>> + '_ {
    pcm.chunks_exact(VAD_WINDOW).enumerate().map(move |(i, window)| {
        let start = i * VAD_WINDOW;
        let mut frame = Vec::with_capacity(VAD_CONTEXT + VAD_WINDOW);
        if start == 0 {
            frame.resize(VAD_CONTEXT, 0.0);
        } else {
            frame.extend_from_slice(&pcm[start - VAD_CONTEXT..start]);
        }
        frame.extend_from_slice(window);
        frame
    })
}

// ---------- model lookup ----------

/// The Silero VAD model speech-driven ducking runs on.
pub const SILERO_VAD_FILE: &str = "silero_vad.onnx";
/// Extra model folder(s), separated like `PATH`, searched after
/// `[project].models_dir`.
pub const MODELS_DIR_ENV: &str = "VIBECUT_MODELS_DIR";
/// Where the Docker image installs its bundled models: the last fallback.
pub const IMAGE_MODELS_DIR: &str = "/app/models";

/// Which step of the lookup chain a model folder is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelDirKind {
    Project,
    Env,
    Image,
}

impl ModelDirKind {
    pub fn label(self) -> &'static str {
        match self {
            ModelDirKind::Project => "[project].models_dir",
            ModelDirKind::Env => "$VIBECUT_MODELS_DIR",
            ModelDirKind::Image => "bundled models",
        }
    }
}

/// The folders searched for model files, in order: `[project].models_dir`,
/// each folder in `$VIBECUT_MODELS_DIR`, then [`IMAGE_MODELS_DIR`].
#[derive(Debug, Clone)]
pub struct ModelDirs {
    pub dirs: Vec<(Utf8PathBuf, ModelDirKind)>,
}

impl ModelDirs {
    pub fn new(
        project_models_dir: &Utf8Path,
        models_dir_var: Option<&std::ffi::OsStr>,
        image_models_dir: &Utf8Path,
    ) -> Self {
        let mut dirs = vec![(project_models_dir.to_path_buf(), ModelDirKind::Project)];
        if let Some(var) = models_dir_var {
            dirs.extend(
                std::env::split_paths(var)
                    .filter(|p| !p.as_os_str().is_empty())
                    .filter_map(|p| Utf8PathBuf::from_path_buf(p).ok())
                    .map(|p| (p, ModelDirKind::Env)),
            );
        }
        dirs.push((image_models_dir.to_path_buf(), ModelDirKind::Image));
        Self { dirs }
    }

    /// The chain for a project, with `$VIBECUT_MODELS_DIR` read from the
    /// environment.
    pub fn from_env(project_models_dir: &Utf8Path) -> Self {
        Self::new(
            project_models_dir,
            std::env::var_os(MODELS_DIR_ENV).as_deref(),
            Utf8Path::new(IMAGE_MODELS_DIR),
        )
    }

    /// The first `file` in the chain, and the folder kind it came from.
    pub fn find(&self, file: &str) -> Option<(Utf8PathBuf, ModelDirKind)> {
        self.dirs
            .iter()
            .map(|(dir, kind)| (dir.join(file), *kind))
            .find(|(path, _)| path.is_file())
    }

    pub fn describe(&self) -> String {
        let parts: Vec<String> =
            self.dirs.iter().map(|(d, k)| format!("{d} ({})", k.label())).collect();
        parts.join(", ")
    }
}

/// The Silero VAD model a render of `cfg` loads: `None` when it doesn't
/// duck the soundtrack (no `[soundtrack]`, or `duck_count = 0`), else the
/// first `silero_vad.onnx` in `[project].models_dir`, `models_dir_var`
/// (the value of `$VIBECUT_MODELS_DIR`) and `image_models_dir`.
pub fn silero_vad_for(
    cfg: &crate::project_config::Config,
    models_dir_var: Option<&std::ffi::OsStr>,
    image_models_dir: &Utf8Path,
) -> Option<Utf8PathBuf> {
    if !cfg.soundtrack.as_ref().is_some_and(|s| s.duck_count > 0) {
        return None;
    }
    find_silero_vad(&ModelDirs::new(&cfg.project.models_dir, models_dir_var, image_models_dir))
}

/// [`silero_vad_for`] with the real `$VIBECUT_MODELS_DIR` and
/// [`IMAGE_MODELS_DIR`]: what the pipeline calls.
pub fn silero_vad_from_env(cfg: &crate::project_config::Config) -> Option<Utf8PathBuf> {
    silero_vad_for(cfg, std::env::var_os(MODELS_DIR_ENV).as_deref(), Utf8Path::new(IMAGE_MODELS_DIR))
}

/// The Silero VAD model for speech-driven ducking, or `None` when there is
/// none anywhere in the chain (ducking then picks windows by loudness
/// alone). Logs the outcome at info level either way.
pub fn find_silero_vad(dirs: &ModelDirs) -> Option<Utf8PathBuf> {
    match dirs.find(SILERO_VAD_FILE) {
        Some((path, kind)) => {
            tracing::info!(path = %path, from = kind.label(), "Silero VAD model found; speech-driven ducking enabled");
            Some(path)
        }
        None => {
            tracing::info!(
                searched = %dirs.describe(),
                "{SILERO_VAD_FILE} not found; speech-driven ducking disabled. Get it from \
                 https://github.com/snakers4/silero-vad (src/silero_vad/data/) and put it in \
                 [project].models_dir or ${MODELS_DIR_ENV} to enable it (the Docker image \
                 ships one in {IMAGE_MODELS_DIR})"
            );
            None
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct VadSeries {
    pub probs: Vec<f32>,
    pub window_samples: u32,
    pub sample_rate: u32,
}

#[allow(dead_code)] // public summary helpers — callers compute their own slices today, but these are part of the documented surface
impl VadSeries {
    pub fn frame_rate_hz(&self) -> f32 {
        self.sample_rate as f32 / self.window_samples as f32
    }
    pub fn time_of(&self, i: usize) -> f32 {
        i as f32 / self.frame_rate_hz()
    }
    pub fn duration_secs(&self) -> f32 {
        self.probs.len() as f32 / self.frame_rate_hz()
    }
    /// Mean speech probability over the whole series (0..1).
    pub fn mean(&self) -> f32 {
        if self.probs.is_empty() {
            return 0.0;
        }
        self.probs.iter().sum::<f32>() / self.probs.len() as f32
    }
    /// Max speech probability anywhere in the series.
    pub fn peak(&self) -> f32 {
        self.probs.iter().cloned().fold(0.0_f32, f32::max)
    }
    /// Fraction of windows with speech probability > threshold (0..1).
    pub fn coverage(&self, threshold: f32) -> f32 {
        if self.probs.is_empty() {
            return 0.0;
        }
        let n = self.probs.iter().filter(|&&p| p > threshold).count();
        n as f32 / self.probs.len() as f32
    }
}

fn probe_ep() -> EpKind {
    // SessionBuilder accepts EP registrations lazily; success here proves the
    // EP is compiled in and the dylib loads, but true CUDA availability is
    // only confirmed when a session actually commits. We register CPU as a
    // second provider so failures fall back automatically.
    let res = (|| -> std::result::Result<(), Box<dyn std::error::Error>> {
        let b = Session::builder().map_err(|e| e.to_string())?;
        b.with_execution_providers([
            CUDAExecutionProvider::default().build(),
            CPUExecutionProvider::default().build(),
        ]).map_err(|e| e.to_string())?;
        Ok(())
    })();
    match res {
        Ok(_) => EpKind::Cuda,
        Err(e) => {
            tracing::warn!(error = %e, "CUDA EP unavailable, using CPU");
            EpKind::Cpu
        }
    }
}

fn build_session(path: &Path, ep: EpKind) -> Result<Session> {
    // ort 2.x doesn't expose `commit_from_file` directly in all builds; load
    // the bytes ourselves and use `commit_from_memory`. Slight memory cost
    // (~2 MB for Silero) is negligible.
    let bytes = std::fs::read(path)
        .map_err(|e| anyhow!("reading ONNX model {}: {e}", path.display()))?;
    let builder = Session::builder().map_err(ort_err)?;
    let builder = match ep {
        EpKind::Cuda => builder
            .with_execution_providers([
                CUDAExecutionProvider::default().build(),
                CPUExecutionProvider::default().build(),
            ])
            .map_err(ort_err)?,
        EpKind::Cpu => builder
            .with_execution_providers([CPUExecutionProvider::default().build()])
            .map_err(ort_err)?,
    };
    let session = builder
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(ort_err)?
        .commit_from_memory(&bytes)
        .map_err(ort_err)?;
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fresh, empty temp folder for one test.
    fn temp_dir(name: &str) -> Utf8PathBuf {
        let dir = std::env::temp_dir().join(format!("vibecut_models_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Utf8PathBuf::from_path_buf(dir).unwrap()
    }

    /// An empty stand-in model file: the lookup only checks the path.
    fn put_model(dir: &Utf8Path) -> Utf8PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(SILERO_VAD_FILE);
        std::fs::write(&p, b"").unwrap();
        p
    }

    #[test]
    fn vad_frames_prepend_the_previous_64_samples() {
        let pcm: Vec<f32> = (0..3 * VAD_WINDOW + 100).map(|i| i as f32 + 1.0).collect();
        let frames: Vec<Vec<f32>> = vad_frames(&pcm).collect();
        assert_eq!(frames.len(), 3, "the trailing partial window is dropped");
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(f.len(), VAD_CONTEXT + VAD_WINDOW);
            let start = i * VAD_WINDOW;
            assert_eq!(&f[VAD_CONTEXT..], &pcm[start..start + VAD_WINDOW]);
            if i == 0 {
                assert!(f[..VAD_CONTEXT].iter().all(|&s| s == 0.0));
            } else {
                assert_eq!(&f[..VAD_CONTEXT], &pcm[start - VAD_CONTEXT..start]);
            }
        }
    }

    #[test]
    fn model_lookup_runs_project_then_env_then_image() {
        let root = temp_dir("chain");
        let (project, env_a, env_b, image) =
            (root.join("project"), root.join("env_a"), root.join("env_b"), root.join("image"));
        let var = std::env::join_paths([env_a.as_str(), "", env_b.as_str()]).unwrap();
        let dirs = ModelDirs::new(&project, Some(var.as_os_str()), &image);
        let kinds: Vec<_> = dirs.dirs.iter().map(|(d, k)| (d.clone(), *k)).collect();
        assert_eq!(
            kinds,
            [
                (project.clone(), ModelDirKind::Project),
                (env_a.clone(), ModelDirKind::Env),
                (env_b.clone(), ModelDirKind::Env),
                (image.clone(), ModelDirKind::Image),
            ]
        );

        assert_eq!(dirs.find(SILERO_VAD_FILE), None);
        let in_image = put_model(&image);
        assert_eq!(dirs.find(SILERO_VAD_FILE), Some((in_image, ModelDirKind::Image)));
        let in_env_b = put_model(&env_b);
        assert_eq!(dirs.find(SILERO_VAD_FILE), Some((in_env_b, ModelDirKind::Env)));
        let in_project = put_model(&project);
        assert_eq!(dirs.find(SILERO_VAD_FILE), Some((in_project, ModelDirKind::Project)));
        // A folder named like the model doesn't count.
        let odd = root.join("odd");
        std::fs::create_dir_all(odd.join(SILERO_VAD_FILE)).unwrap();
        assert_eq!(ModelDirs::new(&odd, None, &odd).find(SILERO_VAD_FILE), None);
    }

    /// A project config whose `models_dir` is `models_dir`, with a
    /// `[soundtrack]` ducking `duck_count` times (none when `None`).
    fn config(models_dir: &Utf8Path, duck_count: Option<u32>) -> crate::project_config::Config {
        let soundtrack = duck_count
            .map(|n| format!("[soundtrack]\nfile = \"a.mp3\"\nduck_count = {n}\n"))
            .unwrap_or_default();
        let toml = format!(
            r#"
            schema_version = 1
            [project]
            name = "t"
            target_duration = "10s"
            fps = 30
            models_dir = '{models_dir}'
            [input]
            folder = "."
            [[outputs]]
            path = "out.mp4"
            resolution = "1920x1080"
            {soundtrack}
            "#
        );
        toml::from_str(&toml).unwrap()
    }

    #[test]
    fn ducking_finds_the_vad_model_through_the_fallbacks() {
        // A project whose own models_dir has no model, as on a fresh install.
        let root = temp_dir("ducking");
        let cfg = config(&root.join("project_models"), Some(2));
        let (env_dir, image_dir) = (root.join("env_models"), root.join("app_models"));
        let env = Some(env_dir.as_os_str());

        // Nothing anywhere: speech-driven ducking stays off.
        assert_eq!(silero_vad_for(&cfg, env, &image_dir), None);

        // The image's bundled copy turns it on with no env var set.
        let bundled = put_model(&image_dir);
        assert_eq!(silero_vad_for(&cfg, None, &image_dir), Some(bundled.clone()));

        // $VIBECUT_MODELS_DIR wins over the bundled copy.
        let from_env = put_model(&env_dir);
        assert_eq!(silero_vad_for(&cfg, env, &image_dir), Some(from_env));

        // The project's own models_dir wins over both.
        let own = put_model(&cfg.project.models_dir);
        assert_eq!(silero_vad_for(&cfg, env, &image_dir), Some(own));

        // A render that doesn't duck loads no model, wherever one is.
        let no_ducks = config(&cfg.project.models_dir, Some(0));
        assert_eq!(silero_vad_for(&no_ducks, env, &image_dir), None);
        let no_soundtrack = config(&cfg.project.models_dir, None);
        assert_eq!(silero_vad_for(&no_soundtrack, env, &image_dir), None);
    }

    #[test]
    fn the_real_chain_ends_at_the_image_models_dir() {
        let dirs = ModelDirs::from_env(Utf8Path::new("proj/models"));
        assert_eq!(dirs.dirs.first(), Some(&(Utf8PathBuf::from("proj/models"), ModelDirKind::Project)));
        assert_eq!(dirs.dirs.last(), Some(&(Utf8PathBuf::from("/app/models"), ModelDirKind::Image)));
    }

    /// What the pipeline calls ([`silero_vad_from_env`], [`ModelDirs::from_env`])
    /// reads `$VIBECUT_MODELS_DIR` from the real environment, checked in a
    /// child process.
    #[test]
    fn the_pipeline_lookup_reads_the_models_dir_var() {
        const TEST: &str = "ml_engine::tests::the_pipeline_lookup_reads_the_models_dir_var";
        if crate::test_env::is_child(TEST) {
            let env_dir = Utf8PathBuf::from(std::env::var(MODELS_DIR_ENV).unwrap());
            let project = env_dir.with_file_name("project_models");
            let dirs = ModelDirs::from_env(&project);
            assert_eq!(dirs.dirs[1], (env_dir.clone(), ModelDirKind::Env));
            assert_eq!(
                silero_vad_from_env(&config(&project, Some(2))),
                Some(env_dir.join(SILERO_VAD_FILE))
            );
            return;
        }
        let root = temp_dir("pipeline_env");
        let env_dir = root.join("env_models");
        put_model(&env_dir);
        // The documented name, not the constant, so renaming it fails here.
        crate::test_env::run_child(TEST, &[("VIBECUT_MODELS_DIR", env_dir.as_str())]);
    }
}
