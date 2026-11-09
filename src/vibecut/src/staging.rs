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
