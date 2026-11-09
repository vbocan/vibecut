//! Pipeline orchestrator — moved out of `main.rs` so the MCP server (or
//! any other consumer of `vibecut` as a library) can drive the run
//! pipeline in-process. This is what unlocks the `apply_feedback` tool,
//! which needs to mutate a `Config` and re-render without spawning a
//! subprocess.
//!
//! The CLI binary's `Cmd::Run` arm is now a thin call into
//! [`run_cmd`]; everything else here is the same orchestration logic
//! that used to live in `main.rs`.

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

// ---- Pipeline constants ----
/// SplitMix64 stream constant mixed into `[project].seed` to produce an
/// independent RNG stream for speed-ramp coin flips. Chosen to be far from
/// the picker's and burst's seeds so their outputs are uncorrelated.
const RAMP_SEED_MULTIPLIER: u64 = 0xc0ffee_d00d;
/// xfade duration used for hard-cut seams after speed-ramp segments (≈ 3 frames
/// at 30 fps). Long enough to stay above one AAC frame (~21 ms at 48 kHz) so
/// the audio chain doesn't truncate; short enough to feel like a hard cut.
const HARD_CUT_XFADE_SECS: f32 = 0.10;

use crate::progress::{report as progress, Stage};
use crate::project_config::Config;
use crate::selection_planner::{
    DuckWindow, ExtractOpts, Manifest, MANIFEST_SCHEMA_VERSION, Segment, build_score_curve,
    extract_telemetry_with, group_into_sources, parse_resolution, select_duck_windows,
    select_multi_source,
};
use crate::video_renderer::{
    CardSpec, EncoderPref, FfmpegRunner, HwConfig, HwSupport, ImageOverlay, NativeRunner,
    NormalizeArgs, TextOverlay,
};

pub fn run_cmd(
    config_path: &Utf8Path,
    dry_run: bool,
    no_visual: bool,
    no_cache: bool,
    explain: bool,
    print_effective_config: bool,
    random_seed: bool,
) -> Result<()> {
    let mut cfg = Config::load(config_path)
        .with_context(|| format!("loading config {config_path}"))?;

    // `--random-seed` overrides whatever seed the TOML had (or didn't have)
    // with a fresh value derived from the system clock + a SplitMix64 mix.
    // The seed lands in the manifest, so the user can lock it back in via
    // `seed = N` in their TOML if they like this particular cut.
    if random_seed {
        let new_seed = generate_random_seed();
        tracing::info!(seed = new_seed, "random seed (use `seed = {}` in TOML to lock this cut)", new_seed);
        cfg.project.seed = Some(new_seed);
    }

    if print_effective_config {
        // Print merged config (file values + struct defaults) as TOML.
        let s = toml::to_string_pretty(&cfg)
            .context("serializing effective config")?;
        println!("{s}");
        return Ok(());
    }

    println!("config OK · schema v{}", cfg.schema_version);
    println!("  project:        {}", cfg.project.name);
    let (body_w, body_h) = cfg.body_resolution();
    println!("  body render:    {body_w}x{body_h}  ({} output{})", cfg.outputs.len(), plural(cfg.outputs.len()));
    for (i, o) in cfg.outputs.iter().enumerate() {
        println!(
            "    [{i}] {}  {}  enc={:?} crf={} a={}",
            o.path, o.resolution, o.encoder, o.crf, o.audio_bitrate
        );
    }
    println!(
        "  target:         {}s @ {} fps",
        cfg.project.target_duration.as_secs_f32(),
        cfg.project.fps,
    );
    let seed_label = if random_seed { " (--random-seed)" } else { "" };
    let variety_label = if cfg.selection.variety > 0.0 {
        format!(" · variety = {:.2}", cfg.selection.variety)
    } else {
        String::new()
    };
    println!(
        "  seed:           {}{}{}",
        cfg.project.seed.map(|s| s.to_string()).unwrap_or_else(|| "(none → 0)".into()),
        seed_label,
        variety_label,
    );
    if let Some(f) = &cfg.input.folder {
        println!("  input folder:   {f}");
        println!("  include:        {}", cfg.input.include.join(", "));
    }
    if !cfg.input.videos.is_empty() {
        println!("  explicit videos: {}", cfg.input.videos.len());
    }
    if let Some(s) = &cfg.soundtrack {
        let names: Vec<String> = s.files.iter().map(|f| f.file_name().unwrap_or("?").to_string()).collect();
        println!("  soundtrack:     [{}] (beat_align={})", names.join(", "), s.beat_align);
    }
    if cfg.multi_angle.enabled {
        println!("  multi-angle:    sync={:?}", cfg.multi_angle.sync);
    }
    if !cfg.texts.is_empty() {
        println!(
            "  text overlays:  {} (not yet rendered)",
            cfg.texts.len()
        );
    }

    if dry_run {
        println!("\n(dry-run: pipeline not executed; only schema validated)");
        return Ok(());
    }

    execute_pipeline(&cfg, no_visual, no_cache, explain)
}

fn execute_pipeline(cfg: &Config, no_visual: bool, no_cache: bool, explain: bool) -> Result<()> {
    let runner = NativeRunner::new();
    let v = runner
        .version()
        .context("ffmpeg is required to render — install it (winget install Gyan.FFmpeg) and retry")?;
    tracing::info!(version = %v, "ffmpeg ok");

    // Detect hardware acceleration once.
    let support = HwSupport::detect("ffmpeg")
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "hwaccel detection failed; falling back to software");
            HwSupport::default()
        });
    // Body uses the first output's encoder preference. Each output then
    // re-resolves its own encoder during the per-output transcode pass below.
    let pref = encoder_pref_from_cfg(&cfg.outputs[0].encoder);
    let hw = HwConfig::resolve(support, pref);
    println!("hardware accel: {} → encoder = {}", hw.support.summary(), hw.encoder.name());

    // Body render uses CRF chosen for high quality if we'll re-transcode to
    // multiple outputs (avoids generation-loss artifacts), otherwise the
    // single output's configured CRF.
    let body_crf: u32 = if cfg.outputs.len() == 1 {
        cfg.outputs[0].crf
    } else {
        cfg.outputs[0].crf.min(18)
    };

    // 1. Probe inputs.
    progress(Stage::Probing, 0.0, None);
    // Collect video files from the optional folder scan plus any explicit
    // [[input.videos]] entries. Duplicates (same canonical path) are dropped.
    let mut files: Vec<Utf8PathBuf> = Vec::new();
    if let Some(folder) = &cfg.input.folder {
        if !folder.as_std_path().exists() {
            anyhow::bail!("input.folder does not exist: {folder}");
        }
        let scanned = crate::media_probe::discover(folder, &cfg.input.include)
            .with_context(|| format!("discovering files under {folder}"))?;
        files.extend(scanned);
    }
    for v in &cfg.input.videos {
        if !v.path.as_std_path().exists() {
            anyhow::bail!("input.videos path does not exist: {}", v.path);
        }
        if !files.contains(&v.path) {
            files.push(v.path.clone());
        }
    }
    if files.is_empty() && cfg.input.images.is_empty() {
        anyhow::bail!(
            "no inputs found — set [input] folder, add [[input.videos]] entries, \
             or add [[input.images]] entries"
        );
    }
    if !files.is_empty() {
        tracing::info!(count = files.len(), "probing inputs");
    }
    // Metadata-only probe (no hashing) — orders of magnitude faster on large
    // sources. Hashes will be filled in by the cache layer.
    let mut probed = Vec::with_capacity(files.len());
    for f in &files {
        let p = crate::media_probe::probe_metadata(f).with_context(|| format!("probing {f}"))?;
        if p.media.is_none() {
            anyhow::bail!(
                "ffprobe failed for {f}: {}",
                p.probe_error.unwrap_or_default()
            );
        }
        probed.push(p);
    }

    // Build a per-chapter audio-presence map so the normalize stage can
    // synthesise a silent stereo track when the source has no audio (mic-off
    // GoPro recordings, some HERO5/6 captures). Without this, downstream
    // `acrossfade` chains in `concat_with_xfade` fail because some inputs
    // would have no audio stream.
    let chapter_has_audio: std::collections::HashMap<Utf8PathBuf, bool> = probed
        .iter()
        .map(|p| {
            let has = p.media.as_ref().and_then(|m| m.audio.as_ref()).is_some();
            (p.path.clone(), has)
        })
        .collect();

    // 2. Group into sources (chapter-stitching).
    let sources = group_into_sources(&probed);
    if !sources.is_empty() {
        println!("\nsources discovered:");
        for s in &sources {
            println!(
                "  · id={:<6} chapters={}  total={}",
                s.id,
                s.chapters.len(),
                fmt_duration(s.total_duration_secs)
            );
        }
    }

    // Process explicit [[input.images]] entries.
    // Images are treated as *mandatory* segments that bypass the score-based
    // picker. They are always included in the output, interleaved with the
    // auto-selected video clips. The picker's budget is reduced accordingly.
    let image_effect_map: std::collections::HashMap<
        Utf8PathBuf,
        crate::project_config::ImageEffect,
    > = cfg
        .input
        .images
        .iter()
        .map(|img| (img.path.clone(), img.effect))
        .collect();

    let image_segments: Vec<Segment> = cfg
        .input
        .images
        .iter()
        .enumerate()
        .map(|(i, img)| {
            let stem = img
                .path
                .file_stem()
                .unwrap_or("photo")
                .to_string();
            Segment {
                source_id: format!("img:{stem}"),
                chapter_path: img.path.clone(),
                chapter_index: 1,
                in_chapter_start_secs: 0.0,
                duration_secs: img.duration.as_secs_f64(),
                // virtual_start_secs is used as a unique ordering key so images
                // stay in declaration order when chronological sort is applied.
                virtual_start_secs: img.duration.as_secs_f64() * i as f64,
                reason: format!("image (effect={:?})", img.effect),
                score: Some(0.5),
            }
        })
        .collect();

    let image_total_dur: f64 = image_segments.iter().map(|s| s.duration_secs).sum();

    if !image_segments.is_empty() {
        println!(
            "\nimage inputs: {} image(s)  total={}",
            image_segments.len(),
            fmt_duration(image_total_dur)
        );
        for (i, img) in cfg.input.images.iter().enumerate() {
            println!(
                "  [{}] {}  dur={:.1}s  effect={:?}",
                i + 1,
                img.path.file_name().unwrap_or("?"),
                img.duration.as_secs_f64(),
                img.effect,
            );
        }
    }

    // 3. If a soundtrack is configured, analyze it now so we can beat-align
    //    clip lengths and the total output duration.
    let mut target = cfg.project.target_duration.as_secs_f64();
    let mut clip_len = clip_length(cfg);
    let mut audio: Option<crate::audio_analysis::AnalyzedAudio> = None;
    // Resolved soundtrack path (after concat if multiple files were given).
    // Computed early so the analysis stage and the mux stage agree.
    let workspace_pre = cfg.workspace_dir();
    let mut soundtrack_resolved: Option<Utf8PathBuf> = None;
    if let Some(s) = &cfg.soundtrack {
        if s.files.is_empty() {
            anyhow::bail!("[soundtrack] is present but no `file`/`files` given");
        }
        for f in &s.files {
            if !f.as_std_path().exists() {
                anyhow::bail!("soundtrack file does not exist: {f}");
            }
        }
        let resolved = if s.files.len() == 1 {
            s.files[0].clone()
        } else {
            std::fs::create_dir_all(&workspace_pre).ok();
            let combined = workspace_pre.join("soundtrack_combined.m4a");
            println!("\nconcatenating {} soundtrack files → {combined}", s.files.len());
            crate::audio_analysis::concat_soundtrack_files(&s.files, &combined)
                .context("concatenating soundtrack files")?
        };
        progress(Stage::Music, 0.0, None);
        println!("\nanalyzing soundtrack {resolved}...");
        let started = std::time::Instant::now();
        let a = crate::audio_analysis::analyze_file(&resolved)
            .with_context(|| format!("analyzing soundtrack {resolved}"))?;
        soundtrack_resolved = Some(resolved);
        println!(
            "  tempo: {:.2} BPM (beat interval {:.4}s)  beats: {}  ({:.1}s)",
            a.tempo_bpm,
            a.beat_interval_secs(),
            a.beats.len(),
            started.elapsed().as_secs_f32()
        );
        match &a.beat_grid {
            Some(g) => println!("  beat grid: {}", describe_beat_grid(g)),
            None if a.tempo_bpm > 0.0 => {
                println!("  beat grid: too few clear onsets to fit; using the raw tempo")
            }
            None => {}
        }

        if s.beat_align && a.tempo_bpm > 0.0 {
            let bi = a.beat_interval_secs() as f64;
            let min_clip = cfg.selection.min_clip.as_secs_f64();
            let max_clip = cfg.selection.max_clip.as_secs_f64();
            let snapped = crate::audio_analysis::snap_clip_length_to_beats(
                clip_len as f32,
                min_clip as f32,
                max_clip as f32,
                bi as f32,
            ) as f64;
            let (k, total) = crate::audio_analysis::choose_clip_count(target as f32, snapped as f32);
            println!(
                "  beat-aligned: clip_len {:.2}s ({} beats), {} clips, total {:.2}s",
                snapped,
                (snapped / bi).round() as u32,
                k,
                total
            );
            clip_len = snapped;
            target = total as f64;
        }
        audio = Some(a);
    }

    // 4. Multi-source diverse picker over all sources.
    let _ = &audio; // (held for the manifest in a later phase)

    let want_visual = !no_visual
        && (cfg.scoring.motion_peaks > 0.0
            || cfg.scoring.sharpness_floor > 0.0
            || cfg.scoring.exposure_floor > 0.0);
    // Cache directory. Default location is alongside the primary output.
    let cache_dir = cfg
        .primary_output_path()
        .parent()
        .map(|p: &Utf8Path| p.join(".vibecut-cache"))
        .unwrap_or_else(|| Utf8PathBuf::from(".vibecut-cache"));
    let cache_enabled = !no_cache;
    let cache = crate::disk_cache::Cache::open(&cache_dir, cache_enabled)
        .with_context(|| format!("opening cache at {cache_dir}"))?;
    if cache_enabled {
        tracing::info!(dir = %cache_dir, "visual analysis cache enabled");
    }

    let visual_hwaccel_cuda = hw.use_cuda_decode || hw.support.cuda_hwaccel;
    // GPU-side downscale only kicks in if the ffmpeg binary actually has
    // `scale_cuda` (probed by HwSupport::detect from the `-filters` output).
    // Falls back transparently otherwise.
    let visual_use_scale_cuda = visual_hwaccel_cuda && hw.support.scale_cuda;
    if visual_use_scale_cuda {
        tracing::info!("visual analysis: GPU-resident chain (scale_cuda + hwdownload)");
    }
    let extract_opts = ExtractOpts {
        with_visual: want_visual,
        visual_hwaccel_cuda,
        visual_use_scale_cuda,
        cache: if cache_enabled { Some(cache.clone()) } else { None },
        ..ExtractOpts::default()
    };
    // Concurrency cap for the per-source ffmpeg fan-out below. NVDEC supports
    // multiple concurrent decode contexts (≥4 on Ada/Blackwell); CPU-only
    // decode also benefits up to ~num-cores. The cap is resolved with this
    // priority (highest first) by `user_settings::resolve_visual_workers`:
    //   1. `VIBECUT_VISUAL_WORKERS` env var.
    //   2. `visual_workers` in `<config_dir>/vibecut/settings.toml`,
    //      written by `vibecut benchmark --apply`.
    //   3. Conservative default 4 so we don't oversubscribe a 1-NVDEC GPU
    //      or HDD-backed source.
    let cap = crate::user_settings::resolve_visual_workers();
    let n_workers = sources.len().min(cap).max(1);

    let telemetries: Vec<crate::selection_planner::SourceTelemetry>;
    let mut curves: Vec<crate::selection_scoring::ScoreCurve>;

    if sources.is_empty() {
        // Images-only project: no video sources to analyse.
        telemetries = Vec::new();
        curves = Vec::new();
    } else {
        let n_sources = sources.len();
        progress(Stage::Analysing, 0.0, Some(format!("0 of {n_sources} clips")));
        let analysis_reporter = crate::progress::current();
        let analysed = std::sync::atomic::AtomicUsize::new(0);
        println!(
            "\nextracting telemetry from {} source{}{}  (workers={})...",
            sources.len(),
            plural(sources.len()),
            if want_visual { "  (incl. visual)" } else { "" },
            n_workers,
        );
        let started = std::time::Instant::now();

        // Per-source extraction is independent (each spawns its own ffmpeg
        // subprocess and writes its own cache file). Run them in parallel with
        // a bounded thread pool so the GPU/SSD don't get oversubscribed.
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(n_workers)
            .build()
            .context("building visual-analysis thread pool")?;
        use rayon::prelude::*;
        let pairs: Vec<(crate::selection_planner::SourceTelemetry, crate::selection_scoring::ScoreCurve)> =
            pool.install(|| {
                sources
                    .par_iter()
                    .map(|source| {
                        let tel = extract_telemetry_with(source, &extract_opts);
                        let curve = build_score_curve(&tel, &cfg.scoring, 4.0);
                        let done = analysed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        if let Some(r) = &analysis_reporter {
                            r.report(
                                Stage::Analysing,
                                done as f32 / n_sources as f32,
                                Some(format!("{done} of {n_sources} clips")),
                            );
                        }
                        (tel, curve)
                    })
                    .collect()
            });
        let (tels, crvs): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();

        // Print per-source summary in source order so logs stay readable, even
        // though work happened out-of-order.
        for (source, (tel, curve)) in sources.iter().zip(tels.iter().zip(crvs.iter())) {
            let peak = curve.samples.iter().cloned().fold(0.0_f32, f32::max);
            let mean = curve.samples.iter().sum::<f32>() / curve.samples.len().max(1) as f32;
            let vis_extra = if want_visual {
                let avg_sharp = if !tel.visual_sharpness.is_empty() {
                    tel.visual_sharpness.iter().sum::<f32>() / tel.visual_sharpness.len() as f32
                } else { 0.0 };
                let avg_expo = if !tel.visual_exposure.is_empty() {
                    tel.visual_exposure.iter().sum::<f32>() / tel.visual_exposure.len() as f32
                } else { 0.0 };
                format!("  vis_frames={}  sharp_avg={:.0}  expo_avg={:.2}",
                    tel.visual_sharpness.len(), avg_sharp, avg_expo)
            } else { String::new() };
            println!(
                "  · {:<8}  hilights={}  accel={}  curve peak={:.3} mean={:.3}{vis_extra}",
                source.id,
                tel.hilights_secs.len(),
                tel.accel_mag.len(),
                peak,
                mean,
            );
        }
        let visual_total_secs = started.elapsed().as_secs_f32();
        println!("  total: {:.1}s", visual_total_secs);
        // Machine-readable benchmark marker. Stable format for tools that sweep
        // VIBECUT_VISUAL_WORKERS. Always printed, regardless of whether visual
        // analysis actually ran — `phase=telemetry` covers GPMF + visual together.
        println!(
            "[bench] phase=telemetry workers={} sources={} secs={:.3}",
            n_workers,
            sources.len(),
            visual_total_secs
        );

        telemetries = tels;
        progress(Stage::Picking, 0.0, None);
        curves = crvs;
    }

    // Multi-camera angle planning. Discover groups of overlapping
    // sources, compute per-member offsets, and reduce each group's member
    // curves to one combined curve in master scene-time. The picker then
    // operates on that combined curve under the master id; angle selection
    // for each pick happens after.
    let angle_groups: Vec<crate::multi_angle::AngleGroup> = if cfg.multi_angle.enabled {
        let mut groups = crate::multi_angle::discover_groups(&sources, &cfg.multi_angle, 1.0);
        if groups.is_empty() {
            tracing::info!("multi_angle.enabled=true but no groups found (no overlapping recording windows)");
        } else {
            // Audio xcorr refinement is opt-in via sync = "audio" or "auto".
            let use_audio = matches!(
                cfg.multi_angle.sync,
                crate::project_config::SyncMethod::Audio | crate::project_config::SyncMethod::Auto
            );
            let refine: Option<fn(&Utf8Path, &Utf8Path, f32, f32) -> Option<(f64, f32)>> =
                if use_audio { Some(audio_xcorr_refine) } else { None };
            crate::multi_angle::compute_offsets(&mut groups, &sources, &cfg.multi_angle, refine);
            println!("\nangle groups discovered: {}", groups.len());
            for g in &groups {
                println!("  · group master={}", g.master_id);
                for m in &g.members {
                    println!(
                        "      {:<10} offset={:+8.3}s  via={}  window=[{:+.2}s..{:+.2}s] in master-time",
                        m.source_id,
                        m.offset_secs,
                        m.offset_source.as_str(),
                        m.start_in_master,
                        m.end_in_master
                    );
                }
            }
            // Substitute the master's curve with the group's combined curve;
            // zero out non-master members' curves so the picker won't draw
            // from them as standalone sources.
            let mut idx_by_id: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
            for (i, s) in sources.iter().enumerate() {
                idx_by_id.insert(s.id.as_str(), i);
            }
            for g in &groups {
                let master_idx = match idx_by_id.get(g.master_id.as_str()) {
                    Some(&i) => i,
                    None => continue,
                };
                let master_dur = sources[master_idx].total_duration_secs as f32;
                let member_curves: Vec<&crate::selection_scoring::ScoreCurve> = g
                    .members
                    .iter()
                    .filter_map(|m| idx_by_id.get(m.source_id.as_str()).map(|&i| &curves[i]))
                    .collect();
                if member_curves.len() != g.members.len() {
                    tracing::warn!(master = %g.master_id, "missing member curves; skipping combine");
                    continue;
                }
                let combined = crate::multi_angle::combine_curves_in_scene_time(
                    g,
                    &member_curves,
                    master_dur,
                    4.0,
                );
                curves[master_idx] = combined;
                for m in g.members.iter().skip(1) {
                    if let Some(&i) = idx_by_id.get(m.source_id.as_str()) {
                        for v in curves[i].samples.iter_mut() {
                            *v = 0.0;
                        }
                    }
                }
            }
        }
        groups
    } else {
        Vec::new()
    };

    // Pre-compute intro/outro/xfade durations (the rendering happens later
    // but the durations are known up front and feed into K computation).
    let intro_dur: f32 = cfg.intro.as_ref().map(|i| i.duration.as_secs_f32()).unwrap_or(0.0);
    let outro_dur: f32 = cfg.outro.as_ref().map(|o| o.duration.as_secs_f32()).unwrap_or(0.0);
    let trans_dur: f32 = cfg.transition.duration.as_secs_f32();
    let use_transitions = (intro_dur > 0.0 || outro_dur > 0.0 || trans_dur > 0.0)
        && cfg.transition.default != "cut"
        && cfg.transition.default != "none";

    // Compute K so the FINAL output duration ≈ target_duration.
    // Output = K × clip_len + intro + outro − (K + n_endcaps − 1) × xfade
    //        = K × (clip_len − xfade) + intro + outro − (n_endcaps − 1) × xfade
    // Solve for K:
    //   K = (target − intro − outro + (n_endcaps − 1) × xfade) / (clip_len − xfade)
    // Then we pass `K × clip_len` to select_multi_source (which will divide
    // it back out the same way to get K). This collapses to K = target / clip_len
    // when there are no intro/outro/xfade contributions.
    let intro_p: f64 = if intro_dur > 0.0 { 1.0 } else { 0.0 };
    let outro_p: f64 = if outro_dur > 0.0 { 1.0 } else { 0.0 };
    let xfade = if use_transitions { trans_dur as f64 } else { 0.0 };
    let n_endcaps = intro_p + outro_p;
    // Subtract image segment durations from the picker's budget so the total
    // output length stays close to `target_duration` even with images added.
    let video_target = (target - image_total_dur).max(clip_len);
    // With beat-locked cuts (see the beat-lock pass below), each clip runs on
    // through half of each crossfade so the span between its cuts is its
    // full planned length; the crossfades no longer eat into the cut, so:
    //   Output ≈ intro + K × clip_len + outro
    let beat_locked = cfg.soundtrack.as_ref().is_some_and(|s| s.beat_align)
        && audio.as_ref().is_some_and(|a| a.tempo_bpm > 0.0 && !a.beats.is_empty());
    let inflated_target = if beat_locked {
        ((video_target - intro_dur as f64 - outro_dur as f64) / clip_len)
            .round()
            .max(1.0)
            * clip_len
    } else if (clip_len - xfade) > 1e-3 {
        let k_for_output = ((video_target - intro_dur as f64 - outro_dur as f64
            + (n_endcaps - 1.0).max(0.0) * xfade)
            / (clip_len - xfade))
            .round()
            .max(1.0);
        k_for_output * clip_len
    } else {
        video_target
    };
    // Apply [[selection.source_weights]] BEFORE excludes.
    // Last-write-wins on duplicate source_ids; weight is clamped to
    // [0, 10]. weight = 0 effectively drops the source from picks
    // (every score becomes 0 → picker won't choose it).
    if !cfg.selection.source_weights.is_empty() {
        use std::collections::HashMap;
        let mut weights: HashMap<&str, f32> = HashMap::new();
        for w in &cfg.selection.source_weights {
            weights.insert(w.source_id.as_str(), w.weight.clamp(0.0, 10.0));
        }
        let mut applied = 0usize;
        for (src, curve) in sources.iter().zip(curves.iter_mut()) {
            if let Some(&w) = weights.get(src.id.as_str()) {
                curve.scale(w);
                applied += 1;
            }
        }
        if applied > 0 {
            println!(
                "selection.source_weights: scaled {} of {} source curves",
                applied,
                sources.len()
            );
        }
    }

    // Apply [[selection.exclude]] entries by zeroing the
    // matching ranges in each source's score curve. Each entry matches
    // chapter files by case-insensitive basename suffix, so a feedback
    // action that wrote `file = "GH010320.MP4"` lines up with a source
    // chapter whose absolute path is `E:/.../GH010320.MP4`. After this
    // step, `select_multi_source` naturally avoids excluded windows
    // because their score is 0.
    if !cfg.selection.exclude.is_empty() {
        let mut applied = 0usize;
        for excl in &cfg.selection.exclude {
            let lo = excl.start.as_secs_f32();
            let hi = excl.end.as_secs_f32();
            if !(hi > lo) {
                tracing::warn!(file = %excl.file, "selection.exclude entry has end <= start; skipping");
                continue;
            }
            let filter_lc = excl.file.to_ascii_lowercase();
            for (src, curve) in sources.iter().zip(curves.iter_mut()) {
                let mut chapter_offset = 0.0_f32;
                for ch in &src.chapters {
                    let basename = ch
                        .path
                        .file_name()
                        .map(|n| n.to_ascii_lowercase())
                        .unwrap_or_default();
                    let matches =
                        basename == filter_lc || basename.ends_with(&filter_lc);
                    if matches {
                        curve.zero_range(chapter_offset + lo, chapter_offset + hi);
                        applied += 1;
                    }
                    chapter_offset += ch.duration_secs as f32;
                }
            }
        }
        println!(
            "\nselection.exclude: applied {} chapter range zero(s) from {} entry(ies)",
            applied,
            cfg.selection.exclude.len()
        );
    }

    let mut segments = select_multi_source(
        &sources,
        &curves,
        inflated_target,
        clip_len,
        cfg.selection.max_per_source,
        cfg.selection.variety,
        cfg.project.seed.unwrap_or(0),
    );

    // For each segment whose source_id is a group master, pick the
    // best angle to actually display. Rewrites the segment's source/chapter/
    // offset to point at the chosen member. Honors `min_hold` to prevent
    // bouncing between angles too quickly.
    if !angle_groups.is_empty() {
        use std::collections::HashMap;
        let mut tels_by_id: HashMap<&str, &crate::selection_planner::SourceTelemetry> = HashMap::new();
        for (s, t) in sources.iter().zip(telemetries.iter()) {
            tels_by_id.insert(s.id.as_str(), t);
        }
        let min_hold = cfg.multi_angle.min_hold.as_secs_f64();
        // (group_master_id, last_chosen_member_id, last_t_master)
        let mut prev_per_group: HashMap<String, (String, f64)> = HashMap::new();
        let mut switched = 0u32;
        let mut kept = 0u32;
        for seg in &mut segments {
            let Some(group) = angle_groups.iter().find(|g| g.master_id == seg.source_id) else {
                continue;
            };
            let t_center = seg.virtual_start_secs + seg.duration_secs * 0.5;
            let mem_tels: Vec<&crate::selection_planner::SourceTelemetry> = group
                .members
                .iter()
                .map(|m| {
                    *tels_by_id
                        .get(m.source_id.as_str())
                        .expect("telemetry must exist for every group member")
                })
                .collect();
            let prev = prev_per_group
                .get(&group.master_id)
                .map(|(id, t)| (id.as_str(), *t));
            let choice = crate::multi_angle::pick_best_angle_for(
                group,
                &mem_tels,
                t_center,
                prev,
                min_hold,
                0.10, // break-margin: alternative must beat current angle by 10% to switch within min_hold
            );
            prev_per_group.insert(group.master_id.clone(), (choice.member_id.clone(), t_center));

            if choice.member_id == group.master_id {
                seg.reason = format!("{} [angle=master:{}]", seg.reason, group.master_id);
                kept += 1;
                continue;
            }
            // Rewrite segment to use the chosen member's chapter+offset.
            let member = &group.members[choice.member_index];
            let Some(member_src) = sources.iter().find(|s| s.id == choice.member_id) else {
                continue;
            };
            let member_virtual_start = seg.virtual_start_secs + member.offset_secs;
            if let Some(new_seg) = crate::selection_planner::snap_to_chapter(
                member_src,
                member_virtual_start,
                seg.duration_secs,
            ) {
                seg.source_id = new_seg.source_id;
                seg.chapter_path = new_seg.chapter_path;
                seg.chapter_index = new_seg.chapter_index;
                seg.in_chapter_start_secs = new_seg.in_chapter_start_secs;
                // Keep virtual_start_secs in master scene-time so chronological
                // sort across groups still works.
                seg.reason = format!(
                    "{} [angle={} via offset {:+.2}s]",
                    seg.reason, choice.member_id, member.offset_secs
                );
                switched += 1;
            } else {
                tracing::warn!(
                    member = %choice.member_id,
                    t = member_virtual_start,
                    "could not snap chosen angle's segment to a chapter; keeping master pick"
                );
                seg.reason = format!("{} [angle=master:{} (snap_failed)]", seg.reason, group.master_id);
                kept += 1;
            }
        }
        if switched + kept > 0 {
            println!(
                "\nangle decisions: {} switched to alternate angle, {} kept on master",
                switched, kept
            );
        }
    }

    // Inject `[[input.ranges]]` as mandatory picks. Each range identifies a
    // file by its filename suffix; we resolve it to a source + chapter and
    // create a Segment that bypasses scoring. Used for clips the picker would
    // otherwise skip (e.g. phone videos with no GoPro telemetry).
    let mut forced_added = 0usize;
    for r in &cfg.input.ranges {
        let needle = r.file.as_str();
        let Some(matching) = sources.iter().find_map(|s| {
            s.chapters.iter().find_map(|ch| {
                let name = ch.path.file_name().unwrap_or("");
                if name.eq_ignore_ascii_case(needle) || name.ends_with(needle) {
                    Some((s, ch))
                } else {
                    None
                }
            })
        }) else {
            tracing::warn!(file = needle, "input.ranges: no source matched; range skipped");
            continue;
        };
        let (src, ch) = matching;
        let start = r.start.as_secs_f64();
        let end = r.end.as_secs_f64();
        let dur = (end - start).max(0.0);
        if dur <= 0.0 {
            continue;
        }
        if start >= ch.duration_secs {
            tracing::warn!(file = needle, start, chapter_dur = ch.duration_secs,
                "input.ranges: start past end of chapter; skipped");
            continue;
        }
        let clamped_end = end.min(ch.duration_secs);
        let clamped_dur = clamped_end - start;
        segments.push(Segment {
            source_id: src.id.clone(),
            chapter_path: ch.path.clone(),
            chapter_index: ch.chapter_index,
            in_chapter_start_secs: start,
            duration_secs: clamped_dur,
            virtual_start_secs: start,
            reason: format!("forced via [[input.ranges]] {start:.2}s..{clamped_end:.2}s"),
            score: Some(1.0),
        });
        forced_added += 1;
    }
    if forced_added > 0 {
        println!("\ninput.ranges: {forced_added} mandatory segment(s) injected");
    }

    // Apply the configured ordering.
    //
    // - Chronological (default) sorts by `creation_time` + in-chapter offset.
    //   Safe for multi-event projects; for overlapping multi-camera sources
    //   it interleaves them correctly.
    // - Random (Fisher-Yates with SplitMix64 seeded by `[project].seed`)
    //   shuffles the picks for music-video pacing where strict chronology
    //   isn't desired.
    // - EnergyArc is reserved in the schema but not yet implemented; falls
    //   back to chronological with a warning.
    use std::collections::HashMap;
    let mut creation_for: HashMap<(String, u32), Option<String>> = HashMap::new();
    for source in &sources {
        for ch in &source.chapters {
            creation_for.insert((source.id.clone(), ch.chapter_index), ch.creation_time.clone());
        }
    }
    // Mirror each image's declared `timestamp` into `creation_for` under the
    // same synthetic source_id used when the image_segments were built above
    // (`img:<file_stem>`, chapter_index = 1). Only needed for the strict
    // chronological merge, but cheap and harmless under any order.
    for img in &cfg.input.images {
        if let Some(ts) = img.timestamp.as_deref() {
            let stem = img
                .path
                .file_stem()
                .unwrap_or("photo")
                .to_string();
            creation_for.insert((format!("img:{stem}"), 1), Some(ts.to_string()));
        }
    }
    let chronological_sort = |segs: &mut Vec<Segment>| {
        segs.sort_by(|a, b| {
            let ka = abs_recording_secs(&creation_for, &a.source_id, a.chapter_index, a.in_chapter_start_secs);
            let kb = abs_recording_secs(&creation_for, &b.source_id, b.chapter_index, b.in_chapter_start_secs);
            ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
        });
    };
    let strict_chrono = matches!(
        cfg.selection.order,
        crate::project_config::Order::StrictChronological
    );
    match cfg.selection.order {
        crate::project_config::Order::Chronological => chronological_sort(&mut segments),
        crate::project_config::Order::EnergyArc => {
            // Center-out zigzag placement: strongest pick at the middle
            // position, second-strongest just to one side, third to the
            // other, alternating outward — produces a triangular score
            // profile across the output timeline.
            arrange_energy_arc(&mut segments);
            println!("\norder: energy_arc (peak in the middle)");
        }
        crate::project_config::Order::StrictChronological => {
            // Merge image segments into the video segments before sorting,
            // then chronologically sort the unified list by recording time.
            segments.extend(image_segments.iter().cloned());
            chronological_sort(&mut segments);
            println!(
                "\norder: strict_chronological ({} video + {} image segments merged by timestamp)",
                segments.len() - image_segments.len(),
                image_segments.len()
            );
        }
    }
    if segments.is_empty() && image_segments.is_empty() {
        anyhow::bail!(
            "selection produced no segments — every source's score curve is empty. \
             Check that ffmpeg can extract gpmd, or lower the scoring weights so even-spaced fallback could apply (planned)."
        );
    }

    // Interleave mandatory image segments with the ordered video segments.
    // Images are distributed at evenly-spaced positions, preserving their
    // declaration order. The ordering that was just applied to video segments
    // is respected — images are simply inserted at fixed intervals.
    //
    // Under StrictChronological the images were already merged before the
    // sort, so this step is skipped.
    let mut segments = if strict_chrono {
        segments
    } else if !image_segments.is_empty() {
        interleave_images_with_videos(image_segments, segments)
    } else {
        segments
    };

    // Insert a beat-aligned rapid-cut burst at the soundtrack's
    // energy peak when the music is suitable. The burst replaces a contiguous
    // slice of normal-paced segments around that output time. Tracked by
    // `burst_range` so the renderer can fold those clips into one stream-copy
    // concat (= internal hard cuts, while the burst as a whole still xfades
    // into and out of its neighbors).
    let mut burst_range: Option<(usize, usize, f32)> = None; // (start, end_exclusive, total_secs)
    if let Some(audio_ref) = audio.as_ref() {
        if let Some(snd) = &cfg.soundtrack {
            // Compute provisional output layout so plan_burst knows where each
            // segment sits in output time.
            let (out_starts, out_durs) =
                compute_segment_output_layout(&segments, intro_dur, use_transitions, trans_dur);
            let total_output =
                compute_total_output(&out_durs, intro_dur, outro_dur, use_transitions, trans_dur);
            if let Some(plan) = crate::beat_burst::plan_burst(
                audio_ref,
                snd,
                &segments,
                &out_starts,
                &out_durs,
                &sources,
                &curves,
                intro_dur,
                total_output,
            ) {
                println!(
                    "\nbeat-burst: {} cuts × {:.3}s @ output_t={:.2}s (every {} beat{})",
                    plan.sub_clips.len(),
                    plan.beat_secs,
                    plan.output_start_secs,
                    plan.beats_per_cut,
                    if plan.beats_per_cut == 1 { "" } else { "s" }
                );
                let total_burst_secs: f32 =
                    plan.sub_clips.iter().map(|c| c.duration_secs as f32).sum();
                // Splice: remove the M existing segments, insert N burst sub-clips.
                let new_segments: Vec<Segment> = plan
                    .sub_clips
                    .iter()
                    .enumerate()
                    .map(|(i, c)| Segment {
                        source_id: c.source_id.clone(),
                        chapter_path: c.chapter_path.clone(),
                        chapter_index: c.chapter_index,
                        in_chapter_start_secs: c.in_chapter_start_secs,
                        duration_secs: c.duration_secs,
                        virtual_start_secs: c.virtual_start_secs,
                        reason: format!(
                            "beat-burst {}/{}  score={:.3}",
                            i + 1,
                            plan.sub_clips.len(),
                            c.score
                        ),
                        score: Some(c.score),
                    })
                    .collect();
                let start = plan.replace_at;
                let end = (plan.replace_at + plan.replace_count).min(segments.len());
                segments.splice(start..end, new_segments);
                burst_range = Some((start, start + plan.sub_clips.len(), total_burst_secs));
            } else {
                // Surface the verdict briefly when off-by-criterion (helps users tune).
                let verdict = crate::beat_burst::check_suitability(audio_ref);
                if !verdict.is_suitable() {
                    let reason = match verdict {
                        crate::beat_burst::SuitabilityVerdict::SkippedTempo { bpm } => {
                            format!("tempo {bpm:.1} BPM outside [80, 180]")
                        }
                        crate::beat_burst::SuitabilityVerdict::SkippedPulseStrength {
                            pulse_strength,
                        } => format!("pulse_strength {pulse_strength:.2} < 1.8"),
                        crate::beat_burst::SuitabilityVerdict::SkippedLoudTooShort {
                            loud_secs,
                        } => format!("loud region {loud_secs:.1}s < 6s"),
                        _ => "unknown".into(),
                    };
                    if !matches!(snd.beat_burst, crate::project_config::BeatBurstMode::Off) {
                        println!("\nbeat-burst: skipped — {reason}");
                    }
                }
            }
        }
    }

    println!(
        "\nselected {} segments of ~{:.2}s each (target={:.1}s):",
        segments.len(),
        clip_len,
        target
    );
    for (i, s) in segments.iter().enumerate() {
        println!(
            "  [{:02}] src={:<6} vt={:>7.2}s  ch{}  +{:>6.2}s  · {}  · {}",
            i + 1,
            s.source_id,
            s.virtual_start_secs,
            s.chapter_index,
            s.in_chapter_start_secs,
            s.chapter_path.file_name().unwrap_or("?"),
            s.reason
        );
    }

    // 5. Render. Body is rendered at the highest resolution among all outputs;
    // the per-output transcode pass downscales/copies to each target.
    let (width, height) = cfg.body_resolution();
    if width == 0 || height == 0 {
        anyhow::bail!("could not parse any [[outputs]].resolution as WIDTHxHEIGHT");
    }
    let workspace = cfg.workspace_dir();
    std::fs::create_dir_all(&workspace)
        .with_context(|| format!("creating workspace {workspace}"))?;

    let stabilize = cfg.grading.stabilize;
    let lut3d_path: Option<Utf8PathBuf> = cfg.grading.lut.clone();

    // Per-segment outgoing speed-ramp decision. Each transition between
    // adjacent normal-paced segments may receive a "whip-up" ramp: the last
    // `speed_ramp_duration` of the outgoing clip plays at `speed_ramp_factor`
    // speed before the cut. The decision is keyed by `[project].seed` for
    // reproducibility, and excludes:
    //   - the LAST segment (no transition out)
    //   - synthetic burst segments (`source_id` starts with "burst:") — the
    //     burst is already a rapid-cut sequence; ramping its tail would feel
    //     like double-stuttering
    //   - the segment immediately PRECEDING a burst — its tail already
    //     transitions into a kinetic moment; piling a speed-ramp on top is
    //     visually busy
    //   - segments where the source chapter doesn't have enough headroom
    //     to read the extra footage the ramp needs
    let ramp_prob = cfg.transition.speed_ramp_probability.clamp(0.0, 1.0);
    let ramp_dur_secs = cfg.transition.speed_ramp_duration.as_secs_f32();
    let ramp_factor = cfg.transition.speed_ramp_factor.max(1.01);
    let ramp_flash = cfg.transition.speed_ramp_flash;
    let ramp_extra_input = ramp_dur_secs * (1.0 - 1.0 / ramp_factor);
    let ramp_seed = cfg.project.seed.unwrap_or(0).wrapping_mul(RAMP_SEED_MULTIPLIER);
    let chapter_dur: std::collections::HashMap<&Utf8Path, f64> = sources
        .iter()
        .flat_map(|src| src.chapters.iter().map(|c| (c.path.as_path(), c.duration_secs)))
        .collect();
    // Burst sub-clips (not yet folded here, so identified by index — the
    // "burst:" id only appears once they're folded) and the clip leading
    // into the burst never ramp.
    let burst_span = burst_range;
    let in_burst = move |i: usize| burst_span.is_some_and(|(bs, be, _)| i >= bs && i < be);
    let mut seg_ramps: Vec<Option<crate::video_renderer::SpeedRamp>> = segments
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if ramp_prob <= 0.0 || ramp_dur_secs <= 0.0 || ramp_factor <= 1.0 {
                return None;
            }
            // Last segment has no transition out.
            if i + 1 >= segments.len() {
                return None;
            }
            // Burst itself, or segment right before a burst.
            if in_burst(i) || in_burst(i + 1) {
                return None;
            }
            // Images have no outgoing footage for a speed ramp.
            if crate::media_probe::is_image_path(&s.chapter_path) {
                return None;
            }
            // Source chapter must hold the extra footage we want to read.
            let avail = chapter_dur
                .get(s.chapter_path.as_path())
                .copied()
                .unwrap_or(0.0);
            if s.in_chapter_start_secs + s.duration_secs + ramp_extra_input as f64 > avail {
                return None;
            }
            // Seeded coin flip per segment.
            if crate::rng::coin_f32(ramp_seed, i) < ramp_prob {
                Some(crate::video_renderer::SpeedRamp {
                    duration_secs: ramp_dur_secs,
                    factor: ramp_factor,
                    flash: ramp_flash,
                })
            } else {
                None
            }
        })
        .collect();
    let n_ramped = seg_ramps.iter().filter(|r| r.is_some()).count();
    if ramp_prob > 0.0 {
        println!(
            "\nspeed-ramp: {} of {} transitions ({}%, {:.0}ms @ {:.1}x)",
            n_ramped,
            segments.len().saturating_sub(1),
            (ramp_prob * 100.0) as u32,
            ramp_dur_secs * 1000.0,
            ramp_factor,
        );
    }

    // Beat lock: give every clip the exact length that puts the cut after
    // it on a beat of the soundtrack as it plays in the output (see
    // `beat_lock`). Runs after the ramp decisions (a ramped seam collapses to
    // a near-cut, which moves the cut) and before the per-segment cache keys.
    // `intro_card_len` is the intro card's beat-locked length, when it moved.
    let mut intro_card_len: Option<f64> = None;
    if let (Some(a), Some(snd)) = (audio.as_ref(), cfg.soundtrack.as_ref()) {
        if snd.beat_align && a.tempo_bpm > 0.0 && !a.beats.is_empty() && !segments.is_empty() {
            use crate::beat_lock::{lock_cuts, LockClip, LockStart};
            use crate::project_config::CardMode;
            let fps = cfg.project.fps;
            let xfade_path = use_transitions && trans_dur > 0.0;
            let seam = |w: f32| w.max(crate::video_renderer::MIN_XFADE_SECS) as f64;
            let default_seam = if xfade_path { seam(trans_dur) } else { 0.0 };
            let intro_card = cfg
                .intro
                .as_ref()
                .filter(|c| matches!(c.mode, CardMode::Card))
                .map(|c| c.duration.as_secs_f64());
            let outro_card = cfg.outro.as_ref().is_some_and(|c| matches!(c.mode, CardMode::Card));
            // Beats as they play in the output. The loop layout's prefix
            // doesn't depend on the body length, so any horizon past the end
            // of the cut gives the same beats.
            let planned_total: f64 = segments.iter().map(|s| s.duration_secs).sum();
            let horizon = (intro_card.unwrap_or(0.0) + planned_total * 1.6 + 60.0) as f32;
            let spans = soundtrack_loop_plan(a).spans(horizon, a.duration_secs);
            let beats = crate::beat_sync::beats_in_output(&a.beats, &spans);
            // Cards only join the chain on the crossfade path. The intro
            // card's length moves by at most half a beat so the footage comes
            // in on a beat.
            let start = match intro_card.filter(|_| xfade_path) {
                Some(d) => {
                    let len = crate::beat_lock::lock_card_end(
                        d,
                        default_seam,
                        &beats,
                        a.beat_interval_secs() as f64,
                        fps,
                    );
                    if (len - d).abs() > 1e-6 {
                        intro_card_len = Some(len);
                    }
                    LockStart { output_secs: len - default_seam, seam_in_secs: default_seam }
                }
                None => LockStart { output_secs: 0.0, seam_in_secs: 0.0 },
            };
            let n = segments.len();
            let clips: Vec<LockClip> = segments
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let ramped = seg_ramps[i].is_some();
                    // Must match the concat stage's per-seam widths.
                    let seam_out = if i + 1 < n {
                        Some(if !xfade_path || (in_burst(i) && in_burst(i + 1)) {
                            0.0
                        } else if ramped {
                            seam(HARD_CUT_XFADE_SECS)
                        } else {
                            default_seam
                        })
                    } else if xfade_path && outro_card {
                        Some(default_seam)
                    } else {
                        None
                    };
                    let source_end = if crate::media_probe::is_image_path(&s.chapter_path) {
                        None
                    } else {
                        Some(
                            chapter_dur
                                .get(s.chapter_path.as_path())
                                .copied()
                                .unwrap_or(s.in_chapter_start_secs + s.duration_secs),
                        )
                    };
                    LockClip {
                        planned_secs: s.duration_secs,
                        in_start_secs: s.in_chapter_start_secs,
                        source_end_secs: source_end,
                        tail_extra_secs: if ramped { ramp_extra_input as f64 } else { 0.0 },
                        seam_out_secs: seam_out,
                        seam_out_without_ramp: (ramped && xfade_path).then_some(default_seam),
                    }
                })
                .collect();
            let locked = lock_cuts(&clips, start, &beats, fps);

            let (mut lockable, mut on_beat, mut slid, mut dropped) = (0usize, 0usize, 0usize, 0usize);
            let (mut min_delta, mut max_delta) = (f64::INFINITY, f64::NEG_INFINITY);
            for (i, (s, l)) in segments.iter_mut().zip(&locked).enumerate() {
                if clips[i].seam_out_secs.is_some() {
                    lockable += 1;
                    on_beat += l.on_beat as usize;
                }
                let delta = l.duration_secs - s.duration_secs;
                min_delta = min_delta.min(delta);
                max_delta = max_delta.max(delta);
                let shift = l.in_start_secs - s.in_chapter_start_secs;
                if shift.abs() > 1e-9 {
                    slid += 1;
                    s.in_chapter_start_secs = l.in_start_secs;
                    s.virtual_start_secs += shift;
                }
                s.duration_secs = l.duration_secs;
                if l.drop_ramp {
                    seg_ramps[i] = None;
                    dropped += 1;
                }
            }
            if let Some((bs, be, total)) = burst_range.as_mut() {
                *total = segments[*bs..*be].iter().map(|s| s.duration_secs as f32).sum();
            }
            println!(
                "\nbeat-lock: {on_beat} of {lockable} cut{} placed on the beat (clip lengths {:+.2}…{:+.2}s; {slid} window{} slid earlier, {dropped} speed-ramp{} dropped)",
                plural(lockable),
                min_delta,
                max_delta,
                plural(slid),
                plural(dropped),
            );
            if let (Some(len), Some(d)) = (intro_card_len, intro_card) {
                println!("  intro card {d:.2}s → {len:.3}s so the footage comes in on a beat");
            }
        }
    }

    // Per-segment fingerprint (covers source path + start + duration + render
    // params + ramp settings). Used to name `seg_NNN_HASH.{trf,mp4}` so a
    // re-run with different picker output OR different ramp decisions never
    // collides with a stale cached clip in the same slot.
    let seg_hashes: Vec<String> = segments
        .iter()
        .enumerate()
        .map(|(i, s)| {
            segment_fingerprint(
                s,
                width,
                height,
                cfg.project.fps,
                body_crf,
                lut3d_path.as_deref(),
                stabilize,
                seg_ramps[i],
                image_effect_map.get(&s.chapter_path).copied(),
            )
        })
        .collect();

    if stabilize {
        println!("\nstabilization detect pass on {} segments...", segments.len());
        let started = std::time::Instant::now();
        let mut hits = 0u32;
        for (i, s) in segments.iter().enumerate() {
            // Images are animated by ffmpeg filters at normalize time; no
            // video stream to run vidstab on.
            if crate::media_probe::is_image_path(&s.chapter_path) {
                continue;
            }
            let trf = workspace.join(format!("seg_{:03}_{}.trf", i + 1, seg_hashes[i]));
            // Cache: skip detect if the fingerprinted .trf already exists and
            // is fresher than the source. The fingerprint guarantees the file
            // was produced for this exact (source, window, params) tuple, so
            // mtime is the only remaining check. Delete the workspace dir to
            // force a recompute.
            if trf_is_fresh(&trf, &s.chapter_path) {
                hits += 1;
                continue;
            }
            crate::video_renderer::stabilize_detect(
                &runner,
                &s.chapter_path,
                s.in_chapter_start_secs,
                s.duration_secs,
                width,
                height,
                &trf,
                &hw,
            )
            .with_context(|| format!("stabilize_detect segment {}", i + 1))?;
        }
        if hits > 0 {
            println!(
                "  total: {:.1}s  ({} of {} from cache)",
                started.elapsed().as_secs_f32(),
                hits,
                segments.len()
            );
        } else {
            println!("  total: {:.1}s", started.elapsed().as_secs_f32());
        }
    }
    if let Some(p) = &lut3d_path {
        println!("\napplying LUT: {p}");
    }

    // Determine intro/outro mode FIRST — overlay mode is consumed by the
    // normalize loop below; card mode files are produced after.
    let mut intro_path: Option<Utf8PathBuf> = None;
    let mut intro_dur: f32 = 0.0;
    let mut intro_overlays: Vec<TextOverlay> = Vec::new();
    let mut intro_image_overlays: Vec<ImageOverlay> = Vec::new();
    let mut intro_overlay_dur: f32 = 0.0;
    let mut outro_path: Option<Utf8PathBuf> = None;
    let mut outro_dur: f32 = 0.0;
    let mut outro_overlays: Vec<TextOverlay> = Vec::new();
    let mut outro_image_overlays: Vec<ImageOverlay> = Vec::new();
    let mut outro_overlay_dur: f32 = 0.0;
    // The intro card at its beat-locked length, if the beat lock moved it.
    let intro_cfg: Option<crate::project_config::TextCard> = cfg.intro.clone().map(|mut c| {
        if let Some(len) = intro_card_len {
            c.duration = std::time::Duration::from_secs_f64(len);
        }
        c
    });
    if let Some(intro) = &intro_cfg {
        let dur = intro.duration.as_secs_f32();
        let (texts, images) = card_to_layers(intro);
        match intro.mode {
            crate::project_config::CardMode::Card => {
                let path = workspace.join("intro.mp4");
                println!(
                    "\nrendering intro CARD → {path} ({dur:.2}s, bg={}, {} text, {} image)",
                    intro.bg_color, texts.len(), images.len()
                );
                crate::video_renderer::render_text_card(
                    &runner,
                    &path,
                    &CardSpec {
                        width,
                        height,
                        fps: cfg.project.fps,
                        duration_secs: dur,
                        bg_color_hex: &intro.bg_color,
                        crf: body_crf,
                    },
                    &texts,
                    &images,
                    &hw,
                )
                .context("intro card")?;
                intro_path = Some(path);
                intro_dur = dur;
            }
            crate::project_config::CardMode::Overlay => {
                println!(
                    "\nintro overlay ({} text + {} image on first segment, {dur:.2}s)",
                    texts.len(), images.len()
                );
                intro_overlays = texts;
                intro_image_overlays = images;
                intro_overlay_dur = dur;
            }
        }
    }
    if let Some(outro) = &cfg.outro {
        let dur = outro.duration.as_secs_f32();
        let (texts, images) = card_to_layers(outro);
        match outro.mode {
            crate::project_config::CardMode::Card => {
                let path = workspace.join("outro.mp4");
                println!(
                    "\nrendering outro CARD → {path} ({dur:.2}s, bg={}, {} text, {} image)",
                    outro.bg_color, texts.len(), images.len()
                );
                crate::video_renderer::render_text_card(
                    &runner,
                    &path,
                    &CardSpec {
                        width,
                        height,
                        fps: cfg.project.fps,
                        duration_secs: dur,
                        bg_color_hex: &outro.bg_color,
                        crf: body_crf,
                    },
                    &texts,
                    &images,
                    &hw,
                )
                .context("outro card")?;
                outro_path = Some(path);
                outro_dur = dur;
            }
            crate::project_config::CardMode::Overlay => {
                println!(
                    "\noutro overlay ({} text + {} image on last segment, {dur:.2}s)",
                    texts.len(), images.len()
                );
                outro_overlays = texts;
                outro_image_overlays = images;
                outro_overlay_dur = dur;
            }
        }
    }

    progress(Stage::Clips, 0.0, Some(format!("0 of {} clips", segments.len())));
    println!("\nnormalizing segments → {workspace}");
    let mut normalized: Vec<Utf8PathBuf> = Vec::with_capacity(segments.len());
    let last_idx = segments.len().saturating_sub(1);
    let mut normalize_hits = 0u32;
    for (i, s) in segments.iter().enumerate() {
        let out = workspace.join(format!("seg_{:03}_{}.mp4", i + 1, seg_hashes[i]));
        let trf = if stabilize && !crate::media_probe::is_image_path(&s.chapter_path) {
            Some(workspace.join(format!("seg_{:03}_{}.trf", i + 1, seg_hashes[i])))
        } else {
            None
        };
        // Overlay mode: attach intro layers to seg 0, outro to last seg.
        let mut overlays: Vec<crate::video_renderer::OverlayDirective> = Vec::new();
        let mut image_overlays: Vec<crate::video_renderer::ImageOverlayDirective> = Vec::new();
        if i == 0 {
            for o in &intro_overlays {
                let d = intro_overlay_dur.min(s.duration_secs as f32);
                overlays.push(crate::video_renderer::OverlayDirective {
                    overlay: o,
                    start_secs: 0.0,
                    duration_secs: d,
                });
            }
            for im in &intro_image_overlays {
                let d = intro_overlay_dur.min(s.duration_secs as f32);
                image_overlays.push(crate::video_renderer::ImageOverlayDirective {
                    overlay: im,
                    start_secs: 0.0,
                    duration_secs: d,
                });
            }
        }
        if i == last_idx {
            for o in &outro_overlays {
                let d = outro_overlay_dur.min(s.duration_secs as f32);
                let start = (s.duration_secs as f32 - d).max(0.0);
                overlays.push(crate::video_renderer::OverlayDirective {
                    overlay: o,
                    start_secs: start,
                    duration_secs: d,
                });
            }
            for im in &outro_image_overlays {
                let d = outro_overlay_dur.min(s.duration_secs as f32);
                let start = (s.duration_secs as f32 - d).max(0.0);
                image_overlays.push(crate::video_renderer::ImageOverlayDirective {
                    overlay: im,
                    start_secs: start,
                    duration_secs: d,
                });
            }
        }
        // Cache: skip normalize when the fingerprinted `seg_NNN_HASH.mp4`
        // already exists, is fresher than the source, has the right duration,
        // AND this segment carries no overlays (text or image — neither is
        // baked into the fingerprint, so we re-encode whenever they apply).
        if overlays.is_empty()
            && image_overlays.is_empty()
            && normalized_is_fresh(&out, &s.chapter_path, s.duration_secs)
        {
            normalize_hits += 1;
            normalized.push(out);
            let n = segments.len();
            progress(Stage::Clips, (i + 1) as f32 / n as f32, Some(format!("{} of {n} clips", i + 1)));
            continue;
        }
        let started = std::time::Instant::now();
        let input_has_audio = chapter_has_audio
            .get(&s.chapter_path)
            .copied()
            .unwrap_or(true); // unknown sources default to "assume audio" (matches old behaviour)
        crate::video_renderer::normalize_segment(
            &runner,
            &NormalizeArgs {
                input: &s.chapter_path,
                output: &out,
                start_secs: s.in_chapter_start_secs,
                duration_secs: s.duration_secs,
                width,
                height,
                fps: cfg.project.fps,
                crf: body_crf,
                lut3d: lut3d_path.as_deref(),
                stabilize_transforms: trf.as_deref(),
                overlays: &overlays,
                image_overlays: &image_overlays,
                input_has_audio,
                outgoing_ramp: seg_ramps[i],
                is_image: crate::media_probe::is_image_path(&s.chapter_path),
                image_effect: image_effect_map.get(&s.chapter_path).copied(),
                image_effect_seed: cfg.project.seed.unwrap_or(0).wrapping_add(i as u64),
            },
            &hw,
        )
        .with_context(|| format!("normalize segment {}", i + 1))?;
        let elapsed = started.elapsed();
        println!(
            "  [{:02}/{:02}] {} ({:.1}s)",
            i + 1,
            segments.len(),
            out.file_name().unwrap_or("?"),
            elapsed.as_secs_f32()
        );
        normalized.push(out);
        let n = segments.len();
        progress(Stage::Clips, (i + 1) as f32 / n as f32, Some(format!("{} of {n} clips", i + 1)));
    }
    if normalize_hits > 0 {
        println!(
            "  ({} of {} normalized clips reused from cache)",
            normalize_hits,
            segments.len()
        );
    }

    // Fold the burst sub-clips into ONE stream-copy concat so the
    // main concat-with-xfade pass sees them as a single segment. Internal
    // cuts inside the burst become hard cuts (zero crossfade), while the
    // burst as a whole still xfades into and out of its neighbors.
    // `burst_sub_durs` keeps (segment index, on-disk sub-clip durations) so
    // the beat-sync check can place the burst's internal cuts.
    let mut burst_sub_durs: Option<(usize, Vec<f32>)> = None;
    if let Some((bs, be, total_secs)) = burst_range {
        if be > bs && bs < normalized.len() {
            let burst_intermediate = workspace.join("beat_burst.mp4");
            let burst_inputs: Vec<Utf8PathBuf> = normalized[bs..be].to_vec();
            let sub_durs: Vec<f32> = burst_inputs
                .iter()
                .zip(&segments[bs..be])
                .map(|(p, s)| {
                    let d = crate::media_probe::probe_video_duration(p) as f32;
                    if d > 0.0 { d } else { s.duration_secs as f32 }
                })
                .collect();
            burst_sub_durs = Some((bs, sub_durs));
            println!(
                "\nfolding {} beat-burst clips → {burst_intermediate}",
                burst_inputs.len()
            );
            // Use the concat *filter* (re-encode) here, not the demuxer-based
            // stream-copy concat: the burst clips are sub-second and AAC frames
            // don't align cleanly to those boundaries, so stream-copy yields
            // non-monotonic timestamps that the downstream xfade rejects.
            //
            // `silent_audio = true`: replace the 12 quarter-second source-audio
            // fragments with silence so the burst rides only the soundtrack.
            // Concatenating real source audio at beat-rate produces pop / click
            // artefacts at every cut (waveform discontinuities + AAC frame
            // misalignment) that ruin the music-video feel.
            crate::video_renderer::concat_re_encode(
                &runner,
                &burst_inputs,
                width,
                height,
                cfg.project.fps,
                body_crf,
                /* silent_audio = */ true,
                &hw,
                &burst_intermediate,
            )
            .context("beat-burst concat stage")?;
            // Replace M normalized entries with one entry; collapse segments[]
            // in parallel into one synthetic Segment whose duration = total.
            let first = segments[bs].clone();
            let synth = crate::selection_planner::Segment {
                source_id: format!("burst:{}", first.source_id),
                chapter_path: burst_intermediate.clone(),
                chapter_index: 1,
                in_chapter_start_secs: 0.0,
                duration_secs: total_secs as f64,
                virtual_start_secs: first.virtual_start_secs,
                reason: format!(
                    "beat-burst x{} ({:.2}s total)",
                    be - bs,
                    total_secs
                ),
                // Synthetic placeholder for the folded burst — a high score so
                // the energy_arc placement keeps it near the peak (it's the
                // most kinetic moment of the cut by construction).
                score: Some(1.0),
            };
            normalized.splice(bs..be, std::iter::once(burst_intermediate));
            segments.splice(bs..be, std::iter::once(synth));
            // Keep the per-seam ramp lookup in step with the folded list
            // (the concat stage indexes it by post-fold position).
            seg_ramps.splice(bs..be, std::iter::once(None));
        }
    }

    // (intro/outro setup moved earlier so overlay mode can attach to normalize)

    progress(Stage::Joining, 0.0, None);
    // 7. Concat (with or without xfade transitions). use_transitions and
    // trans_dur were hoisted earlier so the K computation could see them.
    let body = workspace.join("body.mp4");
    // How the body was assembled, for the beat-sync check after the mux.
    let concat_layout: ConcatLayout;

    if use_transitions && trans_dur > 0.0 {
        // Assemble the input list: [intro], normalized..., [outro].
        //
        // CRITICAL: the duration we pass to `concat_with_xfade` MUST be the
        // actual on-disk duration of the rendered file, not the planner's
        // requested duration. The renderer applies `,fps={N}` which snaps
        // to frame boundaries, and the beat-burst fold concatenates
        // frame-rounded sub-clips — both introduce small drifts (e.g. a
        // 6.132 s planned burst lands as exactly 6.000 s on disk). Each
        // drift becomes a constant offset error in the xfade chain. With
        // wide xfades (0.35 s) the seam absorbs it, but a hard-cut seam
        // (0.10 s) after a drifted input makes xfade try to fade past the
        // end of the left input — xfade then silently stops emitting
        // video while acrossfade keeps going, producing a body.mp4 with
        // video=29s / audio=80s. Probe each clip so the offsets reflect
        // reality.
        let probe_dur = |p: &Utf8Path, fallback: f32| -> f32 {
            let d = crate::media_probe::probe_video_duration(p) as f32;
            if d > 0.0 { d } else { fallback }
        };
        let mut inputs: Vec<(Utf8PathBuf, f32)> = Vec::new();
        if let Some(p) = &intro_path {
            inputs.push((p.clone(), probe_dur(p, intro_dur)));
        }
        for (i, p) in normalized.iter().enumerate() {
            inputs.push((p.clone(), probe_dur(p, segments[i].duration_secs as f32)));
        }
        if let Some(p) = &outro_path {
            inputs.push((p.clone(), probe_dur(p, outro_dur)));
        }
        // Per-pair transition spec: same xfade name everywhere, default
        // duration, EXCEPT after a segment that has an outgoing speed-ramp
        // — there we collapse the xfade to ~1 frame so the ramp itself is
        // the visible transition effect (a Reels-style hard-cut whip-up
        // rather than a crossfade that masks the speed change).
        let intro_count = if intro_path.is_some() { 1usize } else { 0 };
        let mut trans_pairs: Vec<(String, f32)> = Vec::with_capacity(inputs.len().saturating_sub(1));
        let mut hard_cuts = 0u32;
        for k in 0..(inputs.len().saturating_sub(1)) {
            // Is the OUTGOING side at position k a segment with an outgoing
            // ramp? If so, collapse the seam to a near-cut.
            let outgoing_seg_idx: Option<usize> =
                if k >= intro_count && k < intro_count + segments.len() {
                    Some(k - intro_count)
                } else {
                    None
                };
            let is_ramped = outgoing_seg_idx
                .and_then(|i| seg_ramps.get(i))
                .and_then(|r| r.as_ref())
                .is_some();
            let dur = if is_ramped {
                hard_cuts += 1;
                // See HARD_CUT_XFADE_SECS: just above one AAC frame so
                // the audio chain doesn't truncate, yet short enough to
                // feel like a hard cut and keep the speed-ramp flash visible.
                HARD_CUT_XFADE_SECS
            } else {
                trans_dur
            };
            trans_pairs.push((cfg.transition.default.clone(), dur));
        }
        println!(
            "\nconcat with xfade ({} clips, {} default × {:.2}s, {} hard-cut after ramps) → {body}",
            inputs.len(),
            cfg.transition.default,
            trans_dur,
            hard_cuts,
        );
        concat_layout = ConcatLayout {
            input_durs: inputs.iter().map(|(_, d)| *d).collect(),
            seam_durs: trans_pairs
                .iter()
                .map(|(_, d)| d.max(crate::video_renderer::MIN_XFADE_SECS))
                .collect(),
            has_intro: intro_path.is_some(),
            has_outro: outro_path.is_some(),
            burst: burst_sub_durs.map(|(bs, d)| (intro_count + bs, d)),
        };
        crate::video_renderer::concat_with_xfade(
            &runner,
            &inputs,
            &trans_pairs,
            width,
            height,
            cfg.project.fps,
            body_crf,
            cfg.project.fade_in.as_secs_f32(),
            cfg.project.fade_out.as_secs_f32(),
            &hw,
            &body,
        )
        .context("xfade concat stage")?;
    } else {
        println!("\nconcatenating (hard cuts) → {body}");
        crate::video_renderer::concat_segments(&runner, &normalized, &workspace, &body)
            .context("concat stage")?;
        let input_durs: Vec<f32> = normalized
            .iter()
            .zip(&segments)
            .map(|(p, s)| {
                let d = crate::media_probe::probe_video_duration(p) as f32;
                if d > 0.0 { d } else { s.duration_secs as f32 }
            })
            .collect();
        concat_layout = ConcatLayout {
            seam_durs: vec![0.0; input_durs.len().saturating_sub(1)],
            input_durs,
            has_intro: false,
            has_outro: false,
            burst: burst_sub_durs,
        };
    }

    progress(Stage::Mixing, 0.0, None);
    // 7. Mux soundtrack into a body+audio file. Per-output transcoding to
    // each `[[outputs]]` target happens after.
    let muxed_body: Utf8PathBuf = if cfg.outputs.len() == 1 {
        // Single output: write straight to the user's chosen path.
        let p = cfg.outputs[0].path.clone();
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        p
    } else {
        // Multiple outputs: write the body+audio to a workspace intermediate.
        workspace.join("body_with_audio.mp4")
    };
    let mut duck_windows: Vec<DuckWindow> = Vec::new();
    // (loop plan, body duration) the mux used — the beat-sync check maps the
    // soundtrack's beats into output time through the same layout.
    let mut soundtrack_layout: Option<(crate::video_renderer::SoundtrackLoopPlan, f32)> = None;
    if let Some(snd) = &cfg.soundtrack {
        let snd_path = soundtrack_resolved
            .as_ref()
            .expect("soundtrack_resolved set when [soundtrack] is configured");

        if snd.duck_count > 0 {
            println!("\nscoring source audio per segment for ducking...");
            let started = std::time::Instant::now();

            // Spin up the ML engine. Always tried; gracefully falls
            // back to non-ML scoring when the model file is absent or the
            // ONNX runtime fails to initialise. CUDA EP is probed first; if
            // unavailable, automatic CPU fallback.
            let mut ml_engine: Option<crate::ml_engine::MlEngine> = None;
            let vad_path = cfg.project.models_dir.join("silero_vad.onnx");
            if !vad_path.as_std_path().exists() {
                tracing::info!(
                    path = %vad_path,
                    "silero_vad.onnx not found; speech-driven ducking disabled. \
                     Download from https://github.com/snakers4/silero-vad and place \
                     it in `{models_dir}` to enable.",
                    models_dir = cfg.project.models_dir
                );
            } else {
                match crate::ml_engine::MlEngine::new() {
                    Ok(mut eng) => {
                        println!("  ML engine: {} EP", eng.ep_name());
                        if let Err(e) = eng.with_silero_vad(&vad_path) {
                            tracing::warn!(error = %e, "loading Silero VAD failed");
                        } else {
                            ml_engine = Some(eng);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "ML engine init failed");
                    }
                }
            }
            // Compute output offsets for each segment, accounting for xfade overlap.
            let (output_starts, output_durs) =
                compute_segment_output_layout(&segments, intro_dur, use_transitions, trans_dur);
            let total_output_dur =
                compute_total_output(&output_durs, intro_dur, outro_dur, use_transitions, trans_dur);
            // Per-segment audio presence — short-circuits the duck audio
            // decode for sources we already know have no audio (saves ~100 ms
            // per segment + silences "audio decode for ducking failed"
            // spam). Synthetic burst segments (`source_id` starts with
            // "burst:") have `chapter_path` pointing at the workspace's
            // beat_burst intermediate which IS silent, so mark them as
            // no-audio too.
            let seg_has_audio: Vec<bool> = segments
                .iter()
                .map(|s| {
                    if s.source_id.starts_with("burst:") {
                        false
                    } else {
                        chapter_has_audio
                            .get(&s.chapter_path)
                            .copied()
                            .unwrap_or(true)
                    }
                })
                .collect();
            duck_windows = select_duck_windows(
                &segments,
                &output_starts,
                &output_durs,
                snd.duck_count,
                snd.duck_min_spacing.as_secs_f32(),
                snd.duck_ramp.as_secs_f32(),
                snd.duck_min_dur.as_secs_f32(),
                snd.duck_max_dur.as_secs_f32(),
                total_output_dur,
                ml_engine.as_mut(),
                snd.vad_speech_weight,
                &seg_has_audio,
            );
            println!(
                "  picked {} duck window{} ({:.1}s)",
                duck_windows.len(),
                plural(duck_windows.len()),
                started.elapsed().as_secs_f32()
            );
            for w in &duck_windows {
                println!(
                    "    · {:>5.2}s → {:>5.2}s  src={}  audio_score={:.2}",
                    w.start_secs, w.end_secs, w.source_id, w.audio_score
                );
            }
        }

        // Always route through `mux_with_ducking` — it handles the
        // empty-windows case internally (returns to the simple mux only when
        // there's also no fade-out, no loop, and no first-play-end). Going
        // straight to `mux_soundtrack` here used to drop the user's
        // `[soundtrack].fade_out` and the smart-loop crossfade whenever
        // `duck_count = 0`, ending the music abruptly.
        {
            if duck_windows.is_empty() {
                println!("\nmuxing soundtrack (no ducks) → {muxed_body}");
            } else {
                println!("\nmuxing soundtrack with {} duck window(s) → {muxed_body}", duck_windows.len());
            }
            let win_args: Vec<(f32, f32, f32)> = duck_windows
                .iter()
                .map(|w| (w.start_secs, w.end_secs, w.ramp_secs))
                .collect();
            // Probe the ACTUAL body video duration. The computed total based on
            // segment metadata can drift from the encoded reality by several
            // seconds due to (a) per-pair xfade-vs-acrossfade length differences
            // when transitions vary (some 0.4 s, some 0.05 s for ramped seams),
            // (b) frame-quantisation when segment durations don't fall on
            // 1/fps boundaries, and (c) AAC frame padding lengthening the
            // audio stream past the video. We use the actual video stream
            // duration so the soundtrack fade-out is anchored to the real end
            // of the picture and the muxed output's audio gets capped to it.
            let body_video_secs = crate::media_probe::probe_video_duration(&body) as f32;
            let (_out_starts2, out_durs2) =
                compute_segment_output_layout(&segments, intro_dur, use_transitions, trans_dur);
            let body_computed =
                compute_total_output(&out_durs2, intro_dur, outro_dur, use_transitions, trans_dur);
            let body_total = if body_video_secs > 0.0 { body_video_secs } else { body_computed };
            if body_video_secs > 0.0 && (body_video_secs - body_computed).abs() > 0.5 {
                println!(
                    "  body video probed at {:.2}s (computed estimate was {:.2}s; using probed value for soundtrack timing)",
                    body_video_secs, body_computed
                );
            }
            // Smart loop (default): the first playthrough plays soundtrack
            // [0 .. loud_end] — preserving the natural intro at the very start
            // of the video. Subsequent loops cycle [loud_start .. loud_end] —
            // the loud region only — so no quiet seam ever appears.
            //
            // Only activated when the loud region differs meaningfully from
            // the full track. For tracks that are already loud throughout,
            // we fall back to a simple full-track loop (loud_start = 0).
            let loop_plan = audio.as_ref().map(soundtrack_loop_plan).unwrap_or_default();
            let crate::video_renderer::SoundtrackLoopPlan {
                first_play_end_secs,
                loop_start_secs,
                loop_secs,
                ..
            } = loop_plan;
            if loop_secs > 0.0 {
                tracing::info!(
                    first_play_end = first_play_end_secs,
                    loop_start = loop_start_secs,
                    loop_dur = loop_secs,
                    soundtrack_total = audio.as_ref().map(|a| a.duration_secs).unwrap_or(0.0),
                    smart = first_play_end_secs > 0.0,
                    "soundtrack loop plan"
                );
            }
            crate::video_renderer::mux_with_ducking(
                &runner,
                &body,
                snd_path,
                &win_args,
                snd.ducking_db,
                body_total,
                snd.fade_out.as_secs_f32(),
                loop_plan,
                &muxed_body,
            )
            .context("mux+duck stage")?;
            soundtrack_layout = Some((loop_plan, body_total));
        }
    } else {
        println!("\nno soundtrack configured; copying body → {muxed_body}");
        std::fs::copy(&body, &muxed_body)
            .with_context(|| format!("copying {body} to {muxed_body}"))?;
    }

    // 7b. Per-output transcode pass. With a single output, the muxed body IS
    // the user's output (no extra pass). With multiple, transcode each.
    let final_outputs: Vec<Utf8PathBuf> = if cfg.outputs.len() == 1 {
        vec![muxed_body.clone()]
    } else {
        let mut paths = Vec::with_capacity(cfg.outputs.len());
        println!("\ntranscoding to {} output{}...", cfg.outputs.len(), plural(cfg.outputs.len()));
        for (i, o) in cfg.outputs.iter().enumerate() {
            progress(
                Stage::Encoding,
                i as f32 / cfg.outputs.len() as f32,
                Some(format!("output {} of {}", i + 1, cfg.outputs.len())),
            );
            if let Some(parent) = o.path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let (ow, oh) = parse_resolution(&o.resolution)
                .with_context(|| format!("outputs[{i}].resolution"))?;
            let pref = encoder_pref_from_cfg(&o.encoder);
            let out_hw = HwConfig::resolve(hw.support.clone(), pref);
            let started = std::time::Instant::now();
            println!(
                "  [{i}] {} → {ow}x{oh}  enc={}  crf={}  a={}",
                o.path, out_hw.encoder.name(), o.crf, o.audio_bitrate
            );
            crate::video_renderer::transcode_output(
                &runner,
                &muxed_body,
                &o.path,
                ow,
                oh,
                o.crf,
                &o.audio_bitrate,
                &out_hw,
            )
            .with_context(|| format!("transcoding output {i} → {}", o.path))?;
            println!("    done ({:.1}s)", started.elapsed().as_secs_f32());
            paths.push(o.path.clone());
        }
        paths
    };
    let primary_output = &cfg.outputs[0].path;

    progress(Stage::Checking, 0.0, None);
    // 7c. Beat-sync check: where did the cuts land against the music?
    // Informational only — never fails the render.
    let beat_sync = match (&audio, &cfg.soundtrack, soundtrack_layout, &soundtrack_resolved) {
        (Some(a), Some(snd), Some((plan, body_secs)), Some(snd_path)) if a.tempo_bpm > 0.0 => {
            check_beat_sync(BeatSyncInputs {
                audio: a,
                concat: &concat_layout,
                loop_plan: plan,
                body_secs,
                fade_out_secs: snd.fade_out.as_secs_f32(),
                duck_windows: &duck_windows,
                soundtrack: snd_path,
                rendered: primary_output,
                fps: cfg.project.fps,
            })
        }
        _ => None,
    };

    // 8. Manifest.
    let manifest_path = manifest_path_for(primary_output);
    let manifest = Manifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        project_name: cfg.project.name.clone(),
        output: primary_output.clone(),
        target_duration_secs: target,
        actual_duration_secs: segments.iter().map(|s: &Segment| s.duration_secs).sum(),
        resolution: cfg.outputs[0].resolution.clone(),
        fps: cfg.project.fps,
        seed: cfg.project.seed,
        soundtrack: cfg
            .soundtrack
            .as_ref()
            .map(|s| crate::track_meta::soundtrack_credits(&s.files))
            .unwrap_or_default(),
        sources,
        segments,
        duck_windows,
        angle_groups: angle_groups
            .iter()
            .map(|g| crate::selection_planner::AngleGroupRecord {
                master_id: g.master_id.clone(),
                members: g
                    .members
                    .iter()
                    .map(|m| crate::selection_planner::AngleMemberRecord {
                        source_id: m.source_id.clone(),
                        offset_secs: m.offset_secs,
                        start_in_master: m.start_in_master,
                        end_in_master: m.end_in_master,
                        offset_source: m.offset_source.as_str().to_string(),
                    })
                    .collect(),
            })
            .collect(),
        beat_sync,
        generator: format!("vibecut {}", env!("CARGO_PKG_VERSION")),
    };
    manifest
        .write_to(&manifest_path)
        .with_context(|| format!("writing manifest {manifest_path}"))?;
    progress(Stage::Checking, 1.0, None);

    println!("\n✓ done");
    for p in &final_outputs {
        println!("  output:    {p}");
    }
    println!("  manifest:  {manifest_path}");

    if explain {
        println!("\n--- explain ---");
        println!("  effective scoring: hilight={:.2} accel={:.2} motion={:.2} sharpness_floor={:.2} exposure_floor={:.2}",
            cfg.scoring.hilight_tags, cfg.scoring.accel_peaks, cfg.scoring.motion_peaks,
            cfg.scoring.sharpness_floor, cfg.scoring.exposure_floor);
        println!("  diversity: per_source_cap={} (= ceil(K/n_sources_with_signal))", "auto");
        println!("  cache: {} (dir={})", if cache_enabled { "enabled" } else { "disabled" }, cache_dir);
        println!("  encoder: {}", hw.encoder.name());
        if let Some(snd) = &cfg.soundtrack {
            let names: Vec<&str> = snd.files.iter()
                .filter_map(|f| f.file_name())
                .collect();
            println!("  soundtrack: [{}] duck_count={} duck_dur={}-{}s fade_out={}s",
                names.join(", "),
                snd.duck_count,
                snd.duck_min_dur.as_secs_f32(),
                snd.duck_max_dur.as_secs_f32(),
                snd.fade_out.as_secs_f32());
        }
        println!("  picks (sorted chronologically by recording time):");
        for (i, w) in manifest.segments.iter().enumerate() {
            println!(
                "    [{:02}] src={:<6} ch{} +{:.2}s  dur={:.2}s  · {}",
                i + 1, w.source_id, w.chapter_index, w.in_chapter_start_secs,
                w.duration_secs,
                w.reason
            );
        }
        if let Some(r) = &manifest.beat_sync {
            println!("  beat sync per cut (error = cut − nearest beat; + = late):");
            for (i, c) in r.cuts.iter().enumerate() {
                let flag = if c.error_frames.abs() <= r.tolerance_frames { "" } else { "  ← off" };
                println!(
                    "    [{:02}] {:>7.3}s  beat {:>7.3}s  {:>+7.1} ms ({:>+5.1} f)  {:?}{flag}",
                    i + 1, c.output_secs, c.beat_secs, c.error_ms, c.error_frames, c.kind
                );
            }
        }
        if !manifest.duck_windows.is_empty() {
            println!("  duck windows:");
            for (i, w) in manifest.duck_windows.iter().enumerate() {
                println!(
                    "    [{}] {:.2}s → {:.2}s  ({:.2}s) · src={} score={:.2}",
                    i + 1, w.start_secs, w.end_secs, w.end_secs - w.start_secs,
                    w.source_id, w.audio_score
                );
            }
        }
    }

    Ok(())
}

/// Picks a single clip length: the midpoint of the configured min/max.
fn clip_length(cfg: &Config) -> f64 {
    let lo = cfg.selection.min_clip.as_secs_f64();
    let hi = cfg.selection.max_clip.as_secs_f64();
    (lo + hi) / 2.0
}

/// True iff `trf` exists, is non-empty, and was modified after `source`.
/// Used to skip the (slow) vidstab pass-1 on re-runs over an unchanged source.
fn trf_is_fresh(trf: &Utf8Path, source: &Utf8Path) -> bool {
    let trf_meta = match std::fs::metadata(trf.as_std_path()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if trf_meta.len() == 0 {
        return false;
    }
    let src_meta = match std::fs::metadata(source.as_std_path()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let trf_mtime = trf_meta.modified().ok();
    let src_mtime = src_meta.modified().ok();
    match (trf_mtime, src_mtime) {
        (Some(t), Some(s)) => t >= s,
        _ => false,
    }
}

/// True iff `out` exists, is fresher than `source`, and its duration matches
/// `duration_secs` to within 50 ms. Filename uniqueness across runs is
/// guaranteed by [`segment_fingerprint`] so this freshness check doesn't
/// need to verify the spec — only that a previous encode wasn't truncated
/// or made stale by a re-extracted source.
fn normalized_is_fresh(out: &Utf8Path, source: &Utf8Path, duration_secs: f64) -> bool {
    let out_meta = match std::fs::metadata(out.as_std_path()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if out_meta.len() == 0 {
        return false;
    }
    let src_meta = match std::fs::metadata(source.as_std_path()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    match (out_meta.modified().ok(), src_meta.modified().ok()) {
        (Some(t), Some(s)) if t < s => return false,
        (None, _) | (_, None) => return false,
        _ => {}
    }
    match crate::media_probe::probe_metadata(out) {
        Ok(p) => match p.media.as_ref() {
            Some(m) => (m.duration_secs - duration_secs).abs() < 0.05,
            None => false,
        },
        Err(_) => false,
    }
}

/// 8-hex-char fingerprint of a segment's render spec. Embedded in the
/// `seg_NNN_HASH.{trf,mp4}` filenames so two runs with the same picker
/// output collide on the same files (cache hit) while a re-pick produces
/// different filenames (no stale-content collision).
fn segment_fingerprint(
    s: &Segment,
    width: u32,
    height: u32,
    fps: u32,
    crf: u32,
    lut: Option<&Utf8Path>,
    stabilize: bool,
    ramp: Option<crate::video_renderer::SpeedRamp>,
    image_effect: Option<crate::project_config::ImageEffect>,
) -> String {
    let mut h = blake3::Hasher::new();
    // Bump when normalize's output changes for the same inputs, so cached
    // clips from an older renderer aren't reused. v2: frame-exact length
    // (v1 clips came out one frame short).
    h.update(b"normalize-v2");
    h.update(s.chapter_path.as_str().as_bytes());
    h.update(&((s.in_chapter_start_secs * 1000.0).round() as i64).to_le_bytes());
    h.update(&((s.duration_secs * 1000.0).round() as i64).to_le_bytes());
    h.update(&width.to_le_bytes());
    h.update(&height.to_le_bytes());
    h.update(&fps.to_le_bytes());
    h.update(&crf.to_le_bytes());
    h.update(&[stabilize as u8]);
    if let Some(p) = lut {
        h.update(p.as_str().as_bytes());
    }
    // Speed-ramp parameters are part of the cache key so a re-run that
    // changes `speed_ramp_probability` (or the seed driving which transitions
    // get ramped) doesn't reuse a cached non-ramped clip in place of the
    // ramped version (or vice versa). Quantise the f32s to 0.1 ms / 0.001×
    // before hashing so trivial float jitter doesn't bust the cache.
    if let Some(r) = ramp {
        h.update(&[1u8]);
        h.update(&((r.duration_secs * 10_000.0).round() as i64).to_le_bytes());
        h.update(&((r.factor * 1_000.0).round() as i64).to_le_bytes());
        h.update(&[r.flash as u8]);
    } else {
        h.update(&[0u8]);
    }
    // Image effect is part of the key so changing the effect on a re-run
    // produces a new cached file rather than reusing the old animation.
    if let Some(eff) = image_effect {
        h.update(&[eff as u8]);
    }
    let hex = h.finalize().to_hex();
    hex[..8].to_string()
}

/// Bridge from `multi_angle`'s offset-refinement callback signature to the
/// existing `audio_analysis::sync_two_files` API.
fn audio_xcorr_refine(
    master: &Utf8Path,
    member: &Utf8Path,
    master_anchor_secs: f32,
    member_anchor_secs: f32,
) -> Option<(f64, f32)> {
    crate::audio_analysis::sync_two_files(
        master,
        member,
        master_anchor_secs,
        member_anchor_secs,
        30.0,
        60.0,
    )
    .ok()
    .map(|r| (r.offset_secs as f64, r.confidence))
}

/// Re-arrange `segs` so the score curve over output position forms a
/// triangular arc: the strongest-scoring pick lands at the middle output
/// position, second-strongest just to one side, third to the other,
/// alternating outward. Result: weakest segments at the edges, climax in
/// the middle. Used by `[selection].order = "energy_arc"`.
///
/// Picks with `score = None` (e.g. fillers, or backwards-compat segments
/// from old manifests) are treated as score 0 — they end up at the edges,
/// which is the right behaviour: filler shouldn't anchor the peak.
///
/// Tie-break: equal scores resolve by the original index so the result is
/// deterministic (and reproducible across runs with the same seed +
/// variety).
fn arrange_energy_arc(segs: &mut Vec<Segment>) {
    let n = segs.len();
    if n < 3 {
        // 0/1/2 picks: arc has no meaning. Leave as-is.
        return;
    }
    // Sort indices by (score desc, original-index asc) so the strongest pick
    // is first, ties resolve deterministically.
    let mut ordered: Vec<usize> = (0..n).collect();
    ordered.sort_by(|&a, &b| {
        let sa = segs[a].score.unwrap_or(0.0);
        let sb = segs[b].score.unwrap_or(0.0);
        sb.partial_cmp(&sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    });
    // Take ownership of the original segments so we can re-place them by index.
    let original: Vec<Segment> = std::mem::take(segs).into_iter().collect();
    let mut placed: Vec<Option<Segment>> = (0..n).map(|_| None).collect();

    let mid: i64 = (n as i64) / 2;
    // Pick rank 0 → mid, rank 1 → mid-1, rank 2 → mid+1, rank 3 → mid-2, ...
    placed[mid as usize] = Some(original[ordered[0]].clone());
    let mut left = mid - 1;
    let mut right = mid + 1;
    for rank in 1..n {
        let idx = ordered[rank];
        if rank % 2 == 1 {
            // Odd ranks go left of centre.
            if left >= 0 {
                placed[left as usize] = Some(original[idx].clone());
                left -= 1;
            } else {
                placed[right as usize] = Some(original[idx].clone());
                right += 1;
            }
        } else {
            // Even ranks (2, 4, ...) go right of centre.
            if (right as usize) < n {
                placed[right as usize] = Some(original[idx].clone());
                right += 1;
            } else {
                placed[left as usize] = Some(original[idx].clone());
                left -= 1;
            }
        }
    }
    *segs = placed.into_iter().map(|s| s.expect("every position filled")).collect();
}

/// Produce a fresh `u64` seed from the system clock, mixed via SplitMix64
/// so consecutive calls (which differ in nanos) yield well-distributed
/// seeds rather than near-neighbour values. No `rand` crate dependency.
fn generate_random_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0xdeadbeef);
    // Advance the nanos counter by the golden-ratio constant before mixing so
    // consecutive nanosecond-apart calls produce well-distributed outputs.
    crate::rng::splitmix64(nanos.wrapping_add(crate::rng::GOLDEN_RATIO_64))
}

fn manifest_path_for(output: &Utf8Path) -> Utf8PathBuf {
    let parent = output.parent().unwrap_or(Utf8Path::new("."));
    let stem = output.file_stem().unwrap_or("output");
    parent.join(format!("{stem}.manifest.json"))
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} {}", UNITS[u])
    } else {
        format!("{v:.2} {}", UNITS[u])
    }
}

pub fn short_hash(full: &str) -> String {
    // "blake3:abcd..." -> "blake3:abcd...12ab" (first/last 4 of the digest)
    if let Some((prefix, hex)) = full.split_once(':') {
        if hex.len() > 12 {
            return format!("{prefix}:{}…{}", &hex[..6], &hex[hex.len() - 6..]);
        }
    }
    full.to_string()
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn encoder_pref_from_cfg(e: &crate::project_config::Encoder) -> EncoderPref {
    use crate::project_config::Encoder as E;
    match e {
        E::Auto => EncoderPref::Auto,
        E::Libx264 => EncoderPref::Libx264,
        E::Libx265 => EncoderPref::Libx265,
        E::Nvenc => EncoderPref::Nvenc,
        E::Amf => EncoderPref::Amf,
        E::Qsv => EncoderPref::Qsv,
    }
}

fn card_to_layers(card: &crate::project_config::TextCard) -> (Vec<TextOverlay>, Vec<ImageOverlay>) {
    use crate::project_config::CardLayer;
    // Per-layer fade defaults to the card-level fade. This lets users set
    // `[intro].fade_in = "700ms"` once and have every layer respect it,
    // without having to repeat it on each `[[intro.layers]]` entry. Layers
    // that explicitly set their own fade override.
    let card_fade_in = card.fade_in.as_secs_f32();
    let card_fade_out = card.fade_out.as_secs_f32();
    let card_dur = card.duration.as_secs_f32();
    let mut texts = Vec::new();
    let mut images = Vec::new();
    for layer in &card.layers {
        match layer {
            CardLayer::Text(t) => {
                let appear = t.appear_at.map(|d| d.as_secs_f32()).unwrap_or(0.0);
                let disappear = t.disappear_at.map(|d| d.as_secs_f32()).unwrap_or(card_dur);
                texts.push(TextOverlay {
                    content: t.content.clone(),
                    font_path: resolve_font(t.font.as_deref()),
                    size: t.size,
                    color_hex: t.color.clone(),
                    position: t.position.clone(),
                    x_offset: t.x_offset,
                    y_offset: t.y_offset,
                    fade_in_secs: t.fade_in.map(|d| d.as_secs_f32()).unwrap_or(card_fade_in),
                    fade_out_secs: t.fade_out.map(|d| d.as_secs_f32()).unwrap_or(card_fade_out),
                    appear_at_secs: appear,
                    disappear_at_secs: disappear.min(card_dur),
                    bg_color_hex: t.background.as_ref().map(|b| b.color.clone()),
                    bg_padding_px: t.background.as_ref().and_then(|b| {
                        b.padding.as_ref().and_then(|p| p.trim_end_matches("px").trim().parse::<u32>().ok())
                    }),
                });
            }
            CardLayer::Image(im) => {
                let appear = im.appear_at.map(|d| d.as_secs_f32()).unwrap_or(0.0);
                let disappear = im.disappear_at.map(|d| d.as_secs_f32()).unwrap_or(card_dur);
                images.push(ImageOverlay {
                    path: im.path.clone(),
                    position: im.position.clone(),
                    height: im.height,
                    x_offset: im.x_offset,
                    y_offset: im.y_offset,
                    opacity: im.opacity,
                    fade_in_secs: im.fade_in.map(|d| d.as_secs_f32()).unwrap_or(card_fade_in),
                    fade_out_secs: im.fade_out.map(|d| d.as_secs_f32()).unwrap_or(card_fade_out),
                    appear_at_secs: appear,
                    disappear_at_secs: disappear.min(card_dur),
                });
            }
        }
    }
    (texts, images)
}

/// Convert a chapter's `creation_time` ISO-8601 string + an offset within the
/// chapter to seconds-since-epoch. Used to sort picks chronologically across
/// sources whose recording windows may overlap.
fn abs_recording_secs(
    creation_map: &std::collections::HashMap<(String, u32), Option<String>>,
    source_id: &str,
    chapter_index: u32,
    in_chapter_secs: f64,
) -> f64 {
    let key = (source_id.to_string(), chapter_index);
    let creation = creation_map.get(&key).cloned().flatten();
    let base = creation
        .as_deref()
        .and_then(parse_iso_seconds)
        .unwrap_or(0.0);
    base + in_chapter_secs
}

/// Best-effort ISO-8601 parser → seconds-since-epoch. Handles GoPro's
/// `2025-09-17T11:55:48.000000Z` format; doesn't need leap-second precision.
fn parse_iso_seconds(s: &str) -> Option<f64> {
    let s = s.trim();
    let s = s.trim_end_matches('Z');
    let (date_part, time_part) = s.split_once('T')?;
    let mut date_iter = date_part.split('-');
    let y: i64 = date_iter.next()?.parse().ok()?;
    let mo: i64 = date_iter.next()?.parse().ok()?;
    let d: i64 = date_iter.next()?.parse().ok()?;
    let mut time_iter = time_part.split(':');
    let hh: i64 = time_iter.next()?.parse().ok()?;
    let mm: i64 = time_iter.next()?.parse().ok()?;
    let ss: f64 = time_iter.next()?.parse().ok()?;
    // Civil → days since 1970-01-01 (Howard Hinnant's algorithm, simplified).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let doy = (153 * if mo > 2 { mo - 3 } else { mo + 9 } + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days_since_epoch = era * 146097 + doe - 719468;
    Some(days_since_epoch as f64 * 86400.0 + hh as f64 * 3600.0 + mm as f64 * 60.0 + ss)
}

fn resolve_font(name: Option<&str>) -> Option<camino::Utf8PathBuf> {
    let name = name?;
    let p = camino::Utf8PathBuf::from(name);
    if p.is_absolute() {
        return Some(p);
    }
    #[cfg(windows)]
    {
        let win_font = camino::Utf8PathBuf::from(format!("C:/Windows/Fonts/{name}"));
        if win_font.as_std_path().exists() {
            return Some(win_font);
        }
    }
    None
}


/// How the body video was concatenated: what the beat-sync check needs to
/// place every cut exactly where the concat stage put it.
struct ConcatLayout {
    /// On-disk duration of each concat input (intro, clips, outro).
    input_durs: Vec<f32>,
    /// Effective crossfade per seam (0 for hard concat).
    seam_durs: Vec<f32>,
    has_intro: bool,
    has_outro: bool,
    /// (concat input index, sub-clip durations) of a folded beat-burst.
    burst: Option<(usize, Vec<f32>)>,
}

struct BeatSyncInputs<'a> {
    audio: &'a crate::audio_analysis::AnalyzedAudio,
    concat: &'a ConcatLayout,
    loop_plan: crate::video_renderer::SoundtrackLoopPlan,
    body_secs: f32,
    fade_out_secs: f32,
    duck_windows: &'a [DuckWindow],
    soundtrack: &'a Utf8Path,
    rendered: &'a Utf8Path,
    fps: u32,
}

/// How the soundtrack is laid under the body (see `mux_with_ducking`).
/// Smart loop (default): play the natural intro once up to the end of the
/// loud region, then loop the loud region only, so no quiet seam appears.
/// Tracks that are loud throughout loop whole from t = 0, trimmed to a
/// whole number of beats. Both the mux stage and the beat-lock planner use
/// this, so they agree on where every beat plays.
fn soundtrack_loop_plan(a: &crate::audio_analysis::AnalyzedAudio) -> crate::video_renderer::SoundtrackLoopPlan {
    let (raw_start, raw_end) = a.detect_loud_region(0.80);
    let loud_start = a.beat_at_or_after(raw_start);
    let loud_end = a.beat_at_or_before(raw_end);
    let total = a.duration_secs;
    let smart = loud_start > 0.5 || loud_end < total - 0.5;
    let (first_play_end_secs, loop_start_secs, loop_secs) = if smart && loud_end > loud_start {
        (loud_end, loud_start, loud_end - loud_start)
    } else {
        let bi = a.beat_interval_secs();
        let trim = if bi > 0.0 { (total / bi).floor() * bi } else { total };
        (0.0, 0.0, trim)
    };
    crate::video_renderer::SoundtrackLoopPlan {
        first_play_end_secs,
        loop_start_secs,
        loop_secs,
        beat_interval_secs: a.beat_interval_secs(),
    }
}

/// One-line description of a fitted beat grid for the run log.
fn describe_beat_grid(g: &crate::audio_analysis::BeatGridFit) -> String {
    use crate::audio_analysis::GridOctave;
    let octave = match g.octave {
        GridOctave::Kept => String::new(),
        GridOctave::Halved => format!(", halved from {:.1} BPM double-time", g.raw_bpm),
        GridOctave::Doubled => format!(", doubled from {:.1} BPM half-time", g.raw_bpm),
    };
    format!(
        "{} — {:.0}% of beats matched, residual mean {:.1} ms / max {:.1} ms (raw autocorrelation {:.1} BPM{octave})",
        if g.is_tight() { "tight" } else { "loose" },
        g.matched_fraction * 100.0,
        g.mean_abs_residual_ms,
        g.max_abs_residual_ms,
        g.raw_bpm,
    )
}

/// Post-render beat-sync check (see `beat_sync`). Prints a summary and
/// returns the report for the manifest; `None` when there is nothing to
/// check. Measurement failures are logged, never fatal.
fn check_beat_sync(i: BeatSyncInputs<'_>) -> Option<crate::beat_sync::BeatSyncReport> {
    use crate::beat_sync::{
        beats_in_output, measure_audio_offset, pick_reference_window, planned_cuts, score_cuts,
        OFFSET_MIN_CONFIDENCE,
    };
    let cuts = planned_cuts(
        &i.concat.input_durs,
        &i.concat.seam_durs,
        i.concat.has_intro,
        i.concat.has_outro,
        i.concat.burst.as_ref().map(|(idx, d)| (*idx, d.as_slice())),
    );
    if cuts.is_empty() {
        return None;
    }
    let spans = i.loop_plan.spans(i.body_secs, i.audio.duration_secs);
    let beats = beats_in_output(&i.audio.beats, &spans);

    let ducks: Vec<(f32, f32, f32)> =
        i.duck_windows.iter().map(|w| (w.start_secs, w.end_secs, w.ramp_secs)).collect();
    let measured = pick_reference_window(&spans, &ducks, i.body_secs - i.fade_out_secs).and_then(
        |(out_start, src_start, len)| {
            match measure_audio_offset(i.rendered, i.soundtrack, out_start, src_start, len) {
                Ok(x) => Some(x),
                Err(e) => {
                    tracing::warn!(error = %e, "beat-sync: measuring the audio offset failed");
                    None
                }
            }
        },
    );
    let applied = measured
        .as_ref()
        .filter(|x| x.confidence >= OFFSET_MIN_CONFIDENCE)
        .map(|x| x.offset_secs)
        .unwrap_or(0.0);

    let mut report = score_cuts(&cuts, &beats, applied, i.audio.beat_interval_secs(), i.fps)?;
    report.audio_offset_secs = measured.as_ref().map(|x| x.offset_secs);
    report.audio_offset_confidence = measured.as_ref().map(|x| x.confidence);

    let frame_ms = 1000.0 / i.fps.max(1) as f32;
    println!(
        "\nbeat sync: {} cut{} vs the {:.2} BPM grid — {}",
        report.n_cuts,
        plural(report.n_cuts as usize),
        report.tempo_bpm,
        report.verdict()
    );
    println!(
        "  mean |error| {:.0} ms ({:.1} frames), worst {:.0} ms; {}/{} within ±{:.0} frames (random placement would average {:.0} ms)",
        report.mean_abs_error_ms,
        report.mean_abs_error_ms / frame_ms,
        report.max_abs_error_ms,
        report.within_tolerance,
        report.n_cuts,
        report.tolerance_frames,
        report.chance_mean_abs_error_ms,
    );
    match &measured {
        Some(x) if x.confidence >= OFFSET_MIN_CONFIDENCE => println!(
            "  audio offset in the render: {:+.1} ms (correlation {:.2})",
            x.offset_secs * 1000.0,
            x.confidence
        ),
        Some(x) => println!(
            "  audio offset: unclear (correlation {:.2}); assumed 0",
            x.confidence
        ),
        None => println!("  audio offset: not measured; assumed 0"),
    }
    Some(report)
}

/// Compute the total output duration after intro/outro and xfade overlaps.
fn compute_total_output(
    seg_durs: &[f32],
    intro_dur: f32,
    outro_dur: f32,
    use_transitions: bool,
    trans_dur: f32,
) -> f32 {
    let raw: f32 = seg_durs.iter().sum::<f32>() + intro_dur + outro_dur;
    if !use_transitions || trans_dur <= 0.0 {
        return raw;
    }
    let inputs = seg_durs.len() as f32
        + if intro_dur > 0.0 { 1.0 } else { 0.0 }
        + if outro_dur > 0.0 { 1.0 } else { 0.0 };
    let overlaps = (inputs - 1.0).max(0.0);
    (raw - overlaps * trans_dur).max(0.0)
}

/// Compute each segment's `(output_start, visible_duration)` in the rendered
/// video timeline. With transitions, intro/outro and xfade overlaps shift the
/// segment offsets; without transitions, segments concat directly.
fn compute_segment_output_layout(
    segments: &[Segment],
    intro_dur: f32,
    use_transitions: bool,
    trans_dur: f32,
) -> (Vec<f32>, Vec<f32>) {
    let n = segments.len();
    let mut starts = Vec::with_capacity(n);
    let durs: Vec<f32> = segments.iter().map(|s| s.duration_secs as f32).collect();
    if !use_transitions || trans_dur <= 0.0 {
        // Hard concat: segment k starts at sum of preceding durations.
        let mut cum = 0.0_f32;
        for d in &durs {
            starts.push(cum);
            cum += *d;
        }
        return (starts, durs);
    }
    // With xfade: in the input list, intro is index 0 (if present), then
    // segments. After all xfades, segment k starts in the output at
    //   intro_dur + (k * (clip_len - trans_dur))   for uniform clip_len
    // — but more generally, account for variable durations.
    // Output offset of input k (1-indexed past intro) is:
    //   sum(durations of inputs 0..k) - k * trans_dur.
    //
    // The "segment fully visible" interval is [offset+T, offset+D-T] for
    // middle segments. We expose the full segment-input window
    // [offset, offset + D] so the duck ramp can encroach into the xfade —
    // that gives a more natural feel than hard-bounded ducks.
    let intro_count = if intro_dur > 0.0 { 1 } else { 0 };
    for k in 0..n {
        // Input index in the xfade chain.
        let input_idx = intro_count + k;
        // sum of durations up to input_idx.
        let mut sum = 0.0_f32;
        if intro_count == 1 {
            sum += intro_dur;
        }
        for (j, d) in durs.iter().take(k).enumerate() {
            let _ = j;
            sum += *d;
        }
        let off = sum - input_idx as f32 * trans_dur;
        starts.push(off);
    }
    // Visible duration for ducking purposes: keep the full segment duration —
    // the ramp inside `mux_with_ducking` rolls off naturally at the edges.
    (starts, durs)
}


/// Distribute `images` at evenly-spaced positions among the ordered `videos`.
/// Each image i is inserted just before the video at index
/// `floor((2*i + 1) * n_vid / (2 * n_img))`, centering each image in its
/// "slot" so a single image lands in the middle rather than at the start.
fn interleave_images_with_videos(images: Vec<Segment>, videos: Vec<Segment>) -> Vec<Segment> {
    if images.is_empty() {
        return videos;
    }
    if videos.is_empty() {
        return images;
    }
    let n_img = images.len();
    let n_vid = videos.len();
    let mut result = Vec::with_capacity(n_img + n_vid);
    let mut vid_iter = videos.into_iter();
    let mut vids_emitted = 0usize;

    for (img_i, img) in images.into_iter().enumerate() {
        // Target: how many videos should precede image `img_i`.
        let target = (2 * img_i + 1) * n_vid / (2 * n_img);
        while vids_emitted < target {
            match vid_iter.next() {
                Some(v) => { result.push(v); vids_emitted += 1; }
                None => break,
            }
        }
        result.push(img);
    }
    result.extend(vid_iter);
    result
}

pub fn fmt_duration(secs: f64) -> String {
    let total = secs.round() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection_planner::Segment;

    fn seg(score: f32) -> Segment {
        Segment {
            source_id: format!("s_{}", (score * 100.0) as i32),
            chapter_path: "x.mp4".into(),
            chapter_index: 1,
            in_chapter_start_secs: 0.0,
            duration_secs: 1.0,
            virtual_start_secs: 0.0,
            reason: String::new(),
            score: Some(score),
        }
    }

    #[test]
    fn energy_arc_places_strongest_in_the_middle() {
        // 5 picks, scores 0.1, 0.5, 0.9, 0.7, 0.3.
        // After arc placement, middle position should have the 0.9.
        let mut segs = vec![seg(0.1), seg(0.5), seg(0.9), seg(0.7), seg(0.3)];
        arrange_energy_arc(&mut segs);
        assert_eq!(segs.len(), 5);
        // Strongest at the middle.
        assert!((segs[2].score.unwrap() - 0.9).abs() < 1e-6);
        // Edges should be the weakest two.
        let edge_scores: Vec<f32> =
            vec![segs[0].score.unwrap(), segs[4].score.unwrap()];
        assert!(edge_scores.iter().all(|s| *s <= 0.5),
            "edges should be the weakest picks; got {edge_scores:?}");
    }

    #[test]
    fn energy_arc_handles_tiny_inputs() {
        // 0/1/2 picks: arc has no meaning, function should leave them as-is.
        let mut empty: Vec<Segment> = Vec::new();
        arrange_energy_arc(&mut empty);
        assert!(empty.is_empty());

        let mut one = vec![seg(0.5)];
        arrange_energy_arc(&mut one);
        assert_eq!(one.len(), 1);

        let mut two = vec![seg(0.1), seg(0.9)];
        let original_first_score = two[0].score;
        arrange_energy_arc(&mut two);
        assert_eq!(two.len(), 2);
        // Below the 3-pick threshold: order should be unchanged.
        assert_eq!(two[0].score, original_first_score);
    }

    #[test]
    fn strict_chronological_merges_images_with_videos_by_timestamp() {
        // Simulate the merge step that StrictChronological performs:
        // image segments share the synthetic `img:<stem>` source_id used in
        // run_pipeline, and their timestamps are injected into creation_for
        // alongside video chapter timestamps. After merge+sort, the order
        // must reflect real recording time, regardless of declaration order.
        fn vseg(source_id: &str, chapter: u32, in_chapter: f64) -> Segment {
            Segment {
                source_id: source_id.into(),
                chapter_path: "x.mp4".into(),
                chapter_index: chapter,
                in_chapter_start_secs: in_chapter,
                duration_secs: 2.0,
                virtual_start_secs: 0.0,
                reason: "video".into(),
                score: Some(0.7),
            }
        }
        fn iseg(stem: &str) -> Segment {
            Segment {
                source_id: format!("img:{stem}"),
                chapter_path: format!("{stem}.jpg").into(),
                chapter_index: 1,
                in_chapter_start_secs: 0.0,
                duration_secs: 2.0,
                virtual_start_secs: 0.0,
                reason: "image".into(),
                score: Some(0.5),
            }
        }

        let mut creation_for: std::collections::HashMap<(String, u32), Option<String>> =
            std::collections::HashMap::new();
        // Two videos: morning of day 1, midday of day 2.
        creation_for.insert(("vid1".into(), 1), Some("2026-05-20T09:00:00Z".into()));
        creation_for.insert(("vid2".into(), 1), Some("2026-05-21T13:00:00Z".into()));
        // Two photos straddling them: one earlier than vid1, one between them.
        creation_for.insert(("img:dawn".into(), 1), Some("2026-05-20T07:00:00Z".into()));
        creation_for.insert(("img:noon".into(), 1), Some("2026-05-20T12:00:00Z".into()));

        // Declaration order intentionally scrambled — strict-chrono must fix it.
        let videos = vec![vseg("vid2", 1, 0.0), vseg("vid1", 1, 0.0)];
        let images = vec![iseg("noon"), iseg("dawn")];
        let mut merged = videos;
        merged.extend(images.iter().cloned());
        merged.sort_by(|a, b| {
            let ka = abs_recording_secs(&creation_for, &a.source_id, a.chapter_index, a.in_chapter_start_secs);
            let kb = abs_recording_secs(&creation_for, &b.source_id, b.chapter_index, b.in_chapter_start_secs);
            ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
        });

        let order: Vec<&str> = merged.iter().map(|s| s.source_id.as_str()).collect();
        assert_eq!(
            order,
            vec!["img:dawn", "vid1", "img:noon", "vid2"],
            "strict_chronological must interleave images & videos by real timestamps"
        );
    }

    #[test]
    fn energy_arc_score_curve_is_arc_shaped() {
        // 9 picks with strictly increasing scores. After arc placement, the
        // score curve over output position should rise to a peak and fall
        // back: f(0) < f(K/2) and f(K-1) < f(K/2).
        let mut segs: Vec<Segment> = (1..=9).map(|i| seg(i as f32 / 10.0)).collect();
        arrange_energy_arc(&mut segs);
        let scores: Vec<f32> = segs.iter().map(|s| s.score.unwrap()).collect();
        let mid = scores.len() / 2;
        assert!(scores[mid] > scores[0],
            "peak should beat left edge; scores={scores:?}");
        assert!(scores[mid] > scores[scores.len() - 1],
            "peak should beat right edge; scores={scores:?}");
        // Strongest pick (0.9) lands at the middle.
        assert!((scores[mid] - 0.9).abs() < 1e-6);
    }
}
