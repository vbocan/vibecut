//! Mount-point conventions shared by every transport. Mirrors
//! `docker/compose.yml` — `/footage` is the read-only bind-mount for
//! source material, `/projects` is the read-write mount for TOMLs,
//! rendered output, and session logs.

use std::path::{Path, PathBuf};

pub const FOOTAGE_ROOT: &str = "/footage";
pub const PROJECTS_ROOT: &str = "/projects";

/// Resolve a user-supplied footage path against `/footage`. Empty or
/// `"."` means "the whole mount". A browse root's display path (`D:\GoPro`,
/// see [`crate::browse`]) names the folder under its container path; an
/// absolute path that exists is taken as it is.
pub fn resolve_footage(p: &str) -> PathBuf {
    if p.is_empty() || p == "." {
        return PathBuf::from(FOOTAGE_ROOT);
    }
    let path = PathBuf::from(p);
    if path.is_absolute() && path.exists() {
        return path;
    }
    if let Some(mapped) = crate::browse::display_to_container(&crate::browse::roots(), p) {
        return mapped;
    }
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
        assert_eq!(slugify("Zi însorită la mare"), "zi_nsorit_la_mare");
        assert_eq!(slugify("Excursie 2026"), "excursie_2026");
        assert_eq!(slugify(""), "edit");
        assert_eq!(slugify("simple"), "simple");
        assert_eq!(slugify("ALL_CAPS"), "all_caps");
    }

    #[test]
    fn path_helpers_resolve() {
        assert_eq!(resolve_footage(""), PathBuf::from("/footage"));
        assert_eq!(
            resolve_footage("vacation"),
            PathBuf::from("/footage/vacation")
        );
        assert_eq!(resolve_footage("/abs"), PathBuf::from("/abs"));
        assert_eq!(
            resolve_projects("foo.toml"),
            PathBuf::from("/projects/foo.toml")
        );
    }
}
