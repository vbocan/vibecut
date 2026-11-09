//! Media file discovery and metadata extraction.
//!
//! Wraps the system `ffprobe` binary to extract container / codec / stream
//! info, plus dedicated helpers for the GoPro use case (extracting the
//! `gpmd` data track for [`crate::gopro_metadata`] to parse, scanning a
//! file's tail for the legacy MP4 `udta` HMMT atom).
//!
//! # Surfaces
//!
//! - **Folder discovery** ([`discover`]) — walk a directory and return paths
//!   matching the configured include patterns. Sorted, UTF-8 paths only.
//!
//! - **Probing**
//!   - [`probe_metadata`] — *fast path*. ffprobe + filesystem stat, no
//!     content hash. Used by the render pipeline; avoids re-hashing
//!     multi-GB files on every run.
//!   - [`probe_one`] — *slow path*. Same plus a streaming blake3 hash of
//!     the file content. Used by the `probe` CLI subcommand.
//!
//! - **GPMF helpers**
//!   - [`find_gpmf_stream_index`] — discover which stream of an MP4 has
//!     `codec_tag = "gpmd"`.
//!   - [`extract_gpmf_bytes`] — pipe that stream's raw bytes into a `Vec<u8>`
//!     so [`crate::gopro_metadata`] can parse it.
//!   - [`read_head_and_tail`] — cheap range read for scanning the MP4 `udta`
//!     atom for the legacy HMMT HiLight table without loading multi-GB
//!     files into RAM.
//!
//! ffprobe is optional at probe time: if it's not on PATH, probing reports
//! the filesystem-level info only (size, hash) and surfaces the missing
//! tool in the result.

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct ProbedFile {
    pub path: Utf8PathBuf,
    pub size_bytes: u64,
    /// `None` when probed in metadata-only mode (no content hash computed).
    pub blake3: Option<String>,
    pub media: Option<MediaInfo>,
    pub probe_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MediaInfo {
    pub container: String,
    pub duration_secs: f64,
    pub creation_time: Option<String>,
    pub video: Option<VideoStream>,
    pub audio: Option<AudioStream>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VideoStream {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    /// Bit depth, transfer, primaries — what normalize needs to decide
    /// whether a clip must be tone-mapped (see [`ColorInfo`]).
    pub color: ColorInfo,
}

/// Colour description of a video stream, as ffprobe reports it. Fields
/// ffprobe leaves out or reports as `unknown` are `None` (untagged).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct ColorInfo {
    /// Decoded pixel format (`yuv420p`, `yuv420p10le`, `yuvj420p`, …).
    pub pix_fmt: Option<String>,
    /// Bits per component: ffprobe's `bits_per_raw_sample`, else derived
    /// from the pixel format.
    pub bit_depth: Option<u32>,
    /// Transfer characteristic: `bt709`, `arib-std-b67` (HLG),
    /// `smpte2084` (PQ), `bt2020-10`, …
    pub transfer: Option<String>,
    /// Colour primaries: `bt709`, `bt2020`, …
    pub primaries: Option<String>,
    /// Matrix coefficients: `bt709`, `bt2020nc`, …
    pub matrix: Option<String>,
    /// `tv` (limited) or `pc` (full).
    pub range: Option<String>,
}

/// The two HDR transfer functions cameras record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrTransfer {
    /// Hybrid Log-Gamma, ffprobe `arib-std-b67` (GoPro "HLG HDR").
    Hlg,
    /// Perceptual Quantizer, ffprobe `smpte2084` (HDR10).
    Pq,
}

impl HdrTransfer {
    /// The transfer's name in ffmpeg / zscale option syntax.
    pub fn ffmpeg_name(self) -> &'static str {
        match self {
            HdrTransfer::Hlg => "arib-std-b67",
            HdrTransfer::Pq => "smpte2084",
        }
    }
}

impl ColorInfo {
    /// `Some` when the stream is tagged with an HDR transfer (HLG / PQ).
    pub fn hdr_transfer(&self) -> Option<HdrTransfer> {
        match self.transfer.as_deref() {
            Some("arib-std-b67") => Some(HdrTransfer::Hlg),
            Some("smpte2084") => Some(HdrTransfer::Pq),
            _ => None,
        }
    }

    /// Wide-gamut BT.2020 primaries (or, when primaries are untagged, a
    /// BT.2020 matrix).
    pub fn is_bt2020(&self) -> bool {
        match self.primaries.as_deref() {
            Some(p) => p == "bt2020",
            None => matches!(self.matrix.as_deref(), Some("bt2020nc" | "bt2020c")),
        }
    }

    /// More than 8 bits per component (10-bit HEVC Main10, 12-bit ProRes, …).
    pub fn is_high_bit_depth(&self) -> bool {
        self.bit_depth.is_some_and(|d| d > 8)
    }

    /// Short human description: `"10-bit HLG BT.2020"`, `"8-bit"`, …
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(d) = self.bit_depth {
            parts.push(format!("{d}-bit"));
        }
        match self.hdr_transfer() {
            Some(HdrTransfer::Hlg) => parts.push("HLG".into()),
            Some(HdrTransfer::Pq) => parts.push("PQ".into()),
            None => {
                if let Some(t) = &self.transfer {
                    parts.push(t.clone());
                }
            }
        }
        if self.is_bt2020() {
            parts.push("BT.2020".into());
        }
        if parts.is_empty() {
            "untagged".into()
        } else {
            parts.join(" ")
        }
    }
}

/// ffprobe prints `unknown` / `unspecified` for untagged colour fields
/// (or omits them); both mean "no tag".
fn color_tag(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.is_empty() && s != "unknown" && s != "unspecified" && s != "reserved")
}

/// Bits per component implied by an ffmpeg pixel-format name:
/// `yuv420p10le` → 10, `p010le` → 10, `yuv420p` / `nv12` → 8.
fn pix_fmt_bit_depth(pix_fmt: &str) -> Option<u32> {
    if pix_fmt.is_empty() {
        return None;
    }
    let s = pix_fmt
        .strip_suffix("le")
        .or_else(|| pix_fmt.strip_suffix("be"))
        .unwrap_or(pix_fmt);
    for (prefix, depth) in [("p010", 10), ("p012", 12), ("p016", 16), ("p210", 10), ("p410", 10)] {
        if s.starts_with(prefix) {
            return Some(depth);
        }
    }
    let n_digits = s.bytes().rev().take_while(u8::is_ascii_digit).count();
    let (head, digits) = s.split_at(s.len() - n_digits);
    match digits.parse::<u32>() {
        // Only planar names carry their depth as a suffix (`yuv420p10`,
        // `gbrp12`, `gray10`); `nv12` / `rgb24` end in digits that mean
        // something else.
        Ok(d @ 9..=16) if head.ends_with('p') || head.ends_with("gray") => Some(d),
        _ => Some(8),
    }
}

// ---------- GoPro file naming ----------

/// A GoPro chapter file name, parsed. See [`parse_gopro_name`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoProName {
    /// Two-letter naming family, upper-case: `GX` (HEVC), `GH`
    /// (AVC), `GP` (the older scheme — its `GOPR` first chapters too).
    /// Chapters only stitch within one family.
    pub family: String,
    /// 1-based position of the file within its recording: `GOPR0042` = 1,
    /// `GP010042` = 2, `GP020042` = 3; `GX010042` = 1, `GX020042` = 2.
    pub chapter: u32,
    /// The 4-digit recording number all chapters of one take share.
    pub recording: String,
}

/// Parse a GoPro chapter file name (case-insensitive, `.MP4` only).
///
/// - Newer cameras: `G<x><cc><nnnn>.MP4` — `x` is the encoding (`X` =
///   HEVC, `H` = AVC), `cc` the chapter (01, 02, …), `nnnn` the recording.
/// - Older cameras: the first chapter is `GOPR<nnnn>.MP4`, later ones
///   `GP<cc><nnnn>.MP4` where `GP01` is the *second* chapter.
///
/// Returns `None` for anything else — including the `GL<cc><nnnn>` proxies
/// (see [`is_gopro_sidecar`]).
pub fn parse_gopro_name(file_name: &str) -> Option<GoProName> {
    let (stem, ext) = file_name.rsplit_once('.')?;
    if !ext.eq_ignore_ascii_case("mp4") || stem.len() != 8 || !stem.is_ascii() {
        return None;
    }
    let stem = stem.to_ascii_uppercase();
    let all_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if let Some(recording) = stem.strip_prefix("GOPR") {
        return all_digits(recording).then(|| GoProName {
            family: "GP".into(),
            chapter: 1,
            recording: recording.into(),
        });
    }
    let b = stem.as_bytes();
    if b[0] != b'G' || !b[1].is_ascii_uppercase() || b[1] == b'L' {
        return None;
    }
    let (cc, recording) = (&stem[2..4], &stem[4..]);
    if !all_digits(cc) || !all_digits(recording) {
        return None;
    }
    let cc: u32 = cc.parse().ok()?;
    if cc == 0 {
        return None;
    }
    Some(GoProName {
        family: stem[..2].to_string(),
        // Older scheme: GOPR is chapter 1, so GP01 is chapter 2.
        chapter: if b[1] == b'P' { cc + 1 } else { cc },
        recording: recording.to_string(),
    })
}

/// True for the files cameras write *next to* the footage that are never
/// sources: GoPro low-res proxies (`.LRV` — `GL<cc><nnnn>.LRV` next to
/// GX / GH chapters, `GOPR<nnnn>.LRV` / `GP<cc><nnnn>.LRV` next to older
/// ones), GoPro thumbnails (`.THM`), DJI proxies (`.LRF`), and a `GL…`
/// proxy renamed to `.MP4`.
pub fn is_gopro_sidecar(file_name: &str) -> bool {
    let Some((stem, ext)) = file_name.rsplit_once('.') else {
        return false;
    };
    if ["lrv", "thm", "lrf"].iter().any(|e| ext.eq_ignore_ascii_case(e)) {
        return true;
    }
    let s = stem.as_bytes();
    s.len() == 8
        && s[0].eq_ignore_ascii_case(&b'G')
        && s[1].eq_ignore_ascii_case(&b'L')
        && s[2..].iter().all(u8::is_ascii_digit)
}

/// The low-res proxy GoPro writes for chapter `mp4` (whether or not it
/// exists): `GX010042.MP4` → `GL010042.LRV`, `GOPR0042.MP4` →
/// `GOPR0042.LRV`, `GP010042.MP4` → `GP010042.LRV`. Keeps the input's
/// letter case. `None` for non-GoPro names. Not used for rendering.
pub fn gopro_proxy_path(mp4: &Utf8Path) -> Option<Utf8PathBuf> {
    let name = mp4.file_name()?;
    let parsed = parse_gopro_name(name)?;
    let stem = if parsed.family == "GP" {
        if parsed.chapter == 1 {
            format!("GOPR{}", parsed.recording)
        } else {
            format!("GP{:02}{}", parsed.chapter - 1, parsed.recording)
        }
    } else {
        format!("GL{:02}{}", parsed.chapter, parsed.recording)
    };
    let lower = name.bytes().any(|b| b.is_ascii_lowercase());
    let proxy = format!("{stem}.LRV");
    Some(mp4.with_file_name(if lower { proxy.to_ascii_lowercase() } else { proxy }))
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioStream {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
}

/// Return `true` when `path` has a still-image extension.
///
/// Used throughout the pipeline to route image inputs to the Ken Burns
/// renderer instead of the video normalizer.
pub fn is_image_path(path: &Utf8Path) -> bool {
    matches!(
        path.extension().map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("jpg" | "jpeg" | "png" | "bmp" | "gif" | "webp" | "tiff" | "tif")
    )
}

/// Walk `folder` and return paths whose filename matches any of the include
/// patterns. Currently supports the common `**/*.EXT` form (case-insensitive
/// extension match); a real glob engine isn't implemented yet. Camera
/// proxies and thumbnails ([`is_gopro_sidecar`]) are never returned, even
/// when a pattern names their extension.
pub fn discover(folder: &Utf8Path, include: &[String]) -> Result<Vec<Utf8PathBuf>> {
    let exts = extensions_from_patterns(include);
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(folder).follow_links(false) {
        let entry = entry.with_context(|| format!("walking {folder}"))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let p = match Utf8PathBuf::from_path_buf(entry.path().to_path_buf()) {
            Ok(p) => p,
            Err(_) => continue, // skip non-UTF8 paths
        };
        let Some(ext) = p.extension() else {
            continue;
        };
        if p.file_name().is_some_and(is_gopro_sidecar) {
            continue;
        }
        if exts.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
            out.push(p);
        }
    }
    out.sort();
    Ok(out)
}

/// True for a file name [`discover`] picks up with no include patterns:
/// the default video extensions (`.mp4` / `.mov`, any case), camera
/// proxies and thumbnails ([`is_gopro_sidecar`]) excepted. For listing
/// folders without walking them through [`discover`].
pub fn is_default_video_name(file_name: &str) -> bool {
    let Some((_, ext)) = file_name.rsplit_once('.') else {
        return false;
    };
    extensions_from_patterns(&[]).iter().any(|e| e.eq_ignore_ascii_case(ext)) && !is_gopro_sidecar(file_name)
}

fn extensions_from_patterns(patterns: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for p in patterns {
        if let Some(dot) = p.rfind('.') {
            let ext = &p[dot + 1..];
            if !ext.is_empty() && !ext.contains('*') && !ext.contains('?') {
                out.push(ext.to_string());
            }
        }
    }
    if out.is_empty() {
        out.extend(["mp4", "MP4", "mov", "MOV"].into_iter().map(String::from));
    }
    out
}

/// Hash a file with blake3 in 256 KiB chunks. Returns the hex digest.
pub fn hash_file(path: &Utf8Path) -> Result<(String, u64)> {
    let mut hasher = blake3::Hasher::new();
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {path}"))?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hasher.finalize().to_hex().to_string(), total))
}

/// Probe one file: blake3 content hash + ffprobe metadata.
/// Use this when reproducibility matters (e.g. building a cache key or a manifest).
/// For large files, the hash dominates the cost.
pub fn probe_one(path: &Utf8Path) -> Result<ProbedFile> {
    let (digest, size) = hash_file(path)?;
    let (media, probe_error) = match run_ffprobe(path) {
        Ok(m) => (Some(m), None),
        Err(e) => (None, Some(e.to_string())),
    };
    Ok(ProbedFile {
        path: path.to_path_buf(),
        size_bytes: size,
        blake3: Some(format!("blake3:{digest}")),
        media,
        probe_error,
    })
}

/// Probe the duration of the video stream specifically (not the format
/// duration, which is the longest of all streams). Returns 0.0 on error.
/// Used to cap the muxed-output audio length to the actual video length —
/// AAC frame quantisation in `acrossfade` chains can leave the audio stream
/// longer than the video, which would otherwise cause the player to hold
/// the last frame at the end while audio keeps playing.
pub fn probe_video_duration(path: &Utf8Path) -> f64 {
    let output = match Command::new("ffprobe")
        .args([
            "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", "stream=duration",
            "-of", "csv=p=0",
            path.as_str(),
        ])
        .output()
    {
        Ok(o) => o,
        Err(_) => return 0.0,
    };
    if !output.status.success() {
        return 0.0;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse::<f64>().unwrap_or(0.0)
}

/// Metadata-only probe: ffprobe + filesystem size, no content hash.
/// Cheap (milliseconds even for multi-GB files); use this in the render pipeline
/// where we only need durations and codecs. The cache will be the place
/// where hashes get computed lazily.
pub fn probe_metadata(path: &Utf8Path) -> Result<ProbedFile> {
    let size = std::fs::metadata(path.as_std_path())
        .with_context(|| format!("stat {path}"))?
        .len();
    let (media, probe_error) = match run_ffprobe(path) {
        Ok(m) => (Some(m), None),
        Err(e) => (None, Some(e.to_string())),
    };
    Ok(ProbedFile {
        path: path.to_path_buf(),
        size_bytes: size,
        blake3: None,
        media,
        probe_error,
    })
}

fn run_ffprobe(path: &Utf8Path) -> Result<MediaInfo> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            path.as_str(),
        ])
        .output()
        .with_context(|| "spawning ffprobe (is it on PATH?)")?;

    if !output.status.success() {
        anyhow::bail!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let parsed: FfprobeOutput = serde_json::from_slice(&output.stdout)
        .context("parsing ffprobe JSON")?;
    Ok(parsed.into_media_info())
}

#[derive(Deserialize)]
struct FfprobeOutput {
    format: FfprobeFormat,
    #[serde(default)]
    streams: Vec<FfprobeStream>,
}

#[derive(Deserialize)]
struct FfprobeFormat {
    #[serde(default)]
    format_name: String,
    #[serde(default)]
    duration: Option<String>,
    #[serde(default)]
    tags: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Deserialize)]
struct FfprobeStream {
    #[serde(default)]
    codec_type: String,
    #[serde(default)]
    codec_name: String,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    r_frame_rate: Option<String>,
    #[serde(default)]
    sample_rate: Option<String>,
    #[serde(default)]
    channels: Option<u32>,
    #[serde(default)]
    pix_fmt: Option<String>,
    #[serde(default)]
    bits_per_raw_sample: Option<String>,
    #[serde(default)]
    color_transfer: Option<String>,
    #[serde(default)]
    color_primaries: Option<String>,
    #[serde(default)]
    color_space: Option<String>,
    #[serde(default)]
    color_range: Option<String>,
}

impl FfprobeStream {
    fn color_info(&mut self) -> ColorInfo {
        let pix_fmt = color_tag(self.pix_fmt.take());
        let bit_depth = self
            .bits_per_raw_sample
            .as_deref()
            .and_then(|b| b.parse::<u32>().ok())
            .filter(|&b| b > 0)
            .or_else(|| pix_fmt.as_deref().and_then(pix_fmt_bit_depth));
        ColorInfo {
            pix_fmt,
            bit_depth,
            transfer: color_tag(self.color_transfer.take()),
            primaries: color_tag(self.color_primaries.take()),
            matrix: color_tag(self.color_space.take()),
            range: color_tag(self.color_range.take()),
        }
    }
}

impl FfprobeOutput {
    fn into_media_info(self) -> MediaInfo {
        let duration_secs = self
            .format
            .duration
            .as_deref()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);

        let creation_time = self
            .format
            .tags
            .as_ref()
            .and_then(|t| t.get("creation_time").cloned());

        let mut video = None;
        let mut audio = None;
        for mut s in self.streams {
            match s.codec_type.as_str() {
                "video" if video.is_none() => {
                    let color = s.color_info();
                    video = Some(VideoStream {
                        codec: s.codec_name,
                        width: s.width.unwrap_or(0),
                        height: s.height.unwrap_or(0),
                        fps: parse_fraction(s.r_frame_rate.as_deref()),
                        color,
                    });
                }
                "audio" if audio.is_none() => {
                    audio = Some(AudioStream {
                        codec: s.codec_name,
                        sample_rate: s
                            .sample_rate
                            .as_deref()
                            .and_then(|x| x.parse().ok())
                            .unwrap_or(0),
                        channels: s.channels.unwrap_or(0),
                    });
                }
                _ => {}
            }
        }

        MediaInfo {
            container: self.format.format_name,
            duration_secs,
            creation_time,
            video,
            audio,
        }
    }
}

// ---------- GPMF extraction ----------

/// Find the index of the GoPro Metadata stream (codec_tag=`gpmd`), if present.
pub fn find_gpmf_stream_index(path: &Utf8Path) -> Result<Option<u32>> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_streams",
            path.as_str(),
        ])
        .output()
        .with_context(|| "spawning ffprobe")?;
    if !output.status.success() {
        anyhow::bail!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    #[derive(Deserialize)]
    struct ProbeOut {
        #[serde(default)]
        streams: Vec<Stream>,
    }
    #[derive(Deserialize)]
    struct Stream {
        index: u32,
        #[serde(default)]
        codec_tag_string: String,
    }
    let parsed: ProbeOut = serde_json::from_slice(&output.stdout)?;
    Ok(parsed
        .streams
        .into_iter()
        .find(|s| s.codec_tag_string == "gpmd")
        .map(|s| s.index))
}

/// Read up to `head_bytes` from the start and `tail_bytes` from the end of
/// the file. Used to scan for MP4 atoms (moov can be at either end on GoPro
/// files) without loading multi-gigabyte files into memory.
pub fn read_head_and_tail(
    path: &Utf8Path,
    head_bytes: u64,
    tail_bytes: u64,
) -> Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path.as_std_path())
        .with_context(|| format!("opening {path}"))?;
    let len = f.metadata()?.len();
    let take_head = head_bytes.min(len);
    let mut head = vec![0u8; take_head as usize];
    f.read_exact(&mut head)?;

    let mut tail = Vec::new();
    if len > take_head + tail_bytes {
        f.seek(SeekFrom::End(-(tail_bytes as i64)))?;
        tail.resize(tail_bytes as usize, 0);
        f.read_exact(&mut tail)?;
    } else if len > take_head {
        // Read whatever's left.
        let remaining = len - take_head;
        tail.resize(remaining as usize, 0);
        f.read_exact(&mut tail)?;
    }
    head.extend(tail);
    Ok(head)
}

/// Extract the GPMF data track from `path` to a byte vector. Returns `None`
/// if the file has no `gpmd` stream.
pub fn extract_gpmf_bytes(path: &Utf8Path) -> Result<Option<Vec<u8>>> {
    let Some(idx) = find_gpmf_stream_index(path)? else {
        return Ok(None);
    };
    // We pipe via ffmpeg → stdout because gpmd payloads are concatenated raw.
    use crate::cancel::CancellableCommand as _;
    let map_arg = format!("0:{idx}");
    let output = Command::new("ffmpeg")
        .args([
            "-v", "error",
            "-i", path.as_str(),
            "-map", &map_arg,
            "-codec", "copy",
            "-f", "data",
            "-",
        ])
        // Reads the whole chapter (GBs on a real card): keep it stoppable.
        .cancellable_output()
        .with_context(|| "spawning ffmpeg for GPMF extraction")?;
    if !output.status.success() {
        anyhow::bail!(
            "ffmpeg GPMF extract failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(Some(output.stdout))
}

fn parse_fraction(s: Option<&str>) -> f32 {
    let Some(s) = s else {
        return 0.0;
    };
    if let Some((num, den)) = s.split_once('/') {
        let n: f32 = num.parse().unwrap_or(0.0);
        let d: f32 = den.parse().unwrap_or(1.0);
        if d == 0.0 { 0.0 } else { n / d }
    } else {
        s.parse().unwrap_or(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_extracted() {
        let exts = extensions_from_patterns(&[
            "**/*.MP4".into(),
            "**/*.mp4".into(),
        ]);
        assert!(exts.iter().any(|e| e == "MP4"));
        assert!(exts.iter().any(|e| e == "mp4"));
    }

    #[test]
    fn extensions_default_when_empty() {
        let exts = extensions_from_patterns(&[]);
        assert!(exts.iter().any(|e| e.eq_ignore_ascii_case("mp4")));
    }

    #[test]
    fn default_video_names_match_discover_rules() {
        for yes in ["GX010001.MP4", "clip.mov", "a.b.Mp4", "GOPR0042.MP4", "DJI_0001.MOV"] {
            assert!(is_default_video_name(yes), "{yes}");
        }
        for no in ["GL010001.MP4", "GL010001.LRV", "GX010001.THM", "x.LRF", "notes.txt", "mp4", "song.mp3"] {
            assert!(!is_default_video_name(no), "{no}");
        }
    }

    #[test]
    fn fractions_parse() {
        assert_eq!(parse_fraction(Some("30/1")), 30.0);
        assert!((parse_fraction(Some("30000/1001")) - 29.97).abs() < 0.01);
        assert_eq!(parse_fraction(Some("60")), 60.0);
        assert_eq!(parse_fraction(None), 0.0);
        assert_eq!(parse_fraction(Some("1/0")), 0.0);
    }

    fn gp(family: &str, chapter: u32, recording: &str) -> Option<GoProName> {
        Some(GoProName { family: family.into(), chapter, recording: recording.into() })
    }

    #[test]
    fn gopro_names_hero8_and_later() {
        assert_eq!(parse_gopro_name("GX010042.MP4"), gp("GX", 1, "0042"));
        assert_eq!(parse_gopro_name("GX020042.MP4"), gp("GX", 2, "0042"));
        assert_eq!(parse_gopro_name("GH130042.MP4"), gp("GH", 13, "0042"));
        assert_eq!(parse_gopro_name("gx030042.mp4"), gp("GX", 3, "0042"));
        assert_eq!(parse_gopro_name("GX010042.Mp4"), gp("GX", 1, "0042"));
        assert_eq!(parse_gopro_name("GX000042.MP4"), None, "chapter 0 is not a chapter");
        assert_eq!(parse_gopro_name("GX01004a.MP4"), None);
        assert_eq!(parse_gopro_name("GX0100420.MP4"), None);
        assert_eq!(parse_gopro_name("GX010042.MOV"), None);
        assert_eq!(parse_gopro_name("video.mp4"), None);
        assert_eq!(parse_gopro_name("DJI_0042.MP4"), None);
        assert_eq!(parse_gopro_name("GX01ü042.MP4"), None, "non-ASCII must not panic");
    }

    #[test]
    fn gopro_names_hero5_to_7_put_gopr_first() {
        assert_eq!(parse_gopro_name("GOPR0042.MP4"), gp("GP", 1, "0042"));
        assert_eq!(parse_gopro_name("GP010042.MP4"), gp("GP", 2, "0042"));
        assert_eq!(parse_gopro_name("GP020042.MP4"), gp("GP", 3, "0042"));
        assert_eq!(parse_gopro_name("gopr0042.mp4"), gp("GP", 1, "0042"));
        assert_eq!(parse_gopro_name("GP000042.MP4"), None);
        assert_eq!(parse_gopro_name("GOPRabcd.MP4"), None);
    }

    #[test]
    fn proxies_and_thumbnails_are_sidecars_not_chapters() {
        for name in [
            "GL010042.LRV", "gl020042.lrv", "GOPR0042.LRV", "GP010042.LRV",
            "GOPR0042.THM", "GX010042.THM", "gx010042.thm", "DJI_0001.LRF",
            "GL010042.MP4", // a GL proxy renamed to .MP4
        ] {
            assert!(is_gopro_sidecar(name), "{name} should be a sidecar");
            assert_eq!(parse_gopro_name(name), None, "{name} must not parse as a chapter");
        }
        for name in ["GX010042.MP4", "GOPR0042.MP4", "GP010042.mp4", "clip.mp4", "noext"] {
            assert!(!is_gopro_sidecar(name), "{name} is footage");
        }
    }

    #[test]
    fn proxy_path_for_each_naming_scheme() {
        let p = |s: &str| gopro_proxy_path(Utf8Path::new(s)).map(|p| p.to_string());
        assert_eq!(p("card/GX010042.MP4").as_deref(), Some("card/GL010042.LRV"));
        assert_eq!(p("card/GH020042.MP4").as_deref(), Some("card/GL020042.LRV"));
        assert_eq!(p("card/GOPR0042.MP4").as_deref(), Some("card/GOPR0042.LRV"));
        assert_eq!(p("card/GP010042.MP4").as_deref(), Some("card/GP010042.LRV"));
        assert_eq!(p("card/gx010042.mp4").as_deref(), Some("card/gl010042.lrv"));
        assert_eq!(p("card/clip.mp4"), None);
    }

    #[test]
    fn discover_skips_proxies_and_thumbnails() {
        let dir = std::env::temp_dir().join(format!("vibecut_discover_sidecars_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for f in [
            "GOPR0042.MP4", "GP010042.MP4", "GOPR0042.LRV", "GP010042.LRV", "GOPR0042.THM",
            "sub/GX010007.mp4", "sub/GL010007.LRV", "sub/GL010007.MP4", "sub/GX010007.THM",
        ] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let root = Utf8PathBuf::from_path_buf(dir.clone()).unwrap();
        // Even an include list that names the sidecar extensions skips them.
        let include: Vec<String> =
            ["**/*.MP4", "**/*.mp4", "**/*.LRV", "**/*.THM"].iter().map(|s| s.to_string()).collect();
        let found: Vec<String> = discover(&root, &include)
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string())
            .collect();
        assert_eq!(found, vec!["GOPR0042.MP4", "GP010042.MP4", "GX010007.mp4"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pix_fmt_depths() {
        assert_eq!(pix_fmt_bit_depth("yuv420p"), Some(8));
        assert_eq!(pix_fmt_bit_depth("yuvj420p"), Some(8));
        assert_eq!(pix_fmt_bit_depth("nv12"), Some(8));
        assert_eq!(pix_fmt_bit_depth("yuv420p10le"), Some(10));
        assert_eq!(pix_fmt_bit_depth("yuv422p12be"), Some(12));
        assert_eq!(pix_fmt_bit_depth("p010le"), Some(10));
        assert_eq!(pix_fmt_bit_depth("gray10le"), Some(10));
        assert_eq!(pix_fmt_bit_depth(""), None);
    }

    #[test]
    fn ffprobe_colour_fields_parse() {
        let json = br#"{
            "format": {"format_name": "mov,mp4", "duration": "4.0"},
            "streams": [
                {"codec_type": "video", "codec_name": "hevc", "width": 3840, "height": 2160,
                 "r_frame_rate": "50/1", "pix_fmt": "yuv420p10le", "bits_per_raw_sample": "10",
                 "color_range": "tv", "color_space": "bt2020nc",
                 "color_transfer": "arib-std-b67", "color_primaries": "bt2020"},
                {"codec_type": "audio", "codec_name": "aac", "sample_rate": "48000", "channels": 2}
            ]}"#;
        let parsed: FfprobeOutput = serde_json::from_slice(json).unwrap();
        let c = parsed.into_media_info().video.unwrap().color;
        assert_eq!(c.bit_depth, Some(10));
        assert_eq!(c.hdr_transfer(), Some(HdrTransfer::Hlg));
        assert!(c.is_bt2020());
        assert_eq!(c.summary(), "10-bit HLG BT.2020");

        // Untagged 8-bit H.264 (ffprobe says "unknown" or leaves fields out).
        let json = br#"{
            "format": {"format_name": "mov,mp4", "duration": "4.0"},
            "streams": [
                {"codec_type": "video", "codec_name": "h264", "width": 1280, "height": 720,
                 "r_frame_rate": "30/1", "pix_fmt": "yuv420p",
                 "color_transfer": "unknown", "color_primaries": "unknown"}
            ]}"#;
        let parsed: FfprobeOutput = serde_json::from_slice(json).unwrap();
        let c = parsed.into_media_info().video.unwrap().color;
        assert_eq!(c.bit_depth, Some(8), "derived from pix_fmt");
        assert_eq!(c.transfer, None);
        assert_eq!(c.primaries, None);
        assert_eq!(c.hdr_transfer(), None);
        assert!(!c.is_bt2020());
        assert!(!c.is_high_bit_depth());
    }

    #[test]
    fn hashes_a_known_string() {
        let dir = std::env::temp_dir().join("vibecut_test_hash");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("hello.bin");
        std::fs::write(&file, b"hello").unwrap();
        let p = Utf8PathBuf::from_path_buf(file).unwrap();
        let (digest, size) = hash_file(&p).unwrap();
        assert_eq!(size, 5);
        // blake3("hello") is well-known.
        assert_eq!(
            digest,
            "ea8f163db38682925e4491c5e58d4bb3506ef8c14eb78a86e908c5624a67200f"
        );
    }
}
