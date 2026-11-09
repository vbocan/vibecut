//! Vibe library — natural-language intent → starting TOML config.
//!
//! Each vibe is a curated pairing of:
//!
//! - a **rationale** the LLM uses to decide whether to propose this vibe
//!   when the user describes their footage / intent;
//! - a **TOML template** with `{placeholder}` substitutions that bake in
//!   sensible defaults for that aesthetic.
//!
//! Each of the six vibes targets a distinct aesthetic:
//!
//! | Vibe | When to suggest |
//! |---|---|
//! | `kid_reel` | Kinetic family / kid footage; music-driven |
//! | `family_chronicle` | Multi-event family chronicles; chronological |
//! | `drone_cinematic` | DJI / contemplative aerial footage |
//! | `action_highlight` | Sports, ATV, fast motion; punchy |
//! | `documentary` | Voice-led, chronological, ducks on speech |
//! | `memory_keeper` | Long-form vacation; gentle pacing |
//!
//! When the LLM asks "what kind of video do you want?", proposing two
//! or three vibes (not all six) keeps the choice tractable. The
//! mapping intent→vibe is the LLM's job; this module just provides
//! the templates.

pub struct Vibe {
    pub key: &'static str,
    pub name: &'static str,
    pub rationale: &'static str,
    /// Sensible default `target_duration` when the user doesn't say.
    /// The LLM can always override at proposal time.
    pub default_target_duration: &'static str,
    /// What music suits the vibe — drives the server-side song pick
    /// (`music::auto_pick`).
    pub music: MusicProfile,
    /// The vibe's `[soundtrack]` settings (everything but the file),
    /// one `key = value` per line.
    soundtrack: &'static str,
    template: &'static str,
}

/// The kind of song a vibe wants. Bands are inclusive; tracks outside a
/// band still qualify, scored by how far out they are.
pub struct MusicProfile {
    /// Comfortable tempo, BPM. A track at half or double a tempo in the
    /// band also fits (cuts land on every other beat).
    pub bpm: (f32, f32),
    /// Mean loudness relative to the track's peak, 0..1 — the library's
    /// `energy_mean`. Low reads sparse and calm, high reads dense.
    pub energy: (f32, f32),
    /// Library moods (`music::mood`) that suit it, best first.
    pub moods: &'static [&'static str],
}

/// The soundtrack a rendered config gets.
pub struct SoundtrackSub<'a> {
    pub file: &'a str,
    /// Why it was chosen; written as a comment above `[soundtrack]`.
    pub note: &'a str,
}

pub struct Substitutions<'a> {
    pub name: &'a str,
    pub title: &'a str,
    pub folder: &'a str,
    pub output_path: &'a str,
    pub target_duration: &'a str,
    pub seed: u64,
    /// `None` leaves the vibe's soundtrack settings commented out.
    pub soundtrack: Option<SoundtrackSub<'a>>,
}

impl Vibe {
    /// Substitute `{name}` / `{title}` / `{folder}` / `{output_path}` /
    /// `{target_duration}` / `{seed}` / `{soundtrack}` placeholders into
    /// the template.
    /// Naive search-and-replace — we author the templates so we don't
    /// need a real templating engine — but every substituted value
    /// lands inside a TOML basic (double-quoted) string in the
    /// template, so each one is escaped first. Skipping this is fine
    /// on the deployment target (container paths under `/footage` use
    /// forward slashes, nothing TOML-special) but silently corrupts
    /// the written config the moment `folder` is a native Windows
    /// path (`C:\Users\...`) — backslash is TOML's escape character,
    /// so `\U` two segments in reads as a broken `\UXXXXXXXX` unicode
    /// escape. Caught by an actual dev-loop run on Windows, not by
    /// anything that would fire inside the container.
    pub fn render(&self, s: &Substitutions<'_>) -> String {
        // `{soundtrack}` goes last: the block it expands to carries
        // user-facing text (the pick note) that must not be scanned for
        // the other placeholders.
        self.template
            .replace("{name}", &toml_escape(s.name))
            .replace("{title}", &toml_escape(s.title))
            .replace("{folder}", &toml_escape(s.folder))
            .replace("{output_path}", &toml_escape(s.output_path))
            .replace("{target_duration}", &toml_escape(s.target_duration))
            .replace("{seed}", &s.seed.to_string())
            .replace("{soundtrack}", &self.soundtrack_block(s.soundtrack.as_ref()))
    }

    /// The `[soundtrack]` section: live with a picked file, commented out
    /// (as a ready-to-use hint) without one.
    fn soundtrack_block(&self, pick: Option<&SoundtrackSub<'_>>) -> String {
        match pick {
            Some(p) => {
                let mut out = String::new();
                for line in p.note.lines() {
                    for wrapped in wrap_words(line, 76) {
                        out.push_str(&format!("# {wrapped}\n"));
                    }
                }
                out.push_str("[soundtrack]\n");
                out.push_str(&format!("file              = \"{}\"\n", toml_escape(p.file)));
                out.push_str(self.soundtrack.trim_end());
                out
            }
            None => {
                let mut out = String::from(
                    "# No soundtrack: the edit keeps the clips' own sound. Add one to unlock\n\
                     # beat-alignment, ducking, smart-loop and beat-burst:\n#\n\
                     # [soundtrack]\n\
                     # file              = \"/projects/music/your_track.mp3\"",
                );
                for line in self.soundtrack.lines() {
                    out.push_str(&format!("\n# {line}"));
                }
                out
            }
        }
    }
}

/// Split `text` into lines of at most `width` characters at spaces (a
/// single longer word stays whole). Blank input gives no lines.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Escape a value for embedding inside a TOML basic (double-quoted)
/// string: backslash and double-quote are the two characters that
/// matter for every field this module substitutes.
fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// ---------- 1. kid_reel ----------

const KID_REEL: Vibe = Vibe {
    key: "kid_reel",
    name: "Kid reel",
    rationale: "Kinetic family edit with GoPro-Quik-style speed-ramps + flash, energy-arc segment ordering so the strongest moments land near the middle, beat-burst at the energy peak when a soundtrack is provided. Best for active kid footage with audio.",
    default_target_duration: "3m",
    music: MusicProfile { bpm: (100.0, 135.0), energy: (0.35, 0.80), moods: &["upbeat", "driving", "warm"] },
    soundtrack: r#"beat_align        = true
duck_count        = 3
ducking_db        = -18
fade_out          = "3000ms"
beat_burst        = "auto""#,
    template: r##"schema_version = 1

[project]
name            = "{name}"
target_duration = "{target_duration}"
fps             = 30
seed            = {seed}
fade_in         = "300ms"
fade_out        = "1500ms"

[[outputs]]
path          = "{output_path}"
resolution    = "1920x1080"
encoder       = "auto"
crf           = 20
audio_bitrate = "192k"

[input]
folder  = "{folder}"
include = ["**/*.MP4", "**/*.mp4"]

{soundtrack}

[selection]
min_clip       = "2s"
max_clip       = "5s"
order          = "energy_arc"
variety        = 0.15

[transition]
default                = "fade"
duration               = "400ms"
speed_ramp_probability = 0.5
speed_ramp_duration    = "2500ms"
speed_ramp_factor      = 3.0
speed_ramp_flash       = true

[intro]
duration = "4.0s"
mode     = "card"
bg_color = "#1c1c2e"
fade_in  = "500ms"
fade_out = "500ms"

[[intro.layers]]
kind     = "text"
content  = "{title}"
size     = 100
color    = "#FFE0A0"
position = "center"

[grading]
stabilize = false
"##,
};

// ---------- 2. family_chronicle ----------

const FAMILY_CHRONICLE: Vibe = Vibe {
    key: "family_chronicle",
    name: "Family chronicle",
    rationale: "Medium-form, chronological cut with gentle pacing. Ducks the soundtrack so the funny moments / kid voices come through. No speed-ramps — the goal is to remember what happened, not stylise it. Best when the source spans multiple distinct events you want to keep in order.",
    default_target_duration: "5m",
    music: MusicProfile { bpm: (85.0, 120.0), energy: (0.25, 0.65), moods: &["warm", "upbeat", "chill"] },
    soundtrack: r#"beat_align        = true
duck_count        = 4
duck_min_dur      = "5s"
duck_max_dur      = "10s"
ducking_db        = -16
fade_out          = "3000ms"
vad_speech_weight = 1.5"#,
    template: r##"schema_version = 1

[project]
name            = "{name}"
target_duration = "{target_duration}"
fps             = 30
seed            = {seed}
fade_in         = "500ms"
fade_out        = "2000ms"

[[outputs]]
path          = "{output_path}"
resolution    = "1920x1080"
encoder       = "auto"
crf           = 20
audio_bitrate = "192k"

[input]
folder  = "{folder}"
include = ["**/*.MP4", "**/*.mp4"]

{soundtrack}

[selection]
min_clip       = "3s"
max_clip       = "7s"
order          = "chronological"
variety        = 0.05

[transition]
default                = "fade"
duration               = "700ms"
speed_ramp_probability = 0.0

[intro]
duration = "5.0s"
mode     = "card"
bg_color = "#0a1628"
fade_in  = "700ms"
fade_out = "600ms"

[[intro.layers]]
kind     = "text"
content  = "{title}"
size     = 90
color    = "#F5E6C8"
position = "center"

[grading]
stabilize = false
"##,
};

// ---------- 3. drone_cinematic ----------

const DRONE_CINEMATIC: Vibe = Vibe {
    key: "drone_cinematic",
    name: "Drone cinematic",
    rationale: "Contemplative aerial / drone footage, long holds, energy-arc ordering that builds toward the visual peak. No speed-ramps (the calm pace is the point). Long fade-in and especially long fade-out so the closing shot eases to black. Works on DJI footage where there's no source audio.",
    default_target_duration: "2m",
    music: MusicProfile { bpm: (60.0, 100.0), energy: (0.10, 0.50), moods: &["chill", "warm"] },
    soundtrack: r#"beat_align        = true
duck_count        = 0         # DJI = no source audio, ducking has nothing to reveal
fade_out          = "6000ms"  # match the long video fade
beat_burst        = "off"     # contemplative; never break into rapid cuts"#,
    template: r##"schema_version = 1

[project]
name            = "{name}"
target_duration = "{target_duration}"
fps             = 30
seed            = {seed}
fade_in         = "500ms"
fade_out        = "6000ms"

[[outputs]]
path          = "{output_path}"
resolution    = "1920x1080"
encoder       = "auto"
crf           = 20
audio_bitrate = "192k"

[input]
folder  = "{folder}"
include = ["**/*.MP4", "**/*.mp4"]

{soundtrack}

[selection]
min_clip       = "3s"
max_clip       = "8s"
order          = "energy_arc"
variety        = 0.20

[transition]
default                = "fade"
duration               = "600ms"
speed_ramp_probability = 0.0

[intro]
duration = "5.0s"
mode     = "card"
bg_color = "#000000"
fade_in  = "700ms"
fade_out = "700ms"

[[intro.layers]]
kind     = "text"
content  = "{title}"
size     = 110
color    = "#FFFFFF"
position = "center"

[grading]
stabilize = false
"##,
};

// ---------- 4. action_highlight ----------

const ACTION_HIGHLIGHT: Vibe = Vibe {
    key: "action_highlight",
    name: "Action highlight",
    rationale: "Short and punchy. Aggressive speed-ramps + Quik flash on ~70 % of seams; beat-burst on; shorter clips and shorter crossfades. Best for sports / ATV / mountain bike / surfing — content where the kinetic feel reinforces the subject.",
    default_target_duration: "90s",
    music: MusicProfile { bpm: (120.0, 175.0), energy: (0.45, 1.0), moods: &["driving", "upbeat"] },
    soundtrack: r#"beat_align        = true
duck_count        = 2
ducking_db        = -20
fade_out          = "2000ms"
beat_burst        = "auto""#,
    template: r##"schema_version = 1

[project]
name            = "{name}"
target_duration = "{target_duration}"
fps             = 30
seed            = {seed}
fade_in         = "200ms"
fade_out        = "1500ms"

[[outputs]]
path          = "{output_path}"
resolution    = "1920x1080"
encoder       = "auto"
crf           = 20
audio_bitrate = "192k"

[input]
folder  = "{folder}"
include = ["**/*.MP4", "**/*.mp4"]

# Soundtrack strongly recommended for this vibe — beat-burst + speed-ramps
# read best with a driving track underneath.
{soundtrack}

[selection]
min_clip       = "1.5s"
max_clip       = "4s"
order          = "energy_arc"
variety        = 0.10

[transition]
default                = "fade"
duration               = "300ms"
speed_ramp_probability = 0.7
speed_ramp_duration    = "2000ms"
speed_ramp_factor      = 3.0
speed_ramp_flash       = true

[intro]
duration = "3.0s"
mode     = "card"
bg_color = "#0a0a0a"
fade_in  = "300ms"
fade_out = "300ms"

[[intro.layers]]
kind     = "text"
content  = "{title}"
size     = 110
color    = "#FFFFFF"
position = "center"

[grading]
stabilize = false
"##,
};

// ---------- 5. documentary ----------

const DOCUMENTARY: Vibe = Vibe {
    key: "documentary",
    name: "Documentary",
    rationale: "Voice-led, chronological. No speed-ramps, no kinetic effects. Ducks the soundtrack for every speech moment we can detect (set vad_speech_weight high), letting the source audio narrate. Plain text cards, generous fades. Best when the footage tells a story you want to preserve.",
    default_target_duration: "5m",
    music: MusicProfile { bpm: (60.0, 100.0), energy: (0.05, 0.45), moods: &["chill", "warm"] },
    soundtrack: r#"beat_align        = true
duck_count        = 8
duck_min_dur      = "4s"
duck_max_dur      = "12s"
ducking_db        = -20
fade_out          = "3500ms"
vad_speech_weight = 2.5
beat_burst        = "off""#,
    template: r##"schema_version = 1

[project]
name            = "{name}"
target_duration = "{target_duration}"
fps             = 30
seed            = {seed}
fade_in         = "600ms"
fade_out        = "2500ms"

[[outputs]]
path          = "{output_path}"
resolution    = "1920x1080"
encoder       = "auto"
crf           = 20
audio_bitrate = "192k"

[input]
folder  = "{folder}"
include = ["**/*.MP4", "**/*.mp4"]

{soundtrack}

[selection]
min_clip       = "3s"
max_clip       = "8s"
order          = "chronological"
variety        = 0.0

[transition]
default                = "fade"
duration               = "800ms"
speed_ramp_probability = 0.0

[intro]
duration = "5.0s"
mode     = "card"
bg_color = "#000000"
fade_in  = "800ms"
fade_out = "800ms"

[[intro.layers]]
kind     = "text"
content  = "{title}"
size     = 84
color    = "#EEEEEE"
position = "center"

[grading]
stabilize = false
"##,
};

// ---------- 6. memory_keeper ----------

const MEMORY_KEEPER: Vibe = Vibe {
    key: "memory_keeper",
    name: "Memory keeper",
    rationale: "Long-form, gentle, soundtrack-led. Low variety so the picker stays close to its top picks across renders. Few duck windows so the music carries the mood. Best for vacation chronicles where you want a generous final cut.",
    default_target_duration: "8m",
    music: MusicProfile { bpm: (70.0, 110.0), energy: (0.20, 0.60), moods: &["warm", "chill"] },
    soundtrack: r#"beat_align        = true
duck_count        = 2
duck_min_spacing  = "60s"
duck_min_dur      = "5s"
duck_max_dur      = "12s"
ducking_db        = -18
fade_out          = "5000ms"
beat_burst        = "off""#,
    template: r##"schema_version = 1

[project]
name            = "{name}"
target_duration = "{target_duration}"
fps             = 30
seed            = {seed}
fade_in         = "800ms"
fade_out        = "4000ms"

[[outputs]]
path          = "{output_path}"
resolution    = "1920x1080"
encoder       = "auto"
crf           = 20
audio_bitrate = "192k"

[input]
folder  = "{folder}"
include = ["**/*.MP4", "**/*.mp4"]

{soundtrack}

[selection]
min_clip       = "3s"
max_clip       = "8s"
order          = "chronological"
variety        = 0.0
max_per_source = 5

[transition]
default                = "fade"
duration               = "800ms"
speed_ramp_probability = 0.0

[intro]
duration = "6.0s"
mode     = "card"
bg_color = "#142028"
fade_in  = "800ms"
fade_out = "800ms"

[[intro.layers]]
kind     = "text"
content  = "{title}"
size     = 96
color    = "#F0E4C8"
position = "center"

[grading]
stabilize = false
"##,
};

// ---------- registry ----------

const ALL_VIBES: &[&Vibe] = &[
    &KID_REEL,
    &FAMILY_CHRONICLE,
    &DRONE_CINEMATIC,
    &ACTION_HIGHLIGHT,
    &DOCUMENTARY,
    &MEMORY_KEEPER,
];

pub fn get(key: &str) -> Option<&'static Vibe> {
    ALL_VIBES.iter().find(|v| v.key == key).copied()
}

pub fn names() -> Vec<&'static str> {
    ALL_VIBES.iter().map(|v| v.key).collect()
}

/// Catalog summary for the LLM — `key`, `name`, `rationale`, and the
/// default target duration. The MCP server exposes this through a
/// `list_vibes` tool so the LLM can present options without having to
/// inspect each template.
pub fn catalog() -> Vec<serde_json::Value> {
    ALL_VIBES
        .iter()
        .map(|v| {
            serde_json::json!({
                "key": v.key,
                "name": v.name,
                "rationale": v.rationale,
                "default_target_duration": v.default_target_duration,
                "music": {
                    "bpm": [v.music.bpm.0, v.music.bpm.1],
                    "energy": [v.music.energy.0, v.music.energy.1],
                    "moods": v.music.moods,
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_six_vibes_registered() {
        let names = names();
        assert_eq!(names.len(), 6);
        for k in &[
            "kid_reel",
            "family_chronicle",
            "drone_cinematic",
            "action_highlight",
            "documentary",
            "memory_keeper",
        ] {
            assert!(get(k).is_some(), "missing vibe: {k}");
        }
        assert!(get("no_such_vibe").is_none());
    }

    #[test]
    fn every_template_substitutes_all_placeholders() {
        for v in ALL_VIBES {
            let toml = v.render(&Substitutions {
                name: "TestName",
                title: "TestTitle",
                folder: "/footage/test",
                output_path: "/projects/out/test.mp4",
                target_duration: "3m",
                seed: 42,
                soundtrack: None,
            });
            assert!(!toml.contains("{name}"), "{}: unfilled {{name}}", v.key);
            assert!(!toml.contains("{title}"), "{}: unfilled {{title}}", v.key);
            assert!(!toml.contains("{folder}"), "{}: unfilled {{folder}}", v.key);
            assert!(!toml.contains("{output_path}"), "{}: unfilled {{output_path}}", v.key);
            assert!(!toml.contains("{target_duration}"), "{}: unfilled {{target_duration}}", v.key);
            assert!(!toml.contains("{seed}"), "{}: unfilled {{seed}}", v.key);
            assert!(toml.contains("schema_version = 1"), "{}: missing schema_version", v.key);
            assert!(toml.contains("[project]"), "{}: missing [project]", v.key);
        }
    }

    /// A native Windows folder path (`C:\Users\...`) must not corrupt
    /// the generated TOML — regression test for the naive-substitution
    /// bug a Windows dev-loop run surfaced (backslash is TOML's string
    /// escape character; unescaped, `\Users` parses as a broken
    /// `\UXXXXXXXX` unicode escape).
    #[test]
    fn windows_path_with_backslashes_produces_valid_toml() {
        let toml = KID_REEL.render(&Substitutions {
            name: "TestName",
            title: "TestTitle",
            folder: r"C:\Users\vboca\OneDrive\Playground\Vibecut\examples",
            output_path: "/projects/out/test.mp4",
            target_duration: "3m",
            seed: 42,
            soundtrack: None,
        });
        let cfg: vibecut::project_config::Config = toml::from_str(&toml)
            .unwrap_or_else(|e| panic!("Windows path corrupted the TOML: {e}\n--- generated TOML ---\n{toml}"));
        assert_eq!(
            cfg.input.folder.unwrap().as_str(),
            r"C:\Users\vboca\OneDrive\Playground\Vibecut\examples"
        );
    }

    #[test]
    fn toml_escape_handles_backslash_and_quote() {
        assert_eq!(toml_escape(r"C:\Users\x"), r"C:\\Users\\x");
        assert_eq!(toml_escape(r#"a "quoted" value"#), r#"a \"quoted\" value"#);
        assert_eq!(toml_escape("plain"), "plain");
    }

    #[test]
    fn catalog_shape() {
        let c = catalog();
        assert_eq!(c.len(), 6);
        for entry in c {
            assert!(entry.get("key").is_some());
            assert!(entry.get("name").is_some());
            assert!(entry.get("rationale").is_some());
            assert!(entry.get("default_target_duration").is_some());
        }
    }

    /// Every curated vibe must produce a TOML that the vibecut
    /// pipeline's own `Config` can parse + validate. This catches
    /// schema drift the moment a new field lands in vibecut without
    /// being reflected in the templates here.
    #[test]
    fn every_vibe_template_is_valid_vibecut_config() {
        for key in names() {
            let v = get(key).unwrap();
            let toml = v.render(&Substitutions {
                name: "ValidationProbe",
                title: "Validation",
                folder: "/footage/test",
                output_path: "/projects/out/validation.mp4",
                target_duration: v.default_target_duration,
                seed: 42,
                soundtrack: None,
            });
            let cfg: vibecut::project_config::Config =
                toml::from_str(&toml).unwrap_or_else(|e| {
                    panic!("vibe {key} produced invalid TOML: {e}\n--- generated TOML ---\n{toml}")
                });
            cfg.validate()
                .unwrap_or_else(|e| panic!("vibe {key} failed Config::validate: {e}"));
            assert!(cfg.soundtrack.is_none(), "{key}: no pick, no soundtrack");
            assert!(toml.contains("# [soundtrack]"), "{key}: the commented hint stays");
        }
    }

    /// With a picked song every vibe writes a live `[soundtrack]` with
    /// its own settings, a valid config, and the pick note as a comment.
    #[test]
    fn every_vibe_renders_a_picked_soundtrack() {
        for key in names() {
            let v = get(key).unwrap();
            let toml = v.render(&Substitutions {
                name: "Probe",
                title: "Probe",
                folder: "/footage/test",
                output_path: "/projects/out/probe.mp4",
                target_duration: v.default_target_duration,
                seed: 42,
                soundtrack: Some(SoundtrackSub {
                    file: r#"/projects/music/a "quoted" {name}.mp3"#,
                    note: "Picked for you: it fits.\nSecond line.",
                }),
            });
            let cfg: vibecut::project_config::Config = toml::from_str(&toml)
                .unwrap_or_else(|e| panic!("vibe {key} with a soundtrack: {e}\n{toml}"));
            cfg.validate().unwrap_or_else(|e| panic!("vibe {key}: {e}"));
            let snd = cfg.soundtrack.unwrap_or_else(|| panic!("{key}: soundtrack missing"));
            assert_eq!(snd.files.len(), 1);
            assert_eq!(snd.files[0].as_str(), r#"/projects/music/a "quoted" {name}.mp3"#, "{key}");
            assert!(snd.beat_align, "{key}");
            assert!(toml.contains("# Picked for you: it fits.\n# Second line.\n[soundtrack]"), "{key}");
        }
        // The vibe's own settings come through.
        let render = |v: &Vibe| {
            let toml = v.render(&Substitutions {
                name: "p",
                title: "p",
                folder: "/f",
                output_path: "/o.mp4",
                target_duration: "30s",
                seed: 1,
                soundtrack: Some(SoundtrackSub { file: "/m/x.mp3", note: "" }),
            });
            toml::from_str::<vibecut::project_config::Config>(&toml).unwrap().soundtrack.unwrap()
        };
        assert_eq!(render(&DRONE_CINEMATIC).duck_count, 0);
        assert_eq!(render(&DRONE_CINEMATIC).beat_burst, vibecut::project_config::BeatBurstMode::Off);
        assert_eq!(render(&DOCUMENTARY).vad_speech_weight, 2.5);
        assert_eq!(render(&FAMILY_CHRONICLE).ducking_db, -16.0);
    }

    #[test]
    fn long_pick_notes_wrap_into_comment_lines() {
        let note = "Picked for you: Night Drive. 132 BPM sits in Action highlight's 120–175 BPM range, its driving feel suits the vibe, and at 3:12 it covers the whole 30 s cut.";
        let toml = ACTION_HIGHLIGHT.render(&Substitutions {
            name: "p",
            title: "p",
            folder: "/f",
            output_path: "/o.mp4",
            target_duration: "30s",
            seed: 1,
            soundtrack: Some(SoundtrackSub { file: "/m/x.mp3", note }),
        });
        let comments: Vec<&str> = toml
            .lines()
            .skip_while(|l| !l.starts_with("# Picked for you"))
            .take_while(|l| l.starts_with('#'))
            .collect();
        assert!(comments.len() >= 2, "{toml}");
        assert!(comments.iter().all(|l| l.chars().count() <= 78), "{comments:?}");
        let rejoined: Vec<&str> = comments.iter().map(|l| l.trim_start_matches("# ")).collect();
        assert_eq!(rejoined.join(" "), note);
        // The section follows the note directly and ends without a stray blank line.
        assert!(toml.contains("# cut.\n[soundtrack]\n"), "{toml}");
        assert!(toml.contains("beat_burst        = \"auto\"\n\n[selection]"), "{toml}");
        assert_eq!(wrap_words("  ", 10), Vec::<String>::new());
        assert_eq!(wrap_words("a verylongword b", 4), vec!["a", "verylongword", "b"]);
    }

    #[test]
    fn music_profiles_are_sane() {
        for v in ALL_VIBES {
            let m = &v.music;
            assert!(m.bpm.0 >= 50.0 && m.bpm.0 < m.bpm.1 && m.bpm.1 <= 200.0, "{}", v.key);
            assert!(m.energy.0 >= 0.0 && m.energy.0 < m.energy.1 && m.energy.1 <= 1.0, "{}", v.key);
            assert!(!m.moods.is_empty(), "{}", v.key);
            for mood in m.moods {
                assert!(["chill", "warm", "upbeat", "driving"].contains(mood), "{}: {mood}", v.key);
            }
        }
    }
}
