use super::*;
use std::time::Duration;

use vibecut::project_config::{
    BeatBurstMode, CardLayer, CardMode, Encoder, ImageLayer, Order, RenderOutput, SelectionExclude, SourceWeight,
    SyncMethod, TextBackground, TextCard, TextLayer,
};

// ---------- fixtures ----------

/// Deterministic pseudo-random numbers for the property tests.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(1);
        vibecut::rng::splitmix64(self.0)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
    fn ms(&mut self, max: u64, step: u64) -> Duration {
        Duration::from_millis(self.below(max / step + 1) * step)
    }
    /// A float as a person would type it, or now and then an arbitrary one.
    fn f32(&mut self, max: f32) -> f32 {
        if self.chance(15) {
            (self.next() % 1_000_003) as f32 / 1_000_003.0 * max
        } else {
            (self.below((max * 100.0) as u64 + 1)) as f32 / 100.0
        }
    }
}

/// A vibe rendered for one trip, the way propose writes it.
fn proposal(vibe: &str, folder: &str, title: &str, song: Option<&str>) -> Config {
    let v = vibes::get(vibe).unwrap();
    let (_, cfg) = propose::render_checked(
        v,
        &Substitutions {
            name: title,
            title,
            folder,
            output_path: "/projects/out/trip.mp4",
            target_duration: v.default_target_duration,
            seed: 42,
            soundtrack: song.map(|f| SoundtrackSub { file: f, note: "" }),
        },
    )
    .unwrap();
    cfg
}

fn text_layer(content: &str) -> CardLayer {
    CardLayer::Text(TextLayer {
        content: content.into(),
        font: None,
        size: 96,
        color: "#FFFFFF".into(),
        position: "center".into(),
        y_offset: 0,
        x_offset: 0,
        fade_in: None,
        fade_out: None,
        appear_at: None,
        disappear_at: None,
        background: None,
    })
}

/// A random but valid config: a vibe's proposal with every kind of style
/// setting shaken — and footage-specific keys set too, which a recipe
/// must leave behind.
fn random_config(rng: &mut Rng) -> Config {
    let vibe = *rng.pick(&VIBE_IDS);
    let trip = rng.pick(&["Coastline Bike Ride", "Alps Ski Day", "trip", "Été à Nice"]).to_string();
    let title = if rng.chance(50) { format!("{trip} · 2026-11-0{}", rng.below(9) + 1) } else { format!("Our {}", rng.below(1000)) };
    let song = rng.chance(80).then_some("/projects/music/song.mp3");
    let mut cfg = proposal(vibe, &format!("/footage/{trip}"), &title, song);
    let p = &mut cfg.project;
    if rng.chance(70) {
        p.target_duration = Duration::from_millis((rng.below(600) + 5) * 500);
    }
    if rng.chance(20) {
        p.fps = *rng.pick(&[24, 25, 30, 50, 60]);
    }
    if rng.chance(30) {
        p.seed = if rng.chance(10) { None } else { Some(rng.below(1 << 40)) };
    }
    if rng.chance(30) {
        p.fade_in = rng.ms(3000, 50);
        p.fade_out = rng.ms(8000, 1);
    }
    if rng.chance(20) {
        p.temp_dir = Some("/tmp/work".into());
        p.models_dir = "/opt/models".into();
    }
    if let Some(s) = cfg.soundtrack.as_mut() {
        if rng.chance(40) {
            s.duck_count = rng.below(9) as u32;
            s.ducking_db = -(rng.below(30) as f32) - rng.f32(1.0);
            s.duck_min_dur = rng.ms(6000, 500);
            s.duck_max_dur = s.duck_min_dur + rng.ms(9000, 100);
        }
        if rng.chance(40) {
            s.beat_burst = *rng.pick(&[BeatBurstMode::Auto, BeatBurstMode::On, BeatBurstMode::Off]);
            s.beat_burst_min_beats = rng.below(16) as u32;
            s.beat_burst_max_beats = s.beat_burst_min_beats + rng.below(100) as u32;
        }
        if rng.chance(20) {
            s.beat_align = rng.chance(50);
            s.energy_drives_pacing = rng.chance(50);
            s.vad_speech_weight = rng.f32(4.0);
            s.fade_out = rng.ms(10_000, 250);
            s.duck_ramp = rng.ms(1000, 7);
            s.duck_min_spacing = Duration::from_secs(rng.below(200));
        }
        if rng.chance(15) {
            s.files.push("/projects/music/second.mp3".into());
        }
    }
    if rng.chance(50) {
        let t = &mut cfg.transition;
        t.default = rng.pick(&["fade", "cut", "none", "dissolve", "smoothup", "wipeleft"]).to_string();
        t.duration = rng.ms(1500, 10);
        t.speed_ramp_probability = rng.f32(1.0);
        t.speed_ramp_factor = 1.0 + rng.f32(4.0);
        t.speed_ramp_duration = rng.ms(4000, 50);
        t.speed_ramp_flash = rng.chance(50);
        if rng.chance(30) {
            t.palette = (0..rng.below(4)).map(|i| ["fade", "radial", "zoomin"][i as usize % 3].to_string()).collect();
        }
    }
    if rng.chance(50) {
        let s = &mut cfg.selection;
        s.min_clip = Duration::from_millis(500 + rng.below(30) * 100);
        s.max_clip = s.min_clip + rng.ms(8000, 250);
        s.order = *rng.pick(&[Order::Chronological, Order::EnergyArc, Order::StrictChronological]);
        s.variety = rng.f32(1.0);
        s.max_per_source = rng.chance(30).then(|| rng.below(9) as u32 + 1);
    }
    if rng.chance(30) {
        cfg.selection.exclude.push(SelectionExclude {
            file: "GX010301.MP4".into(),
            start: Duration::from_secs(1),
            end: Duration::from_secs(3),
        });
        cfg.selection.source_weights.push(SourceWeight { source_id: "0301".into(), weight: 2.0 });
    }
    if rng.chance(25) {
        let sc = &mut cfg.scoring;
        sc.gps_speed = rng.f32(2.0);
        sc.faces = rng.f32(2.0);
        sc.sharpness_floor = rng.f32(0.9);
    }
    if rng.chance(15) {
        cfg.multi_angle.enabled = true;
        cfg.multi_angle.sync = *rng.pick(&[SyncMethod::Auto, SyncMethod::Audio, SyncMethod::CreationTime]);
        cfg.multi_angle.min_hold = rng.ms(4000, 100);
    }
    if rng.chance(20) {
        cfg.grading.stabilize = true;
        cfg.grading.lut = rng.chance(50).then(|| "/projects/luts/teal.cube".into());
    }
    match rng.below(10) {
        0 => cfg.intro = None,
        1..=4 => {
            let card = cfg.intro.as_mut().unwrap();
            card.duration = rng.ms(8000, 100) + Duration::from_millis(500);
            card.mode = if rng.chance(30) { CardMode::Overlay } else { CardMode::Card };
            card.bg_color = rng.pick(&["#000000", "#112233", "#1c1c2eCC"]).to_string();
            if let Some(CardLayer::Text(t)) = card.layers.first_mut() {
                t.size = 40 + rng.below(100) as u32;
                t.color = "#FFE0A0".into();
                t.y_offset = rng.below(200) as i32 - 100;
                t.fade_in = rng.chance(30).then(|| rng.ms(900, 100));
                t.background = rng.chance(20).then(|| TextBackground { color: "#00000080".into(), padding: Some("12".into()) });
                t.content = rng
                    .pick(&[title.clone(), format!("{trip} · 2026-11-08"), "{trip} · {date}".into(), format!("{title} — part 2"), "Made with Vibecut".into()])
                    .clone();
            }
            if rng.chance(40) {
                card.layers.push(text_layer(rng.pick(&["Day one", "{date}", "Summer"])));
            }
            if rng.chance(30) {
                card.layers.insert(
                    0,
                    CardLayer::Image(ImageLayer {
                        path: "/projects/logo.png".into(),
                        position: "top-left".into(),
                        height: 120,
                        y_offset: 0,
                        x_offset: 0,
                        opacity: 0.8,
                        fade_in: None,
                        fade_out: None,
                        appear_at: None,
                        disappear_at: None,
                    }),
                );
            }
        }
        _ => {}
    }
    if rng.chance(25) {
        cfg.outro = Some(TextCard {
            duration: rng.ms(5000, 500) + Duration::from_secs(1),
            fade_in: rng.ms(800, 100),
            fade_out: rng.ms(800, 100),
            mode: CardMode::Card,
            bg_color: "#000000".into(),
            layers: vec![text_layer(rng.pick(&["Thanks for watching", "{title}", "See you next time"]))],
        });
    }
    if rng.chance(40) {
        let out = |path: &str, preset: Option<&str>, res: Option<&str>| RenderOutput {
            path: path.into(),
            resolution: res.map(str::to_string),
            encoder: Encoder::Auto,
            crf: None,
            preset: preset.map(str::to_string),
            audio_bitrate: None,
            fit: None,
            encoder_preset: None,
            max_bitrate: None,
        };
        let first = &mut cfg.outputs[0];
        if rng.chance(30) {
            first.resolution = Some("3840x2160".into());
            first.crf = Some(18);
            first.encoder = Encoder::Nvenc;
            first.encoder_preset = Some("p5".into());
        }
        for i in 0..rng.below(3) {
            let mut o = out(&format!("/projects/out/x{i}.mp4"), Some(*rng.pick(&["reels", "square", "whatsapp", "tiktok"])), None);
            if rng.chance(40) {
                o.fit = Some(*rng.pick(&[
                    vibecut::output_format::Fit::Blur,
                    vibecut::output_format::Fit::Crop,
                    vibecut::output_format::Fit::Pad,
                ]));
            }
            if rng.chance(20) {
                o.max_bitrate = Some("3M".into());
            }
            cfg.outputs.push(o);
        }
    }
    cfg.validate().unwrap_or_else(|e| panic!("random config must validate: {e}"));
    cfg
}

/// A style as the config reads it: floats as the `f32` a config holds (a
/// decoded `0.15` and a serialized `0.15000000596046448` are the same
/// setting), and a null member the same as a missing one (`None`).
fn normalize(v: &Value) -> Value {
    match v {
        Value::Number(n) if n.is_f64() => {
            Value::Number(Number::from_f64((n.as_f64().unwrap() as f32) as f64).unwrap())
        }
        Value::Array(a) => Value::Array(a.iter().map(normalize).collect()),
        Value::Object(m) => {
            Value::Object(m.iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k.clone(), normalize(v))).collect())
        }
        other => other.clone(),
    }
}

/// The music a config's soundtrack would be shared as (fixture ids).
fn music_of(cfg: &Config) -> Music {
    match &cfg.soundtrack {
        None => Music::Original,
        Some(s) => Music::Songs {
            songs: s
                .files
                .iter()
                .map(|f| SongRef {
                    id: Some(fingerprint(7, f.as_str().as_bytes())),
                    title: vibecut::track_meta::title_from_file_name(f.as_std_path()),
                    artist: None,
                    bpm: Some(128.0),
                })
                .collect(),
        },
    }
}

fn dest<'a>(folder: &'a str, title: &'a str, trip: &'a str) -> Destination<'a> {
    Destination {
        folder,
        output_path: "/projects/out/new_trip.mp4",
        text: CardText { title, trip, date: "2026-11-09" },
    }
}

fn files_of(cfg: &Config) -> Vec<String> {
    cfg.soundtrack.as_ref().map(|s| s.files.iter().map(|f| f.to_string()).collect()).unwrap_or_default()
}

// ---------- the frozen baselines ----------

/// Writes `recipe_baselines_v1.json` from today's vibe templates. Run
/// once, when format 1 was defined; never again for format 1 (codes are
/// differences from it). `VIBECUT_WRITE_BASELINES=1 cargo test … -- --ignored`.
#[test]
#[ignore]
fn write_frozen_baselines() {
    if std::env::var("VIBECUT_WRITE_BASELINES").as_deref() != Ok("1") {
        return;
    }
    let mut all = Map::new();
    for key in VIBE_IDS {
        all.insert(key.to_string(), current_baseline(vibes::get(key).unwrap()).unwrap());
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/recipe_baselines_v1.json");
    std::fs::write(path, serde_json::to_string_pretty(&Value::Object(all)).unwrap() + "\n").unwrap();
}

#[test]
fn every_vibe_has_a_stable_id_and_a_frozen_baseline() {
    assert_eq!(VIBE_IDS.len(), vibes::names().len());
    for key in vibes::names() {
        assert!(VIBE_IDS.contains(&key), "vibe {key} needs an id in VIBE_IDS (append it)");
        let base = frozen_baseline(key, true).unwrap();
        assert!(base["soundtrack"].is_object(), "{key}");
        assert!(frozen_baseline(key, false).unwrap()["soundtrack"].is_null());
        // A frozen baseline is a whole style the current schema reads.
        let r = Recipe { vibe: key.into(), music: Music::Auto, settings: vec![] };
        let cfg = build_config(&r, &dest("/footage/x", "T", "x"), &["/m/a.mp3".into()]).unwrap();
        assert_eq!(normalize(&style_of(&cfg).unwrap()), normalize(&decoded_style(&r).unwrap()), "{key}");
    }
    assert!(SEGMENTS.len() <= SEG_INDEX as usize);
    assert!(VALUES.len() <= 128);
    let unique: BTreeSet<_> = SEGMENTS.iter().collect();
    assert_eq!(unique.len(), SEGMENTS.len(), "SEGMENTS has a duplicate");
    let unique: BTreeSet<_> = VALUES.iter().collect();
    assert_eq!(unique.len(), VALUES.len(), "VALUES has a duplicate");
}

/// A vibe template edited after format 1 doesn't change what old codes
/// mean (they decode against the frozen style), and an untouched vibe's
/// code stays short even then. Informational: how far each template has
/// drifted from its frozen style.
#[test]
fn templates_and_frozen_baselines() {
    for key in VIBE_IDS {
        let cur = current_baseline(vibes::get(key).unwrap()).unwrap();
        let drift = diff_styles(&frozen_baseline(key, true).unwrap(), &cur);
        println!("vibe {key}: {} setting(s) drifted from format 1", drift.len());
        let cfg = proposal(key, "/footage/x", "x", Some("/m/a.mp3"));
        let r = recipe_from_config(&cfg, Some(key), Music::Auto).unwrap();
        assert!(encode(&r).len() < 120, "{key}: {}", encode(&r));
    }
}

// ---------- round trips ----------

/// Encode → decode gives the style back, over many random configs: the
/// decoded style equals the original's style, the code is canonical
/// (re-encoding the decoded recipe gives the same code), and the config
/// it builds on other footage has exactly the original's style.
#[test]
fn round_trip_over_many_configs() {
    let mut rng = Rng(0x5eed);
    let mut lens = Vec::new();
    for n in 0..400 {
        let cfg = random_config(&mut rng);
        let hint = rng.chance(50).then(|| {
            // The vibe it was made from, as the UI passes it.
            VIBE_IDS.iter().copied().find(|k| {
                let base = proposal(k, "/f", "t", None);
                base.selection.min_clip == cfg.selection.min_clip
            })
        });
        let recipe = recipe_from_config(&cfg, hint.flatten(), music_of(&cfg)).unwrap();
        let code = encode(&recipe);
        lens.push(code.len());
        let back = decode(&code).unwrap_or_else(|e| panic!("#{n}: {e}\n{code}"));
        assert_eq!(encode(&back), code, "#{n}: not canonical");
        assert_eq!(back.vibe, recipe.vibe);
        assert_eq!(back.music, recipe.music, "#{n}");
        let want = normalize(&style_of(&cfg).unwrap());
        assert_eq!(normalize(&decoded_style(&back).unwrap()), want, "#{n}: decoded style\n{code}");

        let built = build_config(&back, &dest("/footage/Elsewhere", "New title", "Elsewhere"), &files_of(&cfg))
            .unwrap_or_else(|e| panic!("#{n}: {e}"));
        let got = normalize(&style_of(&built).unwrap());
        if got != want {
            let d = diff_styles(&want, &got);
            panic!("#{n}: built style differs at {:?}\n{code}", d.iter().map(|s| path_string(&s.path)).collect::<Vec<_>>());
        }
        // Re-sharing the built config gives the same code.
        let again = recipe_from_config(&built, Some(&back.vibe), music_of(&cfg)).unwrap();
        assert_eq!(encode(&again), code, "#{n}: re-shared code differs");
    }
    lens.sort();
    println!(
        "random-config code lengths: min {} · median {} · p90 {} · max {}",
        lens[0],
        lens[lens.len() / 2],
        lens[lens.len() * 9 / 10],
        lens[lens.len() - 1]
    );
}

#[test]
fn values_survive_the_round_trip_exactly() {
    let samples = [
        Value::Null,
        Value::Bool(true),
        Value::Bool(false),
        Value::from(0u64),
        Value::from(63u64),
        Value::from(64u64),
        Value::from(u64::MAX),
        Value::from(-1i64),
        Value::from(i64::MIN),
        Value::from(0.15f64),
        Value::from(-18.0f64),
        Value::from(1e300f64),
        Value::from(0.1f32 as f64),
        Value::from(std::f64::consts::PI),
        Value::from("fade"),
        Value::from("400ms"),
        Value::from("1s 500ms"),
        Value::from("1m 30s"),
        Value::from("2h 5m 1s 7ms"),
        Value::from("0s"),
        Value::from("5000ms"),
        Value::from("1.5s"),
        Value::from("Été à Nice · {date}"),
        Value::from(""),
        serde_json::json!([1, "a", [true, null], {"crf": 18, "custom key": "x"}]),
    ];
    for v in samples {
        let mut b = Vec::new();
        encode_value(&v, &mut b);
        let mut r = Reader { b: &b, i: 0 };
        let back = r.value(0).unwrap();
        assert!(r.done());
        let same = match (&v, &back) {
            // An f32-born float may come back in its short decimal form.
            (Value::Number(a), Value::Number(b)) if a.is_f64() => a.as_f64().map(|x| x as f32) == b.as_f64().map(|x| x as f32),
            _ => v == back,
        };
        assert!(same, "{v} came back as {back}");
    }
}

#[test]
fn durations_are_spelled_like_humantime() {
    // What a config itself serializes, for a spread of lengths.
    for ms in [0u64, 1, 7, 100, 400, 999, 1000, 1500, 59_999, 60_000, 90_000, 3_600_000, 3_723_456, 86_399_999] {
        let mut cfg = proposal("kid_reel", "/f", "t", None);
        cfg.transition.duration = Duration::from_millis(ms);
        let json = serde_json::to_value(&cfg).unwrap();
        let spelled = json["transition"]["duration"].as_str().unwrap();
        assert_eq!(fmt_ms(ms), spelled, "{ms} ms");
        assert_eq!(parse_canonical_ms(spelled), Some(ms));
    }
    assert_eq!(parse_canonical_ms("1500ms"), None, "not how a config writes it");
    assert_eq!(parse_canonical_ms("1.5s"), None);
    assert_eq!(parse_canonical_ms("Summer"), None);
    assert_eq!(parse_loose_ms("4.0s"), Some(4000));
    assert_eq!(parse_loose_ms("1m 30s"), Some(90_000));
}

// ---------- tampering and versions ----------

fn sample_code() -> String {
    let mut cfg = proposal("action_highlight", "/footage/Coastline Bike Ride", "Coastline Bike Ride · 2026-11-08", Some("/m/a.mp3"));
    cfg.project.target_duration = Duration::from_secs(30);
    cfg.transition.duration = Duration::from_millis(250);
    encode(&recipe_from_config(&cfg, Some("action_highlight"), music_of(&cfg)).unwrap())
}

#[test]
fn tampered_codes_are_rejected() {
    let code = sample_code();
    assert!(decode(&code).is_ok());
    let body_start = "VC1-".len();
    let alphabet: Vec<char> = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_".chars().collect();
    let chars: Vec<char> = code.chars().collect();
    let mut rejected = 0;
    let mut tried = 0;
    for i in body_start..chars.len() {
        for &c in alphabet.iter().step_by(7) {
            if c == chars[i] {
                continue;
            }
            let mut t = chars.clone();
            t[i] = c;
            let t: String = t.into_iter().collect();
            tried += 1;
            match decode(&t) {
                Err(CodeError::Changed) | Err(CodeError::Damaged(_)) => rejected += 1,
                Err(e) => panic!("{t}: unexpected {e}"),
                // The last character carries spare bits base64 ignores.
                Ok(r) => assert_eq!(encode(&r), code, "{t} decoded to a different style"),
            }
        }
    }
    assert!(rejected * 100 >= tried * 95, "{rejected}/{tried} rejected");
    // Cut short, or extended.
    for n in [5, 10, code.len() - 1] {
        assert!(decode(&code[..n]).is_err(), "{}", &code[..n]);
    }
    assert!(matches!(decode(&format!("{code}AA")), Err(CodeError::Changed)));
}

#[test]
fn versions_and_garbage_get_clear_errors() {
    let code = sample_code();
    let v2 = code.replacen("VC1-", "VC2-", 1);
    let e = decode(&v2).unwrap_err();
    assert_eq!(e, CodeError::UnsupportedVersion(2));
    let msg = OpsError::from(e).to_string();
    assert!(msg.contains("newer Vibecut") && msg.contains("format 2") && msg.contains("update Vibecut"), "{msg}");
    assert_eq!(decode(&code.replacen("VC1-", "VC0-", 1)).unwrap_err(), CodeError::UnsupportedVersion(0));
    assert_eq!(decode("").unwrap_err(), CodeError::Empty);
    assert_eq!(decode("   \n").unwrap_err(), CodeError::Empty);
    assert_eq!(decode("hello there").unwrap_err(), CodeError::NotACode);
    assert_eq!(decode("VCx-AAAA").unwrap_err(), CodeError::NotACode);
    assert_eq!(decode("VC1").unwrap_err(), CodeError::NotACode);
    assert!(matches!(decode("VC1-!!!!").unwrap_err(), CodeError::Damaged(_)));
    assert_eq!(decode("VC1-AAA").unwrap_err(), CodeError::Changed);
    // A forged checksum over garbage is still refused, without panicking.
    for junk in [vec![0xFFu8; 12], vec![0x3F, 0x05], vec![0x02, 0x09], vec![0x00, 0x1F, 0xFF, 0xFF, 0xFF], vec![0x00, 0x11, 0x14, 0xFF, 0xFF, 0xFF, 0x7F]] {
        let mut b = junk.clone();
        b.extend_from_slice(&checksum(1, &junk));
        let forged = format!("VC1-{}", URL_SAFE_NO_PAD.encode(b));
        assert!(decode(&forged).is_err(), "{forged}");
    }
    // Chat-app wrapping is fine: quotes, spaces, line breaks, lower-case prefix.
    let wrapped = format!("“{} \n {}”", &code[..20], &code[20..]);
    assert_eq!(encode(&decode(&wrapped).unwrap()), code);
    assert_eq!(encode(&decode(&code.replacen("VC1", "vc1", 1)).unwrap()), code);
}

#[test]
fn a_code_cannot_smuggle_in_paths() {
    let r = Recipe {
        vibe: "kid_reel".into(),
        music: Music::Auto,
        settings: canonical_settings(vec![
            Setting { path: vec![Seg::Key("soundtrack".into()), Seg::Key("files".into())], op: Op::Set(serde_json::json!(["/etc/passwd"])) },
            Setting { path: vec![Seg::Key("project".into()), Seg::Key("temp_dir".into())], op: Op::Set("/".into()) },
            Setting { path: vec![Seg::Key("grading".into()), Seg::Key("lut".into())], op: Op::Set("/etc/x.cube".into()) },
            Setting { path: vec![Seg::Key("input".into())], op: Op::Set(serde_json::json!({"folder": "/etc"})) },
            Setting {
                path: vec![Seg::Key("intro".into()), Seg::Key("layers".into()), Seg::Index(0)],
                op: Op::Set(serde_json::json!({"kind": "image", "path": "/etc/secret.png"})),
            },
            Setting {
                path: vec![Seg::Key("outputs".into()), Seg::Index(0), Seg::Key("path".into())],
                op: Op::Set("/etc/out.mp4".into()),
            },
        ]),
    };
    let r = decode(&encode(&r)).unwrap();
    let cfg = build_config(&r, &dest("/footage/x", "T", "x"), &["/m/a.mp3".into()]).unwrap();
    assert_eq!(cfg.soundtrack.unwrap().files[0].as_str(), "/m/a.mp3");
    assert!(cfg.project.temp_dir.is_none());
    assert!(cfg.grading.lut.is_none());
    assert_eq!(cfg.input.folder.unwrap().as_str(), "/footage/x");
    assert!(cfg.intro.unwrap().layers.iter().all(|l| matches!(l, CardLayer::Text(_))));
    assert_eq!(cfg.outputs[0].path.as_str(), "/projects/out/new_trip.mp4");
}

#[test]
fn settings_from_a_newer_version_are_refused_clearly() {
    let r = Recipe {
        vibe: "kid_reel".into(),
        music: Music::Auto,
        settings: vec![Setting {
            path: vec![Seg::Key("selection".into()), Seg::Key("phrase_lock".into())],
            op: Op::Set(Value::Bool(true)),
        }],
    };
    let r = decode(&encode(&r)).unwrap();
    let e = build_config(&r, &dest("/footage/x", "T", "x"), &["/m/a.mp3".into()]).unwrap_err().to_string();
    assert!(e.contains("can't use") && e.contains("phrase_lock") && e.contains("update Vibecut"), "{e}");
    let r = Recipe {
        vibe: "kid_reel".into(),
        music: Music::Auto,
        settings: vec![Setting { path: vec![Seg::Key("nope".into()), Seg::Key("x".into())], op: Op::Set(Value::Bool(true)) }],
    };
    let e = decoded_style(&decode(&encode(&r)).unwrap()).unwrap_err().to_string();
    assert!(e.contains("doesn't fit") && e.contains("nope.x"), "{e}");
}

/// A hand-made code can't carry filter-graph syntax (or anything else
/// that isn't the value) into a render: the colour that printed a local
/// file on the title card, and every other free-form style string, is
/// refused as invalid input (a 400) when the code is read.
#[test]
fn codes_carrying_ffmpeg_filter_syntax_are_refused() {
    let set = |path: &[&str], v: Value| Setting {
        path: path
            .iter()
            .map(|p| p.parse::<usize>().map(Seg::Index).unwrap_or_else(|_| Seg::Key(p.to_string())))
            .collect(),
        op: Op::Set(v),
    };
    let exploit = "white,drawtext=textfile=/etc/hostname:fontsize=40:fontcolor=red";
    let cases = [
        set(&["intro", "layers", "0", "color"], exploit.into()),
        set(&["intro", "bg_color"], "black;[0:v]null".into()),
        set(&["intro", "layers", "0", "position"], "center:x=0".into()),
        set(&["intro", "layers", "0", "font"], "..".into()),
        set(&["intro", "layers", "0", "font"], "x\u{7}.ttf".into()),
        set(&["intro", "layers", "0", "background"], serde_json::json!({ "color": "#000000", "padding": "20,drawtext" })),
        set(&["transition", "default"], "fade:offset=0,drawtext=text=x".into()),
        set(&["transition", "palette"], serde_json::json!(["dissolve", "x;y"])),
        set(&["outputs", "0", "audio_bitrate"], "192k -map 0".into()),
        set(&["outputs", "0", "max_bitrate"], "3M,x".into()),
    ];
    for s in cases {
        let what = path_string(&s.path);
        let r = Recipe { vibe: "kid_reel".into(), music: Music::Auto, settings: vec![s] };
        let r = decode(&encode(&r)).unwrap();
        let e = decoded_style(&r).expect_err(&what);
        assert!(matches!(e, OpsError::InvalidInput { .. }), "{what}: {e}");
        assert!(e.to_string().contains("isn't valid"), "{what}: {e}");
        assert!(build_config(&r, &dest("/footage/x", "T", "x"), &["/m/a.mp3".into()]).is_err(), "{what}");
        assert!(summarize(&r).is_err(), "{what}");
    }
    // The values styles really use still pass.
    for s in [
        set(&["intro", "layers", "0", "color"], "#F0E4C8AA".into()),
        set(&["intro", "layers", "0", "color"], "white".into()),
        set(&["intro", "layers", "0", "font"], "Inter Bold.ttf".into()),
        set(&["transition", "default"], "circleopen".into()),
        set(&["outputs", "0", "max_bitrate"], "2.5M".into()),
    ] {
        let what = path_string(&s.path);
        let r = Recipe { vibe: "kid_reel".into(), music: Music::Auto, settings: vec![s] };
        build_config(&decode(&encode(&r)).unwrap(), &dest("/footage/x", "T", "x"), &["/m/a.mp3".into()])
            .unwrap_or_else(|e| panic!("{what}: {e}"));
    }
}

/// A font path is the sharer's machine: a shared style carries the file name.
#[test]
fn shared_fonts_are_bare_file_names() {
    let mut cfg = proposal("kid_reel", "/footage/x", "T", Some("/m/a.mp3"));
    if let Some(CardLayer::Text(t)) = cfg.intro.as_mut().and_then(|c| c.layers.first_mut()) {
        t.font = Some("/usr/share/fonts/truetype/Inter-Bold.ttf".into());
    } else {
        panic!("kid_reel has an intro text layer");
    }
    let style = style_of(&cfg).unwrap();
    assert_eq!(style["intro"]["layers"][0]["font"], "Inter-Bold.ttf");
    // A hand-made code naming a path gets the file name too.
    let r = Recipe {
        vibe: "kid_reel".into(),
        music: Music::Auto,
        settings: vec![Setting {
            path: vec![Seg::Key("intro".into()), Seg::Key("layers".into()), Seg::Index(0), Seg::Key("font".into())],
            op: Op::Set("/etc/passwd".into()),
        }],
    };
    let style = decoded_style(&decode(&encode(&r)).unwrap()).unwrap();
    assert_eq!(style["intro"]["layers"][0]["font"], "passwd");
}

// ---------- applying to other footage ----------

/// The same code on two different trips: the configs differ only in
/// what belongs to the footage.
#[test]
fn two_trips_differ_only_in_footage_specific_keys() {
    let mut rng = Rng(77);
    for n in 0..60 {
        let cfg = random_config(&mut rng);
        let code = encode(&recipe_from_config(&cfg, None, music_of(&cfg)).unwrap());
        let r = decode(&code).unwrap();
        let files = files_of(&cfg);
        let a = build_config(&r, &Destination {
            folder: "/footage/Alps Ski Day",
            output_path: "/projects/out/alps_ski_day.mp4",
            // Not the default "<trip> · <date>": text equal to the title
            // reads back as `{title}`, which renders the same but isn't the
            // same style setting.
            text: CardText { title: "Alps!", trip: "Alps Ski Day", date: "2026-11-09" },
        }, &files)
        .unwrap();
        let b = build_config(&r, &Destination {
            folder: "/footage/Coastline Bike Ride",
            output_path: "/projects/out/coastline_bike_ride.mp4",
            text: CardText { title: "Ride", trip: "Coastline Bike Ride", date: "2026-11-09" },
        }, &files)
        .unwrap();
        let (ja, jb) = (serde_json::to_value(&a).unwrap(), serde_json::to_value(&b).unwrap());
        for s in diff_styles(&ja, &jb) {
            let p = path_string(&s.path);
            let footage = p == "project.name"
                || p == "input.folder"
                || (p.starts_with("outputs.") && p.ends_with(".path"))
                || ((p.starts_with("intro.layers.") || p.starts_with("outro.layers.")) && p.ends_with(".content"));
            assert!(footage, "#{n}: {p} differs between trips\n{code}");
        }
        assert_eq!(normalize(&style_of(&a).unwrap()), normalize(&style_of(&b).unwrap()), "#{n}");
    }
}

#[test]
fn card_text_keeps_its_style_but_not_the_trip() {
    let t = CardText { title: "Coastline Bike Ride · 2026-11-08", trip: "Coastline Bike Ride", date: "" };
    assert_eq!(templatize("Coastline Bike Ride · 2026-11-08", &t), "{title}");
    let t2 = CardText { title: "Our ride", ..t };
    assert_eq!(templatize("Coastline Bike Ride · 2026-11-08", &t2), "{trip} · {date}");
    assert_eq!(templatize("Coastline Bike Ride", &t2), "{trip}");
    assert_eq!(templatize("Coastline Bike Ride · day two", &t2), "{trip} · day two");
    assert_eq!(templatize("2026-11-08", &t2), "{date}");
    assert_eq!(templatize("Day one — 2026-11-08 to 2026-11-09", &t2), "Day one — {date} to {date}");
    assert_eq!(templatize("Made with Vibecut", &t2), "Made with Vibecut");
    assert_eq!(templatize("Été 2026-1-8", &t2), "Été 2026-1-8");
    // Only the whole title is a title; a short one inside other words, or
    // a short folder name, stays as written.
    let t3 = CardText { title: "Ride", trip: "trip", date: "" };
    assert_eq!(templatize("Coastline Bike Ride", &t3), "Coastline Bike Ride");
    assert_eq!(templatize("Our trip to Rome", &t3), "Our trip to Rome");
    assert_eq!(
        fill_template("{trip} · {date} — {title}", &CardText { title: "Ski!", trip: "Alps", date: "2026-11-09" }),
        "Alps · 2026-11-09 — Ski!"
    );

    let cfg = proposal("kid_reel", "/footage/Coastline Bike Ride", "Coastline Bike Ride · 2026-11-08", None);
    let r = recipe_from_config(&cfg, Some("kid_reel"), Music::Original).unwrap();
    assert!(r.settings.is_empty(), "the title is the vibe's {{title}}: {:?}", r.settings);
    let built = build_config(&r, &dest("/footage/Alps", "Alps · 2026-11-09", "Alps"), &[]).unwrap();
    let CardLayer::Text(l) = &built.intro.as_ref().unwrap().layers[0] else { panic!() };
    assert_eq!(l.content, "Alps · 2026-11-09");
    assert_eq!(built.project.name, "Alps · 2026-11-09");
}

#[test]
fn extra_outputs_get_their_own_files() {
    let mut cfg = proposal("kid_reel", "/footage/x", "x", None);
    let mut reels = cfg.outputs[0].clone();
    reels.path = "/projects/out/whatever.mp4".into();
    reels.resolution = None;
    reels.preset = Some("reels".into());
    cfg.outputs.push(reels.clone());
    cfg.outputs.push(reels);
    let r = decode(&encode(&recipe_from_config(&cfg, Some("kid_reel"), Music::Original).unwrap())).unwrap();
    let built = build_config(&r, &dest("/footage/y", "y", "y"), &[]).unwrap();
    let paths: Vec<&str> = built.outputs.iter().map(|o| o.path.as_str()).collect();
    assert_eq!(paths, ["/projects/out/new_trip.mp4", "/projects/out/new_trip.reels.mp4", "/projects/out/new_trip.3.mp4"]);
}

#[test]
fn the_vibe_is_inferred_when_not_given() {
    for key in VIBE_IDS {
        let mut cfg = proposal(key, "/footage/x", "x", Some("/m/a.mp3"));
        cfg.project.target_duration = Duration::from_secs(45);
        let r = recipe_from_config(&cfg, None, Music::Auto).unwrap();
        assert_eq!(r.vibe, key);
        assert_eq!(r.settings.len(), 1, "{key}: only the length differs: {:?}", r.settings);
    }
    assert!(recipe_from_config(&proposal("kid_reel", "/f", "t", None), Some("nope"), Music::Auto).is_err());
}

// ---------- music ----------

/// A library track with a steady beat that covers any cut.
fn track(path: &str, title: &str, artist: Option<&str>, bpm: f32) -> TrackInfo {
    let a = music::TrackAnalysis {
        duration_secs: 200.0,
        bpm,
        beat_interval_secs: 60.0 / bpm,
        first_beat_secs: Some(0.1),
        grid_tight: Some(true),
        pulse_strength: 3.0,
        energy: vec![0.6; 4],
        energy_mean: 0.6,
        loud_region: (0.0, 200.0),
    };
    TrackInfo {
        path: path.into(),
        title: title.into(),
        collection: "yours",
        artist: artist.map(str::to_string),
        licence: music::LicenceInfo {
            id: "unknown".into(),
            label: "unknown".into(),
            kind: vibecut::track_meta::LicenceKind::Unknown,
            note: "",
        },
        attribution_required: false,
        credit: None,
        source_url: None,
        declared_bpm: None,
        metadata: None,
        analysis: a,
        mood: "driving",
        loops_cleanly: true,
    }
}

fn library() -> Vec<TrackInfo> {
    vec![
        track("/m/night_drive.mp3", "Night Drive", Some("Full Send"), 132.0),
        track("/m/waves.mp3", "Sunday Morning Waves", None, 72.0),
        track("/m/pulse.mp3", "Trail Running Pulse", None, 128.5),
        track("/m/golden.mp3", "Golden Hour Drift", None, 98.0),
    ]
}

fn ids(t: &TrackInfo) -> Option<u64> {
    Some(fingerprint(1, t.path.as_bytes()))
}

fn song(id_path: Option<&str>, title: &str, artist: Option<&str>, bpm: Option<f32>) -> Music {
    Music::Songs {
        songs: vec![SongRef {
            id: id_path.map(|p| fingerprint(1, p.as_bytes())),
            title: title.into(),
            artist: artist.map(str::to_string),
            bpm,
        }],
    }
}

#[test]
fn music_is_found_by_id_then_title_then_tempo() {
    let lib = library();
    let v = vibes::get("action_highlight").unwrap();
    let resolve = |m: &Music| resolve_music(m, &lib, &ids, v, 30.0, 42);

    // The same file, renamed and retitled: found by its id.
    let m = resolve(&song(Some("/m/night_drive.mp3"), "Whatever", None, Some(132.0)));
    assert_eq!((m.how, m.files.as_slice()), ("same_song", ["/m/night_drive.mp3".to_string()].as_slice()));
    assert!(m.note.starts_with("Same song as the original: Night Drive"), "{}", m.note);

    // Another copy of the song: same title and artist.
    let m = resolve(&song(Some("/elsewhere.mp3"), "night drive", Some("FULL SEND"), Some(132.0)));
    assert_eq!((m.how, m.files[0].as_str()), ("same_title", "/m/night_drive.mp3"));
    // Same title, a different artist: not the same song.
    let m = resolve(&song(None, "Night Drive", Some("Someone Else"), Some(131.0)));
    assert_ne!(m.how, "same_title");

    // Not there at all: a song at about its tempo (half/double counts).
    let m = resolve(&song(None, "Unknown Anthem", None, Some(130.0)));
    assert_eq!(m.how, "similar_tempo");
    assert!(m.files[0] == "/m/night_drive.mp3" || m.files[0] == "/m/pulse.mp3", "{m:?}");
    assert!(m.note.contains("\u{201c}Unknown Anthem\u{201d} isn't in your music library") && m.note.contains("close to its 130 BPM"), "{}", m.note);
    let m = resolve(&song(None, "Slow One", None, Some(36.0)));
    assert_eq!((m.how, m.files[0].as_str()), ("similar_tempo", "/m/waves.mp3"), "72 BPM is double 36");

    // No tempo match: Vibecut's pick for the vibe.
    let m = resolve(&song(None, "Odd", None, Some(111.0)));
    assert_eq!(m.how, "vibecut_pick");
    assert!(m.note.contains("nothing there has its tempo"), "{}", m.note);

    // Auto, original sound, and an empty library.
    assert_eq!(resolve(&Music::Auto).how, "vibecut_pick");
    let m = resolve(&Music::Original);
    assert_eq!((m.how, m.files.len()), ("original_sound", 0));
    let empty = resolve_music(&song(None, "X", None, Some(120.0)), &[], &ids, v, 30.0, 42);
    assert_eq!((empty.how, empty.files.len()), ("no_music", 0));
    assert!(empty.note.contains("isn't in your music library") && empty.note.contains("library is empty"), "{}", empty.note);
    assert_eq!(resolve_music(&Music::Auto, &[], &ids, v, 30.0, 42).how, "no_music");

    // Two songs, one missing: plays the one there is.
    let two = Music::Songs {
        songs: vec![
            SongRef { id: Some(fingerprint(1, b"/m/waves.mp3")), title: "Waves".into(), artist: None, bpm: Some(72.0) },
            SongRef { id: None, title: "Gone".into(), artist: None, bpm: Some(90.0) },
        ],
    };
    let m = resolve(&two);
    assert_eq!((m.how, m.files.len()), ("same_song", 1));
    assert!(m.note.contains("1 of its 2 songs"), "{}", m.note);
}

#[test]
fn track_ids_follow_the_file_not_its_name() {
    let dir = std::env::temp_dir().join(format!("vibecut_recipe_ids_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let data: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(dir.join("a.mp3"), &data).unwrap();
    std::fs::write(dir.join("renamed copy.mp3"), &data).unwrap();
    let mut other = data.clone();
    other[10] ^= 1;
    std::fs::write(dir.join("b.mp3"), &other).unwrap();
    let a = track_id(&dir.join("a.mp3")).unwrap();
    assert_eq!(track_id(&dir.join("renamed copy.mp3")), Some(a));
    assert_ne!(track_id(&dir.join("b.mp3")), Some(a));
    assert!(a > 0 && a <= ID_MASK);
    assert_eq!(track_id(&dir.join("missing.mp3")), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn songs_carry_title_artist_and_tempo() {
    let lib = library();
    let dir = std::env::temp_dir().join(format!("vibecut_recipe_songs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let outside = dir.join("my_road_trip_song.mp3");
    std::fs::write(&outside, b"not really audio").unwrap();
    let manifest = serde_json::json!({
        "beat_sync": { "tempo_bpm": 121.34 },
        "soundtrack": [{ "file": outside.to_string_lossy(), "title": "Road Trip", "artist": "Band" }]
    });
    // From the library: its title, artist and detected tempo.
    let s = songs_for(&[PathBuf::from("/m/night_drive.mp3")], &lib, None);
    assert_eq!((s[0].title.as_str(), s[0].artist.as_deref(), s[0].bpm), ("Night Drive", Some("Full Send"), Some(132.0)));
    // Not in the library: the render's credit and tempo.
    let s = songs_for(std::slice::from_ref(&outside), &lib, Some(&manifest));
    assert_eq!((s[0].title.as_str(), s[0].artist.as_deref(), s[0].bpm), ("Road Trip", Some("Band"), Some(121.3)));
    assert!(s[0].id.is_some());
    // Without a manifest: the file name.
    let s = songs_for(std::slice::from_ref(&outside), &lib, None);
    assert_eq!((s[0].title.as_str(), s[0].bpm), ("My road trip song", None));
    // Long titles are cut to fit a chat message; matching still works.
    let long = "A".repeat(80);
    let lib2 = vec![track("/m/long.mp3", &long, None, 100.0)];
    let s = songs_for(&[PathBuf::from("/m/long.mp3")], &lib2, None);
    assert_eq!(s[0].title.len(), TITLE_MAX);
    let m = resolve_music(&Music::Songs { songs: vec![SongRef { id: None, ..s[0].clone() }] }, &lib2, &ids, vibes::get("kid_reel").unwrap(), 30.0, 1);
    assert_eq!(m.how, "same_title");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------- code lengths ----------

/// The length report: how long typical codes are.
#[test]
fn code_lengths() {
    let song = |title: &str, artist: Option<&str>| Music::Songs {
        songs: vec![SongRef { id: Some(0xA1B2_C3D4_E5F6), title: title.into(), artist: artist.map(str::to_string), bpm: Some(132.0) }],
    };
    let mut report = Vec::new();
    let mut check = |label: &str, cfg: &Config, vibe: Option<&str>, music: Music, max: usize| {
        let code = encode(&recipe_from_config(cfg, vibe, music).unwrap());
        report.push(format!("{:>3} chars  {label}: {code}", code.len()));
        assert!(code.len() <= max, "{label}: {} chars: {code}", code.len());
    };
    for key in VIBE_IDS {
        let cfg = proposal(key, "/footage/x", "x", Some("/m/a.mp3"));
        check(&format!("{key}, untouched, Vibecut picks"), &cfg, Some(key), Music::Auto, 20);
        let mut cfg = cfg.clone();
        cfg.project.target_duration = Duration::from_secs(30);
        check(&format!("{key}, 30 s, a song"), &cfg, Some(key), song("Full Send Night Drive", None), 80);
    }
    // The demo project: action highlight, 30 s, its song, a 3 s card.
    let demo = r##"
schema_version = 1
[project]
name = "Coastline Bike Ride · 2026-11-08"
target_duration = "30s"
fps = 30
seed = 42
fade_in = "200ms"
fade_out = "1s 500ms"
[input]
folder = "/demo/footage/Coastline Bike Ride"
[soundtrack]
files = ["/demo/music/Full_Send_Night_Drive.mp3"]
ducking_db = -20.0
fade_out = "2s"
[intro]
duration = "3s"
fade_in = "300ms"
fade_out = "300ms"
bg_color = "#0a0a0a"
[[intro.layers]]
kind = "text"
content = "Coastline Bike Ride · 2026-11-08"
size = 110
[selection]
min_clip = "1s 500ms"
max_clip = "4s"
order = "energy_arc"
variety = 0.1
[transition]
default = "fade"
duration = "300ms"
speed_ramp_probability = 0.7
speed_ramp_duration = "2s"
speed_ramp_factor = 3.0
[[outputs]]
path = "/tmp/out/coastline_bike_ride.mp4"
resolution = "1920x1080"
crf = 20
audio_bitrate = "192k"
"##;
    let demo: Config = toml::from_str(demo).unwrap();
    check("demo coastline.toml, its song", &demo, Some("action_highlight"), song("Full Send Night Drive", Some("Vibecut Demo")), 90);
    check("demo coastline.toml, vibe inferred", &demo, None, song("Full Send Night Drive", Some("Vibecut Demo")), 90);
    // Tweaked: shorter, punchier, hard cuts, a Reels version, an end card.
    let mut t = demo.clone();
    t.project.target_duration = Duration::from_secs(20);
    t.transition.default = "cut".into();
    t.transition.speed_ramp_probability = 0.9;
    t.selection.max_clip = Duration::from_millis(2500);
    let mut reels = t.outputs[0].clone();
    reels.resolution = None;
    reels.preset = Some("reels".into());
    t.outputs.push(reels);
    t.outro = Some(TextCard {
        duration: Duration::from_secs(2),
        fade_in: Duration::from_millis(300),
        fade_out: Duration::from_millis(300),
        mode: CardMode::Card,
        bg_color: "#000000".into(),
        layers: vec![text_layer("See you next ride")],
    });
    // Heavier than typical: a whole new end card travels in the code.
    check("tweaked: 20 s, hard cuts, Reels, end card", &t, Some("action_highlight"), song("Full Send Night Drive", Some("Vibecut Demo")), 190);
    println!("{}", report.join("\n"));
}

// ---------- operations ----------

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("vibecut_recipe_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn saved_styles_round_trip_and_skip_bad_files() {
    let dir = temp_dir("saved");
    let code = sample_code();
    let saved = save_in(&dir, "  Summer reel ", &code).unwrap();
    assert_eq!(saved.name, "Summer reel");
    assert!(saved.file.ends_with("summer_reel.json"), "{}", saved.file);
    let accented = save_in(&dir, "Été", &code).unwrap();
    assert!(!accented.file.ends_with("t.json") && accented.file.contains("_"), "{}", accented.file);
    assert!(save_in(&dir, "", &code).is_err());
    assert!(save_in(&dir, &"x".repeat(61), &code).is_err());
    assert!(save_in(&dir, "bad", "VC1-nope").is_err());
    std::fs::write(dir.join("broken.json"), "{ nope").unwrap();
    std::fs::write(dir.join("future.json"), r#"{"name":"Future","code":"VC9-AAAA"}"#).unwrap();
    let list = list_in(&dir);
    let names: BTreeSet<&str> = list.styles.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, BTreeSet::from(["Summer reel", "Été"]));
    assert_eq!(list.warnings.len(), 2, "{:?}", list.warnings);
    assert!(list.warnings.iter().any(|w| w.contains("newer Vibecut")), "{:?}", list.warnings);
    assert_eq!(list.styles[0].style.vibe, "action_highlight");
    // Saving under the same name replaces it.
    save_in(&dir, "summer reel", &code).unwrap();
    assert_eq!(list_in(&dir).styles.len(), 2);
    // A name finds its code; a code is itself.
    assert_eq!(resolve_code_in(&dir, "SUMMER REEL").unwrap(), code);
    assert_eq!(resolve_code_in(&dir, &code).unwrap(), code);
    assert!(matches!(resolve_code_in(&dir, "winter"), Err(OpsError::NotFound { .. })));
    assert!(list_in(&dir.join("missing")).styles.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// End to end without a server: a proposal from a code, written like
/// propose's, for a real folder; the song comes from the library.
#[tokio::test]
async fn applying_writes_a_proposal_like_propose() {
    let root = temp_dir("apply");
    let footage = root.join("footage/Alps Ski Day");
    std::fs::create_dir_all(&footage).unwrap();
    let mut cfg = proposal("action_highlight", "/footage/Coastline Bike Ride", "Coastline Bike Ride · 2026-11-08", Some("/m/night_drive.mp3"));
    cfg.project.target_duration = Duration::from_secs(30);
    cfg.transition.duration = Duration::from_millis(250);
    let music = Music::Songs { songs: vec![SongRef { id: None, title: "Night Drive".into(), artist: Some("Full Send".into()), bpm: Some(132.0) }] };
    let code = encode(&recipe_from_config(&cfg, Some("action_highlight"), music).unwrap());

    let r = apply_in(&root, ApplyArgs { code: &code, folder: &footage.to_string_lossy(), title: None }, library()).await.unwrap();
    assert_eq!((r.proposal.vibe, r.code.as_str()), ("action_highlight", code.as_str()));
    assert_eq!(r.music_match.how, "same_title");
    let snd = r.proposal.soundtrack.as_ref().unwrap();
    assert_eq!((snd.path.as_str(), snd.picked_by), ("/m/night_drive.mp3", "you"));
    assert!(r.proposal.config_path.contains("proposal_alps_ski_day_"), "{}", r.proposal.config_path);
    assert!(r.proposal.output_path.ends_with("/out/alps_ski_day.mp4"));
    let text = std::fs::read_to_string(&r.proposal.config_path).unwrap();
    assert_eq!(text, r.proposal.config_toml);
    assert!(text.starts_with(&format!("# Made from a shared style code: {code}\n")), "{text}");
    let written: Config = toml::from_str(&text).unwrap();
    written.validate().unwrap();
    assert_eq!(written.project.target_duration, Duration::from_secs(30));
    assert_eq!(written.transition.duration, Duration::from_millis(250));
    assert_eq!(written.input.folder.as_ref().unwrap().as_str(), footage.to_string_lossy());
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    assert_eq!(written.project.name, format!("Alps Ski Day · {today}"));
    assert_eq!(normalize(&style_of(&written).unwrap()), normalize(&style_of(&cfg).unwrap()));
    assert_eq!(r.style.chips[0], Chip { label: "Vibe", value: "Action highlight".into() });

    // A title, and an empty library.
    let r = apply_in(&root, ApplyArgs { code: &code, folder: &footage.to_string_lossy(), title: Some("Ski!") }, vec![]).await.unwrap();
    assert!(r.proposal.soundtrack.is_none());
    assert_eq!(r.music_match.how, "no_music");
    assert!(r.proposal.music_note.as_deref().unwrap().contains("library is empty"));
    let written: Config = toml::from_str(&r.proposal.config_toml).unwrap();
    assert_eq!(written.project.name, "Ski!");
    assert!(written.soundtrack.is_none());

    // Missing footage, bad code.
    assert!(matches!(
        apply_in(&root, ApplyArgs { code: &code, folder: "/no/such/folder", title: None }, vec![]).await,
        Err(OpsError::NotFound { .. })
    ));
    assert!(matches!(
        apply_in(&root, ApplyArgs { code: "VC2-AAAA", folder: &footage.to_string_lossy(), title: None }, vec![]).await,
        Err(OpsError::InvalidInput { .. })
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn sharing_a_config_file_makes_a_code() {
    let root = temp_dir("share");
    let path = root.join("p.toml");
    let mut cfg = proposal("drone_cinematic", "/footage/x", "x", None);
    cfg.project.target_duration = Duration::from_secs(75);
    std::fs::write(&path, toml::to_string_pretty(&cfg).unwrap()).unwrap();
    let r = share_style(ShareArgs { config_path: &path.to_string_lossy(), manifest_path: None, vibe: None, let_vibecut_pick: false })
        .await
        .unwrap();
    assert_eq!((r.style.vibe, r.chars), ("drone_cinematic", r.code.len()));
    assert_eq!(r.style.music, Music::Original);
    assert_eq!(r.style.target_secs, 75.0);
    assert!(r.style.chips.iter().any(|c| c.label == "Song" && c.value == "Original sound"));
    assert!(!r.style.chips.iter().any(|c| c.label == "Ducking"), "no music, no ducking chip");
    let p = summarize(&decode(&r.code).unwrap()).unwrap();
    assert_eq!(p.chips, r.style.chips);
    assert!(matches!(
        share_style(ShareArgs { config_path: "/no/such.toml", manifest_path: None, vibe: None, let_vibecut_pick: false }).await,
        Err(OpsError::NotFound { .. })
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn chips_read_like_a_summary() {
    let mut cfg = proposal("action_highlight", "/footage/x", "x", Some("/m/a.mp3"));
    cfg.project.target_duration = Duration::from_secs(100);
    cfg.transition.default = "cut".into();
    cfg.grading.stabilize = true;
    let r = recipe_from_config(&cfg, Some("action_highlight"), Music::Songs {
        songs: vec![SongRef { id: None, title: "Night Drive".into(), artist: Some("Full Send".into()), bpm: Some(132.0) }],
    })
    .unwrap();
    let s = summarize(&r).unwrap();
    let chip = |l: &str| s.chips.iter().find(|c| c.label == l).map(|c| c.value.clone()).unwrap_or_default();
    assert_eq!(chip("Vibe"), "Action highlight");
    assert_eq!(chip("Length"), "1 min 40 s");
    assert_eq!(chip("Song"), "Night Drive — Full Send · 132 BPM");
    assert_eq!(chip("Cuts"), "Hard cuts");
    assert_eq!(chip("Speed ramps"), "70% of cuts · 3.0×");
    assert_eq!(chip("Clips"), "1.5–4 s · energy arc");
    assert_eq!(chip("Beat burst"), "Auto");
    assert_eq!(chip("Ducking"), "2× to -20 dB");
    assert_eq!(chip("Title card"), "#0a0a0a · 3 s");
    assert_eq!(chip("Formats"), "1920x1080");
    assert_eq!(chip("Fades"), "200 ms in · 1.5 s out");
    assert_eq!(chip("Stabilized"), "Yes");
    assert_eq!((s.target_duration.as_str(), s.target_secs, s.seed), ("1m 40s", 100.0, 42));
    let paths: Vec<String> = s.settings.iter().map(|x| path_string(&x.path)).collect();
    assert_eq!(paths, ["grading.stabilize", "project.target_duration", "transition.default"]);
    let json = serde_json::to_value(&s).unwrap();
    assert_eq!(json["settings"][2], serde_json::json!({"path": "transition.default", "value": "cut"}));
    assert_eq!(json["music"]["kind"], "songs");
    assert!(json["music"]["songs"][0]["id"].is_null());
}

/// Every key the vibe templates render today is in the frozen file
/// (inside arrays too): format 1 hasn't shipped, so a key added to the
/// schema goes into the file now (VIBECUT_WRITE_BASELINES=1), rather than
/// leaning on the current-template fallback.
#[test]
fn the_frozen_file_has_every_key_the_templates_render() {
    fn missing(frozen: &Value, current: &Value, path: &str, out: &mut Vec<String>) {
        match (frozen, current) {
            (Value::Object(f), Value::Object(c)) => {
                for (k, cv) in c {
                    match f.get(k) {
                        Some(fv) => missing(fv, cv, &format!("{path}.{k}"), out),
                        None => out.push(format!("{path}.{k}")),
                    }
                }
            }
            (Value::Array(f), Value::Array(c)) => {
                for (i, cv) in c.iter().enumerate() {
                    match f.get(i) {
                        Some(fv) => missing(fv, cv, &format!("{path}.{i}"), out),
                        None => out.push(format!("{path}.{i}")),
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for key in VIBE_IDS {
        let frozen = &super::frozen_baselines()[key];
        missing(frozen, &current_baseline(vibes::get(key).unwrap()).unwrap(), key, &mut out);
    }
    assert!(out.is_empty(), "the frozen baselines lack {out:?}");
}

#[test]
fn keys_added_after_format_1_take_the_current_template() {
    // A key the frozen file lacks is filled in; a key it has keeps its
    // frozen value even when the template changed.
    let mut frozen = serde_json::json!({ "scoring": { "gps_speed": 0.4 }, "project": { "fps": 30 } });
    let current = serde_json::json!({ "scoring": { "gps_speed": 0.9, "gyro": 0.4 }, "grading": { "stabilize": false } });
    super::fill_missing(&mut frozen, &current);
    assert_eq!(
        frozen,
        serde_json::json!({ "scoring": { "gps_speed": 0.4, "gyro": 0.4 }, "project": { "fps": 30 }, "grading": { "stabilize": false } })
    );
    // Arrays pair up by position; extra elements on either side stay as they are.
    let mut frozen = serde_json::json!({ "intro": { "layers": [{ "size": 96 }, { "size": 48 }] }, "palette": ["fade"] });
    let current = serde_json::json!({ "intro": { "layers": [{ "size": 80, "shadow": true }] }, "palette": ["cut", "fade"] });
    super::fill_missing(&mut frozen, &current);
    assert_eq!(
        frozen,
        serde_json::json!({ "intro": { "layers": [{ "size": 96, "shadow": true }, { "size": 48 }] }, "palette": ["fade"] })
    );
    // And an untouched vibe has no settings at all, whatever was added.
    for key in vibes::names() {
        let cfg = build_config(
            &Recipe { vibe: key.into(), music: Music::Auto, settings: vec![] },
            &dest("/footage/x", "T", "x"),
            &["/m/a.mp3".into()],
        )
        .unwrap();
        let r = recipe_from_config(&cfg, Some(key), Music::Auto).unwrap();
        assert!(r.settings.is_empty(), "{key}: {:?}", r.settings);
    }
}
