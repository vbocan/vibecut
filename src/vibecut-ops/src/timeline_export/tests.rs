use super::*;
use vibecut::output_timeline::{
    ChainItem, ChainLayout, ClipSource, OutputTimeline, TimelineMusic, TimelineMusicFile, TimelineMusicSpan,
    TimelineTitle,
};
use vibecut::video_renderer::SpeedRamp;

// ---------- A tiny XML reader: well-formedness plus a tree to query ----------

#[derive(Debug, Default)]
struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
    text: String,
}

impl Node {
    fn attr(&self, k: &str) -> Option<&str> {
        self.attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }

    fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    fn all<'a>(&'a self, name: &str, out: &mut Vec<&'a Node>) {
        for c in &self.children {
            if c.name == name {
                out.push(c);
            }
            c.all(name, out);
        }
    }

    fn find_all(&self, name: &str) -> Vec<&Node> {
        let mut v = Vec::new();
        self.all(name, &mut v);
        v
    }
}

fn unescape(s: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let end = rest[i..].find(';').ok_or_else(|| format!("bare & in {s:?}"))? + i;
        let ent = &rest[i + 1..end];
        let ch = match ent {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ if ent.starts_with("#x") => {
                char::from_u32(u32::from_str_radix(&ent[2..], 16).map_err(|e| e.to_string())?).ok_or("bad char ref")?
            }
            _ if ent.starts_with('#') => {
                char::from_u32(ent[1..].parse::<u32>().map_err(|e| e.to_string())?).ok_or("bad char ref")?
            }
            _ => return Err(format!("unknown entity &{ent};")),
        };
        out.push(ch);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.')
}

/// Parse `s` as one XML document: a declaration, an optional doctype,
/// exactly one root element, matched tags, quoted attributes (no
/// duplicates, no raw `<`), only known entities.
fn parse_xml(s: &str) -> Result<Node, String> {
    let mut stack: Vec<Node> = vec![Node::default()];
    let mut i = 0;
    let b = s.as_bytes();
    while i < s.len() {
        if b[i] == b'<' {
            if s[i..].starts_with("<?") {
                i += s[i..].find("?>").ok_or("unterminated declaration")? + 2;
            } else if s[i..].starts_with("<!--") {
                i += s[i..].find("-->").ok_or("unterminated comment")? + 3;
            } else if s[i..].starts_with("<!DOCTYPE") {
                i += s[i..].find('>').ok_or("unterminated doctype")? + 1;
            } else if s[i..].starts_with("</") {
                let end = s[i..].find('>').ok_or("unterminated end tag")? + i;
                let name = s[i + 2..end].trim();
                let node = stack.pop().ok_or("stack underflow")?;
                if node.name != name {
                    return Err(format!("</{name}> closes <{}>", node.name));
                }
                stack.last_mut().ok_or(format!("</{name}> closes nothing"))?.children.push(node);
                i = end + 1;
            } else {
                // Start tag.
                let mut j = i + 1;
                let name: String = s[j..].chars().take_while(|&c| is_name_char(c)).collect();
                if name.is_empty() {
                    return Err(format!("bad tag at {i}"));
                }
                j += name.len();
                let mut node = Node { name, ..Default::default() };
                loop {
                    let ws = s[j..].chars().take_while(|c| c.is_whitespace()).map(char::len_utf8).sum::<usize>();
                    j += ws;
                    if s[j..].starts_with("/>") {
                        stack.last_mut().unwrap().children.push(node);
                        j += 2;
                        break;
                    }
                    if s[j..].starts_with('>') {
                        stack.push(node);
                        j += 1;
                        break;
                    }
                    if ws == 0 {
                        return Err(format!("attributes need a space before them in <{}>", node.name));
                    }
                    let an: String = s[j..].chars().take_while(|&c| is_name_char(c)).collect();
                    if an.is_empty() {
                        return Err(format!("bad attribute in <{}>", node.name));
                    }
                    j += an.len();
                    if !s[j..].starts_with("=\"") {
                        return Err(format!("attribute {an} of <{}> isn't =\"…\"", node.name));
                    }
                    j += 2;
                    let end = s[j..].find('"').ok_or("unterminated attribute")? + j;
                    let raw = &s[j..end];
                    if raw.contains('<') {
                        return Err(format!("raw < in attribute {an}"));
                    }
                    if node.attrs.iter().any(|(n, _)| *n == an) {
                        return Err(format!("duplicate attribute {an} in <{}>", node.name));
                    }
                    node.attrs.push((an, unescape(raw)?));
                    j = end + 1;
                }
                i = j;
            }
        } else {
            let end = s[i..].find('<').map_or(s.len(), |k| k + i);
            let text = unescape(&s[i..end])?;
            if stack.len() == 1 && !text.trim().is_empty() {
                return Err(format!("text outside the root: {text:?}"));
            }
            stack.last_mut().unwrap().text.push_str(&text);
            i = end;
        }
    }
    if stack.len() != 1 {
        return Err(format!("unclosed <{}>", stack.last().unwrap().name));
    }
    let mut doc = stack.pop().unwrap();
    if doc.children.len() != 1 {
        return Err(format!("{} root elements", doc.children.len()));
    }
    Ok(doc.children.pop().unwrap())
}

/// An FCPXML time (`17/30s`, `5s`) as an exact fraction.
fn rt(s: &str) -> (i64, i64) {
    let v = s.strip_suffix('s').unwrap_or_else(|| panic!("time without s: {s}"));
    match v.split_once('/') {
        Some((n, d)) => (n.parse().unwrap(), d.parse().unwrap()),
        None => (v.parse().unwrap(), 1),
    }
}

/// Whole frames of `rate` in an FCPXML time, or panic if it's off the grid.
fn frames_of(s: &str, rate: Rate) -> i64 {
    let (n, d) = rt(s);
    let (num, den) = (n as i128 * rate.num as i128, d as i128 * rate.den as i128);
    assert_eq!(num % den, 0, "{s} is not a whole frame at {}", rate.label());
    (num / den) as i64
}

#[test]
fn the_xml_reader_rejects_what_it_should() {
    assert!(parse_xml("<a><b x=\"1\"/></a>").is_ok());
    assert!(parse_xml("<a><b></a>").is_err());
    assert!(parse_xml("<a x=\"&\"/>").is_err());
    assert!(parse_xml("<a x=\"1\" x=\"2\"/>").is_err());
    assert!(parse_xml("<a>&nope;</a>").is_err());
    assert!(parse_xml("<a/><b/>").is_err());
    assert!(parse_xml("<a x=\"<\"/>").is_err());
    assert_eq!(parse_xml("<a x=\"&lt;&amp;&#233;\"/>").unwrap().attr("x"), Some("<&é"));
}

// ---------- An OTIO schema walk ----------

fn otio_check(v: &Value, path: &str) -> Result<(), String> {
    let obj = v.as_object().ok_or(format!("{path}: not an object"))?;
    let schema = obj.get("OTIO_SCHEMA").and_then(Value::as_str).ok_or(format!("{path}: no OTIO_SCHEMA"))?;
    let need = |keys: &[&str]| -> Result<(), String> {
        for k in keys {
            if !obj.contains_key(*k) {
                return Err(format!("{path} ({schema}): missing {k}"));
            }
        }
        Ok(())
    };
    let is_str = |k: &str| obj.get(k).is_some_and(Value::is_string);
    let item = ["name", "metadata", "source_range", "effects", "markers", "enabled"];
    let sub = |k: &str| -> Result<(), String> { otio_check(&obj[k], &format!("{path}.{k}")) };
    let list = |k: &str| -> Result<(), String> {
        let a = obj[k].as_array().ok_or(format!("{path}.{k}: not a list"))?;
        a.iter().enumerate().try_for_each(|(i, x)| otio_check(x, &format!("{path}.{k}[{i}]")))
    };
    let range_or_null = |k: &str| -> Result<(), String> {
        match &obj[k] {
            Value::Null => Ok(()),
            r => otio_check(r, &format!("{path}.{k}")),
        }
    };
    match schema {
        "Timeline.1" => {
            need(&["name", "metadata", "global_start_time", "tracks"])?;
            range_or_null("global_start_time")?;
            if obj["tracks"]["OTIO_SCHEMA"] != "Stack.1" {
                return Err(format!("{path}: tracks isn't a Stack.1"));
            }
            sub("tracks")
        }
        "Stack.1" | "Track.1" => {
            need(&item)?;
            need(&["children"])?;
            if schema == "Track.1" && !matches!(obj.get("kind").and_then(Value::as_str), Some("Video" | "Audio")) {
                return Err(format!("{path}: track kind"));
            }
            range_or_null("source_range")?;
            list("markers")?;
            list("effects")?;
            list("children")?;
            if schema == "Track.1" {
                // Transitions sit between two items, never at an end or
                // next to another one, and fit inside both neighbours.
                let kids = obj["children"].as_array().unwrap();
                for (i, k) in kids.iter().enumerate() {
                    if k["OTIO_SCHEMA"] == "Transition.1" {
                        let (Some(a), Some(b)) = (i.checked_sub(1).and_then(|p| kids.get(p)), kids.get(i + 1)) else {
                            return Err(format!("{path}: transition at the end of a track"));
                        };
                        if a["OTIO_SCHEMA"] == "Transition.1" || b["OTIO_SCHEMA"] == "Transition.1" {
                            return Err(format!("{path}: adjacent transitions"));
                        }
                        let secs = |t: &Value| t["value"].as_f64().unwrap() / t["rate"].as_f64().unwrap();
                        let dur = |x: &Value| secs(&x["source_range"]["duration"]);
                        if secs(&k["in_offset"]) > dur(a) + 1e-9 || secs(&k["out_offset"]) > dur(b) + 1e-9 {
                            return Err(format!("{path}: transition {i} longer than its neighbours"));
                        }
                    }
                }
            }
            Ok(())
        }
        "Clip.1" => {
            need(&item)?;
            need(&["media_reference"])?;
            sub("source_range")?;
            list("markers")?;
            list("effects")?;
            if obj["media_reference"]["OTIO_SCHEMA"] != "ExternalReference.1" {
                return Err(format!("{path}: media isn't an ExternalReference.1"));
            }
            sub("media_reference")
        }
        "Gap.1" => {
            need(&item)?;
            sub("source_range")?;
            list("markers")?;
            list("effects")
        }
        "Transition.1" => {
            need(&["name", "metadata", "transition_type", "in_offset", "out_offset"])?;
            if !is_str("transition_type") {
                return Err(format!("{path}: transition_type"));
            }
            sub("in_offset")?;
            sub("out_offset")
        }
        "ExternalReference.1" => {
            need(&["name", "metadata", "available_range", "target_url"])?;
            if !obj["target_url"].as_str().is_some_and(|u| u.starts_with("file:///")) {
                return Err(format!("{path}: target_url"));
            }
            range_or_null("available_range")
        }
        "Marker.1" => {
            need(&["name", "metadata", "color", "marked_range"])?;
            sub("marked_range")
        }
        "LinearTimeWarp.1" => {
            need(&["name", "metadata", "effect_name", "time_scalar"])?;
            obj["time_scalar"].as_f64().map(|_| ()).ok_or(format!("{path}: time_scalar"))
        }
        "TimeRange.1" => {
            need(&["start_time", "duration"])?;
            sub("start_time")?;
            sub("duration")
        }
        "RationalTime.1" => {
            let rate = obj.get("rate").and_then(Value::as_f64).ok_or(format!("{path}: rate"))?;
            let value = obj.get("value").and_then(Value::as_f64).ok_or(format!("{path}: value"))?;
            if rate <= 0.0 || value < 0.0 {
                return Err(format!("{path}: rate {rate} value {value}"));
            }
            Ok(())
        }
        other => Err(format!("{path}: unexpected schema {other}")),
    }
}

/// Seconds a track's items (not its transitions) take.
fn otio_track_secs(track: &Value) -> f64 {
    track["children"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["OTIO_SCHEMA"] != "Transition.1")
        .map(|c| {
            let d = &c["source_range"]["duration"];
            d["value"].as_f64().unwrap() / d["rate"].as_f64().unwrap()
        })
        .sum()
}

// ---------- The fixture: a real chain, laid out by the engine ----------

const A: &str = "/footage/Été & <Sea>/GX010042.MP4";
const B: &str = "/footage/trip/GX010043.MP4";
const C: &str = "/footage/trip/GX010044.MP4";
const SONG_A: &str = "/projects/music/Night Drive & Co.mp3";
const SONG_B: &str = "/projects/music/b.mp3";

fn clip(i: usize, path: &str, at: f64, d: f64) -> ClipSource {
    ClipSource {
        segment_index: i,
        source_id: format!("s{i}"),
        source_path: path.into(),
        source_in_secs: at,
        duration_secs: d,
        ramp: None,
    }
}

/// Intro card (3 s), a speed-ramped clip, a three-part beat-burst (hard
/// cuts inside), two clips and an outro card, crossfaded 0.3 s except a
/// 0.1 s near-cut after the ramp; music from two joined files, the
/// second looping with a 1 s crossfade.
fn fixture() -> Manifest {
    let layout = ChainLayout::new(&[3.0, 2.0, 1.5, 2.0, 2.0, 2.0], &[0.3, 0.1, 0.3, 0.3, 0.3], 30);
    let mut ramped = clip(0, A, 10.0, 2.0);
    ramped.ramp = Some(SpeedRamp { duration_secs: 0.6, factor: 3.0, flash: false });
    let burst = vec![(clip(1, B, 3.0, 0.5), 0.5), (clip(1, A, 20.0, 0.5), 0.5), (clip(1, B, 7.0, 0.5), 0.5)];
    let items = vec![
        ChainItem::IntroCard,
        ChainItem::Clip(ramped),
        ChainItem::Burst(burst),
        ChainItem::Clip(clip(2, B, 12.0, 2.0)),
        ChainItem::Clip(clip(3, C, 1.0, 2.0)),
        ChainItem::OutroCard,
    ];
    let mut tl = OutputTimeline::build(&layout, &items);
    let total = tl.duration_secs;
    tl.music = Some(TimelineMusic {
        files: vec![
            TimelineMusicFile { path: SONG_A.into(), duration_secs: 6.0 },
            TimelineMusicFile { path: SONG_B.into(), duration_secs: 30.0 },
        ],
        spans: vec![
            TimelineMusicSpan { output_start_secs: 0.0, output_end_secs: 8.0, source_start_secs: 0.0 },
            TimelineMusicSpan { output_start_secs: 8.0, output_end_secs: total, source_start_secs: 6.5 },
        ],
        crossfade_secs: 1.0,
        fade_out_secs: 1.5,
        beats_secs: (0..).map(|i| i as f64 * 0.5).take_while(|&b| b < total).collect(),
    });
    tl.titles = vec![
        TimelineTitle {
            role: "intro".into(),
            overlay: false,
            output_start_secs: 0.0,
            output_end_secs: 3.0,
            lines: vec!["Coast & <Sea>".into(), "Été 2026".into()],
        },
        TimelineTitle {
            role: "outro".into(),
            overlay: false,
            output_start_secs: total - 2.0,
            output_end_secs: total,
            lines: vec!["Thanks".into()],
        },
    ];
    let seg = |id: &str, path: &str, reason: &str| {
        json!({
            "source_id": id, "chapter_path": path, "chapter_index": 1, "in_chapter_start_secs": 0.0,
            "duration_secs": 2.0, "virtual_start_secs": 0.0, "reason": reason,
        })
    };
    let mut m: Manifest = serde_json::from_value(json!({
        "schema_version": 1,
        "project_name": "Coast & <Sea> · Été",
        "output": "/projects/out/coast.mp4",
        "target_duration_secs": 12.0,
        "actual_duration_secs": 11.2,
        "resolution": "1920x1080",
        "fps": 30,
        "seed": 1,
        "soundtrack": [
            { "file": SONG_A, "title": "Night Drive", "artist": null, "licence": "CC0-1.0", "licence_label": "CC0",
              "source_url": null, "credit": null, "attribution_required": false, "sha256": null, "size_bytes": 0 },
        ],
        "sources": [],
        "segments": [
            seg("s0", A, "hilight tag"), seg("burst:1", "/tmp/burst.mp4", "beat-burst"),
            seg("s2", B, "motion peak"), seg("s3", C, "even spacing"),
        ],
        "duck_windows": [
            { "start_secs": 6.0, "end_secs": 8.5, "ramp_secs": 0.3, "source_id": "s2", "segment_index": 2, "audio_score": 1.0 },
        ],
        "generator": "vibecut test",
    }))
    .unwrap();
    m.timeline = Some(tl);
    m
}

fn facts() -> HashMap<String, MediaFacts> {
    let footage = |rate: Rate, tc: Option<&str>| MediaFacts {
        video: Some(VideoFacts { rate, width: 3840, height: 2160 }),
        audio: Some(AudioFacts { sample_rate: 48_000, channels: 2 }),
        duration_secs: Some(60.0),
        timecode: tc.map(str::to_string),
    };
    let song = |d: f64| MediaFacts {
        audio: Some(AudioFacts { sample_rate: 44_100, channels: 2 }),
        duration_secs: Some(d),
        ..Default::default()
    };
    HashMap::from([
        // NTSC footage with a timecode track, as a GoPro writes it.
        (A.to_string(), footage(Rate::new(30000, 1001), Some("00:00:10:00"))),
        (B.to_string(), footage(Rate::new(30, 1), None)),
        (C.to_string(), footage(Rate::new(30, 1), None)),
        (SONG_A.to_string(), song(6.0)),
        (SONG_B.to_string(), song(30.0)),
    ])
}

fn edit_at(rate: Option<Rate>) -> Edit {
    build_edit(&fixture(), &facts(), &ExportOptions { rate, path_map: vec![] }).unwrap()
}

fn clips(e: &Edit) -> Vec<&VideoClip> {
    e.video.iter().filter_map(|v| if let VideoItem::Clip(c) = v { Some(c) } else { None }).collect()
}

fn transitions(e: &Edit) -> Vec<Transition> {
    e.video.iter().filter_map(|v| if let VideoItem::Transition(t) = v { Some(*t) } else { None }).collect()
}

// ---------- The edit ----------

#[test]
fn items_partition_the_sequence_frame_for_frame() {
    let m = fixture();
    let tl = m.timeline.as_ref().unwrap();
    for rate in [None, Some(Rate::new(30000, 1001))] {
        let e = edit_at(rate);
        let r = e.rate;
        assert_eq!(e.frames, r.frames(tl.duration_secs));
        let mut at = 0;
        for (offset, duration) in e.spine() {
            assert_eq!(offset, at, "items run back to back");
            assert!(duration > 0);
            at += duration;
        }
        assert_eq!(at, e.frames, "durations add up to the sequence");
        // Every cut on one of the two frames either side of the render's
        // (mid-crossfade) cut.
        let render_cuts: Vec<f64> = std::iter::once(tl.intro.as_ref().unwrap().cut_secs)
            .chain(tl.clips.iter().skip(1).map(|c| c.cut_in_secs))
            .chain(std::iter::once(tl.outro.as_ref().unwrap().cut_secs))
            .collect();
        let cuts: Vec<i64> = e
            .video
            .iter()
            .filter_map(|v| match v {
                VideoItem::Clip(c) if !c.ramp_tail => Some(c.offset),
                VideoItem::Gap(g) if g.offset > 0 => Some(g.offset),
                _ => None,
            })
            .collect();
        assert_eq!(cuts.len(), render_cuts.len());
        for (c, want) in cuts.iter().zip(&render_cuts) {
            assert!((r.secs(*c) - want).abs() < 1.0 / r.fps(), "cut {c} vs {want} at {}", r.label());
        }
    }
    // At the render's own rate the sequence is exactly the render.
    assert_eq!(edit_at(None).frames, tl.frames as i64);
}

#[test]
fn cuts_between_frames_go_to_the_frame_nearer_the_beat() {
    let r = Rate::new(30, 1);
    // 81.5 frames: the beat just after picks 82, just before picks 81.
    assert_eq!(choose_cut(81.5 / 30.0, 77, 86, &[2.7268], r), 82);
    assert_eq!(choose_cut(81.5 / 30.0, 77, 86, &[2.70], r), 81);
    // No beat nearby: plain rounding. Whole frames stay put.
    assert_eq!(choose_cut(81.4 / 30.0, 77, 86, &[], r), 81);
    assert_eq!(choose_cut(80.0 / 30.0, 77, 86, &[2.7268], r), 80);
    // Never outside the frames both clips share.
    assert_eq!(choose_cut(81.5 / 30.0, 77, 81, &[2.7268], r), 81);
    // In the fixture the intro cut (85.5 frames) leans to the 3.0 s beat.
    let e = edit_at(None);
    assert_eq!(clips(&e)[0].offset, 86);
}

#[test]
fn dissolves_are_centred_inside_the_rendered_crossfades() {
    let m = fixture();
    let tl = m.timeline.as_ref().unwrap();
    let e = edit_at(None);
    let ts = transitions(&e);
    // Intro → clip, ramp near-cut → burst, burst → B, B → C, C → outro;
    // the burst's own cuts are hard.
    assert_eq!(ts.len(), 5, "{ts:?}");
    // Each dissolve stays in the frames both sides were blended in the render.
    let mut frames: Vec<(u32, u32)> = vec![(tl.intro.as_ref().unwrap().start_frame, tl.intro.as_ref().unwrap().end_frame)];
    frames.extend(tl.clips.iter().map(|c| (c.start_frame, c.end_frame)));
    let o = tl.outro.as_ref().unwrap();
    frames.push((o.start_frame, o.end_frame));
    for t in &ts {
        assert!(t.half >= 1);
        let shared = frames
            .windows(2)
            .map(|w| (w[1].0 as i64, w[0].1 as i64))
            .find(|&(a, b)| (a..=b).contains(&t.cut))
            .unwrap_or_else(|| panic!("cut {} isn't in any crossfade", t.cut));
        assert!(t.cut - t.half >= shared.0 && t.cut + t.half <= shared.1, "{t:?} outside {shared:?}");
    }
    // The 0.3 s (9-frame) intro crossfade: cut at 86, ±4 frames.
    assert_eq!(ts[0], Transition { cut: 86, half: 4 });
}

#[test]
fn a_speed_ramp_becomes_a_clip_and_its_fast_tail() {
    let m = fixture();
    let src = &m.timeline.as_ref().unwrap().clips[0];
    let e = edit_at(None);
    let cs = clips(&e);
    let (body, tail) = (cs[0], cs[1]);
    assert_eq!((body.segment_index, tail.segment_index), (0, 0));
    assert!(body.speed.is_normal() && !body.ramp_tail);
    assert_eq!((tail.speed, tail.ramp_tail), (Speed { p: 3, q: 1 }, true));
    // The tail starts on the ramp's frame (4.5 s), and the two cover the
    // source the render read: 1× up to the ramp, 3× after.
    assert_eq!(tail.offset, 135);
    assert_eq!(body.offset + body.duration, tail.offset);
    let med = &e.media[body.media];
    let secs = |u: i64| (u - med.start_units) as f64 / med.grid.fps();
    assert!((secs(tail.start_units) - (secs(body.start_units) + e.rate.secs(body.duration))).abs() <= 1.0 / 29.97);
    let shown_end = secs(tail.start_units) + e.rate.secs(tail.duration) * 3.0;
    let render_end = src.source_out_secs - (src.output_end_secs - e.rate.secs(tail.offset + tail.duration)) * 3.0;
    assert!((shown_end - render_end).abs() <= 1.5 / 29.97, "{shown_end} vs {render_end}");
    assert!(tail.name.contains("3× ramp"), "{}", tail.name);
}

#[test]
fn source_times_sit_on_the_source_frames_after_its_timecode() {
    let e = edit_at(None);
    let a = e.media.iter().find(|m| m.path == A).unwrap();
    assert_eq!(a.grid, Rate::new(30000, 1001));
    assert_eq!(a.start_units, 300, "10 s of 29.97 timecode = 300 frames");
    let c0 = clips(&e)[0];
    // Cut in at frame 86 of the sequence (2.8667 s) = 0.1667 s into the clip,
    // which starts 10 s into the file: 10.1667 s × 29.97 = 304.7 → 305.
    assert_eq!(c0.start_units, 300 + 305);
    let x = to_fcpxml(&e);
    let doc = parse_xml(&x).unwrap();
    let asset = doc.find_all("asset").into_iter().find(|n| n.attr("name") == Some("GX010042")).unwrap();
    assert_eq!(asset.attr("start"), Some("300300/30000s"));
    // 60 s of it: 1798 frames (1798.2 rounded).
    assert_eq!(asset.attr("duration"), Some("1799798/30000s"));
    let clip = doc.find_all("asset-clip").into_iter().find(|n| n.attr("offset") == Some("86/30s")).unwrap();
    assert_eq!(clip.attr("start"), Some("605605/30000s"));
    // Its format is the source's own: 4K at 1001/30000 s a frame.
    let fid = asset.attr("format").unwrap();
    let fmt = doc.find_all("format").into_iter().find(|f| f.attr("id") == Some(fid)).unwrap();
    assert_eq!((fmt.attr("frameDuration"), fmt.attr("width")), (Some("1001/30000s"), Some("3840")));
}

// ---------- FCPXML ----------

#[test]
fn fcpxml_is_well_formed_and_frame_exact_at_30() {
    let e = edit_at(None);
    let x = to_fcpxml(&e);
    assert!(x.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE fcpxml>"));
    let doc = parse_xml(&x).unwrap_or_else(|err| panic!("{err}\n{x}"));
    assert_eq!((doc.name.as_str(), doc.attr("version")), ("fcpxml", Some("1.10")));
    let seq = doc.find_all("sequence")[0];
    let rate = e.rate;
    assert_eq!(frames_of(seq.attr("duration").unwrap(), rate), e.frames);
    let fmt = doc.find_all("format")[0];
    assert_eq!(fmt.attr("frameDuration"), Some("1/30s"));
    // The storyline: items back to back, every time a whole frame.
    let spine = seq.child("spine").unwrap();
    let mut at = 0;
    for item in &spine.children {
        let off = frames_of(item.attr("offset").unwrap(), rate);
        let dur = frames_of(item.attr("duration").unwrap(), rate);
        match item.name.as_str() {
            "transition" => assert!(off < at && off + dur > at, "a dissolve straddles the cut at {at}"),
            "asset-clip" | "gap" => {
                assert_eq!(off, at);
                at += dur;
            }
            other => panic!("unexpected {other} in the spine"),
        }
    }
    assert_eq!(at, e.frames);
    // N/30s style, not reduced to other denominators.
    assert!(spine.children.iter().any(|c| c.attr("offset") == Some("86/30s")));
}

#[test]
fn fcpxml_at_2997_counts_ntsc_frames() {
    let ntsc = Rate::new(30000, 1001);
    let e = edit_at(Some(ntsc));
    let doc = parse_xml(&to_fcpxml(&e)).unwrap();
    assert_eq!(doc.find_all("format")[0].attr("frameDuration"), Some("1001/30000s"));
    let seq = doc.find_all("sequence")[0];
    assert_eq!(frames_of(seq.attr("duration").unwrap(), ntsc), e.frames);
    let mut sum = 0;
    for item in &seq.child("spine").unwrap().children {
        let off = item.attr("offset").unwrap();
        let dur = item.attr("duration").unwrap();
        // Whole NTSC frames: k·1001/30000 s.
        for t in [off, dur] {
            let (n, d) = rt(t);
            assert!(d == 30000 && n % 1001 == 0 || d == 1, "{t}");
        }
        if item.name != "transition" {
            sum += frames_of(dur, ntsc);
        }
    }
    assert_eq!(sum, e.frames);
    // 11.2 s of render = 335.7 NTSC frames → 336.
    assert_eq!(e.frames, 336);
}

#[test]
fn fcpxml_dissolves_speed_cards_and_music() {
    let e = edit_at(None);
    let x = to_fcpxml(&e);
    let doc = parse_xml(&x).unwrap();
    // One Cross Dissolve effect, referenced by every transition.
    let effects = doc.find_all("effect");
    let dissolve = effects.iter().find(|f| f.attr("name") == Some("Cross Dissolve")).unwrap();
    assert_eq!(dissolve.attr("uid"), Some(CROSS_DISSOLVE_UID));
    let ts = doc.find_all("transition");
    assert_eq!(ts.len(), 5);
    for t in &ts {
        assert_eq!(t.child("filter-video").and_then(|f| f.attr("ref")), dissolve.attr("id"));
    }
    assert_eq!((ts[0].attr("offset"), ts[0].attr("duration")), (Some("82/30s"), Some("8/30s")));

    // The ramp's tail: a constant 3× time map through zero.
    let tail = doc.find_all("asset-clip").into_iter().find(|c| c.attr("name").is_some_and(|n| n.contains("ramp"))).unwrap();
    let tm = tail.child("timeMap").expect("retimed clip has a timeMap");
    let pts: Vec<(&str, &str)> = tm.children.iter().map(|p| (p.attr("time").unwrap(), p.attr("value").unwrap())).collect();
    assert_eq!(pts[0], ("0s", "0s"));
    let (t, v) = (rt(pts[1].0), rt(pts[1].1));
    assert_eq!(v.0 as i128 * t.1 as i128, 3 * t.0 as i128 * v.1 as i128, "value = 3 × time");
    // Its start is in its own (retimed) time: source start ÷ 3.
    let (s, d) = rt(tail.attr("start").unwrap());
    let c = clips(&e)[1];
    assert_eq!(s as i128 * 30000 * 3, (c.start_units as i128 * 1001) * d as i128);

    // Cards: gaps named after their text, the text as a marker.
    let gaps = doc.find_all("gap");
    assert_eq!(gaps.len(), 2);
    assert_eq!(gaps[0].attr("name"), Some("Coast & <Sea> · Été 2026"));
    assert_eq!(gaps[0].child("note").map(|n| n.text.as_str()), Some("Coast & <Sea>\nÉté 2026"));
    let title = gaps[0].children.iter().find(|c| c.name == "marker").unwrap();
    assert_eq!(title.attr("value"), Some("Title: Coast & <Sea> / Été 2026"));

    // Music: connected on lane −1, one clip per file and loop copy, in the
    // gap that opens the storyline (offset in its local time).
    let music: Vec<&Node> = doc.find_all("asset-clip").into_iter().filter(|c| c.attr("lane") == Some("-1")).collect();
    assert_eq!(music.len(), 3);
    assert!(music.iter().all(|c| c.attr("audioRole") == Some("music")));
    assert_eq!(gaps[0].children.iter().filter(|c| c.name == "asset-clip").count(), 3);
    // Frames 0, 180 and 240 (whole seconds are written as such).
    assert_eq!(music.iter().map(|c| c.attr("offset").unwrap()).collect::<Vec<_>>(), ["0s", "6s", "8s"]);
    // The loop copy starts 0.5 s into the second file, on its sample grid.
    assert_eq!(music[2].attr("start"), Some("22050/44100s"));
    let total: i64 = music.iter().map(|c| frames_of(c.attr("duration").unwrap(), e.rate)).sum();
    assert_eq!(total, e.frames, "the music runs the whole sequence");
    // Beats are markers on the music, in its own time.
    let beats: Vec<&Node> = music[0].children.iter().filter(|c| c.name == "marker").collect();
    assert!(beats.iter().any(|b| b.attr("value") == Some("Beat 2") && b.attr("start") == Some("15/30s")));

    // Camera sound: only the ducked moment (segment 2) keeps it.
    let muted = doc.find_all("asset-clip").into_iter().filter(|c| c.child("adjust-volume").is_some()).count();
    let footage = clips(&e).len();
    assert_eq!(muted, footage - 1);
    // Assets point at percent-encoded file URLs.
    let reps: Vec<&str> = doc.find_all("media-rep").iter().filter_map(|r| r.attr("src")).collect();
    assert!(reps.contains(&"file:///footage/%C3%89t%C3%A9%20%26%20%3CSea%3E/GX010042.MP4"), "{reps:?}");
    // The project carries the reel's (escaped) name.
    assert_eq!(doc.find_all("project")[0].attr("name"), Some("Coast & <Sea> · Été"));
}

// ---------- OTIO ----------

#[test]
fn otio_follows_the_schema_and_adds_up() {
    for rate in [None, Some(Rate::new(30000, 1001))] {
        let e = edit_at(rate);
        let v = to_otio(&e);
        otio_check(&v, "timeline").unwrap();
        // It round-trips through text as plain JSON.
        let text = serde_json::to_string_pretty(&v).unwrap();
        let back: Value = serde_json::from_str(&text).unwrap();
        let tracks = back["tracks"]["children"].as_array().unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!((tracks[0]["kind"].as_str(), tracks[1]["kind"].as_str()), (Some("Video"), Some("Audio")));
        let want = e.rate.secs(e.frames);
        assert!((otio_track_secs(&tracks[0]) - want).abs() < 1e-9, "video adds up");
        assert!((otio_track_secs(&tracks[1]) - want).abs() < 1e-9, "music adds up");
    }
}

#[test]
fn otio_items_dissolves_warp_and_markers() {
    let e = edit_at(None);
    let v = to_otio(&e);
    let video = v["tracks"]["children"][0]["children"].as_array().unwrap();
    let kinds: Vec<&str> = video.iter().map(|c| c["OTIO_SCHEMA"].as_str().unwrap()).collect();
    assert_eq!(kinds[..4], ["Gap.1", "Transition.1", "Clip.1", "Clip.1"]);
    assert_eq!(*kinds.last().unwrap(), "Gap.1");
    // The intro dissolve: SMPTE, 4 frames each side of the cut.
    let t = &video[1];
    assert_eq!(t["transition_type"], "SMPTE_Dissolve");
    assert_eq!((t["in_offset"]["value"].as_f64(), t["out_offset"]["value"].as_f64()), (Some(4.0), Some(4.0)));
    // Card gap: its text as name and marker.
    assert_eq!(video[0]["name"], "Coast & <Sea> · Été 2026");
    assert_eq!(video[0]["markers"][0]["name"], "Title: Coast & <Sea> / Été 2026");
    // The clip reads its file from its own frames (with the timecode)…
    let c = &video[2];
    assert_eq!(c["source_range"]["start_time"]["value"].as_f64(), Some(605.0));
    assert!((c["source_range"]["start_time"]["rate"].as_f64().unwrap() - 30000.0 / 1001.0).abs() < 1e-9);
    assert_eq!(c["media_reference"]["available_range"]["start_time"]["value"].as_f64(), Some(300.0));
    assert_eq!(c["media_reference"]["target_url"], "file:///footage/%C3%89t%C3%A9%20%26%20%3CSea%3E/GX010042.MP4");
    // …and the ramp tail plays 3× fast.
    assert_eq!(video[3]["effects"][0]["OTIO_SCHEMA"], "LinearTimeWarp.1");
    assert_eq!(video[3]["effects"][0]["time_scalar"].as_f64(), Some(3.0));
    // Music: file A, file B, then the loop copy crossfaded in.
    let audio = v["tracks"]["children"][1]["children"].as_array().unwrap();
    let kinds: Vec<&str> = audio.iter().map(|c| c["OTIO_SCHEMA"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["Clip.1", "Clip.1", "Transition.1", "Clip.1"]);
    assert_eq!(audio[2]["in_offset"]["value"].as_f64(), Some(15.0), "1 s crossfade = ±15 frames");
    assert_eq!(audio[3]["source_range"]["start_time"]["value"].as_f64(), Some(22050.0));
    assert_eq!(audio[3]["source_range"]["start_time"]["rate"].as_f64(), Some(44100.0));
    // Markers: cuts on the picture, beats / ducks / the fade on the music.
    let vm = v["tracks"]["children"][0]["markers"].as_array().unwrap();
    assert_eq!(vm.len(), 5 + 2, "five dissolved cuts and two burst cuts");
    assert!(vm.iter().all(|mk| mk["color"] == "RED"));
    assert!(vm[0]["name"].as_str().unwrap().contains("ms from the beat"), "{}", vm[0]["name"]);
    let am = v["tracks"]["children"][1]["markers"].as_array().unwrap();
    assert!(am.iter().any(|mk| mk["name"] == "Beat 1"));
    assert!(am.iter().any(|mk| mk["color"] == "CYAN" && mk["marked_range"]["duration"]["value"].as_f64() == Some(75.0)));
    assert!(am.iter().any(|mk| mk["name"] == "Music fades out"));
}

// ---------- Inputs and edges ----------

#[test]
fn rates_and_timecodes_parse_exactly() {
    assert_eq!(Rate::parse("29.97"), Some(Rate::new(30000, 1001)));
    assert_eq!(Rate::parse("30000/1001"), Some(Rate::new(30000, 1001)));
    assert_eq!(Rate::parse("2997/100"), Some(Rate::new(30000, 1001)));
    assert_eq!(Rate::parse("23.976"), Some(Rate::new(24000, 1001)));
    assert_eq!(Rate::parse("59.94"), Some(Rate::new(60000, 1001)));
    assert_eq!(Rate::parse("25"), Some(Rate::new(25, 1)));
    assert_eq!(Rate::parse("60/2"), Some(Rate::new(30, 1)));
    assert_eq!(Rate::parse("12.5"), Some(Rate::new(25, 2)));
    for bad in ["", "0", "abc", "30/0", "-30", "5000"] {
        assert_eq!(Rate::parse(bad), None, "{bad}");
    }
    assert_eq!((Rate::new(30000, 1001).label(), Rate::new(24000, 1001).label(), Rate::new(30, 1).label()), ("29.97".into(), "23.976".into(), "30".into()));
    let ntsc = Rate::new(30000, 1001);
    assert_eq!(timecode_frames("01:00:00:00", Rate::new(30, 1)), Some(108_000));
    assert_eq!(timecode_frames("01:00:00:00", ntsc), Some(108_000), "NDF counts frames");
    // Drop-frame skips ;00 and ;01 each minute but every tenth.
    assert_eq!(timecode_frames("00:01:00;02", ntsc), Some(1800));
    assert_eq!(timecode_frames("00:10:00;00", ntsc), Some(17_982));
    assert_eq!(timecode_frames("00:00:00:30", Rate::new(30, 1)), None);
    assert_eq!(timecode_frames("nonsense", ntsc), None);
    // Exact sums across timebases.
    assert_eq!(Rt::units(17, Rate::new(30, 1)).fcpxml(), "17/30s");
    assert_eq!(Rt::units(60, Rate::new(30, 1)).fcpxml(), "2s");
    assert_eq!(Rt::units(1, ntsc).fcpxml(), "1001/30000s");
    assert_eq!(Rt::units(22050, Rate::new(44100, 1)).add(Rt::units(3, Rate::new(30, 1))).fcpxml(), "26460/44100s");
}

#[test]
fn escaping_urls_and_host_paths() {
    assert_eq!(xml_escape("a & b < c > d \"e\" 'f'"), "a &amp; b &lt; c &gt; d &quot;e&quot; &apos;f&apos;");
    assert_eq!(xml_escape("line\nbreak\u{1}"), "line&#10;break");
    assert_eq!(file_url("/footage/a b/GX01.MP4"), "file:///footage/a%20b/GX01.MP4");
    assert_eq!(file_url("/m/Été & <x>#?.mp3"), "file:///m/%C3%89t%C3%A9%20%26%20%3Cx%3E%23%3F.mp3");
    assert_eq!(file_url("E:\\GoPro\\day 1.MP4"), "file:///E:/GoPro/day%201.MP4");
    let map = vec![("/footage".to_string(), "E:\\GoPro\\".to_string())];
    assert_eq!(map_path("/footage/trip/GX01.MP4", &map), "E:/GoPro/trip/GX01.MP4");
    assert_eq!(map_path("/footage", &map), "E:/GoPro");
    assert_eq!(map_path("/footage-old/x.MP4", &map), "/footage-old/x.MP4", "whole components only");
    assert_eq!(map_path("/projects/music/a.mp3", &map), "/projects/music/a.mp3");
    // Only absolute host folders map; compose's relative default doesn't.
    assert_eq!(
        path_map(Some("E:/GoPro"), Some("/Users/me/vibecut")),
        vec![("/footage".to_string(), "E:/GoPro".to_string()), ("/projects".to_string(), "/Users/me/vibecut".to_string())]
    );
    assert!(path_map(Some("./footage"), Some("")).is_empty());
    assert_eq!(path_map(None, Some("\\\\nas\\clips")).len(), 1);
    // The map reaches the export's URLs and clip names, not the probes.
    let e = build_edit(
        &fixture(),
        &facts(),
        &ExportOptions { rate: None, path_map: vec![("/footage".into(), "/Volumes/GoPro".into())] },
    )
    .unwrap();
    let b = e.media.iter().find(|m| m.path == B).unwrap();
    assert_eq!((b.url.as_str(), b.name.as_str()), ("file:///Volumes/GoPro/trip/GX010043.MP4", "GX010043"));
    assert!(b.probed);
}

#[test]
fn a_manifest_without_a_timeline_is_refused() {
    let mut m = fixture();
    m.timeline = None;
    match build_edit(&m, &HashMap::new(), &ExportOptions::default()) {
        Err(OpsError::InvalidInput { message, .. }) => assert!(message.contains("no timeline"), "{message}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn unprobed_files_and_older_manifests_still_export() {
    // Nothing probed: the footage is assumed to be in the sequence format,
    // and the export says so.
    let mut m = fixture();
    let e = build_edit(&m, &HashMap::new(), &ExportOptions::default()).unwrap();
    assert!(e.media.iter().all(|x| !x.probed));
    assert!(e.notes.iter().any(|n| n.contains("not found")));
    let doc = parse_xml(&to_fcpxml(&e)).unwrap();
    let asset = doc.find_all("asset").into_iter().find(|a| a.attr("name") == Some("GX010043")).unwrap();
    assert_eq!(asset.attr("format"), Some("r1"));
    // Declared as long as what the edit reads, the dissolve out included:
    // clip B (12 s in, from 5.8 s) runs to the end of the dissolve at frame
    // 233, 13.97 s into the file.
    let b = e.media.iter().find(|x| x.path == B).unwrap();
    assert_eq!(b.used_end_units, 419);
    assert_eq!(asset.attr("duration"), Some("419/30s"));
    // A manifest from before the music layout: the track from its start.
    m.timeline.as_mut().unwrap().music = None;
    let e = build_edit(&m, &facts(), &ExportOptions::default()).unwrap();
    assert_eq!(e.music.len(), 1);
    assert_eq!((e.music[0].offset, e.music[0].start_units), (0, 0));
    assert_eq!(e.music[0].duration, 180, "a 6 s track covers 6 s");
    assert!(e.notes.iter().any(|n| n.contains("predates")));
    otio_check(&to_otio(&e), "timeline").unwrap();
}

#[test]
fn ffprobe_json_gives_rate_size_audio_and_timecode() {
    let v = json!({
        "streams": [
            { "codec_type": "video", "r_frame_rate": "30000/1001", "avg_frame_rate": "30000/1001", "width": 3840, "height": 2160 },
            { "codec_type": "audio", "sample_rate": "48000", "channels": 2 },
            { "codec_type": "data", "tags": { "timecode": "13:45:10:12" } },
        ],
        "format": { "duration": "61.394000" },
    });
    let f = facts_from_ffprobe(&v);
    assert_eq!(f.video, Some(VideoFacts { rate: Rate::new(30000, 1001), width: 3840, height: 2160 }));
    assert_eq!(f.audio, Some(AudioFacts { sample_rate: 48_000, channels: 2 }));
    assert_eq!((f.duration_secs, f.timecode.as_deref()), (Some(61.394), Some("13:45:10:12")));
    // Cover art is not a video stream.
    let art = json!({ "streams": [{ "codec_type": "video", "r_frame_rate": "90000/1", "width": 500, "height": 500, "disposition": { "attached_pic": 1 } }] });
    assert_eq!(facts_from_ffprobe(&art).video, None);
}

#[test]
fn export_finds_the_manifest_of_a_render_or_its_versions() {
    let dir = std::env::temp_dir().join(format!("vibecut_timeline_export_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("reel.manifest.json"), serde_json::to_string(&fixture()).unwrap()).unwrap();
    std::fs::write(dir.join("reel.mp4"), b"x").unwrap();
    let video = dir.join("reel.mp4").to_string_lossy().into_owned();

    let t = export_timeline(&video, "fcpxml", None).unwrap();
    assert_eq!((t.file_name.as_str(), t.format.content_type()), ("reel.fcpxml", "application/xml"));
    parse_xml(&t.body).unwrap();
    // The ramped clip (2 parts), three burst parts and two more clips.
    assert_eq!(t.summary["clips"], 7);
    let t = export_timeline(&video, "OTIO", Some("29.97")).unwrap();
    assert_eq!(t.file_name, "reel.otio");
    let v: Value = serde_json::from_str(&t.body).unwrap();
    otio_check(&v, "timeline").unwrap();
    assert_eq!(v["global_start_time"]["rate"].as_f64(), Some(30000.0 / 1001.0));
    // A platform version exports its render's timeline; so does the manifest.
    std::fs::write(dir.join("reel.square.mp4"), b"x").unwrap();
    assert!(export_timeline(&dir.join("reel.square.mp4").to_string_lossy(), "otio", None).is_ok());
    let t = export_timeline(&dir.join("reel.manifest.json").to_string_lossy(), "otio", None).unwrap();
    assert_eq!(t.file_name, "reel.otio");

    assert!(matches!(export_timeline(&video, "edl", None), Err(OpsError::InvalidInput { .. })));
    assert!(matches!(export_timeline(&video, "otio", Some("fast")), Err(OpsError::InvalidInput { .. })));
    let missing = dir.join("other.mp4").to_string_lossy().into_owned();
    assert!(matches!(export_timeline(&missing, "otio", None), Err(OpsError::NotFound { .. })));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_file_export_writes_next_to_the_manifest() {
    let dir = std::env::temp_dir().join(format!("vibecut_timeline_export_file_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("coast.manifest.json"), serde_json::to_string(&fixture()).unwrap()).unwrap();
    let out = export_timeline_to_file(&dir.join("coast.mp4").to_string_lossy(), "fcpxml", None).await.unwrap();
    let path = PathBuf::from(out["path"].as_str().unwrap());
    assert_eq!(path, dir.join("coast.fcpxml"));
    parse_xml(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
