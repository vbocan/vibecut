//! Write-then-rename for a render's outputs.
//!
//! ffmpeg writes in place (`-y` truncates the target first), so a render
//! that fails or is cancelled halfway through the mux would leave a
//! half-written file where the last good video was. Instead every
//! `[[outputs]]` file is written under a staging name next to it and only
//! renamed into place by [`StagedOutputs::commit`], once the whole render
//! has succeeded. Dropping uncommitted staged outputs deletes them.

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

/// Where a render writes `final_path` until it commits: same directory
/// (so the rename stays on one filesystem and is atomic), same extension
/// (ffmpeg picks the muxer from it). `reel.mp4` → `reel.partial.mp4`.
pub fn staging_path(final_path: &Utf8Path) -> Utf8PathBuf {
    let stem = final_path.file_stem().unwrap_or("output");
    let name = match final_path.extension() {
        Some(ext) => format!("{stem}.partial.{ext}"),
        None => format!("{stem}.partial"),
    };
    final_path.with_file_name(name)
}

/// A render's outputs while they're being written.
#[derive(Debug)]
pub struct StagedOutputs {
    /// `(staged, final)`, in `[[outputs]]` order.
    files: Vec<(Utf8PathBuf, Utf8PathBuf)>,
    committed: bool,
}

impl StagedOutputs {
    /// Stage `finals`. Leftovers from a render that died without
    /// cleaning up (killed process) are removed first.
    pub fn new<'a>(finals: impl IntoIterator<Item = &'a Utf8Path>) -> Self {
        let files: Vec<_> = finals
            .into_iter()
            .map(|f| (staging_path(f), f.to_path_buf()))
            .collect();
        for (staged, _) in &files {
            let _ = std::fs::remove_file(staged);
        }
        Self { files, committed: false }
    }

    /// The staging path of output `i`.
    pub fn path(&self, i: usize) -> &Utf8Path {
        &self.files[i].0
    }

    /// Move every staged file into place. Call only once nothing can fail
    /// or be cancelled any more.
    pub fn commit(mut self) -> Result<()> {
        for (staged, fin) in &self.files {
            std::fs::rename(staged, fin).with_context(|| format!("moving {staged} into place as {fin}"))?;
        }
        self.committed = true;
        Ok(())
    }
}

impl Drop for StagedOutputs {
    fn drop(&mut self) {
        if !self.committed {
            for (staged, _) in &self.files {
                let _ = std::fs::remove_file(staged);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> Utf8PathBuf {
        let d = std::env::temp_dir().join(format!("vibecut_staging_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Utf8PathBuf::from_path_buf(d).unwrap()
    }

    #[test]
    fn staging_keeps_dir_and_extension() {
        assert_eq!(staging_path(Utf8Path::new("/p/out/reel.mp4")), "/p/out/reel.partial.mp4");
        assert_eq!(staging_path(Utf8Path::new("reel")), "reel.partial");
    }

    #[test]
    fn an_abandoned_render_leaves_the_last_good_output_alone() {
        let d = tmp("abandon");
        let out = d.join("reel.mp4");
        std::fs::write(&out, b"last good render").unwrap();
        {
            let staged = StagedOutputs::new([out.as_path()]);
            // ffmpeg got halfway through the mux, then the render stopped.
            std::fs::write(staged.path(0), b"half a vid").unwrap();
        }
        assert_eq!(std::fs::read(&out).unwrap(), b"last good render");
        assert!(!staging_path(&out).exists(), "partial file left behind");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn commit_replaces_every_output() {
        let d = tmp("commit");
        let a = d.join("a.mp4");
        let b = d.join("b.mp4");
        std::fs::write(&a, b"old a").unwrap();
        // A stale partial from a crashed run is cleared on stage.
        std::fs::write(staging_path(&b), b"stale").unwrap();
        let staged = StagedOutputs::new([a.as_path(), b.as_path()]);
        assert!(!staged.path(1).exists());
        std::fs::write(staged.path(0), b"new a").unwrap();
        std::fs::write(staged.path(1), b"new b").unwrap();
        staged.commit().unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), b"new a");
        assert_eq!(std::fs::read(&b).unwrap(), b"new b");
        assert!(!staging_path(&a).exists() && !staging_path(&b).exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// The pipeline's side of the contract: `run_cmd` writes every
/// `[[outputs]]` file through [`StagedOutputs`], so a render stopped at
/// any point leaves the last good outputs and their manifest
/// byte-for-byte alone — and a Stop lands where it's pressed.
#[cfg(test)]
mod stopped_render_tests {
    use super::*;
    use crate::cancel::{is_cancelled, CancelToken};
    use crate::progress::{with_sink_and_cancel, Progress, Stage};
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};

    const OLD_VIDEO: &[u8] = b"the last good render";
    const OLD_MANIFEST: &[u8] = b"{\"the\": \"last good manifest\"}";

    fn ffmpeg_available() -> bool {
        Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    fn tmp(name: &str) -> Utf8PathBuf {
        let d = std::env::temp_dir().join(format!("vibecut_stopped_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Utf8PathBuf::from_path_buf(d).unwrap()
    }

    /// A tiny project: two 6 s lavfi clips with sound, `n_outputs` outputs
    /// that already hold an earlier render (placeholder bytes).
    struct Project {
        dir: Utf8PathBuf,
        config: Utf8PathBuf,
        outputs: Vec<Utf8PathBuf>,
        manifest: Utf8PathBuf,
    }

    fn project(name: &str, n_outputs: usize, folder: Option<&str>) -> Project {
        let dir = tmp(name);
        let footage = dir.join("footage");
        std::fs::create_dir_all(&footage).unwrap();
        if folder.is_none() {
            for i in 1..=2 {
                let clip = footage.join(format!("clip{i}.mp4"));
                let ok = Command::new("ffmpeg")
                    .args([
                        "-y", "-v", "error", "-nostdin",
                        "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=30:duration=6",
                        "-f", "lavfi", "-i", &format!("sine=frequency={}:duration=6", 220 * i),
                        "-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-shortest",
                        clip.as_str(),
                    ])
                    .status()
                    .is_ok_and(|s| s.success());
                assert!(ok, "generating {clip}");
            }
        }
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let outputs: Vec<Utf8PathBuf> = (0..n_outputs).map(|i| out.join(format!("reel{i}.mp4"))).collect();
        for o in &outputs {
            std::fs::write(o, OLD_VIDEO).unwrap();
        }
        let manifest = out.join("reel0.manifest.json");
        std::fs::write(&manifest, OLD_MANIFEST).unwrap();
        let fwd = |p: &Utf8Path| p.as_str().replace('\\', "/");
        let mut toml = format!(
            "schema_version = 1\n\n[project]\nname = \"StopTest\"\ntarget_duration = \"6s\"\nfps = 30\n\
             seed = 1\ntemp_dir = \"{work}\"\n\n[input]\nfolder = \"{folder}\"\ninclude = [\"*.mp4\"]\n\n\
             [selection]\nmin_clip = \"1500ms\"\nmax_clip = \"2s\"\n",
            work = fwd(&dir.join("work")),
            folder = folder.map(str::to_string).unwrap_or_else(|| fwd(&footage)),
        );
        for o in &outputs {
            toml.push_str(&format!(
                "\n[[outputs]]\npath = \"{}\"\nresolution = \"320x180\"\nencoder = \"libx264\"\ncrf = 30\n",
                fwd(o)
            ));
        }
        let config = dir.join("p.toml");
        std::fs::write(&config, toml).unwrap();
        Project { dir, config, outputs, manifest }
    }

    fn render(
        p: &Project,
        token: CancelToken,
        sink: impl Fn(&Progress) + Send + Sync + 'static,
    ) -> anyhow::Result<()> {
        with_sink_and_cancel(sink, token, || {
            crate::run_pipeline::run_cmd(
                &p.config, /*dry_run*/ false, /*no_visual*/ true, /*no_cache*/ false,
                /*explain*/ false, /*print_effective_config*/ false, /*random_seed*/ false,
            )
        })
    }

    fn partials_left(p: &Project) -> Vec<String> {
        std::fs::read_dir(p.dir.join("out"))
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.contains(".partial"))
            .collect()
    }

    /// Stop once every output has been written (the Checking stage, right
    /// before the commit): the new files existed, yet none replaced the
    /// last good ones. Fails if any output is written to its final path.
    fn stop_after_writing(p: &Project) {
        let token = CancelToken::new();
        let stop = token.clone();
        let staged_at_stop: Arc<Mutex<Option<Vec<bool>>>> = Arc::default();
        let seen = staged_at_stop.clone();
        let finals = p.outputs.clone();
        let r = render(p, token, move |pr| {
            if pr.stage == Stage::Checking {
                *seen.lock().unwrap() = Some(finals.iter().map(|o| staging_path(o).exists()).collect());
                stop.cancel();
            }
        });
        let err = r.expect_err("a render stopped at the beat check must not succeed");
        assert!(is_cancelled(&err), "stopped with a non-cancel error: {err:#}");
        let staged = staged_at_stop.lock().unwrap().clone().expect("the render never reached the beat check");
        assert!(staged.iter().all(|&b| b), "the outputs weren't written under their staging names: {staged:?}");
        for o in &p.outputs {
            assert_eq!(std::fs::read(o).unwrap(), OLD_VIDEO, "{o} was replaced by a stopped render");
        }
        assert_eq!(std::fs::read(&p.manifest).unwrap(), OLD_MANIFEST, "manifest replaced by a stopped render");
        assert!(partials_left(p).is_empty(), "partial outputs left: {:?}", partials_left(p));
    }

    #[test]
    fn a_stopped_render_keeps_the_last_good_output_and_a_finished_one_replaces_it() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not on PATH; skipping");
            return;
        }
        let p = project("single", 1, None);
        stop_after_writing(&p);

        // The same render, not stopped, commits: the outputs and the
        // manifest are replaced, and nothing is left under a staging name.
        render(&p, CancelToken::new(), |_| {}).expect("an unstopped render succeeds");
        let video = std::fs::read(&p.outputs[0]).unwrap();
        assert_eq!(&video[4..8], b"ftyp", "the output isn't the new mp4");
        let manifest = std::fs::read(&p.manifest).unwrap();
        assert!(serde_json::from_slice::<serde_json::Value>(&manifest).is_ok(), "manifest not rewritten");
        assert!(partials_left(&p).is_empty());
        let _ = std::fs::remove_dir_all(&p.dir);
    }

    #[test]
    fn a_stopped_render_keeps_every_last_good_output() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not on PATH; skipping");
            return;
        }
        // Two outputs: each is transcoded from the body, the path that
        // writes through `staged.path(i)`.
        let p = project("multi", 2, None);
        stop_after_writing(&p);
        let _ = std::fs::remove_dir_all(&p.dir);
    }

    #[test]
    fn a_stop_while_finding_clips_ends_the_render_there() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not on PATH; skipping");
            return;
        }
        // Probing this "clip" would fail the render; a Stop pressed while
        // the probing report is up must end it as cancelled before that.
        let p = project("probing", 1, None);
        std::fs::write(p.dir.join("footage").join("clip1.mp4"), b"not a video").unwrap();
        let token = CancelToken::new();
        let stop = token.clone();
        let r = render(&p, token, move |pr| {
            if pr.stage == Stage::Probing {
                stop.cancel();
            }
        });
        let err = r.expect_err("a stopped render must not succeed");
        assert!(is_cancelled(&err), "the render went on probing after the Stop: {err:#}");
        assert_eq!(std::fs::read(&p.outputs[0]).unwrap(), OLD_VIDEO);
        let _ = std::fs::remove_dir_all(&p.dir);
    }

    #[test]
    fn a_render_stopped_before_it_starts_reports_cancelled() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not on PATH; skipping");
            return;
        }
        // Stopped while queued for its thread: it must say "cancelled",
        // not trip over the first thing it would have done (here: a
        // missing footage folder).
        let p = project("before", 1, Some("/no/such/footage/folder"));
        let token = CancelToken::new();
        token.cancel();
        let err = render(&p, token, |_| {}).expect_err("a stopped render must not succeed");
        assert!(is_cancelled(&err), "{err:#}");
        assert_eq!(std::fs::read(&p.outputs[0]).unwrap(), OLD_VIDEO);
        let _ = std::fs::remove_dir_all(&p.dir);
    }
}
