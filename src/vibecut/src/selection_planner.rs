//! Selection planner — the pipeline's brain.
//!
//! Takes the output of [`crate::media_probe`] (per-file metadata) and
//! [`crate::project_config`] (user preferences), then produces an ordered
//! list of [`Segment`]s that the renderer will turn into the final video.
//!
//! # Phases of planning
//!
//! 1. **Source assembly** ([`group_into_sources`]) — GoPro chapter files
//!    of one recording (`GX{NN}{ID}.MP4` / `GH{NN}{ID}.MP4`, or
//!    `GOPR{ID}.MP4` + `GP{NN}{ID}.MP4` on older cameras) in one folder are
//!    stitched into one virtual [`Source`] with multiple [`Chapter`]s. Standalone clips
//!    become single-chapter sources; `.LRV` / `.THM` sidecars are dropped.
//! 2. **Telemetry harvesting** ([`extract_telemetry`] /
//!    [`extract_telemetry_with`] → [`SourceTelemetry`]) — for each source,
//!    extract GPMF accelerometer / HiLight tags, scan the MP4 udta atom for
//!    legacy HMMT, and (optionally) run [`crate::visual_analysis`] for
//!    per-frame sharpness / exposure / motion. Visual results are cached
//!    via [`crate::disk_cache`] so re-runs are seconds, not minutes.
//! 3. **Score curve composition** ([`build_score_curve`]) — combines the
//!    weighted signals into a single 4 Hz score curve per source, applies
//!    sharpness / exposure percentile floors that zero out bad frames.
//! 4. **Multi-source diverse selection** ([`select_multi_source`]) —
//!    delegates to [`crate::selection_scoring::pick_diverse`] with a
//!    per-source cap (auto or override) and within-source spacing,
//!    returning [`Segment`]s sorted chronologically by recording time.
//! 5. **Duck-window selection** ([`select_duck_windows`] →
//!    [`DuckWindow`]) — for each segment, decode source audio + optionally
//!    run Silero VAD; pick the top-N most "interesting" segments to fade
//!    the soundtrack down on. Lengths vary 4–10 s by score.
//! 6. **Manifest emission** ([`Manifest`]) — JSON sidecar describing every
//!    selected segment, duck window, and source for full reproducibility.

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use crate::media_probe::ProbedFile;

// ---------- Source / Chapter model ----------

/// One or more chapter files that together form one continuous take.
/// For non-GoPro inputs and standalone clips, `chapters` has length 1.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct Source {
    /// Recording id. For GoPro: the 4-digit recording number (`0042` for
    /// `GX010042.MP4` or `GOPR0042.MP4`). For standalone clips: the file
    /// stem. When two sources would share an id (two cameras both at
    /// recording 0042, or two cards), both get a lowercase id made from the
    /// first chapter's file stem (`gopr0042`, `gx010042`), prefixed with
    /// the first folder that tells them apart if needed (`carda-gx010042`),
    /// numbered as a last resort (`gx010042-2`).
    pub id: String,
    pub chapters: Vec<Chapter>,
    pub total_duration_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct Chapter {
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub path: Utf8PathBuf,
    /// 1-based chapter index (1, 2, 3, ... for GoPro chapters; always 1 otherwise).
    pub chapter_index: u32,
    pub duration_secs: f64,
    pub creation_time: Option<String>,
    /// `None` when probed in metadata-only mode (the render pipeline doesn't
    /// need hashes; only `probe`/cache do).
    pub blake3: Option<String>,
    /// The chapter's colour description (bit depth, transfer, primaries) as
    /// probed — what decided whether normalize tone-mapped it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub color: Option<crate::media_probe::ColorInfo>,
}

impl Source {
    /// Resolve a virtual offset (across the concatenated chapter timeline) to
    /// the chapter that contains it and the in-chapter offset.
    #[allow(dead_code)] // public timeline-resolution helper; current callers use `snap_to_chapter` directly
    pub fn locate(&self, virtual_offset_secs: f64) -> Option<(&Chapter, f64)> {
        let mut acc = 0.0_f64;
        for ch in &self.chapters {
            let end = acc + ch.duration_secs;
            if virtual_offset_secs < end {
                return Some((ch, virtual_offset_secs - acc));
            }
            acc = end;
        }
        None
    }
}

// ---------- GoPro chapter-stitching ----------

/// Parse a GoPro chapter filename. Returns `(chapter_index, recording_id)`.
///
/// Both naming schemes are understood (see
/// [`crate::media_probe::parse_gopro_name`]): `GX{NN}{XXXX}.MP4` /
/// `GH{NN}{XXXX}.MP4` where `NN` is the chapter, and the older
/// `GOPR{XXXX}.MP4` (chapter 1) followed by `GP{NN}{XXXX}.MP4` (chapter
/// `NN + 1`). Examples:
/// `GX010384` → (1, "0384"), `GX020384` → (2, "0384"), `GOPR0384` →
/// (1, "0384"), `GP010384` → (2, "0384"). Proxies (`.LRV`, `GL…`) and
/// thumbnails (`.THM`) are not chapters.
pub fn parse_gopro_filename(name: &str) -> Option<(u32, String)> {
    crate::media_probe::parse_gopro_name(name).map(|n| (n.chapter, n.recording))
}

/// Group probed files into Sources, stitching GoPro chapters of one
/// recording. Chapters group by folder + naming family + recording number,
/// so an older camera's `GOPR0042` and a newer one's `GX010042` in one
/// folder (or two cards both at recording 0042) stay separate takes; within a take they
/// sort by chapter index (`GOPR` before `GP01`). Camera sidecars
/// (`.LRV` proxies, `.THM` thumbnails) are dropped. Source ids are unique.
pub fn group_into_sources(files: &[ProbedFile]) -> Vec<Source> {
    use std::collections::BTreeMap;

    // (folder, family, recording) → chapters.
    type Key<'a> = (&'a str, String, String);
    let mut groups: BTreeMap<Key<'_>, Vec<(u32, &ProbedFile)>> = BTreeMap::new();
    let mut other: Vec<&ProbedFile> = Vec::new();

    for f in files {
        let name = f.path.file_name().unwrap_or("");
        if crate::media_probe::is_gopro_sidecar(name) {
            tracing::debug!(file = %f.path, "skipping camera proxy / thumbnail");
            continue;
        }
        match crate::media_probe::parse_gopro_name(name) {
            Some(n) => {
                let folder = f.path.parent().map(|p| p.as_str()).unwrap_or("");
                groups
                    .entry((folder, n.family, n.recording))
                    .or_default()
                    .push((n.chapter, f));
            }
            None => other.push(f),
        }
    }

    let mut sources = Vec::new();

    for ((_, _, recording), mut chs) in groups {
        chs.sort_by(|(a, fa), (b, fb)| a.cmp(b).then_with(|| fa.path.cmp(&fb.path)));
        let chapters: Vec<Chapter> = chs
            .into_iter()
            .map(|(idx, f)| chapter_from(f, idx))
            .collect();
        let total = chapters.iter().map(|c| c.duration_secs).sum();
        sources.push(Source {
            id: recording,
            chapters,
            total_duration_secs: total,
        });
    }

    for f in other {
        let id = f
            .path
            .file_stem()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".into());
        let dur = f.media.as_ref().map(|m| m.duration_secs).unwrap_or(0.0);
        sources.push(Source {
            id,
            chapters: vec![chapter_from(f, 1)],
            total_duration_secs: dur,
        });
    }

    make_ids_unique(&mut sources);
    // Stable-ish ordering: by id ascending.
    sources.sort_by(|a, b| a.id.cmp(&b.id));
    sources
}

/// Whether a source id written somewhere (a `source_weights` key, a
/// `[[multi_angle.groups]]` master / member / offset key, a "more of / less
/// of" feedback request) names the source `id`. Letter case doesn't count,
/// the same rule [`group_into_sources`] uses to decide that two ids clash
/// (so no two sources ever match one key): "less of clip dji_0001", which
/// the UI's tell-it grammar lowercases, finds the source `DJI_0001`. Keys
/// keep whatever case they were written with.
pub fn same_source_id(written: &str, id: &str) -> bool {
    written.eq_ignore_ascii_case(id)
}

/// Give every source a distinct id. Ids are what `[selection]
/// source_weights`, `[multi_angle]` groups and the manifest refer to, so a
/// clash (recording 0042 on two cameras or two cards) must not silently
/// alias two takes. Ids that don't clash are left alone. Ids that clash
/// (compared ignoring letter case) are replaced by an [`id_slug`], built
/// in up to three steps until they are unique:
///
/// 1. the first chapter's file stem: `gopr0042`, `gx010042`;
/// 2. prefixed with the first folder that differs between the clashing
///    sources: `carda-gx010042` / `cardb-gx010042` for two cards copied with
///    the same `DCIM/100GOPRO` layout, `day2-gx010042` for a subfolder;
/// 3. a numeric suffix in path order (`gx010042`, `gx010042-2`) when even
///    the folders don't tell them apart.
///
/// Generated ids only use `a-z`, `0-9`, `_` and `-`, so they are typed
/// back as they read: the UI's "more of / less of clip …" grammar
/// (`tellit.ts`) lowercases its input and only captures those characters.
fn make_ids_unique(sources: &mut [Source]) {
    use std::collections::{HashMap, HashSet};

    fn first_stem(s: &Source) -> String {
        s.chapters
            .first()
            .and_then(|c| c.path.file_stem())
            .map(str::to_string)
            .unwrap_or_else(|| s.id.clone())
    }
    fn folder_parts(s: &Source) -> Vec<&str> {
        s.chapters
            .first()
            .and_then(|c| c.path.parent())
            .map(|p| p.components().map(|c| c.as_str()).collect())
            .unwrap_or_default()
    }
    fn clashing(sources: &[Source]) -> Vec<bool> {
        let mut n: HashMap<String, usize> = HashMap::new();
        for s in sources {
            *n.entry(s.id.to_ascii_lowercase()).or_default() += 1;
        }
        sources.iter().map(|s| n[&s.id.to_ascii_lowercase()] > 1).collect()
    }

    // 1. File stem.
    let clash = clashing(sources);
    if !clash.contains(&true) {
        return;
    }
    for (s, c) in sources.iter_mut().zip(clash) {
        if c {
            s.id = id_slug(&first_stem(s));
        }
    }

    // 2. The first folder that differs within each clashing group.
    let clash = clashing(sources);
    if !clash.contains(&true) {
        return;
    }
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for i in (0..sources.len()).filter(|&i| clash[i]) {
        groups.entry(sources[i].id.to_ascii_lowercase()).or_default().push(i);
    }
    let mut renames: Vec<(usize, String)> = Vec::new();
    for members in groups.values() {
        let dirs: Vec<Vec<&str>> = members.iter().map(|&i| folder_parts(&sources[i])).collect();
        let common = (0..)
            .take_while(|&k| dirs[0].get(k).is_some_and(|c| dirs.iter().all(|d| d.get(k) == Some(c))))
            .count();
        for (&i, d) in members.iter().zip(&dirs) {
            let label = d.get(common).map(|c| id_slug(c)).unwrap_or_default();
            if !label.is_empty() {
                renames.push((i, format!("{label}-{}", sources[i].id)));
            }
        }
    }
    for (i, id) in renames {
        sources[i].id = id;
    }

    // 3. Still clashing (same folders, same file name): number them in
    // path order, skipping any id already taken.
    let clash = clashing(sources);
    let mut used: HashSet<String> = sources
        .iter()
        .zip(&clash)
        .filter(|(_, c)| !**c)
        .map(|(s, _)| s.id.to_ascii_lowercase())
        .collect();
    let mut order: Vec<usize> = (0..sources.len()).filter(|&i| clash[i]).collect();
    order.sort_by_key(|&i| sources[i].chapters.first().map(|c| c.path.clone()));
    for i in order {
        let base = sources[i].id.clone();
        let mut id = base.clone();
        let mut n = 1;
        while used.contains(&id.to_ascii_lowercase()) {
            n += 1;
            id = format!("{base}-{n}");
        }
        used.insert(id.to_ascii_lowercase());
        sources[i].id = id;
    }
}

/// A source id made from a file or folder name: lowercase ASCII letters,
/// digits, `_` and `-`; every other run of characters becomes one `-`
/// (`Card A` → `card-a`). Falls back to `take` when nothing is left.
fn id_slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-');
    if out.is_empty() { "take".into() } else { out.to_string() }
}

fn chapter_from(f: &ProbedFile, idx: u32) -> Chapter {
    Chapter {
        path: f.path.clone(),
        chapter_index: idx,
        duration_secs: f.media.as_ref().map(|m| m.duration_secs).unwrap_or(0.0),
        creation_time: f.media.as_ref().and_then(|m| m.creation_time.clone()),
        blake3: f.blake3.clone(),
        color: f
            .media
            .as_ref()
            .and_then(|m| m.video.as_ref())
            .map(|v| v.color.clone()),
    }
}

// ---------- Segment selection (evenly spaced, single source) ----------

/// One planned cut. `chapter_path`/`in_chapter_start_secs` are render-ready.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct Segment {
    pub source_id: String,
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub chapter_path: Utf8PathBuf,
    pub chapter_index: u32,
    /// Offset within the chapter file (not the virtual stitched timeline).
    pub in_chapter_start_secs: f64,
    pub duration_secs: f64,
    /// Where this segment falls on the *virtual* (stitched) source timeline.
    pub virtual_start_secs: f64,
    /// Why we picked this segment (e.g. "even-spaced" for the simple
    /// fallback, or a full score breakdown for score-driven selection).
    pub reason: String,
    /// Composite score the picker assigned (`hilight + accel + motion + ...`).
    /// Optional for backwards-compat with old manifests; populated by
    /// `select_multi_source` and the beat-burst path. Used by
    /// `[selection].order = "energy_arc"` to rank picks for arc placement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
}

/// Pick the longest source, place N evenly-spaced segments
/// across its virtual timeline, and snap each to fit within a single chapter
/// (so the render pipeline can stay one-input-one-output per segment).
///
/// `clip_length_secs` is the desired length per segment. The *number* of
/// segments is determined by `target_duration_secs / clip_length_secs`.
#[allow(dead_code)] // historical fallback, kept as a documented entry point
pub fn select_evenly_spaced(
    sources: &[Source],
    target_duration_secs: f64,
    clip_length_secs: f64,
) -> Vec<Segment> {
    let Some(source) = sources
        .iter()
        .max_by(|a, b| {
            a.total_duration_secs
                .partial_cmp(&b.total_duration_secs)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    else {
        return Vec::new();
    };

    if clip_length_secs <= 0.0 || target_duration_secs <= 0.0 {
        return Vec::new();
    }

    let n = ((target_duration_secs / clip_length_secs).round() as usize).max(1);
    let usable = (source.total_duration_secs - clip_length_secs).max(0.0);
    if usable <= 0.0 || source.total_duration_secs < clip_length_secs {
        // Source too short to take even one full clip.
        return Vec::new();
    }

    let mut segments = Vec::with_capacity(n);
    for i in 0..n {
        let virtual_start = if n == 1 {
            usable / 2.0
        } else {
            i as f64 * usable / (n - 1) as f64
        };
        if let Some(seg) = snap_to_chapter(source, virtual_start, clip_length_secs) {
            segments.push(seg);
        }
    }
    segments
}

pub fn snap_to_chapter(
    source: &Source,
    virtual_start: f64,
    duration: f64,
) -> Option<Segment> {
    let mut acc = 0.0_f64;
    for ch in &source.chapters {
        let chapter_start = acc;
        let chapter_end = acc + ch.duration_secs;
        if virtual_start >= chapter_start && virtual_start < chapter_end {
            // Does the clip fit before the chapter boundary?
            let in_chapter = virtual_start - chapter_start;
            let in_chapter_start = if in_chapter + duration <= ch.duration_secs {
                in_chapter
            } else if ch.duration_secs >= duration {
                // Slide back to fit.
                ch.duration_secs - duration
            } else {
                // Chapter is shorter than the clip — skip.
                return None;
            };
            return Some(Segment {
                source_id: source.id.clone(),
                chapter_path: ch.path.clone(),
                chapter_index: ch.chapter_index,
                in_chapter_start_secs: in_chapter_start,
                duration_secs: duration,
                virtual_start_secs: chapter_start + in_chapter_start,
                reason: "even-spaced".into(),
                score: None,
            });
        }
        acc = chapter_end;
    }
    None
}

// ---------- Telemetry extraction ----------

/// Per-source telemetry signals harvested for scoring. All time-axes are
/// expressed in the *virtual* (stitched) source timeline.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // `source_id` is part of the public telemetry record; internal callers identify by index
pub struct SourceTelemetry {
    pub source_id: String,
    /// HiLight tag times (seconds since virtual source start).
    pub hilights_secs: Vec<f32>,
    /// Accelerometer magnitude samples plus a parallel time vector.
    /// `accel_mag[i]` paired with `accel_time_secs[i]`.
    pub accel_mag: Vec<f32>,
    pub accel_time_secs: Vec<f32>,
    pub total_duration_secs: f32,
    // ---- Visual signals (one entry per analyzed frame) ----
    pub visual_fps_hz: f32,
    /// Wall-clock time in the virtual source timeline.
    pub visual_time_secs: Vec<f32>,
    pub visual_sharpness: Vec<f32>,
    pub visual_exposure: Vec<f32>,
    pub visual_motion: Vec<f32>,
}

/// Options for `extract_telemetry`. `with_visual = false` skips the (expensive)
/// per-frame Laplacian/exposure/motion analysis — useful when scoring weights
/// don't need it or for fast dev iteration.
#[derive(Debug, Clone)]
pub struct ExtractOpts {
    pub with_visual: bool,
    pub visual_fps: f32,
    pub visual_w: u32,
    pub visual_h: u32,
    /// Use NVDEC (`-hwaccel cuda`) when decoding for visual analysis.
    pub visual_hwaccel_cuda: bool,
    /// Use `scale_cuda` GPU-side downscale (only effective when
    /// `visual_hwaccel_cuda` is also true). When the host ffmpeg lacks
    /// `scale_cuda`, callers should leave this at `false` and let the CPU
    /// scale path run.
    pub visual_use_scale_cuda: bool,
    /// Optional persistent cache. When `Some`, visual analysis results are
    /// keyed by (file_size, mtime, params) and skipped on hit.
    pub cache: Option<crate::disk_cache::Cache>,
    /// The colour filters ffmpeg has (`zscale` / `tonemap` / `colorspace`):
    /// each chapter's frames are read through the conversion normalize
    /// will apply (`plan_color_conversion` on the chapter's colour), so HDR
    /// footage is scored on its tone-mapped picture. The default (none)
    /// leaves every chapter as decoded.
    pub color_support: crate::video_renderer::HwSupport,
}

impl Default for ExtractOpts {
    fn default() -> Self {
        Self {
            with_visual: true,
            visual_fps: crate::visual_analysis::DEFAULT_FPS,
            visual_w: crate::visual_analysis::DEFAULT_W,
            visual_h: crate::visual_analysis::DEFAULT_H,
            visual_hwaccel_cuda: false,
            visual_use_scale_cuda: false,
            cache: None,
            color_support: crate::video_renderer::HwSupport::default(),
        }
    }
}

/// Extract every chapter's GPMF + udta HMMT atom for `source` and assemble
/// a single `SourceTelemetry` aligned to the virtual stitched timeline.
#[allow(dead_code)] // convenience wrapper; production code passes explicit `ExtractOpts`
pub fn extract_telemetry(source: &Source) -> SourceTelemetry {
    extract_telemetry_with(source, &ExtractOpts::default())
}

pub fn extract_telemetry_with(source: &Source, opts: &ExtractOpts) -> SourceTelemetry {
    let mut tel = SourceTelemetry {
        source_id: source.id.clone(),
        total_duration_secs: source.total_duration_secs as f32,
        ..Default::default()
    };

    let mut chapter_offset = 0.0_f32;
    for ch in &source.chapters {
        let dur = ch.duration_secs as f32;

        // GPMF: accelerometer + HiLight stream.
        match crate::media_probe::extract_gpmf_bytes(&ch.path) {
            Ok(Some(bytes)) => {
                let accel = crate::gopro_metadata::extract_accel(&bytes);
                let n_payloads = accel.payload_lens.len().max(1) as f32;
                let secs_per_payload = if accel.payload_lens.is_empty() {
                    1.0
                } else {
                    dur / n_payloads
                };
                let mut sample_idx = 0usize;
                for (p, &len) in accel.payload_lens.iter().enumerate() {
                    let payload_start = chapter_offset + (p as f32) * secs_per_payload;
                    for j in 0..len as usize {
                        if sample_idx >= accel.magnitude.len() {
                            break;
                        }
                        let frac = if len > 0 { j as f32 / len as f32 } else { 0.0 };
                        let t = payload_start + frac * secs_per_payload;
                        tel.accel_time_secs.push(t);
                        tel.accel_mag.push(accel.magnitude[sample_idx]);
                        sample_idx += 1;
                    }
                }

                let gpmf_hl = crate::gopro_metadata::extract_hilights_from_gpmf(&bytes);
                for ms in gpmf_hl {
                    tel.hilights_secs
                        .push(chapter_offset + ms as f32 / 1000.0);
                }
            }
            Ok(None) => {
                tracing::debug!(file = %ch.path, "no GPMF stream");
            }
            Err(e) => {
                tracing::warn!(file = %ch.path, error = %e, "GPMF extraction failed");
            }
        }

        // Legacy HiLight tags in the MP4 udta atom (HMMT). Cheap: only scan
        // 4 MB at the head + 32 MB at the tail (moov location varies).
        match crate::media_probe::read_head_and_tail(&ch.path, 4 * 1024 * 1024, 32 * 1024 * 1024) {
            Ok(window) => {
                let hmmt = crate::gopro_metadata::parse_hmmt_from_mp4(&window);
                for ms in hmmt {
                    tel.hilights_secs
                        .push(chapter_offset + ms as f32 / 1000.0);
                }
            }
            Err(e) => tracing::warn!(file = %ch.path, error = %e, "HMMT scan failed"),
        }

        // Visual analysis: one ffmpeg decode per chapter, streaming.
        // Results cache by (size, mtime, params) when configured.
        if opts.with_visual {
            let color = ch
                .color
                .as_ref()
                .map(|c| crate::video_renderer::plan_color_conversion(c, &opts.color_support))
                .unwrap_or_default();
            let visual_result = if let Some(cache) = &opts.cache {
                crate::visual_analysis::analyze_visual_cached(
                    &ch.path,
                    opts.visual_fps,
                    opts.visual_w,
                    opts.visual_h,
                    opts.visual_hwaccel_cuda,
                    opts.visual_use_scale_cuda,
                    &color,
                    cache,
                )
            } else {
                crate::visual_analysis::analyze_visual(
                    &ch.path,
                    opts.visual_fps,
                    opts.visual_w,
                    opts.visual_h,
                    opts.visual_hwaccel_cuda,
                    opts.visual_use_scale_cuda,
                    &color,
                )
            };
            match visual_result {
                Ok(vis) => {
                    tel.visual_fps_hz = vis.sample_rate_hz;
                    for i in 0..vis.frame_count {
                        let t = chapter_offset + (i as f32) / vis.sample_rate_hz;
                        if t > chapter_offset + dur + 0.5 {
                            // Defensive: ffmpeg sometimes emits a few extra frames due
                            // to fps filter rounding. Drop tail beyond the chapter.
                            break;
                        }
                        tel.visual_time_secs.push(t);
                        tel.visual_sharpness.push(vis.sharpness[i]);
                        tel.visual_exposure.push(vis.exposure[i]);
                        tel.visual_motion.push(vis.motion[i]);
                    }
                }
                Err(e) => {
                    tracing::warn!(file = %ch.path, error = %e, "visual analysis failed");
                }
            }
        }

        chapter_offset += dur;
    }

    tel.hilights_secs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    tel.hilights_secs.dedup_by(|a, b| (*a - *b).abs() < 0.5);
    tel
}

// ---------- Score-driven selection ----------

/// Build a combined score curve for a source using the configured weights.
/// Returns the curve sampled at `sample_rate` Hz over the source's full duration.
pub fn build_score_curve(
    tel: &SourceTelemetry,
    weights: &crate::project_config::Scoring,
    sample_rate: f32,
) -> crate::selection_scoring::ScoreCurve {
    let mut combined = crate::selection_scoring::ScoreCurve::zeros(tel.total_duration_secs, sample_rate);

    // HiLight contribution.
    if weights.hilight_tags > 0.0 && !tel.hilights_secs.is_empty() {
        let ms: Vec<u32> = tel.hilights_secs.iter().map(|t| (t * 1000.0) as u32).collect();
        let hl =
            crate::selection_scoring::hilight_curve(&ms, tel.total_duration_secs, sample_rate);
        combined.add_weighted(&hl, weights.hilight_tags);
    }

    // Accelerometer contribution.
    if weights.accel_peaks > 0.0 && !tel.accel_mag.is_empty() {
        let times = tel.accel_time_secs.clone();
        let acc = crate::selection_scoring::accel_curve(
            &tel.accel_mag,
            move |i| times.get(i).copied().unwrap_or(0.0),
            tel.total_duration_secs,
            sample_rate,
        );
        combined.add_weighted(&acc, weights.accel_peaks);
    }

    // Visual motion contribution. Same relative-spike treatment as
    // accelerometer but on frame-difference MSD: surfaces visual action
    // (camera motion, scene changes, fast subjects) above the baseline.
    if weights.motion_peaks > 0.0 && !tel.visual_motion.is_empty() {
        let mc = crate::selection_scoring::spike_curve(
            &tel.visual_motion,
            &tel.visual_time_secs,
            tel.total_duration_secs,
            sample_rate,
            10.0, // baseline window seconds
            0.5,  // smooth window seconds
        );
        combined.add_weighted(&mc, weights.motion_peaks);
    }

    // Floors: zero out moments where the source content is too blurry
    // or too clipped. These act *after* the additive contributions so a great
    // accel spike on a blurry frame is correctly suppressed.
    if !tel.visual_sharpness.is_empty() {
        crate::selection_scoring::apply_percentile_floor(
            &mut combined,
            &tel.visual_sharpness,
            tel.visual_fps_hz,
            weights.sharpness_floor,
        );
    }
    if !tel.visual_exposure.is_empty() {
        crate::selection_scoring::apply_percentile_floor(
            &mut combined,
            &tel.visual_exposure,
            tel.visual_fps_hz,
            weights.exposure_floor,
        );
    }

    combined
}

/// Across **all** sources, pick clip centers respecting
/// per-source caps and within-source spacing, then resolve each pick to a
/// chapter file + offset (snapping to chapter boundaries).
///
/// `curves` must be parallel to `sources` — `curves[i]` is the score curve
/// for `sources[i]`. Sources whose curve is all-zero are skipped automatically.
///
/// Segments are returned in stable order: by source id, then chronologically
/// within each source. (The render order is decided later by the timeline
/// arranger; for now the manifest reflects this stable ordering.)
pub fn select_multi_source(
    sources: &[Source],
    curves: &[crate::selection_scoring::ScoreCurve],
    target_duration_secs: f64,
    clip_length_secs: f64,
    cap_override: Option<u32>,
    variety: f32,
    seed: u64,
) -> Vec<Segment> {
    assert_eq!(sources.len(), curves.len(), "sources and curves must be parallel");
    if clip_length_secs <= 0.0 {
        return Vec::new();
    }
    let k = ((target_duration_secs / clip_length_secs).round() as usize).max(1);
    let floor_min_spacing = ((clip_length_secs * 1.2).max(clip_length_secs + 0.5)) as f32;

    let curve_refs: Vec<(String, &crate::selection_scoring::ScoreCurve)> = sources
        .iter()
        .zip(curves)
        .map(|(s, c)| (s.id.clone(), c))
        .collect();

    let picks = crate::selection_scoring::pick_diverse(
        &curve_refs,
        k,
        clip_length_secs as f32,
        floor_min_spacing,
        cap_override.map(|c| c as usize),
        variety,
        seed,
    );
    tracing::debug!(
        target_k = k,
        clip_len = clip_length_secs,
        n_sources = sources.len(),
        picks = picks.len(),
        "multi-source picks"
    );

    let mut out = Vec::with_capacity(picks.len());
    for p in picks {
        let Some(source) = sources.iter().find(|s| s.id == p.source_id) else {
            continue;
        };
        let virtual_start = ((p.center_secs as f64) - clip_length_secs * 0.5).max(0.0);
        if let Some(mut seg) = snap_to_chapter(source, virtual_start, clip_length_secs) {
            seg.reason = format!("score={:.3} (hilight+accel, multi-source)", p.score);
            seg.score = Some(p.score);
            out.push(seg);
        }
    }
    out
}

// ---------- Audio ducking ----------

/// One window of the output timeline where the soundtrack should fade out
/// and the source audio should fade in. Times are in *output* (rendered)
/// seconds, not source seconds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct DuckWindow {
    pub start_secs: f32,
    pub end_secs: f32,
    pub ramp_secs: f32,
    pub source_id: String,
    pub segment_index: usize,
    pub audio_score: f32,
}

/// Score every segment by its source-audio interestingness, then pick up to
/// `count` segments to duck — favouring high-score segments while enforcing
/// `min_spacing_secs` separation in the output timeline. Returns duck windows
/// in output-timeline order.
///
/// `output_starts[i]` and `output_durations[i]` describe where each segment
/// falls in the rendered output timeline (these may differ from each segment's
/// own `duration_secs` once xfade transitions overlap clips). Pass cumulative
/// segment durations for the no-transitions case.
pub fn select_duck_windows(
    segments: &[Segment],
    output_starts: &[f32],
    output_durations: &[f32],
    count: u32,
    min_spacing_secs: f32,
    ramp_secs: f32,
    duck_min_dur: f32,
    duck_max_dur: f32,
    total_output_dur: f32,
    // Optional Silero VAD engine for speech-driven ducking. When `Some`,
    // each segment's audio is also scored for speech presence; the final
    // score combines audio_interestingness × (1 + vad_speech_weight × peak_speech_prob).
    vad_engine: Option<&mut crate::ml_engine::MlEngine>,
    vad_speech_weight: f32,
    // Per-segment audio presence — `false` short-circuits the per-segment
    // audio decode that drives interestingness + VAD scoring (those would
    // log noisy `audio decode for ducking failed` warnings on every call
    // for the same audio-less source). Such segments score 0 and are
    // therefore never picked as duck candidates. Defaults to "true" for
    // every segment when callers pass an empty slice (preserves the
    // pre-fix behaviour).
    segment_has_audio: &[bool],
) -> Vec<DuckWindow> {
    if count == 0 || segments.is_empty() {
        return Vec::new();
    }
    assert_eq!(segments.len(), output_starts.len());
    assert_eq!(segments.len(), output_durations.len());
    let assume_audio = segment_has_audio.is_empty();

    // Score each segment by source audio interestingness, optionally boosted
    // by VAD speech-presence when an ML engine is provided.
    let mut vad_engine = vad_engine;
    let mut scored: Vec<(usize, f32)> = Vec::with_capacity(segments.len());
    for (i, seg) in segments.iter().enumerate() {
        // Sources known to have no audio stream get a zero score without
        // attempting the decode (which would always fail and log noise).
        let has_audio = assume_audio || segment_has_audio.get(i).copied().unwrap_or(true);
        if !has_audio {
            scored.push((i, 0.0));
            continue;
        }
        let dur = seg.duration_secs as f32;
        let pcm_8k = crate::audio_analysis::decode_pcm_range(
            &seg.chapter_path,
            8_000,
            Some(seg.in_chapter_start_secs as f32),
            Some(dur),
        );
        let audio_score = match pcm_8k {
            Ok(ref pcm) => crate::audio_analysis::audio_interestingness(pcm, 8_000),
            Err(ref e) => {
                tracing::warn!(file = %seg.chapter_path, error = %e, "audio decode for ducking failed");
                0.0
            }
        };

        // VAD pass — decode at 16 kHz (Silero's required rate) and score
        // by peak speech probability over the segment.
        let vad_factor = if let Some(eng) = vad_engine.as_deref_mut() {
            match crate::audio_analysis::decode_pcm_range(
                &seg.chapter_path,
                16_000,
                Some(seg.in_chapter_start_secs as f32),
                Some(dur),
            ) {
                Ok(pcm16k) => match eng.vad(&pcm16k) {
                    Ok(series) => {
                        let p = series.peak();
                        tracing::debug!(seg = i, peak_speech = p, "VAD scored");
                        1.0 + vad_speech_weight * p
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "VAD inference failed");
                        1.0
                    }
                },
                Err(_) => 1.0,
            }
        } else {
            1.0
        };

        scored.push((i, audio_score * vad_factor));
    }

    // Greedy top-K with min-spacing on the output start times.
    let mut by_score: Vec<&(usize, f32)> = scored.iter().collect();
    by_score.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut chosen: Vec<usize> = Vec::new();
    for &(idx, score) in by_score {
        if score <= 1e-3 {
            continue;
        }
        let cand_start = output_starts[idx];
        let too_close = chosen
            .iter()
            .any(|&j| (output_starts[j] - cand_start).abs() < min_spacing_secs);
        if too_close {
            continue;
        }
        chosen.push(idx);
        if chosen.len() >= count as usize {
            break;
        }
    }

    // Compute score range across chosen picks to drive variable duck length.
    let chosen_scores: Vec<f32> = chosen.iter().map(|&i| scored[i].1).collect();
    let s_min = chosen_scores.iter().cloned().fold(f32::INFINITY, f32::min);
    let s_max = chosen_scores.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

    let mut windows: Vec<DuckWindow> = chosen
        .into_iter()
        .map(|idx| {
            let seg_start = output_starts[idx];
            let seg_dur = output_durations[idx];
            let center = seg_start + seg_dur * 0.5;
            let score = scored[idx].1;
            // Higher score = longer duck. Linear interp between min and max.
            let t = if (s_max - s_min).abs() < 1e-6 {
                0.5
            } else {
                ((score - s_min) / (s_max - s_min)).clamp(0.0, 1.0)
            };
            let dur = duck_min_dur + t * (duck_max_dur - duck_min_dur);
            // Center on segment midpoint, clamp to [0, total_output].
            let half = dur * 0.5;
            let mut start = (center - half).max(0.0);
            let mut end = (center + half).min(total_output_dur.max(0.0));
            // If we ran into a boundary and the duck was clipped, slide the
            // window so it keeps its target length when there's room.
            if end - start < dur && total_output_dur >= dur {
                if start <= 0.0 {
                    end = dur;
                }
                if end >= total_output_dur {
                    start = total_output_dur - dur;
                }
            }
            DuckWindow {
                start_secs: start,
                end_secs: end,
                ramp_secs,
                source_id: segments[idx].source_id.clone(),
                segment_index: idx,
                audio_score: score,
            }
        })
        .collect();
    windows.sort_by(|a, b| a.start_secs.partial_cmp(&b.start_secs).unwrap());
    // Resolve any overlap caused by extension: if two ducks now overlap, push
    // the later one's start to be at least `min_spacing_secs` after the prior
    // one's end (and re-clamp).
    for i in 1..windows.len() {
        let prev_end = windows[i - 1].end_secs;
        if windows[i].start_secs < prev_end {
            let dur = windows[i].end_secs - windows[i].start_secs;
            let new_start = (prev_end + 0.5).min(total_output_dur - dur).max(0.0);
            windows[i].start_secs = new_start;
            windows[i].end_secs = new_start + dur;
        }
    }
    windows
}

// ---------- Manifest ----------

/// JSON sidecar describing the rendered output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct Manifest {
    pub schema_version: u32,
    pub project_name: String,
    #[cfg_attr(feature = "ts-export", ts(type = "string"))]
    pub output: Utf8PathBuf,
    pub target_duration_secs: f64,
    pub actual_duration_secs: f64,
    pub resolution: String,
    pub fps: u32,
    pub seed: Option<u64>,
    /// Output-time `[start, end]` spans that carry the intro/outro titles
    /// (card or overlay mode). A reframe of this render to a narrower shape
    /// keeps these spans inside its safe area (`output_format`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub card_windows: Vec<[f64; 2]>,
    /// Who made the music, its licence, credit line and SHA-256 — one
    /// entry per soundtrack file (see `track_meta`). Absent without music.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub soundtrack: Vec<crate::track_meta::SoundtrackCredit>,
    pub sources: Vec<Source>,
    pub segments: Vec<Segment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub duck_windows: Vec<DuckWindow>,
    /// Angle groups discovered or configured for this run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub angle_groups: Vec<AngleGroupRecord>,
    /// Post-render check of where the cuts landed against the soundtrack's
    /// beats (see `beat_sync`). Absent without a soundtrack or when the
    /// check couldn't run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub beat_sync: Option<crate::beat_sync::BeatSyncReport>,
    /// Exactly where every clip and card plays in the rendered video, as
    /// the concat stage laid it out (see `output_timeline`). Absent in
    /// manifests written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub timeline: Option<crate::output_timeline::OutputTimeline>,
    pub generator: String,
}

/// Manifest record for one angle group — written for explainability and so a
/// downstream tool could re-render with a different angle choice without
/// re-running the planner.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct AngleGroupRecord {
    pub master_id: String,
    pub members: Vec<AngleMemberRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct AngleMemberRecord {
    pub source_id: String,
    /// `member_local_file_time = master_scene_time + offset_secs`.
    pub offset_secs: f64,
    pub start_in_master: f64,
    pub end_in_master: f64,
    pub offset_source: String, // "self" | "creation_time" | "audio_xcorr" | "manual"
}

pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

impl Manifest {
    pub fn write_to(&self, path: &Utf8Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let s = serde_json::to_string_pretty(self)?;
        std::fs::write(path, s)?;
        Ok(())
    }
}

// ---------- helpers ----------

/// Parse "WxH" style resolution.
pub fn parse_resolution(s: &str) -> anyhow::Result<(u32, u32)> {
    let (w, h) = s
        .split_once('x')
        .ok_or_else(|| anyhow::anyhow!("resolution must look like \"1920x1080\", got {s:?}"))?;
    let w: u32 = w.trim().parse().map_err(|_| {
        anyhow::anyhow!("invalid width in resolution {s:?}")
    })?;
    let h: u32 = h.trim().parse().map_err(|_| {
        anyhow::anyhow!("invalid height in resolution {s:?}")
    })?;
    Ok((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_probe::{AudioStream, MediaInfo, ProbedFile, VideoStream};

    fn fake(path: &str, dur: f64) -> ProbedFile {
        ProbedFile {
            path: path.into(),
            size_bytes: 0,
            blake3: Some(format!("blake3:{path}")),
            media: Some(MediaInfo {
                container: "mov,mp4".into(),
                duration_secs: dur,
                creation_time: None,
                video: Some(VideoStream {
                    codec: "hevc".into(),
                    width: 3840,
                    height: 2160,
                    fps: 50.0,
                    color: Default::default(),
                }),
                audio: Some(AudioStream {
                    codec: "aac".into(),
                    sample_rate: 48000,
                    channels: 2,
                }),
            }),
            probe_error: None,
        }
    }

    #[test]
    fn gopro_filename_parsing() {
        assert_eq!(parse_gopro_filename("GX010384.MP4"), Some((1, "0384".into())));
        assert_eq!(parse_gopro_filename("GX040384.MP4"), Some((4, "0384".into())));
        assert_eq!(parse_gopro_filename("GX010382.mp4"), Some((1, "0382".into())));
        assert_eq!(parse_gopro_filename("GH011234.MP4"), Some((1, "1234".into())));
        assert_eq!(parse_gopro_filename("video.mp4"), None);
        assert_eq!(parse_gopro_filename("GX00abcd.MP4"), None);
        assert_eq!(parse_gopro_filename("GX001234.MP4"), None); // chapter 0 invalid
        // Older scheme: GOPR is the first chapter, GP01 the second.
        assert_eq!(parse_gopro_filename("GOPR0042.MP4"), Some((1, "0042".into())));
        assert_eq!(parse_gopro_filename("GP010042.MP4"), Some((2, "0042".into())));
        assert_eq!(parse_gopro_filename("gp020042.mp4"), Some((3, "0042".into())));
        // Proxies are not chapters.
        assert_eq!(parse_gopro_filename("GL010384.LRV"), None);
        assert_eq!(parse_gopro_filename("GX010384.LRV"), None);
        assert_eq!(parse_gopro_filename("GOPR0042.LRV"), None);
    }

    fn ids_and_chapters(sources: &[Source]) -> Vec<(String, Vec<String>)> {
        sources
            .iter()
            .map(|s| {
                (
                    s.id.clone(),
                    s.chapters.iter().map(|c| c.path.to_string()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn legacy_chapters_stitch_gopr_first() {
        let files = vec![
            fake("card/GP020042.MP4", 30.0),
            fake("card/GP010042.MP4", 20.0),
            fake("card/GOPR0042.MP4", 10.0),
        ];
        let sources = group_into_sources(&files);
        assert_eq!(sources.len(), 1, "{:?}", ids_and_chapters(&sources));
        let s = &sources[0];
        assert_eq!(s.id, "0042");
        assert_eq!(
            s.chapters.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
            vec!["card/GOPR0042.MP4", "card/GP010042.MP4", "card/GP020042.MP4"]
        );
        assert_eq!(s.chapters.iter().map(|c| c.chapter_index).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert!((s.total_duration_secs - 60.0).abs() < 1e-9);
        // The first chapter starts the virtual timeline.
        let (ch, off) = s.locate(5.0).unwrap();
        assert_eq!(ch.path, "card/GOPR0042.MP4");
        assert!((off - 5.0).abs() < 1e-9);
    }

    #[test]
    fn lowercase_and_mixed_case_names_stitch() {
        let files = vec![
            fake("card/gp010042.mp4", 20.0),
            fake("card/GOPR0042.mp4", 10.0),
            fake("card/gx020007.MP4", 5.0),
            fake("card/GX010007.mp4", 5.0),
        ];
        let sources = group_into_sources(&files);
        assert_eq!(
            ids_and_chapters(&sources),
            vec![
                ("0007".into(), vec!["card/GX010007.mp4".into(), "card/gx020007.MP4".into()]),
                ("0042".into(), vec!["card/GOPR0042.mp4".into(), "card/gp010042.mp4".into()]),
            ]
        );
    }

    #[test]
    fn proxies_and_thumbnails_never_join_a_source() {
        let files = vec![
            fake("card/GX010384.MP4", 100.0),
            fake("card/GL010384.LRV", 100.0),
            fake("card/GX010384.THM", 0.04),
            fake("card/GX020384.MP4", 50.0),
            fake("card/GL020384.LRV", 50.0),
            fake("card/GL020384.MP4", 50.0), // proxy renamed to .MP4
            fake("old/GOPR0042.MP4", 10.0),
            fake("old/GOPR0042.LRV", 10.0),
            fake("old/GOPR0042.THM", 0.04),
            fake("old/GP010042.MP4", 10.0),
            fake("old/GP010042.LRV", 10.0),
        ];
        let sources = group_into_sources(&files);
        assert_eq!(
            ids_and_chapters(&sources),
            vec![
                ("0042".into(), vec!["old/GOPR0042.MP4".into(), "old/GP010042.MP4".into()]),
                ("0384".into(), vec!["card/GX010384.MP4".into(), "card/GX020384.MP4".into()]),
            ]
        );
        assert!((sources[1].total_duration_secs - 150.0).abs() < 1e-9);
    }

    #[test]
    fn mixed_cameras_with_the_same_recording_number_stay_separate() {
        // An older camera (GOPR/GP), a newer one in HEVC (GX) and one in AVC (GH) all
        // at recording 0042 in one folder, plus a second card's GX 0042 in
        // another folder: four takes, none merged.
        let files = vec![
            fake("trip/GOPR0042.MP4", 10.0),
            fake("trip/GP010042.MP4", 10.0),
            fake("trip/GX010042.MP4", 20.0),
            fake("trip/GX020042.MP4", 20.0),
            fake("trip/GH010042.MP4", 30.0),
            fake("trip/day2/GX010042.MP4", 40.0),
            fake("trip/GX010099.MP4", 50.0),
        ];
        let sources = group_into_sources(&files);
        assert_eq!(
            ids_and_chapters(&sources),
            vec![
                ("0099".into(), vec!["trip/GX010099.MP4".into()]),
                ("day2-gx010042".into(), vec!["trip/day2/GX010042.MP4".into()]),
                ("gh010042".into(), vec!["trip/GH010042.MP4".into()]),
                ("gopr0042".into(), vec!["trip/GOPR0042.MP4".into(), "trip/GP010042.MP4".into()]),
                ("gx010042".into(), vec!["trip/GX010042.MP4".into(), "trip/GX020042.MP4".into()]),
            ]
        );
    }

    #[test]
    fn standalone_clips_with_the_same_name_get_distinct_ids() {
        let files = vec![
            fake("a/clip.mp4", 10.0),
            fake("b/clip.mp4", 10.0),
            fake("b/other.mp4", 10.0),
        ];
        let ids: Vec<String> = group_into_sources(&files).into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["a-clip", "b-clip", "other"]);
    }

    #[test]
    fn two_cards_with_the_same_dcim_layout_stay_separate() {
        // Same folder name and file names on both cards: chapters still
        // only stitch within their own folder, and the ids name the card
        // (the first folder that differs), not the shared `100GOPRO`.
        let files = vec![
            fake("/footage/trip/cardA/DCIM/100GOPRO/GX010042.MP4", 10.0),
            fake("/footage/trip/cardA/DCIM/100GOPRO/GX020042.MP4", 10.0),
            fake("/footage/trip/cardB/DCIM/100GOPRO/GX010042.MP4", 20.0),
        ];
        assert_eq!(
            ids_and_chapters(&group_into_sources(&files)),
            vec![
                (
                    "carda-gx010042".into(),
                    vec![
                        "/footage/trip/cardA/DCIM/100GOPRO/GX010042.MP4".into(),
                        "/footage/trip/cardA/DCIM/100GOPRO/GX020042.MP4".into()
                    ]
                ),
                ("cardb-gx010042".into(), vec!["/footage/trip/cardB/DCIM/100GOPRO/GX010042.MP4".into()]),
            ]
        );
    }

    /// What the UI's no-LLM "tell it" grammar (`tellit.ts`) makes of
    /// "less of clip <id>": the input is lowercased and the id is the run
    /// of `[a-z0-9_-]` that follows.
    fn tell_it_source_id(typed: &str) -> String {
        let t = typed.to_lowercase();
        let rest = t.strip_prefix("less of clip ").unwrap();
        rest.chars()
            .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
            .collect()
    }

    #[test]
    fn clashing_ids_can_be_typed_back_in_the_tell_it_grammar() {
        let files = vec![
            // Two cameras at one recording number, in one folder.
            fake("/footage/trip/GOPR0042.MP4", 10.0),
            fake("/footage/trip/GX010042.MP4", 10.0),
            // Two cards with the same layout, folder names with spaces.
            fake("/footage/trip/Card A/DCIM/100GOPRO/GX010077.MP4", 10.0),
            fake("/footage/trip/Card B/DCIM/100GOPRO/GX010077.MP4", 10.0),
            // Standalone clips whose names differ only by letter case.
            fake("/footage/trip/Phone/IMG_0001.MOV", 10.0),
            fake("/footage/trip/Drone/img_0001.mov", 10.0),
        ];
        let sources = group_into_sources(&files);
        let ids: Vec<&str> = sources.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "card-a-gx010077",
                "card-b-gx010077",
                "drone-img_0001",
                "gopr0042",
                "gx010042",
                "phone-img_0001",
            ]
        );
        for id in ids {
            assert_eq!(tell_it_source_id(&format!("Less of clip {id}")), id);
        }
    }

    #[test]
    fn ids_get_numbered_only_when_folders_dont_tell_them_apart() {
        let files = vec![
            fake("x/clip.mp4", 10.0),
            fake("x/CLIP.MOV", 10.0),
            fake("y/clip-2.mp4", 10.0), // already owns the first free suffix
            // Three cards where two share the first differing folder.
            fake("t/a/100GOPRO/GX010042.MP4", 10.0),
            fake("t/b/100GOPRO/GX010042.MP4", 10.0),
            fake("t/b/101GOPRO/GX010042.MP4", 10.0),
        ];
        assert_eq!(
            ids_and_chapters(&group_into_sources(&files)),
            vec![
                ("a-gx010042".into(), vec!["t/a/100GOPRO/GX010042.MP4".into()]),
                ("b-gx010042".into(), vec!["t/b/100GOPRO/GX010042.MP4".into()]),
                ("b-gx010042-2".into(), vec!["t/b/101GOPRO/GX010042.MP4".into()]),
                ("clip".into(), vec!["x/CLIP.MOV".into()]),
                ("clip-2".into(), vec!["y/clip-2.mp4".into()]),
                ("clip-3".into(), vec!["x/clip.mp4".into()]),
            ]
        );
    }

    #[test]
    fn unclashed_ids_keep_their_names() {
        // Existing `source_weights` keys must keep matching.
        let files = vec![fake("trip/GX010042.MP4", 10.0), fake("trip/DJI_0001.MP4", 10.0)];
        let ids: Vec<String> = group_into_sources(&files).into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["0042", "DJI_0001"]);
    }

    #[test]
    fn id_slug_keeps_only_tell_it_characters() {
        assert_eq!(id_slug("GX010042"), "gx010042");
        assert_eq!(id_slug("Card A"), "card-a");
        assert_eq!(id_slug("  Ski Day (2) "), "ski-day-2");
        assert_eq!(id_slug("DJI_0001"), "dji_0001");
        assert_eq!(id_slug("Öland"), "land");
        assert_eq!(id_slug("日本"), "take");
    }

    #[test]
    fn chapters_record_probed_colour() {
        let mut f = fake("card/GX010001.MP4", 10.0);
        f.media.as_mut().unwrap().video.as_mut().unwrap().color = crate::media_probe::ColorInfo {
            bit_depth: Some(10),
            transfer: Some("arib-std-b67".into()),
            primaries: Some("bt2020".into()),
            ..Default::default()
        };
        let sources = group_into_sources(&[f]);
        let c = sources[0].chapters[0].color.as_ref().unwrap();
        assert_eq!(c.summary(), "10-bit HLG BT.2020");
        let json = serde_json::to_value(&sources[0].chapters[0]).unwrap();
        assert_eq!(json["color"]["transfer"], "arib-std-b67");
    }

    #[test]
    fn chapters_stitch_correctly() {
        let files = vec![
            fake("dir/GX020384.MP4", 1856.0),
            fake("dir/GX010384.MP4", 1856.0),
            fake("dir/GX040384.MP4", 1856.0),
            fake("dir/GX030384.MP4", 1856.0),
            fake("dir/GX010382.MP4", 346.36),
        ];
        let sources = group_into_sources(&files);
        assert_eq!(sources.len(), 2);

        let s_0382 = sources.iter().find(|s| s.id == "0382").unwrap();
        assert_eq!(s_0382.chapters.len(), 1);
        assert!((s_0382.total_duration_secs - 346.36).abs() < 0.001);

        let s_0384 = sources.iter().find(|s| s.id == "0384").unwrap();
        assert_eq!(s_0384.chapters.len(), 4);
        assert_eq!(
            s_0384.chapters.iter().map(|c| c.chapter_index).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert!((s_0384.total_duration_secs - 7424.0).abs() < 0.001);
    }

    #[test]
    fn locate_resolves_virtual_offset() {
        let files = vec![
            fake("dir/GX010384.MP4", 100.0),
            fake("dir/GX020384.MP4", 200.0),
            fake("dir/GX030384.MP4", 50.0),
        ];
        let sources = group_into_sources(&files);
        let s = &sources[0];
        let (ch, off) = s.locate(50.0).unwrap();
        assert_eq!(ch.chapter_index, 1);
        assert!((off - 50.0).abs() < 1e-9);
        let (ch, off) = s.locate(150.0).unwrap();
        assert_eq!(ch.chapter_index, 2);
        assert!((off - 50.0).abs() < 1e-9);
        let (ch, off) = s.locate(310.0).unwrap();
        assert_eq!(ch.chapter_index, 3);
        assert!((off - 10.0).abs() < 1e-9);
        assert!(s.locate(999.0).is_none());
    }

    #[test]
    fn evenly_spaced_picks_correct_count_and_stays_in_bounds() {
        let files = vec![fake("dir/clip.mp4", 60.0)];
        let sources = group_into_sources(&files);
        let segs = select_evenly_spaced(&sources, 10.0, 2.0); // 5 segments of 2s
        assert_eq!(segs.len(), 5);
        for s in &segs {
            assert_eq!(s.duration_secs, 2.0);
            assert!(s.in_chapter_start_secs >= 0.0);
            assert!(s.in_chapter_start_secs + s.duration_secs <= 60.0 + 1e-9);
        }
        // First segment near start, last segment near end:
        assert!(segs[0].virtual_start_secs < 1.0);
        assert!(segs[4].virtual_start_secs > 55.0);
    }

    #[test]
    fn evenly_spaced_snaps_to_chapter_when_crossing_boundary() {
        // Two 10s chapters; a 4s clip placed at virtual 8.0 would cross
        // the boundary at 10.0 — must snap back to start at 6.0 in chapter 1.
        let files = vec![
            fake("dir/GX010001.MP4", 10.0),
            fake("dir/GX020001.MP4", 10.0),
        ];
        let sources = group_into_sources(&files);
        // Single segment with target_duration < clip_length -> n = 1, placed at center.
        let segs = select_evenly_spaced(&sources, 4.0, 4.0);
        assert_eq!(segs.len(), 1);

        // Direct test: ask for a clip placed at 8.0 (would cross boundary).
        let snapped = snap_to_chapter(&sources[0], 8.0, 4.0).unwrap();
        assert_eq!(snapped.chapter_index, 1);
        assert!((snapped.in_chapter_start_secs - 6.0).abs() < 1e-9);
        assert!(snapped.in_chapter_start_secs + snapped.duration_secs <= 10.0 + 1e-9);
    }

    #[test]
    fn evenly_spaced_returns_empty_if_source_too_short() {
        let files = vec![fake("dir/clip.mp4", 1.0)];
        let sources = group_into_sources(&files);
        let segs = select_evenly_spaced(&sources, 10.0, 3.0);
        assert!(segs.is_empty());
    }

    #[test]
    fn resolution_parser() {
        assert_eq!(parse_resolution("1920x1080").unwrap(), (1920, 1080));
        assert_eq!(parse_resolution("3840x2160").unwrap(), (3840, 2160));
        assert!(parse_resolution("1080p").is_err());
        assert!(parse_resolution("axb").is_err());
    }
}
