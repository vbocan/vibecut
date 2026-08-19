//! Mount-point conventions shared by every transport. Mirrors
//! `docker/compose.yml` — `/footage` is the read-only bind-mount for
//! source material, `/projects` is the read-write mount for TOMLs,
//! rendered output, and session logs.

use std::path::{Path, PathBuf};

pub const FOOTAGE_ROOT: &str = "/footage";
pub const PROJECTS_ROOT: &str = "/projects";

/// Resolve a user-supplied footage path against `/footage`. Empty or
/// `"."` means "the whole mount".
pub fn resolve_footage(p: &str) -> PathBuf {
    if p.is_empty() || p == "." {
        return PathBuf::from(FOOTAGE_ROOT);
    }
    let path = PathBuf::from(p);
    if path.is_absolute() {
        path
    } else {
        Path::new(FOOTAGE_ROOT).join(p)
    }
}

/// Resolve a user-supplied path against `/projects`.
pub fn resolve_projects(p: &str) -> PathBuf {
    if p.is_empty() || p == "." {
        return PathBuf::from(PROJECTS_ROOT);
    }
    let path = PathBuf::from(p);
    if path.is_absolute() {
        path
    } else {
        Path::new(PROJECTS_ROOT).join(p)
    }
}

/// Turn an arbitrary title into a filesystem-safe basename.
pub fn slugify(s: &str) -> String {
    let re = regex::Regex::new(r"[^A-Za-z0-9_-]+").unwrap();
    let result = re.replace_all(s, "_").trim_matches('_').to_lowercase();
    if result.is_empty() {
        "edit".to_string()
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Ne dăm cu mașinuța"), "ne_d_m_cu_ma_inu_a");
        assert_eq!(slugify("Tomești 2026"), "tome_ti_2026");
        assert_eq!(slugify(""), "edit");
        assert_eq!(slugify("simple"), "simple");
        assert_eq!(slugify("ALL_CAPS"), "all_caps");
    }

    #[test]
    fn path_helpers_resolve() {
        assert_eq!(resolve_footage(""), PathBuf::from("/footage"));
        assert_eq!(
            resolve_footage("masinuta"),
            PathBuf::from("/footage/masinuta")
        );
        assert_eq!(resolve_footage("/abs"), PathBuf::from("/abs"));
        assert_eq!(
            resolve_projects("foo.toml"),
            PathBuf::from("/projects/foo.toml")
        );
    }
}
