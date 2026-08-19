//! Vibecut CLI — a GoPro-Quik-like highlight reel generator.
//!
//! Takes a folder of GoPro footage and a TOML config; produces a single
//! social-ready video (intro/outro cards, beat-aligned cuts, transitions,
//! optional stabilization, optional speech-aware soundtrack ducking).
//!
//! # Module map
//!
//! | Module                 | Responsibility                                        |
//! |------------------------|-------------------------------------------------------|
//! | [`project_config`]     | TOML schema + defaults + validation                   |
//! | [`media_probe`]        | walk a folder, ffprobe each file, extract GPMF        |
//! | [`gopro_metadata`]     | parse GPMF (accelerometer, GPS, HiLight tags, …)      |
//! | [`audio_analysis`]     | PCM decode, beats, tempo, energy, xcorr sync, loud-region |
//! | [`visual_analysis`]    | per-frame sharpness, exposure score, motion           |
//! | [`ml_engine`]          | ONNX Runtime + Silero VAD (CUDA EP / CPU fallback)    |
//! | [`selection_scoring`]  | combine signals into score curves; multi-source picker |
//! | [`selection_planner`]  | source/chapter model, telemetry orchestration, manifest |
//! | [`disk_cache`]         | JSON sidecar cache keyed by `(path, size, mtime, params)` |
//! | [`video_renderer`]     | ffmpeg staged render: normalize → xfade → mux         |
//! | [`main`] (this file)   | clap subcommands + the `run` pipeline orchestration   |
//!
//! # Subcommands
//!
//! - `run <config>` — execute the full pipeline (the main feature).
//! - `validate <config>` — parse + validate a TOML, no execution.
//! - `probe <path>` — show ffprobe metadata for a file or folder.
//! - `gpmf <file>` — dump the GPMF metadata tree of a GoPro MP4.
//! - `beats <audio>` — analyze tempo, beat times, and the loud region.
//! - `sync <a> <b>` — cross-correlate two audio tracks for multi-cam alignment.

// The 14 modules below now live in `src/lib.rs` (so vibecut-mcp and
// any other consumer can call them directly). The binary uses them via
// the library crate — `vibecut::media_probe` etc. — instead of
// `mod media_probe`. Inside this main.rs `vibecut::` refers to the binary
// crate (which has no modules of its own), so we resolve everything via
// the `vibecut::*` namespace below.

// The orchestration types and helpers that used to live here (run_cmd,
// execute_pipeline, ~30 helper fns) moved to `vibecut::run_pipeline`.
// main.rs now only contains diagnostic CLI subcommands
// (probe, beats, sync, gpmf) plus the clap dispatch.
use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "vibecut", version, about = "Turn GoPro footage + TOML into a finished video")]
struct Cli {
    /// Logging filter. Defaults to `info`. Examples: `debug`, `vibecut::probe=trace,info`.
    #[arg(long, global = true, env = "VIBECUT_LOG")]
    log: Option<String>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Print metadata for a video file, or every video in a folder.
    Probe {
        /// Path to a single video file or a folder to walk.
        path: Utf8PathBuf,
        /// Emit machine-readable JSON instead of a human-readable table.
        #[arg(long)]
        json: bool,
    },
    /// Validate a config and (later) execute the pipeline.
    Run {
        config: Utf8PathBuf,
        /// Parse + validate only; do not execute the pipeline.
        #[arg(long)]
        dry_run: bool,
        /// Skip per-frame visual analysis (sharpness / exposure / motion).
        /// Useful for fast iteration during development; output cuts will
        /// still come from telemetry signals only.
        #[arg(long)]
        no_visual: bool,
        /// Disable the on-disk visual analysis cache for this run.
        #[arg(long)]
        no_cache: bool,
        /// After the run completes, print a per-pick score breakdown.
        #[arg(long)]
        explain: bool,
        /// Print the merged config (defaults + your overrides) and exit.
        #[arg(long)]
        print_effective_config: bool,
        /// Override `[project].seed` with a random value derived from the
        /// system clock — useful when paired with `[selection].variety > 0`
        /// to get a fresh cut on each render. The actual seed used is logged
        /// and written to the manifest so you can lock it back in via
        /// `seed = N` in your TOML if you like the result.
        #[arg(long)]
        random_seed: bool,
    },
    /// Validate a config without running anything. Equivalent to `run --dry-run`.
    Validate { config: Utf8PathBuf },
    /// Extract and dump the GoPro Metadata (GPMF) tree from a file. Useful
    /// for understanding what telemetry / HiLight tags / accelerometer data
    /// is present.
    Gpmf {
        file: Utf8PathBuf,
        /// Limit how many top-level DEVC payloads to print (default: 2).
        #[arg(long, default_value_t = 2)]
        max_payloads: usize,
        /// Write extracted raw GPMF bytes to this path (debug aid).
        #[arg(long)]
        save_raw: Option<Utf8PathBuf>,
    },
    /// Analyze an audio file and print detected tempo + beat times. Useful
    /// for sanity-checking the soundtrack analyzer before running a render.
    Beats {
        file: Utf8PathBuf,
        /// Show at most this many beat times (default: 32).
        #[arg(long, default_value_t = 32)]
        max_print: usize,
    },
    /// Sweep the per-source visual-analysis worker count to find the best
    /// value for this machine, and (with --apply) write it to the user
    /// settings file so subsequent runs use it automatically. Generates
    /// synthetic test fixtures on first run; no real footage required.
    Benchmark {
        /// Worker counts to sweep.
        #[arg(long, value_delimiter = ',', default_values_t = [1u32, 2, 3, 4, 6])]
        threads: Vec<u32>,
        /// Runs per thread count; reports best of N.
        #[arg(long, default_value_t = 1)]
        repeats: u32,
        /// Number of synthetic test sources.
        #[arg(long, default_value_t = 8)]
        num_files: u32,
        /// Per-fixture duration in seconds.
        #[arg(long, default_value_t = 120)]
        duration_secs: u32,
        /// Resolution for the synthetic fixtures (`WIDTHxHEIGHT`). Default
        /// 4K matches real GoPro footage so parallelism pays off; lighter
        /// fixtures (1080p, short durations) collapse into the noise floor
        /// and produce misleading recommendations.
        #[arg(long, default_value = "3840x2160")]
        resolution: String,
        /// Encoder for the synthetic fixtures. `libx265` matches GoPro HEVC.
        #[arg(long, default_value = "libx265")]
        codec: String,
        /// Persist the recommendation to your user settings file so future
        /// `vibecut run` invocations pick it up without an env var.
        #[arg(long)]
        apply: bool,
        /// Regenerate fixtures even if they already exist on disk.
        #[arg(long)]
        force: bool,
    },
    /// Cross-correlate the audio tracks of two files to recover the time
    /// offset between them — for syncing multiple cameras of the same scene.
    /// Reports `b started X.XXs after/before a` with a confidence in [0,1].
    Sync {
        file_a: Utf8PathBuf,
        file_b: Utf8PathBuf,
        /// Where to start sampling within `file_a` (seconds).
        #[arg(long, default_value_t = 0.0)]
        start_a: f32,
        /// Where to start sampling within `file_b` (seconds).
        #[arg(long, default_value_t = 0.0)]
        start_b: f32,
        /// Window length (seconds) — longer = more confidence.
        #[arg(long, default_value_t = 30.0)]
        window: f32,
        /// Maximum |offset| to search (seconds).
        #[arg(long, default_value_t = 60.0)]
        max_lag: f32,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.log.as_deref());

    match cli.command {
        Cmd::Probe { path, json } => probe_cmd(&path, json),
        Cmd::Run { config, dry_run, no_visual, no_cache, explain, print_effective_config, random_seed } => {
            vibecut::run_pipeline::run_cmd(&config, dry_run, no_visual, no_cache, explain, print_effective_config, random_seed)
        }
        Cmd::Validate { config } => vibecut::run_pipeline::run_cmd(&config, true, false, false, false, false, false),
        Cmd::Gpmf { file, max_payloads, save_raw } => gpmf_cmd(&file, max_payloads, save_raw.as_deref()),
        Cmd::Beats { file, max_print } => beats_cmd(&file, max_print),
        Cmd::Sync { file_a, file_b, start_a, start_b, window, max_lag } => {
            sync_cmd(&file_a, &file_b, start_a, start_b, window, max_lag)
        }
        Cmd::Benchmark {
            threads, repeats, num_files, duration_secs, resolution, codec, apply, force,
        } => vibecut::benchmark::run(vibecut::benchmark::BenchmarkArgs {
            threads,
            repeats,
            num_files,
            duration_secs,
            resolution,
            codec,
            apply,
            force_fixtures: force,
        }),
    }
}

fn sync_cmd(
    file_a: &Utf8Path,
    file_b: &Utf8Path,
    start_a: f32,
    start_b: f32,
    window: f32,
    max_lag: f32,
) -> Result<()> {
    if !file_a.exists() {
        anyhow::bail!("file does not exist: {file_a}");
    }
    if !file_b.exists() {
        anyhow::bail!("file does not exist: {file_b}");
    }
    println!("audio cross-correlation:");
    println!("  a:      {file_a}  (start {start_a:.2}s)");
    println!("  b:      {file_b}  (start {start_b:.2}s)");
    println!("  window: {window:.2}s   max-lag: ±{max_lag:.2}s");
    let started = std::time::Instant::now();
    let r = vibecut::audio_analysis::sync_two_files(file_a, file_b, start_a, start_b, window, max_lag)
        .with_context(|| "audio sync")?;
    let dt = started.elapsed().as_secs_f32();
    println!("\n  offset:     {:+.4}s  ({})", r.offset_secs,
        if r.offset_secs > 0.0 { "b started LATER" }
        else if r.offset_secs < 0.0 { "b started EARLIER" }
        else { "perfectly aligned" });
    println!("  confidence: {:.3}  ({})", r.confidence,
        if r.confidence > 0.7 { "strong" }
        else if r.confidence > 0.3 { "moderate" }
        else { "weak — possibly mismatched audio" });
    println!("  time:       {dt:.2}s");
    Ok(())
}

fn beats_cmd(file: &Utf8Path, max_print: usize) -> Result<()> {
    if !file.exists() {
        anyhow::bail!("file does not exist: {file}");
    }
    let started = std::time::Instant::now();
    let analyzed = vibecut::audio_analysis::analyze_file(file)
        .with_context(|| format!("analyzing {file}"))?;
    let elapsed = started.elapsed();

    println!("{file}");
    println!("  duration:    {:.2}s", analyzed.duration_secs);
    println!("  sample rate: {} Hz (analyzer)", analyzed.sample_rate);
    println!("  tempo:       {:.1} BPM (beat interval {:.3}s)",
        analyzed.tempo_bpm, analyzed.beat_interval_secs());
    println!("  beats:       {} detected", analyzed.beats.len());

    let (raw_s, raw_e) = analyzed.detect_loud_region(0.80);
    let snap_s = analyzed.beat_at_or_after(raw_s);
    let snap_e = analyzed.beat_at_or_before(raw_e);
    println!(
        "  loud region: raw [{:.2}s..{:.2}s]  beat-snapped [{:.2}s..{:.2}s] = {:.2}s loop length",
        raw_s, raw_e, snap_s, snap_e, (snap_e - snap_s).max(0.0)
    );

    let preview: Vec<String> = analyzed
        .beats
        .iter()
        .take(max_print)
        .map(|t| format!("{t:.3}"))
        .collect();
    if !preview.is_empty() {
        println!("    {}", preview.join("  "));
        if analyzed.beats.len() > max_print {
            println!("    … +{} more", analyzed.beats.len() - max_print);
        }
    }

    let energy_peak = analyzed
        .energy_curve
        .iter()
        .cloned()
        .fold(0.0_f32, f32::max);
    let energy_mean: f32 =
        analyzed.energy_curve.iter().sum::<f32>() / analyzed.energy_curve.len().max(1) as f32;
    println!(
        "  energy:      {} bins @ {} Hz · peak={:.3} mean={:.3}",
        analyzed.energy_curve.len(),
        analyzed.energy_rate,
        energy_peak,
        energy_mean
    );
    println!("  analyzed in {:.2}s", elapsed.as_secs_f32());
    Ok(())
}

fn gpmf_cmd(file: &Utf8Path, max_payloads: usize, save_raw: Option<&Utf8Path>) -> Result<()> {
    if !file.exists() {
        anyhow::bail!("file does not exist: {file}");
    }
    let bytes = vibecut::media_probe::extract_gpmf_bytes(file)
        .with_context(|| format!("extracting GPMF from {file}"))?;
    let Some(bytes) = bytes else {
        eprintln!("no GPMF (gpmd) stream in {file}");
        return Ok(());
    };
    println!("GPMF: {} bytes", bytes.len());

    if let Some(out) = save_raw {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(out, &bytes)
            .with_context(|| format!("writing {out}"))?;
        println!("raw GPMF saved to {out}");
    }

    let payload_count = vibecut::gopro_metadata::iter_payloads(&bytes).count();
    println!("top-level DEVC payloads: {payload_count}");
    println!("\n--- tree (first {max_payloads} DEVC payload{}):\n",
        if max_payloads == 1 { "" } else { "s" });
    print!("{}", vibecut::gopro_metadata::format_tree(&bytes, max_payloads));
    Ok(())
}

fn init_tracing(filter: Option<&str>) {
    use tracing_subscriber::{EnvFilter, fmt};
    let env_filter = match filter {
        Some(f) => EnvFilter::new(f),
        None => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
    };
    fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .compact()
        .init();
}

fn probe_cmd(path: &Utf8Path, json: bool) -> Result<()> {
    if !path.exists() {
        anyhow::bail!("path does not exist: {path}");
    }
    let files: Vec<Utf8PathBuf> = if path.is_file() {
        vec![path.to_path_buf()]
    } else {
        let found = vibecut::media_probe::discover(path, &[])
            .with_context(|| format!("discovering files under {path}"))?;
        if found.is_empty() {
            eprintln!("no video files found under {path}");
            return Ok(());
        }
        found
    };

    let mut probed = Vec::with_capacity(files.len());
    for f in &files {
        tracing::info!(file = %f, "probing");
        match vibecut::media_probe::probe_one(f) {
            Ok(p) => probed.push(p),
            Err(e) => tracing::warn!(file = %f, error = %e, "probe failed"),
        }
    }

    if json {
        let s = serde_json::to_string_pretty(&probed)?;
        println!("{s}");
    } else {
        print_probed_table(&probed);
    }
    Ok(())
}

fn print_probed_table(files: &[vibecut::media_probe::ProbedFile]) {
    for f in files {
        println!("{}", f.path);
        println!("  size:  {}", vibecut::run_pipeline::human_bytes(f.size_bytes));
        match &f.blake3 {
            Some(h) => println!("  hash:  {}", vibecut::run_pipeline::short_hash(h)),
            None => println!("  hash:  <not computed>"),
        }
        match (&f.media, &f.probe_error) {
            (Some(m), _) => {
                println!(
                    "  cont:  {}  duration: {}",
                    m.container,
                    vibecut::run_pipeline::fmt_duration(m.duration_secs)
                );
                if let Some(t) = &m.creation_time {
                    println!("  ctime: {t}");
                }
                if let Some(v) = &m.video {
                    println!(
                        "  video: {} {}x{} @ {:.2} fps",
                        v.codec, v.width, v.height, v.fps
                    );
                }
                if let Some(a) = &m.audio {
                    println!(
                        "  audio: {} {}Hz {}ch",
                        a.codec, a.sample_rate, a.channels
                    );
                }
            }
            (None, Some(e)) => {
                println!("  media: <unavailable> ({e})");
            }
            (None, None) => {}
        }
        println!();
    }
}

