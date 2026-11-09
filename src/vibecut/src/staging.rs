//! Write-then-rename for a render's outputs.
//!
//! ffmpeg writes in place (`-y` truncates the target first), so a render
//! that fails or is cancelled halfway through the mux would leave a
//! half-written file where the last good video was. Instead every
//! `[[outputs]]` file is written under a staging name next to it and only
//! renamed into place by [`StagedOutputs::commit`], once the whole render
//! has succeeded. Dropping uncommitted staged outputs deletes them.
//!
//! Two writers can aim at the same file: a render whose extra output is
//! `out/edit.reels.mp4` and a `reformat out/edit.mp4 --preset reels` (the
//! same name, by design), possibly in another process — the CLI, the MCP
//! server, the web server. So each writer stages under a name of its own
//! (`edit.reels.partial-<pid>-<n>.mp4`) and never touches another live
//! writer's file; the last commit wins, with a whole file either way.
//! Leftovers of a writer that died are removed by the next one (see
//! [`StagedOutputs::new`]).
//!
//! [`StagedOutputs::commit`] stamps every file it moves into place with the
//! commit time, so "newer than the render" means made after the render
//! landed — what a reformat's cache check relies on.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

/// Another process's staging file not written for this long is a leftover
/// of a writer that died. A live writer's file is idle at most while the
/// render's other outputs are encoded, minutes, not hours.
const STALE_AFTER: Duration = Duration::from_secs(6 * 3600);

/// Staging files held by live [`StagedOutputs`] in this process.
static LIVE: LazyLock<Mutex<HashSet<Utf8PathBuf>>> = LazyLock::new(Mutex::default);
static NEXT: AtomicU64 = AtomicU64::new(0);

fn live() -> std::sync::MutexGuard<'static, HashSet<Utf8PathBuf>> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// `final_path`'s name split around where the staging tag goes:
/// `reel.mp4` → (`reel`, `.mp4`).
fn stem_and_ext(final_path: &Utf8Path) -> (&str, String) {
    let stem = final_path.file_stem().unwrap_or("output");
    let ext = final_path.extension().map(|e| format!(".{e}")).unwrap_or_default();
    (stem, ext)
}

/// A new staging path for `final_path`, unique to this writer: same
/// directory (so the rename stays on one filesystem and is atomic), same
/// extension (ffmpeg picks the muxer from it).
/// `reel.mp4` → `reel.partial-<pid>-<n>.mp4`.
pub fn staging_path(final_path: &Utf8Path) -> Utf8PathBuf {
    let (stem, ext) = stem_and_ext(final_path);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    final_path.with_file_name(format!("{stem}.partial-{}-{n}{ext}", std::process::id()))
}

/// Whether `name` is a staging file of `final_path`, and whose:
/// `Some(Some(pid))` for one of [`staging_path`]'s names, `Some(None)` for
/// the fixed `reel.partial.mp4` older versions used, `None` otherwise.
fn staging_owner(final_path: &Utf8Path, name: &str) -> Option<Option<u32>> {
    let (stem, ext) = stem_and_ext(final_path);
    let tag = name.strip_prefix(stem)?.strip_suffix(ext.as_str())?.strip_prefix(".partial")?;
    if tag.is_empty() {
        return Some(None);
    }
    let (pid, n) = tag.strip_prefix('-')?.split_once('-')?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok().map(Some)
}

/// The staging files of `final_path` on disk now, any writer's.
pub fn staged_files(final_path: &Utf8Path) -> Vec<Utf8PathBuf> {
    let dir = match final_path.parent() {
        Some(d) if !d.as_str().is_empty() => d,
        _ => Utf8Path::new("."),
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Utf8PathBuf> = entries
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| staging_owner(final_path, n).is_some())
        .map(|n| final_path.with_file_name(n))
        .collect();
    out.sort();
    out
}

/// A staging file nobody is writing any more.
fn is_leftover(path: &Utf8Path, owner: Option<u32>) -> bool {
    match owner {
        // The fixed name of older versions, which cleared it on start too.
        None => true,
        // Ours (this process, or an earlier one that had our pid — a
        // restarted container's server is pid 1 every time): live only
        // while a `StagedOutputs` here holds it.
        Some(pid) if pid == std::process::id() => !live().contains(path),
        // Another process's: we can't ask it, so wait until it's stale.
        Some(_) => std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age > STALE_AFTER),
    }
}

/// A render's outputs while they're being written.
#[derive(Debug)]
pub struct StagedOutputs {
    /// `(staged, final)`, in `[[outputs]]` order.
    files: Vec<(Utf8PathBuf, Utf8PathBuf)>,
    committed: bool,
}

impl StagedOutputs {
    /// Stage `finals`, each under a staging name of its own. Leftovers of
    /// writers that died without cleaning up (a killed process) are
    /// removed first; another live writer's staging file is left alone.
    pub fn new<'a>(finals: impl IntoIterator<Item = &'a Utf8Path>) -> Self {
        let finals: Vec<&Utf8Path> = finals.into_iter().collect();
        for f in &finals {
            for old in staged_files(f) {
                let name = old.file_name().unwrap_or_default();
                if staging_owner(f, name).is_some_and(|owner| is_leftover(&old, owner)) {
                    let _ = std::fs::remove_file(&old);
                }
            }
        }
        let files: Vec<_> = finals.iter().map(|f| (staging_path(f), f.to_path_buf())).collect();
        let mut held = live();
        for (staged, _) in &files {
            held.insert(staged.clone());
        }
        drop(held);
        Self { files, committed: false }
    }

    /// The staging path of output `i`.
    pub fn path(&self, i: usize) -> &Utf8Path {
        &self.files[i].0
    }

    /// Move every staged file into place. Call only once nothing can fail
    /// or be cancelled any more. Every file gets the commit time as its
    /// modification time (one time for all, so a render's outputs are
    /// equally new): a file written earlier and committed later must not
    /// look older than something made from the output it replaced.
    pub fn commit(mut self) -> Result<()> {
        let now = SystemTime::now();
        for (staged, fin) in &self.files {
            stamp(staged, now);
            std::fs::rename(staged, fin).with_context(|| format!("moving {staged} into place as {fin}"))?;
        }
        self.committed = true;
        Ok(())
    }
}

/// Set `path`'s modification time; best effort (the rename is what matters).
fn stamp(path: &Utf8Path, t: SystemTime) {
    let r = std::fs::File::options().write(true).open(path).and_then(|f| f.set_modified(t));
    if let Err(e) = r {
        tracing::debug!(%path, error = %e, "could not stamp the commit time");
    }
}

impl Drop for StagedOutputs {
    fn drop(&mut self) {
        let mut held = live();
        for (staged, _) in &self.files {
            if !self.committed {
                let _ = std::fs::remove_file(staged);
            }
            held.remove(staged);
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
    fn staging_keeps_dir_and_extension_and_is_unique() {
        let fin = Utf8Path::new("/p/out/reel.mp4");
        let a = staging_path(fin);
        let b = staging_path(fin);
        assert_ne!(a, b, "two writers share a staging file");
        for s in [&a, &b] {
            assert_eq!(s.parent(), Some(Utf8Path::new("/p/out")));
            assert_eq!(s.extension(), Some("mp4"));
            let name = s.file_name().unwrap();
            assert!(name.starts_with(&format!("reel.partial-{}-", std::process::id())), "{name}");
            assert_eq!(staging_owner(fin, name), Some(Some(std::process::id())));
        }
        assert_eq!(staging_owner(fin, "reel.partial.mp4"), Some(None));
        assert_eq!(staging_owner(fin, "reel.partial-12-3.mp4"), Some(Some(12)));
        // Not reel.mp4's: another output's, the final file, odd names.
        for n in [
            "reel.reels.partial-12-3.mp4",
            "reel.mp4",
            "reel.partial-x-3.mp4",
            "reel.partial-12-.mp4",
            "reel.partial-12-3.mov",
        ] {
            assert_eq!(staging_owner(fin, n), None, "{n}");
        }
        assert!(staging_path(Utf8Path::new("reel")).as_str().starts_with("reel.partial-"));
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
        assert!(staged_files(&out).is_empty(), "partial file left behind");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn commit_replaces_every_output() {
        let d = tmp("commit");
        let a = d.join("a.mp4");
        let b = d.join("b.mp4");
        std::fs::write(&a, b"old a").unwrap();
        // Stale partials from crashed runs are cleared on stage: the fixed
        // name of older versions, and one of an earlier life of this pid.
        std::fs::write(d.join("b.partial.mp4"), b"stale").unwrap();
        std::fs::write(d.join(format!("b.partial-{}-999999999.mp4", std::process::id())), b"stale").unwrap();
        let staged = StagedOutputs::new([a.as_path(), b.as_path()]);
        assert!(staged_files(&b).is_empty(), "stale partials kept: {:?}", staged_files(&b));
        std::fs::write(staged.path(0), b"new a").unwrap();
        std::fs::write(staged.path(1), b"new b").unwrap();
        staged.commit().unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), b"new a");
        assert_eq!(std::fs::read(&b).unwrap(), b"new b");
        assert!(staged_files(&a).is_empty() && staged_files(&b).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A render with `edit.reels.mp4` as an extra output and a reformat of
    /// `edit.mp4` to reels write the same file. Neither may delete or
    /// write over the other's partial file; each commits a whole file.
    #[test]
    fn two_writers_of_one_file_keep_their_own_partials() {
        let d = tmp("two_writers");
        let out = d.join("edit.reels.mp4");
        let render = StagedOutputs::new([out.as_path()]);
        std::fs::write(render.path(0), b"the render's reel, half written").unwrap();
        // Another process's live partial, recently written.
        let other = d.join("edit.reels.partial-4000000001-0.mp4");
        std::fs::write(&other, b"another process's reel").unwrap();
        let reformat = StagedOutputs::new([out.as_path()]);
        assert_ne!(render.path(0), reformat.path(0));
        assert_eq!(std::fs::read(render.path(0)).unwrap(), b"the render's reel, half written");
        assert!(other.exists(), "another live writer's partial was removed");
        std::fs::write(reformat.path(0), b"the reformat's reel").unwrap();
        reformat.commit().unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"the reformat's reel");
        std::fs::write(render.path(0), b"the render's reel").unwrap();
        render.commit().unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"the render's reel");
        assert_eq!(staged_files(&out), vec![other.clone()]);
        // Once it hasn't been written for hours, it's a dead writer's.
        stamp(&other, SystemTime::now() - STALE_AFTER - Duration::from_secs(60));
        drop(StagedOutputs::new([out.as_path()]));
        assert!(staged_files(&out).is_empty(), "a stale partial was kept");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The commit time, not ffmpeg's last write, is what a committed file
    /// is dated: a version made in the meantime from the old output must
    /// not look newer than the new one.
    #[test]
    fn commit_dates_the_outputs_when_they_land() {
        let d = tmp("dates");
        let master = d.join("edit.mp4");
        let reels = d.join("edit.reels.mp4");
        let staged = StagedOutputs::new([master.as_path(), reels.as_path()]);
        std::fs::write(staged.path(0), b"new master").unwrap();
        std::fs::write(staged.path(1), b"new reels").unwrap();
        let written = SystemTime::now() - Duration::from_secs(600);
        for i in 0..2 {
            stamp(staged.path(i), written);
        }
        // Something made now from the old master.
        let version = d.join("edit.square.mp4");
        std::fs::write(&version, b"square of the old master").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        staged.commit().unwrap();
        let mtime = |p: &Utf8Path| std::fs::metadata(p).unwrap().modified().unwrap();
        assert!(mtime(&master) > mtime(&version), "the new master looks older than a version of the old one");
        assert_eq!(mtime(&master), mtime(&reels), "a render's outputs aren't equally new");
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
        let plain = vec!["resolution = \"320x180\""; n_outputs];
        project_with(name, &plain, folder)
    }

    /// [`project`] with one `[[outputs]]` entry per item of
    /// `keys_per_output`, each adding its own keys (a resolution, a
    /// platform preset, a fit).
    fn project_with(name: &str, keys_per_output: &[&str], folder: Option<&str>) -> Project {
        let n_outputs = keys_per_output.len();
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
        for (o, keys) in outputs.iter().zip(keys_per_output) {
            toml.push_str(&format!(
                "\n[[outputs]]\npath = \"{}\"\n{keys}\nencoder = \"libx264\"\ncrf = 30\n",
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
                *seen.lock().unwrap() = Some(finals.iter().map(|o| !staged_files(o).is_empty()).collect());
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
    fn a_stopped_render_keeps_its_reframed_platform_outputs() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not on PATH; skipping");
            return;
        }
        // A lone 9:16 platform output isn't the body's shape, so it gets
        // its own reframe pass into `staged.path(0)` instead of the mux
        // writing there; next to a master it is output 1.
        let reel = "preset = \"reels\"\nresolution = \"180x320\"\nfit = \"blur\"";
        let p = project_with("reels_only", &[reel], None);
        stop_after_writing(&p);
        render(&p, CancelToken::new(), |_| {}).expect("an unstopped render succeeds");
        let made = crate::output_format::probe_source(&p.outputs[0]).unwrap();
        assert_eq!((made.width, made.height), (180, 320), "the reel wasn't reframed");
        assert!(partials_left(&p).is_empty());
        let _ = std::fs::remove_dir_all(&p.dir);

        let p = project_with("master_and_reels", &["resolution = \"320x180\"", reel], None);
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
