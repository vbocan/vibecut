//! Scan a folder of GoPro / DJI footage and summarise what's there.
//! The transport-free counterpart of the old `discover_sources_impl`
//! in `vibecut-mcp`.

use std::path::Path;

use camino::Utf8PathBuf;
use serde::Serialize;
use vibecut::media_probe;

use crate::error::OpsError;
use crate::paths::resolve_footage_checked;

#[derive(Debug, Clone, Serialize)]
pub struct DiscoverResult {
    pub folder: String,
    pub sources: Vec<serde_json::Value>,
    pub n_sources: usize,
    pub total_secs: f64,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// Scan `folder` (relative to `/footage`, or an absolute path under
/// `/footage` or `/projects`; see [`resolve_footage_checked`]) and
/// group GoPro chapters (`GH010023.MP4`, `GH020023.MP4`, …) into
/// logical sources.
pub async fn discover_sources(folder: &str) -> Result<DiscoverResult, OpsError> {
    let target = resolve_footage_checked(folder)?;
    if !target.exists() {
        return Err(OpsError::not_found_with(
            "footage folder",
            format!(
                "{} (check the docker -v bind-mount; folder is resolved relative to /footage)",
                target.display()
            ),
        ));
    }
    if !target.is_dir() {
        return Err(OpsError::invalid(format!(
            "not a directory: {}",
            target.display()
        )));
    }

    let target_camino = Utf8PathBuf::from_path_buf(target.clone())
        .map_err(|_| OpsError::invalid(format!("non-UTF-8 folder path: {}", target.display())))?;

    // ffprobe + walk happen on a blocking thread so we don't tie up the
    // async runtime while ffprobe runs (it can take seconds per file on
    // slow disks).
    let probed_target = target_camino.clone();
    let probed = tokio::task::spawn_blocking(
        move || -> Result<Vec<media_probe::ProbedFile>, anyhow::Error> {
            let files = media_probe::discover(&probed_target, &[])?;
            let mut out = Vec::with_capacity(files.len());
            for f in &files {
                if let Ok(p) = media_probe::probe_metadata(f) {
                    out.push(p);
                }
                // Probe failures are logged at the binary side; here we
                // just skip them so partial discovery still returns
                // useful data.
            }
            Ok(out)
        },
    )
    .await
    .map_err(|e| OpsError::internal(format!("discovery task panicked: {e}")))?
    .map_err(|e| OpsError::internal(format!("discovery failed: {e}")))?;

    if probed.is_empty() {
        return Ok(DiscoverResult {
            folder: target.to_string_lossy().into_owned(),
            sources: vec![],
            n_sources: 0,
            total_secs: 0.0,
            summary: "No video files found.".to_string(),
            hint: Some(
                "No .MP4/.mp4 files were found under this folder. Check the docker -v bind-mount and the folder name you passed."
                    .to_string(),
            ),
        });
    }

    Ok(group_sources_from_probed(&target, &probed))
}

// ---------- grouping helpers ----------

/// GoPro chapter filenames look like `GH010023.MP4` / `GX020023.MP4` —
/// the trailing 4 digits identify the source, the 2 digits before them
/// identify the chapter. Other filenames become one-chapter sources
/// keyed by their stem.
fn parse_chapter(name: &str) -> (String, u32) {
    let re = regex::Regex::new(r"^(?:GH|GX)(?P<ch>\d{2})(?P<src>\d{4})\.(?:mp4|MP4)$").unwrap();
    if let Some(caps) = re.captures(name) {
        let src = caps.name("src").unwrap().as_str().to_string();
        let ch: u32 = caps.name("ch").unwrap().as_str().parse().unwrap_or(1);
        (src, ch)
    } else {
        let stem = Path::new(name)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.to_string());
        (stem, 1)
    }
}

struct SourceAcc {
    source_id: String,
    chapters: Vec<serde_json::Value>,
    total_duration_secs: f64,
    has_audio: bool,
    fps: std::collections::BTreeSet<i64>,
    codecs: std::collections::BTreeSet<String>,
}

fn group_sources_from_probed(folder: &Path, probed: &[media_probe::ProbedFile]) -> DiscoverResult {
    use std::collections::BTreeMap;
    let mut by_id: BTreeMap<String, SourceAcc> = BTreeMap::new();

    for entry in probed {
        let path_str = entry.path.to_string();
        let duration = entry.media.as_ref().map(|m| m.duration_secs).unwrap_or(0.0);
        let video = entry.media.as_ref().and_then(|m| m.video.as_ref());
        let audio = entry.media.as_ref().and_then(|m| m.audio.as_ref());

        let fname = entry
            .path
            .file_name()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let (src_id, ch_idx) = parse_chapter(&fname);

        let acc = by_id.entry(src_id.clone()).or_insert_with(|| SourceAcc {
            source_id: src_id,
            chapters: Vec::new(),
            total_duration_secs: 0.0,
            has_audio: false,
            fps: std::collections::BTreeSet::new(),
            codecs: std::collections::BTreeSet::new(),
        });
        acc.chapters.push(serde_json::json!({
            "path": path_str,
            "chapter_index": ch_idx,
            "duration_secs": duration,
        }));
        acc.total_duration_secs += duration;
        if audio.is_some() {
            acc.has_audio = true;
        }
        if let Some(v) = video {
            // VideoStream::fps is f32; quantise to integer for the rollup.
            acc.fps.insert(v.fps.round() as i64);
            if !v.codec.is_empty() {
                acc.codecs.insert(v.codec.clone());
            }
        }
    }

    let mut sources: Vec<serde_json::Value> = Vec::with_capacity(by_id.len());
    let mut total_secs = 0.0_f64;
    let mut n_audio = 0usize;
    for (_, mut a) in by_id {
        a.chapters.sort_by_key(|c| {
            c.get("chapter_index").and_then(|v| v.as_u64()).unwrap_or(0)
        });
        if a.has_audio {
            n_audio += 1;
        }
        total_secs += a.total_duration_secs;
        sources.push(serde_json::json!({
            "source_id": a.source_id,
            "chapters": a.chapters,
            "n_chapters": a.chapters.len(),
            "total_duration_secs": (a.total_duration_secs * 100.0).round() / 100.0,
            "has_audio": a.has_audio,
            "fps": a.fps.into_iter().collect::<Vec<_>>(),
            "codecs": a.codecs.into_iter().collect::<Vec<_>>(),
        }));
    }
    let n_sources = sources.len();

    DiscoverResult {
        folder: folder.to_string_lossy().into_owned(),
        summary: footage_summary(n_sources, total_secs, n_audio),
        sources,
        n_sources,
        total_secs: (total_secs * 100.0).round() / 100.0,
        hint: None,
    }
}

fn footage_summary(n_sources: usize, total_secs: f64, n_audio: usize) -> String {
    if n_sources == 0 {
        return "No video files found.".to_string();
    }
    let minutes = total_secs / 60.0;
    let audio_note = if n_audio == n_sources {
        "all with audio".to_string()
    } else if n_audio == 0 {
        "no audio (DJI / mic-off)".to_string()
    } else {
        format!("{n_audio} of {n_sources} with audio")
    };
    format!("{n_sources} sources, {minutes:.1} min total, {audio_note}.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chapter_regex() {
        assert_eq!(parse_chapter("GH010023.MP4"), ("0023".into(), 1));
        assert_eq!(parse_chapter("GX020023.MP4"), ("0023".into(), 2));
        let (sid, ch) = parse_chapter("DJI_0042.MP4");
        assert_eq!(sid, "DJI_0042");
        assert_eq!(ch, 1);
    }

    #[test]
    fn footage_summary_phrases() {
        assert_eq!(footage_summary(0, 0.0, 0), "No video files found.");
        assert!(footage_summary(3, 600.0, 3).contains("all with audio"));
        assert!(footage_summary(2, 200.0, 0).contains("no audio"));
        assert!(footage_summary(2, 200.0, 1).contains("1 of 2 with audio"));
    }
}
