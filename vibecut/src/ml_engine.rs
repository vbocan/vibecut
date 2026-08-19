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
//! `silero_vad.onnx` (≈2 MB, MIT-licensed) must be supplied. Download it
//! from <https://github.com/snakers4/silero-vad/raw/master/src/silero_vad/data/silero_vad.onnx>
//! and pass the path via `MlEngine::with_silero_vad(path)`.

use anyhow::{anyhow, Result};
use camino::Utf8Path;
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
        const WINDOW: usize = 512;
        const SR: i64 = 16_000;
        let mut state: Array3<f32> = Array3::zeros((2, 1, 128));
        let mut probs: Vec<f32> = Vec::with_capacity(pcm_16khz.len() / WINDOW);

        for chunk in pcm_16khz.chunks_exact(WINDOW) {
            let input: Array2<f32> = Array2::from_shape_vec((1, WINDOW), chunk.to_vec())
                .map_err(|e| anyhow!("input shape: {e}"))?;
            let sr_arr: Array1<i64> = Array1::from_vec(vec![SR]);

            let outputs = session.run(ort::inputs! [
                "input" => Tensor::from_array(input).map_err(ort_err)?,
                "state" => Tensor::from_array(state.clone()).map_err(ort_err)?,
                "sr"    => Tensor::from_array(sr_arr).map_err(ort_err)?,
            ]).map_err(ort_err)?;

            let prob_view = outputs[0].try_extract_array::<f32>().map_err(ort_err)?;
            let prob = prob_view.iter().next().copied().unwrap_or(0.0);
            probs.push(prob);

            let state_view = outputs[1].try_extract_array::<f32>().map_err(ort_err)?;
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
            window_samples: WINDOW as u32,
            sample_rate: SR as u32,
        })
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
