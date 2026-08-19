//! Built-in `vibecut benchmark` — sweeps the per-source visual-analysis
//! worker count and (optionally) writes the recommendation to the user
//! settings file so subsequent `vibecut run` invocations pick it up.
//!
//! Self-contained: synthetic 1080p HEVC fixtures are generated with `ffmpeg
//! -f lavfi -i testsrc2` on first run and cached under
//! `<config_dir>/vibecut/bench_fixtures/`. No real footage required.
//!
//! Implementation strategy: recursively invoke `vibecut run` (via
//! `std::env::current_exe`) on a generated benchmark config, varying the
//! `VIBECUT_VISUAL_WORKERS` env var per iteration, and parse the
//! `[bench] phase=telemetry workers=N sources=M secs=S.SSS` line that
//! `main` already prints. Same code path as a normal run, no parallel
//! re-implementation to keep in sync.

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::user_settings;

#[derive(Debug, Clone)]
pub struct BenchmarkArgs {
    pub threads: Vec<u32>,
    pub repeats: u32,
    pub num_files: u32,
    pub duration_secs: u32,
    pub resolution: String, // "WIDTHxHEIGHT"
    pub codec: String,      // "libx264" | "libx265"
    pub apply: bool,
    pub force_fixtures: bool,
}

impl Default for BenchmarkArgs {
    fn default() -> Self {
        // 4K HEVC × 120 s × 8 files. NVDEC on Blackwell decodes 4K HEVC at
        // ~1000+ fps, so smaller fixtures finish in a few seconds — too
        // brief to discriminate thread counts cleanly AND too brief to
        // observe NVDEC utilisation in Task Manager / nvidia-smi. With these
        // defaults, each iteration covers ~28 800 frames (8 files × 30 fps
        // × 120 s) so even threads=6 takes ~20–40 s of actual decode work
        // — long enough to watch in real time and well clear of the 3 s
        // noise floor. First-time fixture generation takes ~15–20 min on a
        // typical box; cached afterward.
        Self {
            threads: vec![1, 2, 3, 4, 6],
            repeats: 1,
            num_files: 8,
            duration_secs: 120,
            resolution: "3840x2160".into(),
            codec: "libx265".into(),
            apply: false,
            force_fixtures: false,
        }
    }
}

pub fn run(args: BenchmarkArgs) -> Result<()> {
    if !ffmpeg_on_path() {
        anyhow::bail!("ffmpeg not found on PATH (install it and retry)");
    }
    let exe = std::env::current_exe().context("locating own executable")?;
    let exe_utf8 = Utf8PathBuf::from_path_buf(exe.clone())
        .map_err(|_| anyhow::anyhow!("vibecut path is not valid UTF-8: {:?}", exe))?;

    // Fixtures live alongside user settings so they persist across cargo
    // rebuilds and are shared between repos. Per-shape (NxD@WxH/codec) cache
    // tag means changing parameters doesn't invalidate other shapes' fixtures.
    let bench_dir = bench_fixtures_dir()?;
    let tag = format!(
        "n{}_d{}_{}_{}",
        args.num_files, args.duration_secs, args.resolution, args.codec
    );
    let fixture_dir = bench_dir.join(&tag);
    let signature = fixture_dir.join(".signature");

    if args.force_fixtures && fixture_dir.as_std_path().exists() {
        println!("(--force) deleting cached fixtures at {fixture_dir}");
        std::fs::remove_dir_all(fixture_dir.as_std_path())
            .with_context(|| format!("removing {fixture_dir}"))?;
    }

    if signature.as_std_path().exists() {
        println!("Reusing cached fixtures: {fixture_dir}");
    } else {
        std::fs::create_dir_all(fixture_dir.as_std_path())
            .with_context(|| format!("creating {fixture_dir}"))?;
        // Rough heads-up so users don't think the tool froze. 4K HEVC
        // generation runs at ~1× real-time on most CPUs, so an honest
        // estimate is `num_files * duration_secs` seconds total — bumped a
        // bit because preset=fast on libx265 is closer to 0.7-1.0× real-time.
        let est_secs = args.num_files as f32 * args.duration_secs as f32 * 1.2;
        println!(
            "Generating {} x {}s {} {} test files in {} ...",
            args.num_files, args.duration_secs, args.resolution, args.codec, fixture_dir
        );
        if est_secs > 60.0 {
            println!(
                "  (one-time fixture build; estimated ~{:.0}s for first generation, then cached)",
                est_secs
            );
        }
        for i in 1..=args.num_files {
            let name = format!("src{:02}.MP4", i);
            let out = fixture_dir.join(&name);
            print!("  [{i}/{}] {name} ... ", args.num_files);
            std::io::Write::flush(&mut std::io::stdout()).ok();
            generate_fixture(&out, &args, i)?;
            println!("ok");
        }
        std::fs::write(signature.as_std_path(), &tag)
            .with_context(|| format!("writing signature {signature}"))?;
    }

    // Tiny project pointing at the fixtures. The render after telemetry is
    // fast and its time is shed across iterations consistently — only the
    // [bench] line we parse is relevant for the comparison.
    let bench_toml = fixture_dir.join("bench.toml");
    let workspace_dir = fixture_dir.join("workspace");
    let output_path = fixture_dir.join("out.mp4");
    let cache_dir = output_path
        .parent()
        .map(|p| p.join(".vibecut-cache"))
        .unwrap_or_else(|| fixture_dir.join(".vibecut-cache"));
    write_bench_toml(&bench_toml, &fixture_dir, &workspace_dir, &output_path, &args)?;

    println!();
    println!("Benchmark plan:");
    println!(
        "  fixtures: {} x {}s {} {}",
        args.num_files, args.duration_secs, args.resolution, args.codec
    );
    println!(
        "  threads:  {}",
        args.threads
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("  repeats:  {} (best of N reported)", args.repeats);
    println!();
    println!("To watch the actual decode load while this runs:");
    println!("  - Task Manager: GPU pane, switch one of the four small charts to \"Video Decode\"");
    println!("    (the headline \"GPU\" number averages across engines and is misleading)");
    println!("  - or: nvidia-smi dmon -s u    # `dec` column = NVDEC utilisation");
    println!();

    let mut results: Vec<(u32, f32)> = Vec::new();
    for &n in &args.threads {
        let mut best = f32::INFINITY;
        for r in 1..=args.repeats {
            // Cold cache between iterations so each thread count is a fair
            // fight: visual analysis and segment normalize both re-run.
            for d in [&cache_dir, &workspace_dir] {
                let p = d.as_std_path();
                if p.exists() {
                    let _ = std::fs::remove_dir_all(p);
                }
            }
            // Print the "running" line BEFORE the iteration so the user can
            // correlate the GPU monitor with the active thread count.
            print!("  [threads={n} run={r}/{}] running...", args.repeats);
            std::io::Write::flush(&mut std::io::stdout()).ok();
            let started = Instant::now();
            let secs = invoke_bench_run(&exe_utf8, &bench_toml, n)?;
            let wall = started.elapsed().as_secs_f32();
            // Carriage-return overwrite so the result lands on the same line.
            println!(
                "\r  [threads={n} run={r}/{}] {:>7.2}s telemetry, {:>7.2}s wall      ",
                args.repeats, secs, wall
            );
            if secs < best {
                best = secs;
            }
        }
        results.push((n, best));
    }

    println!();
    println!("Results (best of {} per thread count):", args.repeats);
    println!();
    let baseline = results.first().map(|(_, s)| *s).unwrap_or(1.0);
    let (best_n, best_secs) = results
        .iter()
        .copied()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or((args.threads[0], 0.0));
    println!("  threads | telemetry | speedup vs threads={}", args.threads[0]);
    println!("  --------+-----------+---------------------");
    for (n, secs) in &results {
        let sp = if *secs > 0.0 { baseline / *secs } else { 0.0 };
        let marker = if *n == best_n { " <-- best" } else { "" };
        println!(
            "  {:>7} | {:>8.2}s | {:>5.2}x{}",
            n, secs, sp, marker
        );
    }

    // Lowest thread count within 5% of the absolute best — saves subprocess
    // overhead when the curve has plateaued.
    let recommend = results
        .iter()
        .find(|(_, s)| *s <= best_secs * 1.05)
        .map(|(n, _)| *n)
        .unwrap_or(best_n);

    // Noise-floor check. Per-iteration ffmpeg startup + filter-graph compile
    // is ~0.2–0.5 s; below ~3 s of measured work the run-to-run variance
    // swamps the signal we're trying to read. In that regime even small
    // OS-scheduling jitter can flip "is 3 threads faster than 6?", and the
    // GPU sits well under saturation so it's not even the right question.
    const NOISE_FLOOR_SECS: f32 = 3.0;
    let noisy = best_secs < NOISE_FLOOR_SECS;
    println!();
    if noisy {
        println!(
            "Recommendation: visual_workers = {recommend}  [LOW CONFIDENCE]"
        );
        println!(
            "  Best iteration was {:.2}s — below the {:.1}s noise floor for this benchmark.",
            best_secs, NOISE_FLOOR_SECS
        );
        println!(
            "  At this work level subprocess startup + scheduler jitter dominate the measurement."
        );
        println!(
            "  Re-run with heavier fixtures so each iteration takes >3s of real decode work, e.g.:"
        );
        println!(
            "    vibecut benchmark --resolution 3840x2160 --duration-secs 90 --num-files 8"
        );
        println!();
        println!("  Skipping --apply for safety even if it was passed.");
    } else {
        println!("Recommendation: visual_workers = {recommend}");
        if recommend != best_n {
            println!(
                "  ({best_n} is the absolute fastest, but {recommend} is within 5% — \
                 fewer subprocesses, same speed)"
            );
        }
    }

    if args.apply && !noisy {
        let mut settings = user_settings::load();
        settings.visual_workers = Some(recommend);
        settings.benchmark_run_at = Some(now_iso8601());
        settings.benchmark_machine = Some(detect_machine_summary());
        let path = user_settings::save(&settings)?;
        println!();
        println!("Wrote {path}");
        println!("Future `vibecut run` invocations will use {recommend} workers.");
    } else if !noisy {
        println!();
        println!("Re-run with --apply to write this to your user settings file.");
        println!("Or set the env var ad-hoc:  VIBECUT_VISUAL_WORKERS={recommend}");
    }
    // (noisy run already printed its own guidance — no second nudge here)

    Ok(())
}

// ---------- internals ----------

fn invoke_bench_run(exe: &Utf8Path, config: &Utf8Path, workers: u32) -> Result<f32> {
    let output = Command::new(exe.as_std_path())
        .arg("run")
        .arg(config.as_str())
        .env("VIBECUT_VISUAL_WORKERS", workers.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("spawning {exe} run {config}"))?;
    if !output.status.success() {
        let tail: String = String::from_utf8_lossy(&output.stderr)
            .lines()
            .rev()
            .take(15)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n  ");
        anyhow::bail!(
            "bench run failed (exit {}):\n  {tail}",
            output.status.code().unwrap_or(-1)
        );
    }
    parse_bench_secs(&output.stdout)
}

fn parse_bench_secs(stdout_bytes: &[u8]) -> Result<f32> {
    let stdout = String::from_utf8_lossy(stdout_bytes);
    for line in stdout.lines() {
        // Format: `[bench] phase=telemetry workers=N sources=M secs=S.SSS`
        if let Some(rest) = line.strip_prefix("[bench] ") {
            if let Some(secs) = rest.split_whitespace().find_map(|tok| tok.strip_prefix("secs=")) {
                return secs
                    .parse::<f32>()
                    .with_context(|| format!("parsing bench secs from {tok:?}", tok = secs));
            }
        }
    }
    anyhow::bail!("no [bench] line in subprocess output (incompatible build?)");
}

fn write_bench_toml(
    bench_toml: &Utf8Path,
    fixture_dir: &Utf8Path,
    workspace_dir: &Utf8Path,
    output_path: &Utf8Path,
    args: &BenchmarkArgs,
) -> Result<()> {
    // Forward slashes throughout — TOML strings are easier on Windows that way
    // and ffmpeg accepts both.
    let folder = fixture_dir.as_str().replace('\\', "/");
    let workspace = workspace_dir.as_str().replace('\\', "/");
    let out = output_path.as_str().replace('\\', "/");
    let body = format!(
        r#"schema_version = 1

[project]
name            = "VisualWorkerBench"
target_duration = "5s"
fps             = 30
seed            = 1
temp_dir        = "{workspace}"

[[outputs]]
path       = "{out}"
resolution = "{res}"
encoder    = "auto"
crf        = 28

[input]
folder  = "{folder}"
include = ["src*.MP4"]

[selection]
min_clip = "500ms"
max_clip = "1s"
"#,
        res = args.resolution,
    );
    std::fs::write(bench_toml.as_std_path(), body)
        .with_context(|| format!("writing {bench_toml}"))?;
    Ok(())
}

fn generate_fixture(out: &Utf8Path, args: &BenchmarkArgs, idx: u32) -> Result<()> {
    let vsrc = format!(
        "testsrc2=size={res}:rate=30:duration={d}",
        res = args.resolution,
        d = args.duration_secs
    );
    let asrc = format!(
        "sine=frequency={f}:duration={d}",
        f = idx * 110,
        d = args.duration_secs
    );
    let status = Command::new("ffmpeg")
        .args([
            "-y", "-hide_banner", "-loglevel", "error",
            "-f", "lavfi", "-i", &vsrc,
            "-f", "lavfi", "-i", &asrc,
            "-c:v", &args.codec,
            "-preset", "fast",
            "-crf", "22",
            "-c:a", "aac",
            "-shortest",
            out.as_str(),
        ])
        .status()
        .with_context(|| format!("spawning ffmpeg for {out}"))?;
    if !status.success() {
        anyhow::bail!(
            "ffmpeg failed generating fixture {out} (exit {})",
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

fn bench_fixtures_dir() -> Result<Utf8PathBuf> {
    // Same parent as user settings, with a `bench_fixtures/` subdir.
    let path = user_settings::settings_path()
        .context("could not resolve config dir (APPDATA / HOME unset?)")?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("settings path has no parent: {path}"))?;
    Ok(parent.join("bench_fixtures"))
}

fn ffmpeg_on_path() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Minimal ISO-8601 without external deps. Days since epoch via Hinnant.
    let days = (secs / 86400) as i64;
    let secs_in_day = (secs % 86400) as u32;
    let (y, mo, d) = civil_from_days(days);
    let h = secs_in_day / 3600;
    let m = (secs_in_day % 3600) / 60;
    let s = secs_in_day % 60;
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

// Howard Hinnant's "civil_from_days" — std-only date math.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64 + era * 400) as i32;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn detect_machine_summary() -> String {
    use crate::video_renderer::HwSupport;
    HwSupport::detect("ffmpeg")
        .map(|s| s.summary())
        .unwrap_or_else(|_| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bench_secs_from_subprocess_output() {
        let out = b"some line\n[bench] phase=telemetry workers=4 sources=6 secs=12.345\nmore\n";
        assert!((parse_bench_secs(out).unwrap() - 12.345).abs() < 1e-3);
    }

    #[test]
    fn missing_bench_line_errors() {
        let out = b"no marker here\n";
        assert!(parse_bench_secs(out).is_err());
    }

    #[test]
    fn civil_from_days_matches_known_dates() {
        // 2026-05-07 = 20580 days since 1970-01-01
        let (y, m, d) = civil_from_days(20580);
        assert_eq!((y, m, d), (2026, 5, 7));
    }
}
