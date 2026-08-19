//! On-disk content cache. Used to skip the 22-minute Phase-6 visual analysis
//! when the source file (and analysis parameters) haven't changed since last
//! run.
//!
//! # Cache key
//!
//! `blake3(file_path || file_size || file_mtime_ns || params_json)` truncated
//! to 16 hex chars. We deliberately do **not** hash file content — that would
//! cost ~5 minutes on a 48 GB folder per run, defeating the cache's purpose.
//! `(size, mtime)` is sufficient to detect virtually any real edit; on the
//! pathological case of identical-size identical-mtime different-content,
//! the user can `rm -rf <cache_dir>`.
//!
//! # Storage
//!
//! Each entry is one JSON file in `cache_dir/<key>.<scope>.json`. JSON keeps
//! it inspectable; bincode is a future optimization if entry sizes become
//! the bottleneck (visual analysis at 2 fps × 1080p × 16 K frames = ~200 KB
//! per source, well within JSON parse-time tolerance).

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use serde::{de::DeserializeOwned, Serialize};
use std::time::SystemTime;

/// One cache scope (subdirectory + filename suffix). Keeps unrelated entries
/// for the same source file separate (e.g. visual analysis vs. GPMF parse).
#[derive(Debug, Clone, Copy)]
pub struct Scope(pub &'static str);

pub const VISUAL: Scope = Scope("visual");

#[derive(Debug, Clone)]
pub struct Cache {
    dir: Utf8PathBuf,
    enabled: bool,
}

impl Cache {
    /// Open (and create) a cache directory. If `enabled = false`, all
    /// `load`/`save` calls are no-ops — useful for `--no-cache` flag.
    pub fn open(dir: &Utf8Path, enabled: bool) -> Result<Self> {
        if enabled {
            std::fs::create_dir_all(dir.as_std_path())
                .with_context(|| format!("creating cache dir {dir}"))?;
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            enabled,
        })
    }

    #[allow(dead_code)] // public introspection getter, not yet consumed
    pub fn dir(&self) -> &Utf8Path {
        &self.dir
    }

    /// Compute a cache key from a source-file path + an arbitrary
    /// serializable params struct. Returns 16 hex chars (≈ 64-bit), more
    /// than enough to disambiguate entries on a per-user cache.
    pub fn key_for_file(path: &Utf8Path, params: &impl Serialize) -> Result<String> {
        let meta = std::fs::metadata(path.as_std_path())
            .with_context(|| format!("stat {path}"))?;
        let size = meta.len();
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let params_json = serde_json::to_string(params).unwrap_or_default();

        let mut h = blake3::Hasher::new();
        h.update(path.as_str().as_bytes());
        h.update(&size.to_le_bytes());
        h.update(&mtime_ns.to_le_bytes());
        h.update(params_json.as_bytes());
        Ok(h.finalize().to_hex().to_string()[..16].to_string())
    }

    fn entry_path(&self, key: &str, scope: Scope) -> Utf8PathBuf {
        self.dir.join(format!("{key}.{}.json", scope.0))
    }

    /// Load a cached value. Returns `None` if disabled, missing, or unparseable.
    pub fn load<T: DeserializeOwned>(&self, key: &str, scope: Scope) -> Option<T> {
        if !self.enabled {
            return None;
        }
        let p = self.entry_path(key, scope);
        let bytes = std::fs::read(p.as_std_path()).ok()?;
        match serde_json::from_slice::<T>(&bytes) {
            Ok(v) => {
                tracing::debug!(key, scope = scope.0, "cache hit");
                Some(v)
            }
            Err(e) => {
                tracing::warn!(key, scope = scope.0, error = %e, "cache entry corrupt; ignoring");
                None
            }
        }
    }

    pub fn save<T: Serialize>(&self, key: &str, scope: Scope, value: &T) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let p = self.entry_path(key, scope);
        let bytes = serde_json::to_vec(value)?;
        std::fs::write(p.as_std_path(), bytes)
            .with_context(|| format!("writing cache entry {p}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Sample {
        name: String,
        values: Vec<i32>,
    }

    fn tmp_dir(name: &str) -> Utf8PathBuf {
        let p = std::env::temp_dir().join(format!("sv_cache_test_{name}"));
        let _ = std::fs::remove_dir_all(&p);
        Utf8PathBuf::from_path_buf(p).unwrap()
    }

    #[test]
    fn round_trip() {
        let dir = tmp_dir("round_trip");
        let cache = Cache::open(&dir, true).unwrap();
        let v = Sample { name: "x".into(), values: vec![1, 2, 3] };
        cache.save("k1", VISUAL, &v).unwrap();
        let loaded: Sample = cache.load("k1", VISUAL).unwrap();
        assert_eq!(loaded, v);
    }

    #[test]
    fn miss_returns_none() {
        let dir = tmp_dir("miss");
        let cache = Cache::open(&dir, true).unwrap();
        let r: Option<Sample> = cache.load("nope", VISUAL);
        assert!(r.is_none());
    }

    #[test]
    fn disabled_is_noop() {
        let dir = tmp_dir("disabled");
        let cache = Cache::open(&dir, false).unwrap();
        let v = Sample { name: "x".into(), values: vec![1, 2, 3] };
        cache.save("k1", VISUAL, &v).unwrap();
        let r: Option<Sample> = cache.load("k1", VISUAL);
        assert!(r.is_none());
    }

    #[test]
    fn key_changes_with_params() {
        // Synthetic file in tmp.
        let dir = tmp_dir("keys");
        std::fs::create_dir_all(dir.as_std_path()).unwrap();
        let f = dir.join("test.bin");
        std::fs::write(f.as_std_path(), b"hello").unwrap();

        #[derive(Serialize)]
        struct P { fps: f32, w: u32 }
        let k1 = Cache::key_for_file(&f, &P { fps: 2.0, w: 480 }).unwrap();
        let k2 = Cache::key_for_file(&f, &P { fps: 2.0, w: 480 }).unwrap();
        let k3 = Cache::key_for_file(&f, &P { fps: 4.0, w: 480 }).unwrap();
        assert_eq!(k1, k2);
        assert_ne!(k1, k3);
    }
}
