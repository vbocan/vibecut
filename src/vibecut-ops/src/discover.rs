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

/// Scan `folder` (relative to `/footage`, a browse display path, or an
/// absolute path under `/footage`, `/projects` or a browse root; see
/// [`resolve_footage_checked`]) and group GoPro chapters (`GH010023.MP4`,
/// `GH020023.MP4`, …; `GOPR0042.MP4`, `GP010042.MP4`, … on older
/// cameras) into logical sources.
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
    crate::browse::check_footage_folder(&target)?;

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

/// Group probed files into sources exactly as the render pipeline does
/// ([`vibecut::selection_planner::group_into_sources`]): GoPro chapters of
/// one recording (`GX01nnnn` / `GX02nnnn`…, or `GOPRnnnn` then `GP01nnnn`…
/// on older cameras) become one source in chapter order, `.LRV` / `.THM`
/// sidecars are dropped, other files are one-chapter sources keyed by stem.
fn group_sources_from_probed(folder: &Path, probed: &[media_probe::ProbedFile]) -> DiscoverResult {
    use std::collections::{BTreeSet, HashMap};
    let by_path: HashMap<&camino::Utf8Path, &media_probe::ProbedFile> =
        probed.iter().map(|p| (p.path.as_path(), p)).collect();

    let grouped = vibecut::selection_planner::group_into_sources(probed);
    let mut sources: Vec<serde_json::Value> = Vec::with_capacity(grouped.len());
    let mut total_secs = 0.0_f64;
    let mut n_audio = 0usize;
    for s in &grouped {
        let mut has_audio = false;
        let mut fps = BTreeSet::new();
        let mut codecs = BTreeSet::new();
        let mut chapters = Vec::with_capacity(s.chapters.len());
        for ch in &s.chapters {
            chapters.push(serde_json::json!({
                "path": ch.path.to_string(),
                "chapter_index": ch.chapter_index,
                "duration_secs": ch.duration_secs,
            }));
            let media = by_path.get(ch.path.as_path()).and_then(|p| p.media.as_ref());
            if media.and_then(|m| m.audio.as_ref()).is_some() {
                has_audio = true;
            }
            if let Some(v) = media.and_then(|m| m.video.as_ref()) {
                // VideoStream::fps is f32; quantise to integer for the rollup.
                fps.insert(v.fps.round() as i64);
                if !v.codec.is_empty() {
                    codecs.insert(v.codec.clone());
                }
            }
        }
        if has_audio {
            n_audio += 1;
        }
        total_secs += s.total_duration_secs;
        sources.push(serde_json::json!({
            "source_id": s.id,
            "chapters": chapters,
            "n_chapters": chapters.len(),
            "total_duration_secs": (s.total_duration_secs * 100.0).round() / 100.0,
            "has_audio": has_audio,
            "fps": fps.into_iter().collect::<Vec<_>>(),
            "codecs": codecs.into_iter().collect::<Vec<_>>(),
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

    fn probed(path: &str, dur: f64, audio: bool) -> media_probe::ProbedFile {
        media_probe::ProbedFile {
            path: path.into(),
            size_bytes: 0,
            blake3: None,
            media: Some(media_probe::MediaInfo {
                container: "mov,mp4".into(),
                duration_secs: dur,
                creation_time: None,
                video: Some(media_probe::VideoStream {
                    codec: "hevc".into(),
                    width: 1920,
                    height: 1080,
                    fps: 29.97,
                    color: Default::default(),
                }),
                audio: audio.then(|| media_probe::AudioStream {
                    codec: "aac".into(),
                    sample_rate: 48_000,
                    channels: 2,
                }),
            }),
            probe_error: None,
        }
    }

    fn chapters_of(r: &DiscoverResult) -> Vec<(String, Vec<String>)> {
        r.sources
            .iter()
            .map(|s| {
                let paths = s["chapters"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| c["path"].as_str().unwrap().to_string())
                    .collect();
                (s["source_id"].as_str().unwrap().to_string(), paths)
            })
            .collect()
    }

    #[test]
    fn groups_every_gopro_naming_scheme_and_ignores_proxies() {
        let files = vec![
            probed("f/GH020023.MP4", 20.0, true),
            probed("f/GH010023.MP4", 10.0, true),
            probed("f/GP010042.mp4", 20.0, false),
            probed("f/GOPR0042.MP4", 10.0, false),
            probed("f/GOPR0042.LRV", 10.0, false),
            probed("f/GL010023.LRV", 10.0, true),
            probed("f/GOPR0042.THM", 0.0, false),
            probed("f/DJI_0042.MP4", 5.0, false),
        ];
        let r = group_sources_from_probed(Path::new("f"), &files);
        assert_eq!(
            chapters_of(&r),
            vec![
                ("0023".into(), vec!["f/GH010023.MP4".into(), "f/GH020023.MP4".into()]),
                ("0042".into(), vec!["f/GOPR0042.MP4".into(), "f/GP010042.mp4".into()]),
                ("DJI_0042".into(), vec!["f/DJI_0042.MP4".into()]),
            ]
        );
        assert_eq!(r.n_sources, 3);
        assert_eq!(r.total_secs, 65.0);
        let gopr = &r.sources[1];
        assert_eq!(gopr["n_chapters"], 2);
        assert_eq!(gopr["chapters"][0]["chapter_index"], 1);
        assert_eq!(gopr["chapters"][1]["chapter_index"], 2);
        assert_eq!(gopr["has_audio"], false);
        assert_eq!(r.sources[0]["has_audio"], true);
        assert_eq!(r.sources[0]["fps"], serde_json::json!([30]));
        assert!(r.summary.contains("1 of 3 with audio"), "{}", r.summary);
    }

    #[test]
    fn footage_summary_phrases() {
        assert_eq!(footage_summary(0, 0.0, 0), "No video files found.");
        assert!(footage_summary(3, 600.0, 3).contains("all with audio"));
        assert!(footage_summary(2, 200.0, 0).contains("no audio"));
        assert!(footage_summary(2, 200.0, 1).contains("1 of 2 with audio"));
    }
}
