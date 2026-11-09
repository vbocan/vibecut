//! Font lookup for title-card text layers (`font = "..."` → drawtext's
//! `fontfile`).
//!
//! A layer's `font` resolves through this chain, first hit wins:
//!
//! 1. an existing file path, absolute or relative to the working directory;
//! 2. a file of that name under `$VIBECUT_FONTS_DIR` (one or more folders,
//!    separated like `PATH`, searched recursively);
//! 3. a file of that name in the platform's font folders
//!    (`C:/Windows/Fonts`, `/usr/share/fonts`, `~/Library/Fonts`, ...);
//! 4. fontconfig's `fc-match`, when it is installed. It also accepts family
//!    patterns (`"Bebas Neue"`, `"Inter:bold"`) and maps Windows file names
//!    such as `arialbd.ttf` to metric-compatible stand-ins (Liberation Sans).
//!
//! File names match case-insensitively and the extension is optional and
//! interchangeable: `Inter-Bold.ttf` finds `Inter-Bold.otf`.
//!
//! A layer with no `font` (or one nothing matches) gets the first of
//! [`DEFAULT_FONTS`] that is installed, then fontconfig's sans-serif.
//! If even that finds nothing, a warning is logged and drawtext gets no
//! `fontfile`, leaving the choice to ffmpeg, which fails on builds without
//! fontconfig.

use std::ffi::OsStr;
use std::process::{Command, Stdio};

use camino::{Utf8Path, Utf8PathBuf};

/// Extra font folder(s), searched before the system font folders.
pub const FONTS_DIR_ENV: &str = "VIBECUT_FONTS_DIR";

/// Fonts tried, in order, for a text layer that names none: the regular
/// faces of the Docker image's Inter and of DejaVu Sans, then Windows' Arial.
/// Regular, like the fontconfig "Sans" ffmpeg picked before, so long titles
/// that fit then still fit.
pub const DEFAULT_FONTS: &[&str] = &["Inter-Regular", "DejaVuSans", "arial"];

/// fontconfig pattern for the default when none of [`DEFAULT_FONTS`] exists.
pub const DEFAULT_FONT_PATTERN: &str = "sans-serif";

/// Extensions FreeType (drawtext) loads that the name lookup considers.
const FONT_EXTENSIONS: &[&str] = &["ttf", "otf", "ttc", "otc"];

/// Font folders nest by format and foundry
/// (`/usr/share/fonts/truetype/roboto/unhinted/RobotoTTF/Roboto-Bold.ttf`).
const MAX_DEPTH: usize = 6;

/// Windows core-font file names → fontconfig patterns, so a config written
/// on Windows (`font = "arialbd.ttf"`) still gets a matching face elsewhere.
/// fontconfig maps Arial, Times New Roman and Courier New to the
/// metric-compatible Liberation fonts the Docker image installs.
const WINDOWS_FONT_FILES: &[(&str, &str)] = &[
    ("arial", "Arial"),
    ("arialbd", "Arial:bold"),
    ("ariali", "Arial:italic"),
    ("arialbi", "Arial:bold:italic"),
    ("ariblk", "Arial:black"),
    ("times", "Times New Roman"),
    ("timesbd", "Times New Roman:bold"),
    ("timesi", "Times New Roman:italic"),
    ("timesbi", "Times New Roman:bold:italic"),
    ("cour", "Courier New"),
    ("courbd", "Courier New:bold"),
    ("couri", "Courier New:italic"),
    ("courbi", "Courier New:bold:italic"),
    ("verdana", "Verdana"),
    ("verdanab", "Verdana:bold"),
    ("calibri", "Calibri"),
    ("calibrib", "Calibri:bold"),
    ("segoeui", "Segoe UI"),
    ("segoeuib", "Segoe UI:bold"),
    ("impact", "Impact"),
];

/// Which step of the chain found the font.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontSource {
    Path,
    FontsDir,
    SystemDir,
    Fontconfig,
}

impl FontSource {
    pub fn label(self) -> &'static str {
        match self {
            FontSource::Path => "path",
            FontSource::FontsDir => "$VIBECUT_FONTS_DIR",
            FontSource::SystemDir => "system font folder",
            FontSource::Fontconfig => "fontconfig",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFont {
    pub path: Utf8PathBuf,
    pub source: FontSource,
    /// Set when the font asked for isn't installed and this one stands in
    /// for it: the family (or file) used instead.
    pub substitute: Option<String>,
}

/// One `fc-match` answer: the file and the family names it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FcMatch {
    pub file: Utf8PathBuf,
    pub families: Vec<String>,
}

pub type FcMatcher = Box<dyn Fn(&str) -> Option<FcMatch>>;

/// The folders and fontconfig hook one lookup uses. [`FontLookup::from_env`]
/// is the real one; tests build their own from temp dirs.
pub struct FontLookup {
    pub fonts_dirs: Vec<Utf8PathBuf>,
    pub system_dirs: Vec<Utf8PathBuf>,
    pub fc_match: Option<FcMatcher>,
}

impl FontLookup {
    /// `$VIBECUT_FONTS_DIR`, the platform font folders and `fc-match`.
    pub fn from_env() -> Self {
        Self::with_fonts_dir_var(std::env::var_os(FONTS_DIR_ENV).as_deref())
    }

    /// [`Self::from_env`] with the value of `$VIBECUT_FONTS_DIR` passed in.
    pub fn with_fonts_dir_var(fonts_dir_var: Option<&OsStr>) -> Self {
        Self {
            fonts_dirs: split_dirs(fonts_dir_var),
            system_dirs: system_font_dirs(),
            fc_match: Some(Box::new(run_fc_match)),
        }
    }

    /// Resolve a layer's `font` (`None` = not set) to a font file.
    pub fn resolve(&self, name: Option<&str>) -> Option<ResolvedFont> {
        let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) else {
            return self.resolve_default();
        };
        if let Some(found) = self.resolve_named(name) {
            return Some(found);
        }
        // Not installed and no fontconfig to substitute: the default beats
        // leaving it to ffmpeg.
        self.resolve_default().map(|mut f| {
            f.substitute = Some(f.path.file_name().unwrap_or(f.path.as_str()).to_string());
            f
        })
    }

    fn resolve_named(&self, name: &str) -> Option<ResolvedFont> {
        let path = Utf8Path::new(name);
        if path.is_file() {
            return Some(found(path.to_path_buf(), FontSource::Path));
        }
        let file = path.file_name().unwrap_or(name);
        if let Some(p) = self.find_file(file) {
            return Some(p);
        }
        let (pattern, family) = fc_pattern(name);
        self.fontconfig(&pattern, Some(&family))
    }

    fn resolve_default(&self) -> Option<ResolvedFont> {
        DEFAULT_FONTS
            .iter()
            .find_map(|name| self.find_file(name))
            .or_else(|| self.fontconfig(DEFAULT_FONT_PATTERN, None))
    }

    fn find_file(&self, file: &str) -> Option<ResolvedFont> {
        find_in_dirs(&self.fonts_dirs, file)
            .map(|p| found(p, FontSource::FontsDir))
            .or_else(|| find_in_dirs(&self.system_dirs, file).map(|p| found(p, FontSource::SystemDir)))
    }

    /// Ask fontconfig. It always answers with its closest match, so a
    /// different family than `family` is reported as a substitute.
    fn fontconfig(&self, pattern: &str, family: Option<&str>) -> Option<ResolvedFont> {
        let m = (self.fc_match.as_ref()?)(pattern)?;
        if !m.file.is_file() {
            return None;
        }
        let substitute = family
            .filter(|want| !m.families.iter().any(|got| same_family(want, got)))
            .map(|_| m.families.first().cloned().unwrap_or_else(|| m.file.to_string()));
        Some(ResolvedFont { path: m.file, source: FontSource::Fontconfig, substitute })
    }

    /// Where the lookup searched, for the not-found warning.
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = self.fonts_dirs.iter().map(|d| d.to_string()).collect();
        parts.extend(self.system_dirs.iter().map(|d| d.to_string()));
        if self.fc_match.is_some() {
            parts.push("fc-match".into());
        }
        parts.join(", ")
    }
}

fn found(path: Utf8PathBuf, source: FontSource) -> ResolvedFont {
    ResolvedFont { path, source, substitute: None }
}

/// Resolve a text layer's `font` for drawtext and log the outcome: which
/// file and why at info level, a stand-in for a missing font as a warning
/// (info for Windows core fonts, whose stand-ins are metric-compatible), and
/// a warning when nothing is found at all.
pub fn resolve_font(name: Option<&str>) -> Option<Utf8PathBuf> {
    let lookup = FontLookup::from_env();
    let label = name.unwrap_or("(default)");
    match lookup.resolve(name) {
        Some(f) => {
            match &f.substitute {
                Some(used) if name.is_some_and(|n| windows_font_pattern(n).is_some()) => {
                    tracing::info!(font = label, path = %f.path, "Windows font not installed here; using {used} in its place")
                }
                Some(used) => tracing::warn!(
                    font = label,
                    path = %f.path,
                    "font not installed; using {used} instead. Put the font file in \
                     ${FONTS_DIR_ENV} or give its absolute path"
                ),
                None => tracing::info!(font = label, path = %f.path, via = f.source.label(), "title-card font"),
            }
            Some(f.path)
        }
        None => {
            tracing::warn!(
                font = label,
                searched = %lookup.describe(),
                "no font found for a title-card text layer; drawtext falls back to ffmpeg's \
                 own default, which fails on builds without fontconfig. Install a font, \
                 set ${FONTS_DIR_ENV}, or give an absolute path"
            );
            None
        }
    }
}

/// `$VIBECUT_FONTS_DIR`-style value → folders (separated like `PATH`).
pub fn split_dirs(value: Option<&OsStr>) -> Vec<Utf8PathBuf> {
    let Some(value) = value else { return Vec::new() };
    std::env::split_paths(value)
        .filter(|p| !p.as_os_str().is_empty())
        .filter_map(|p| Utf8PathBuf::from_path_buf(p).ok())
        .collect()
}

/// The platform's font folders, user folders first.
fn system_font_dirs() -> Vec<Utf8PathBuf> {
    let env_dir = |var: &str, sub: &str| {
        std::env::var(var).ok().filter(|v| !v.is_empty()).map(|v| Utf8PathBuf::from(v).join(sub))
    };
    let mut dirs: Vec<Utf8PathBuf> = Vec::new();
    if cfg!(windows) {
        dirs.extend(env_dir("LOCALAPPDATA", "Microsoft/Windows/Fonts"));
        dirs.push(env_dir("WINDIR", "Fonts").unwrap_or_else(|| "C:/Windows/Fonts".into()));
    } else if cfg!(target_os = "macos") {
        dirs.extend(env_dir("HOME", "Library/Fonts"));
        dirs.extend(["/Library/Fonts", "/System/Library/Fonts"].map(Utf8PathBuf::from));
    } else {
        dirs.extend(env_dir("HOME", ".local/share/fonts"));
        dirs.extend(env_dir("HOME", ".fonts"));
        dirs.extend(["/usr/local/share/fonts", "/usr/share/fonts"].map(Utf8PathBuf::from));
    }
    dirs
}

/// `file` minus a font extension (`Inter-Bold.ttf` → `Inter-Bold`).
fn font_stem(file: &str) -> &str {
    match file.rsplit_once('.') {
        Some((stem, ext)) if FONT_EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext)) => stem,
        _ => file,
    }
}

/// First font named `file` in `dirs`, searching each folder recursively.
/// Within a folder an exact name beats one that differs only in extension.
pub fn find_in_dirs(dirs: &[Utf8PathBuf], file: &str) -> Option<Utf8PathBuf> {
    let want_stem = font_stem(file);
    for dir in dirs.iter().filter(|d| d.is_dir()) {
        let mut same_stem: Option<Utf8PathBuf> = None;
        let walk = walkdir::WalkDir::new(dir)
            .max_depth(MAX_DEPTH)
            .follow_links(true)
            .sort_by_file_name();
        for entry in walk.into_iter().filter_map(Result::ok) {
            if !entry.file_type().is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str() else { continue };
            if name.eq_ignore_ascii_case(file) {
                return Utf8PathBuf::from_path_buf(entry.into_path()).ok();
            }
            if same_stem.is_none() && font_stem(name) != name && font_stem(name).eq_ignore_ascii_case(want_stem) {
                same_stem = Utf8PathBuf::from_path_buf(entry.into_path()).ok();
            }
        }
        if same_stem.is_some() {
            return same_stem;
        }
    }
    None
}

fn windows_font_pattern(name: &str) -> Option<&'static str> {
    let file = Utf8Path::new(name).file_name().unwrap_or(name);
    let stem = font_stem(file);
    WINDOWS_FONT_FILES
        .iter()
        .find(|(f, _)| f.eq_ignore_ascii_case(stem))
        .map(|&(_, pattern)| pattern)
}

/// A layer's `font` as a fontconfig pattern, plus the family it asks for.
/// File names become `family:style=Style` (`Inter-Bold.ttf` →
/// `Inter:style=Bold`; fontconfig ignores blanks in family names, so
/// `DejaVuSans` finds "DejaVu Sans"); anything else is already a pattern.
pub fn fc_pattern(name: &str) -> (String, String) {
    if let Some(pattern) = windows_font_pattern(name) {
        return (pattern.to_string(), pattern_family(pattern));
    }
    let file = Utf8Path::new(name).file_name().unwrap_or(name);
    let stem = font_stem(file);
    let is_file_name = stem.len() != file.len() || (!file.contains(':') && file.contains('-'));
    if !is_file_name {
        return (name.to_string(), pattern_family(name));
    }
    match stem.rsplit_once('-') {
        Some((family, style)) if !family.is_empty() && !style.is_empty() => {
            (format!("{}:style={}", fc_escape(family), fc_escape(style)), family.to_string())
        }
        _ => (fc_escape(stem), stem.to_string()),
    }
}

/// Backslash-escape the characters fontconfig's pattern syntax reserves.
fn fc_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '-' | ':' | ',') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The first family of a fontconfig pattern (`Inter Display:black` →
/// `Inter Display`).
fn pattern_family(pattern: &str) -> String {
    let mut family = String::new();
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => family.extend(chars.next()),
            ':' | ',' => break,
            c => family.push(c),
        }
    }
    family.trim().to_string()
}

/// Family names compare the way fontconfig does: ignoring case and blanks.
fn same_family(a: &str, b: &str) -> bool {
    let squash = |s: &str| -> String {
        s.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect()
    };
    squash(a) == squash(b)
}

fn run_fc_match(pattern: &str) -> Option<FcMatch> {
    let out = Command::new("fc-match")
        .arg("--format=%{file}\n%{family}\n")
        .arg(pattern)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_fc_match(&String::from_utf8_lossy(&out.stdout))
}

/// `fc-match --format=%{file}\n%{family}\n` output → [`FcMatch`]. A font
/// with localized names lists several families, comma-separated.
pub fn parse_fc_match(out: &str) -> Option<FcMatch> {
    let mut lines = out.lines();
    let file = lines.next()?.trim();
    if file.is_empty() {
        return None;
    }
    let families = lines
        .next()
        .unwrap_or("")
        .split(',')
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .collect();
    Some(FcMatch { file: file.into(), families })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Fresh, empty temp folder for one test.
    fn temp_dir(name: &str) -> Utf8PathBuf {
        let dir = std::env::temp_dir().join(format!("vibecut_fonts_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Utf8PathBuf::from_path_buf(dir).unwrap()
    }

    fn touch(dir: &Utf8Path, rel: &str) -> Utf8PathBuf {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"font").unwrap();
        p
    }

    fn lookup(fonts_dirs: Vec<Utf8PathBuf>, system_dirs: Vec<Utf8PathBuf>) -> FontLookup {
        FontLookup { fonts_dirs, system_dirs, fc_match: None }
    }

    #[test]
    fn an_existing_path_is_used_as_is() {
        let dir = temp_dir("path");
        let file = touch(&dir, "Brand.otf");
        let f = lookup(vec![], vec![]).resolve(Some(file.as_str())).unwrap();
        assert_eq!(f, ResolvedFont { path: file, source: FontSource::Path, substitute: None });
    }

    #[test]
    fn fonts_dir_from_the_env_var_is_searched_recursively() {
        let a = temp_dir("env_a");
        let b = temp_dir("env_b");
        let want = touch(&b, "opentype/inter/Inter-Bold.otf");
        let var = std::env::join_paths([a.as_str(), "", b.as_str()]).unwrap();
        let l = FontLookup { fc_match: None, ..FontLookup::with_fonts_dir_var(Some(var.as_os_str())) };
        assert_eq!(l.fonts_dirs, vec![a.clone(), b.clone()]);
        // Case-insensitive, and `.ttf` in a config finds the `.otf` file.
        let f = l.resolve(Some("inter-bold.TTF")).unwrap();
        assert_eq!(f.path, want);
        assert_eq!(f.source, FontSource::FontsDir);
        assert_eq!(split_dirs(None), Vec::<Utf8PathBuf>::new());
    }

    #[test]
    fn fonts_dir_beats_system_folders_and_exact_names_beat_other_extensions() {
        let user = temp_dir("order_user");
        let sys = temp_dir("order_sys");
        touch(&sys, "Brand-Bold.ttf");
        let user_otf = touch(&user, "a/Brand-Bold.otf");
        let user_ttf = touch(&user, "z/Brand-Bold.ttf");
        let l = lookup(vec![user.clone()], vec![sys.clone()]);
        assert_eq!(l.resolve(Some("Brand-Bold.ttf")).unwrap().path, user_ttf);
        assert_eq!(l.resolve(Some("Brand-Bold.otf")).unwrap().path, user_otf);
        let sys_only = touch(&sys, "Other.ttf");
        let f = l.resolve(Some("Other")).unwrap();
        assert_eq!((f.path, f.source), (sys_only, FontSource::SystemDir));
    }

    #[test]
    fn no_font_set_picks_the_first_installed_default() {
        let user = temp_dir("default_user");
        let sys = temp_dir("default_sys");
        // Only the regular face counts: a wider bold default would push long
        // titles that used to fit off the frame.
        touch(&sys, "truetype/dejavu/DejaVuSans-Bold.ttf");
        let l = lookup(vec![user.clone()], vec![sys.clone()]);
        assert_eq!(l.resolve(None), None);
        let dejavu = touch(&sys, "truetype/dejavu/DejaVuSans.ttf");
        assert_eq!(l.resolve(None).unwrap().path, dejavu);
        assert_eq!(l.resolve(Some("  ")).unwrap().path, dejavu);
        // Inter (the image's sans) comes first in DEFAULT_FONTS.
        touch(&sys, "opentype/inter/Inter-Bold.otf");
        let inter = touch(&sys, "opentype/inter/Inter-Regular.otf");
        assert_eq!(l.resolve(None).unwrap().path, inter);
    }

    #[test]
    fn a_missing_font_falls_back_to_the_default_as_a_substitute() {
        let sys = temp_dir("fallback_sys");
        let dejavu = touch(&sys, "DejaVuSans.ttf");
        let f = lookup(vec![], vec![sys.clone()]).resolve(Some("Missing-Font.ttf")).unwrap();
        assert_eq!(f.path, dejavu);
        assert_eq!(f.substitute.as_deref(), Some("DejaVuSans.ttf"));
    }

    #[test]
    fn nothing_found_is_none() {
        let empty = temp_dir("none");
        let l = lookup(vec![empty.clone()], vec![empty.join("missing")]);
        assert_eq!(l.resolve(None), None);
        assert_eq!(l.resolve(Some("Inter-Bold.ttf")), None);
    }

    #[test]
    fn fontconfig_resolves_what_the_folders_dont_have() {
        let dir = temp_dir("fc");
        let liberation = touch(&dir, "LiberationSans-Bold.ttf");
        let bebas = touch(&dir, "BebasNeue-Regular.otf");
        let asked: Rc<RefCell<Vec<String>>> = Rc::default();
        let log = asked.clone();
        let (lib, beb) = (liberation.clone(), bebas.clone());
        let l = FontLookup {
            fonts_dirs: vec![],
            system_dirs: vec![],
            fc_match: Some(Box::new(move |pattern: &str| {
                log.borrow_mut().push(pattern.to_string());
                let (file, family) = match pattern {
                    p if p.starts_with("Bebas") => (beb.clone(), "Bebas Neue"),
                    "Arial:bold" => (lib.clone(), "Liberation Sans"),
                    "Gone:style=Bold" => ("/nonexistent/font.ttf".into(), "Gone"),
                    _ => (lib.clone(), "Liberation Sans"),
                };
                Some(FcMatch { file, families: vec![family.into()] })
            })),
        };
        // A family pattern, found as asked.
        let f = l.resolve(Some("Bebas Neue")).unwrap();
        assert_eq!((f.path, f.source, f.substitute), (bebas, FontSource::Fontconfig, None));
        // A Windows file name → its fontconfig stand-in, reported as such.
        let f = l.resolve(Some("arialbd.ttf")).unwrap();
        assert_eq!(f.path, liberation);
        assert_eq!(f.substitute.as_deref(), Some("Liberation Sans"));
        // fontconfig pointing at a file that isn't there is no answer; the
        // default pattern is tried next.
        let f = l.resolve(Some("Gone-Bold.ttf")).unwrap();
        assert_eq!(f.path, liberation);
        assert_eq!(
            *asked.borrow(),
            ["Bebas Neue", "Arial:bold", "Gone:style=Bold", DEFAULT_FONT_PATTERN]
        );
    }

    #[test]
    fn config_names_become_fontconfig_patterns() {
        let p = |n: &str| fc_pattern(n);
        assert_eq!(p("Inter-Bold.ttf"), ("Inter:style=Bold".into(), "Inter".into()));
        assert_eq!(p("DejaVuSans-Bold"), ("DejaVuSans:style=Bold".into(), "DejaVuSans".into()));
        assert_eq!(p("Noto-Sans-Bold.otf"), ("Noto\\-Sans:style=Bold".into(), "Noto-Sans".into()));
        assert_eq!(p("Lobster.ttf"), ("Lobster".into(), "Lobster".into()));
        assert_eq!(p("ARIALBD.TTF"), ("Arial:bold".into(), "Arial".into()));
        assert_eq!(p("C:/Windows/Fonts/arial.ttf"), ("Arial".into(), "Arial".into()));
        assert_eq!(p("Bebas Neue"), ("Bebas Neue".into(), "Bebas Neue".into()));
        assert_eq!(p("Inter Display:style=Black"), ("Inter Display:style=Black".into(), "Inter Display".into()));
        assert!(same_family("DejaVuSans", "DejaVu Sans"));
        assert!(!same_family("Inter", "DejaVu Sans"));
    }

    #[test]
    fn fc_match_output_parses() {
        let m = parse_fc_match("/usr/share/fonts/m/Montserrat-Black.otf\nMontserrat,Montserrat Black\n").unwrap();
        assert_eq!(m.file, "/usr/share/fonts/m/Montserrat-Black.otf");
        assert_eq!(m.families, ["Montserrat", "Montserrat Black"]);
        assert_eq!(parse_fc_match(""), None);
        assert_eq!(parse_fc_match("\n"), None);
    }

    /// Runs the real `fc-match` where it is installed (the Docker images);
    /// a no-op elsewhere.
    #[test]
    fn real_fc_match_returns_an_installed_file() {
        let Some(m) = run_fc_match(DEFAULT_FONT_PATTERN) else { return };
        assert!(m.file.is_file(), "fc-match returned {m:?}");
        assert!(!m.families.is_empty());
    }
}
