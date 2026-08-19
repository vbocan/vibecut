//! Multi-camera angle planner.
//!
//! When two or more cameras filmed the same scene, this module groups them
//! into a single logical "angle group", computes per-member time offsets
//! relative to a master camera, builds one combined score curve in master
//! scene-time, and chooses which angle to show at each picked moment.
//!
//! # Pipeline
//!
//! 1. **Discovery** ([`discover_groups`]) — find sources whose recording
//!    windows (creation_time → creation_time + duration) overlap by ≥ 1 s.
//!    Manual `[[multi_angle.groups]]` overrides discovery.
//!
//! 2. **Offset estimation** ([`compute_offsets`]) — per group, derive
//!    `member.offset` such that `member_file_time = master_scene_time + offset`.
//!    `creation_time` mode subtracts ISO timestamps; `audio` mode refines by
//!    `cross_correlate` over a 30 s overlap window; `manual` reads the
//!    `offsets` table.
//!
//! 3. **Combined curve** ([`combine_curves_in_scene_time`]) — for each member,
//!    resample its score curve into master scene-time (shifted by `offset`,
//!    zero outside the recording window), then reduce with `max` per sample.
//!
//! 4. **Angle picking** ([`pick_best_angle_for`]) — given a scene-time, score
//!    each member by `visual_sharpness × visual_exposure` at that moment;
//!    return the highest-scoring member, honoring `min_hold`.
//!
//! Convention everywhere: `member_file_time = master_scene_time + offset_secs`.
//! - master.offset_secs = 0
//! - member that started 2 s LATER than master → offset = -2.0
//!   (because at master scene-time t=10, member's local file is at t=8)
//! - member that started 2 s EARLIER → offset = +2.0

use std::collections::BTreeMap;

use crate::project_config::{AngleGroupCfg, MultiAngle, SyncMethod};
use crate::selection_planner::{Source, SourceTelemetry};
use crate::selection_scoring::ScoreCurve;

/// One discovered (or manually configured) angle group.
#[derive(Debug, Clone)]
pub struct AngleGroup {
    pub master_id: String,
    /// All members of the group INCLUDING the master at index 0.
    pub members: Vec<AngleMember>,
}

#[derive(Debug, Clone)]
pub struct AngleMember {
    pub source_id: String,
    /// `member_file_time = master_scene_time + offset_secs`.
    pub offset_secs: f64,
    /// Member's recording-window start in master scene-time.
    /// Equal to `-offset_secs` (if offset_secs is negative, member started after master).
    pub start_in_master: f64,
    /// Member's recording-window end in master scene-time = start_in_master + duration.
    pub end_in_master: f64,
    /// How this offset was determined; goes into the manifest for explainability.
    pub offset_source: OffsetSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffsetSource {
    /// Master, by definition zero.
    Self_,
    CreationTime,
    AudioXcorr,
    Manual,
}

impl OffsetSource {
    pub fn as_str(self) -> &'static str {
        match self {
            OffsetSource::Self_ => "self",
            OffsetSource::CreationTime => "creation_time",
            OffsetSource::AudioXcorr => "audio_xcorr",
            OffsetSource::Manual => "manual",
        }
    }
}

// -----------------------------------------------------------------------------
// Discovery
// -----------------------------------------------------------------------------

/// Discover angle groups. If the user provided manual `[[multi_angle.groups]]`
/// entries those win; otherwise auto-group by overlapping recording windows.
///
/// `min_overlap_secs` is the minimum window overlap (in seconds) that counts
/// as "same scene". 1 s is a reasonable default — anything shorter is likely
/// noise from a wide camera turning on right as another turns off.
pub fn discover_groups(
    sources: &[Source],
    cfg: &MultiAngle,
    min_overlap_secs: f64,
) -> Vec<AngleGroup> {
    if !cfg.enabled {
        return Vec::new();
    }
    if !cfg.groups.is_empty() {
        return groups_from_config(sources, &cfg.groups);
    }
    auto_groups_from_overlap(sources, min_overlap_secs)
}

/// Build groups straight from the TOML config. Members not found in `sources`
/// are silently dropped with a warning. A group with fewer than two existing
/// members is dropped entirely (it's not multi-camera).
fn groups_from_config(sources: &[Source], cfgs: &[AngleGroupCfg]) -> Vec<AngleGroup> {
    let mut out = Vec::new();
    for c in cfgs {
        let Some(master) = sources.iter().find(|s| s.id == c.master) else {
            tracing::warn!(master = %c.master, "configured master not found among sources");
            continue;
        };
        let mut members = vec![AngleMember {
            source_id: master.id.clone(),
            offset_secs: 0.0,
            start_in_master: 0.0,
            end_in_master: master.total_duration_secs,
            offset_source: OffsetSource::Self_,
        }];
        for mid in &c.members {
            if mid == &c.master {
                continue;
            }
            match sources.iter().find(|s| &s.id == mid) {
                Some(_) => members.push(AngleMember {
                    source_id: mid.clone(),
                    offset_secs: 0.0, // filled in by compute_offsets
                    start_in_master: 0.0,
                    end_in_master: 0.0,
                    offset_source: OffsetSource::Self_,
                }),
                None => tracing::warn!(member = %mid, "configured member not found among sources"),
            }
        }
        if members.len() < 2 {
            tracing::warn!(master = %c.master, "group has fewer than 2 existing members; dropped");
            continue;
        }
        out.push(AngleGroup {
            master_id: c.master.clone(),
            members,
        });
    }
    out
}

/// Auto-discover groups: connected components over the "overlap by ≥ N s" graph.
/// Within each component the master is the longest source.
fn auto_groups_from_overlap(sources: &[Source], min_overlap_secs: f64) -> Vec<AngleGroup> {
    // Build start-time-in-epoch-seconds (or None) per source. A source needs
    // creation_time on its first chapter to be auto-grouped.
    let mut start_epoch: Vec<Option<f64>> = Vec::with_capacity(sources.len());
    for s in sources {
        let t = s
            .chapters
            .first()
            .and_then(|c| c.creation_time.as_deref())
            .and_then(parse_iso_seconds);
        start_epoch.push(t);
    }

    // Union-Find over sources.
    let n = sources.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], x: usize) -> usize {
        let mut r = x;
        while parent[r] != r {
            r = parent[r];
        }
        // Path compression.
        let mut cur = x;
        while parent[cur] != r {
            let next = parent[cur];
            parent[cur] = r;
            cur = next;
        }
        r
    }
    fn union(parent: &mut [usize], a: usize, b: usize) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            parent[ra] = rb;
        }
    }

    for i in 0..n {
        for j in (i + 1)..n {
            let (Some(ai), Some(aj)) = (start_epoch[i], start_epoch[j]) else {
                continue;
            };
            let bi = ai + sources[i].total_duration_secs;
            let bj = aj + sources[j].total_duration_secs;
            let overlap = bi.min(bj) - ai.max(aj);
            if overlap >= min_overlap_secs {
                union(&mut parent, i, j);
            }
        }
    }

    // Bucket sources by their component root.
    let mut buckets: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        buckets.entry(r).or_default().push(i);
    }

    let mut groups = Vec::new();
    for (_, idxs) in buckets {
        if idxs.len() < 2 {
            continue;
        }
        // Master = longest by total_duration_secs. On ties, the source that
        // appears earlier in `sources` wins — gives stable output regardless
        // of the iteration order in the union-find buckets.
        let mut master_idx = idxs[0];
        let mut best_dur = sources[master_idx].total_duration_secs;
        for &i in idxs.iter().skip(1) {
            if sources[i].total_duration_secs > best_dur {
                master_idx = i;
                best_dur = sources[i].total_duration_secs;
            }
        }
        let master_id = sources[master_idx].id.clone();

        let mut members = Vec::with_capacity(idxs.len());
        // Master first.
        members.push(AngleMember {
            source_id: master_id.clone(),
            offset_secs: 0.0,
            start_in_master: 0.0,
            end_in_master: sources[master_idx].total_duration_secs,
            offset_source: OffsetSource::Self_,
        });
        for &i in &idxs {
            if i == master_idx {
                continue;
            }
            members.push(AngleMember {
                source_id: sources[i].id.clone(),
                offset_secs: 0.0,
                start_in_master: 0.0,
                end_in_master: 0.0,
                offset_source: OffsetSource::Self_,
            });
        }
        groups.push(AngleGroup { master_id, members });
    }

    groups
}

// -----------------------------------------------------------------------------
// Offset estimation
// -----------------------------------------------------------------------------

/// Compute `offset_secs`, `start_in_master`, `end_in_master` for every non-master
/// member of every group. Mutates the group list in place.
///
/// `audio_refine` is called with `(master_path, member_path, master_anchor_secs,
/// member_anchor_secs)` and returns `(refined_offset, confidence)` or `None` if
/// the audio could not be sampled. Pass `None` to skip audio refinement entirely.
pub fn compute_offsets<F>(
    groups: &mut [AngleGroup],
    sources: &[Source],
    cfg: &MultiAngle,
    mut audio_refine: Option<F>,
) where
    F: FnMut(&camino::Utf8Path, &camino::Utf8Path, f32, f32) -> Option<(f64, f32)>,
{
    let manual_offsets = manual_offsets_index(&cfg.groups);
    for g in groups.iter_mut() {
        let master = match sources.iter().find(|s| s.id == g.master_id) {
            Some(s) => s,
            None => {
                tracing::warn!(master = %g.master_id, "master source vanished before offset compute");
                continue;
            }
        };
        let master_dur = master.total_duration_secs;
        let master_start = source_start_epoch(master).unwrap_or(0.0);

        // Walk members; index 0 is the master and stays at offset 0.
        for m in g.members.iter_mut().skip(1) {
            let Some(member_src) = sources.iter().find(|s| s.id == m.source_id) else {
                continue;
            };
            let member_dur = member_src.total_duration_secs;
            let member_start = source_start_epoch(member_src);

            let creation_offset: Option<f64> = match member_start {
                Some(ms) => Some(master_start - ms),
                None => None,
            };
            // Above: master_scene_time T → member_local_file_time = T + (master_start - member_start)
            //   When member started LATER (member_start > master_start), creation_offset is negative.

            // Decide which sync mode to apply.
            let mode = cfg.sync;
            let (offset, source) = match mode {
                SyncMethod::Manual => {
                    let manual = manual_offsets
                        .get(&g.master_id)
                        .and_then(|m| m.get(&member_src.id))
                        .and_then(|s| parse_signed_humantime(s));
                    match manual {
                        Some(o) => (o, OffsetSource::Manual),
                        None => {
                            tracing::warn!(member = %member_src.id, "sync=manual but no offset configured");
                            (creation_offset.unwrap_or(0.0), OffsetSource::CreationTime)
                        }
                    }
                }
                SyncMethod::CreationTime => (
                    creation_offset.unwrap_or(0.0),
                    OffsetSource::CreationTime,
                ),
                SyncMethod::Audio | SyncMethod::Auto => {
                    let creation = creation_offset.unwrap_or(0.0);
                    if let Some(refine_fn) = audio_refine.as_mut() {
                        // Pick anchor at center of overlap.
                        let s_in_master = -creation; // member's start in master-time
                        let e_in_master = s_in_master + member_dur;
                        let lo = s_in_master.max(0.0);
                        let hi = e_in_master.min(master_dur);
                        let mid = (lo + hi) * 0.5;
                        if hi - lo < 5.0 {
                            // Not enough overlap to xcorr; fall back to creation_time.
                            (creation, OffsetSource::CreationTime)
                        } else {
                            // Anchor at the center of the master/member overlap.
                            let master_chapter = locate_in_chapters(master, mid);
                            let member_chapter = locate_in_chapters(member_src, mid + creation);
                            match (master_chapter, member_chapter) {
                                (Some((mp, ma)), Some((bp, ba))) => {
                                    match refine_fn(&mp, &bp, ma as f32, ba as f32) {
                                        Some((delta, conf)) if conf >= 0.5 => {
                                            // delta is the residual: member_b started `delta` later than master_a.
                                            // member_local_file = master_scene + creation - delta? Need to think.
                                            //
                                            // We placed master_anchor and member_anchor at content moments
                                            // we BELIEVE align under creation_offset. xcorr returns
                                            // "b started X later than a" — if X != 0 it means our guess
                                            // was off by X.
                                            //
                                            // master_anchor at master scene-time = mid.
                                            // member_anchor at member local-time = mid + creation.
                                            // After xcorr, the actual aligned member local-time is
                                            // member_anchor + delta (because b is `delta` later in source-content time).
                                            // So refined: member_local = mid + creation + delta
                                            // I.e., refined offset = creation + delta.
                                            (creation + delta, OffsetSource::AudioXcorr)
                                        }
                                        Some((_, conf)) => {
                                            tracing::warn!(
                                                member = %member_src.id,
                                                confidence = conf,
                                                "audio xcorr confidence too low; falling back to creation_time"
                                            );
                                            (creation, OffsetSource::CreationTime)
                                        }
                                        None => {
                                            tracing::warn!(
                                                member = %member_src.id,
                                                "audio xcorr failed; falling back to creation_time"
                                            );
                                            (creation, OffsetSource::CreationTime)
                                        }
                                    }
                                }
                                _ => (creation, OffsetSource::CreationTime),
                            }
                        }
                    } else {
                        (creation, OffsetSource::CreationTime)
                    }
                }
            };

            m.offset_secs = offset;
            // Member's recording window in master scene-time:
            //   member spans [0, member_dur] in its local file time.
            //   At local time L, master scene-time T = L - offset.
            //   So member's window in master-time is [-offset, member_dur - offset].
            m.start_in_master = -offset;
            m.end_in_master = member_dur - offset;
            m.offset_source = source;
        }
    }
}

fn manual_offsets_index<'a>(
    groups: &'a [AngleGroupCfg],
) -> BTreeMap<String, &'a BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for g in groups {
        out.insert(g.master.clone(), &g.offsets);
    }
    out
}

/// Parse a signed humantime duration like `"+2.4s"`, `"-800ms"`, `"500ms"`.
/// Returns the value in seconds.
pub fn parse_signed_humantime(s: &str) -> Option<f64> {
    let s = s.trim();
    let (sign, rest) = if let Some(stripped) = s.strip_prefix('+') {
        (1.0, stripped)
    } else if let Some(stripped) = s.strip_prefix('-') {
        (-1.0, stripped)
    } else {
        (1.0, s)
    };
    let d = humantime::parse_duration(rest.trim()).ok()?;
    Some(sign * d.as_secs_f64())
}

fn source_start_epoch(s: &Source) -> Option<f64> {
    s.chapters
        .first()
        .and_then(|c| c.creation_time.as_deref())
        .and_then(parse_iso_seconds)
}

fn locate_in_chapters(source: &Source, virtual_secs: f64) -> Option<(camino::Utf8PathBuf, f64)> {
    let mut acc = 0.0;
    for ch in &source.chapters {
        if virtual_secs < acc + ch.duration_secs {
            return Some((ch.path.clone(), virtual_secs - acc));
        }
        acc += ch.duration_secs;
    }
    None
}

/// Best-effort ISO-8601 → seconds-since-epoch. Same algorithm as the helper in
/// main.rs but local to this module to avoid a public surface.
pub fn parse_iso_seconds(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches('Z');
    let (date_part, time_part) = s.split_once('T')?;
    let mut date_iter = date_part.split('-');
    let y: i64 = date_iter.next()?.parse().ok()?;
    let mo: i64 = date_iter.next()?.parse().ok()?;
    let d: i64 = date_iter.next()?.parse().ok()?;
    let mut time_iter = time_part.split(':');
    let hh: i64 = time_iter.next()?.parse().ok()?;
    let mm: i64 = time_iter.next()?.parse().ok()?;
    let ss: f64 = time_iter.next()?.parse().ok()?;
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * if mo > 2 { mo - 3 } else { mo + 9 } + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days_since_epoch = era * 146097 + doe - 719468;
    Some(days_since_epoch as f64 * 86400.0 + hh as f64 * 3600.0 + mm as f64 * 60.0 + ss)
}

// -----------------------------------------------------------------------------
// Combined score curve in master scene-time
// -----------------------------------------------------------------------------

/// Build one combined score curve for a group, in master scene-time.
///
/// Each member's score curve (sampled in its local file-time) is shifted by
/// `offset_secs` and then reduced into the master-time grid via per-sample
/// `max`. The result has duration = master's source duration.
///
/// `member_curves` must be parallel to `group.members` and the master is at
/// index 0.
pub fn combine_curves_in_scene_time(
    group: &AngleGroup,
    member_curves: &[&ScoreCurve],
    master_duration_secs: f32,
    sample_rate: f32,
) -> ScoreCurve {
    assert_eq!(group.members.len(), member_curves.len());
    let mut out = ScoreCurve::zeros(master_duration_secs, sample_rate);
    for i in 0..out.samples.len() {
        let t_master = i as f32 / sample_rate;
        let mut best = 0.0_f32;
        for (m, c) in group.members.iter().zip(member_curves.iter()) {
            // Skip if outside member's recording window in master-time.
            if (t_master as f64) < m.start_in_master || (t_master as f64) > m.end_in_master {
                continue;
            }
            let t_local = t_master as f64 + m.offset_secs;
            if t_local < 0.0 || (t_local as f32) > c.duration_secs() {
                continue;
            }
            let j = c.time_to_index(t_local as f32);
            let v = c.samples.get(j).copied().unwrap_or(0.0);
            if v > best {
                best = v;
            }
        }
        out.samples[i] = best;
    }
    out
}

// -----------------------------------------------------------------------------
// Angle picking
// -----------------------------------------------------------------------------

/// Choose the best member to show at master scene-time `t_master`. Members
/// outside their recording window are excluded. Among in-window members, the
/// one with the highest `sharpness × exposure` at the corresponding local time
/// wins. If no member has visual telemetry, the master is returned.
///
/// `min_hold_secs` and `prev` together implement the angle-stickiness rule:
/// when the previous pick from the same group was less than `min_hold_secs`
/// ago, prefer keeping `prev_member_id` unless the alternative beats it by
/// the `min_hold_break_margin` threshold. Set `min_hold_secs = 0` to disable.
pub fn pick_best_angle_for(
    group: &AngleGroup,
    member_telemetry: &[&SourceTelemetry],
    t_master: f64,
    prev: Option<(&str, f64)>, // (prev member id, prev t_master)
    min_hold_secs: f64,
    min_hold_break_margin: f32,
) -> AngleChoice {
    assert_eq!(group.members.len(), member_telemetry.len());
    let mut best_idx: Option<usize> = None;
    let mut best_score = f32::NEG_INFINITY;
    let mut scores = Vec::with_capacity(group.members.len());

    for (i, (m, tel)) in group.members.iter().zip(member_telemetry.iter()).enumerate() {
        if t_master < m.start_in_master || t_master > m.end_in_master {
            scores.push(f32::NEG_INFINITY);
            continue;
        }
        let local = t_master + m.offset_secs;
        let s = visual_quality_at(tel, local as f32);
        scores.push(s);
        if s > best_score {
            best_score = s;
            best_idx = Some(i);
        }
    }

    // Apply min_hold preference.
    if let (Some(prev_id), Some(_)) = (prev.map(|p| p.0), prev) {
        if let Some(prev_t) = prev.map(|p| p.1) {
            if (t_master - prev_t) < min_hold_secs {
                // If prev member is in-window and competitive, stay on it.
                if let Some(prev_pos) = group.members.iter().position(|m| m.source_id == prev_id) {
                    let prev_score = scores.get(prev_pos).copied().unwrap_or(f32::NEG_INFINITY);
                    if prev_score.is_finite() {
                        let chosen = best_idx.unwrap_or(prev_pos);
                        let chosen_score = scores[chosen];
                        if chosen_score - prev_score < min_hold_break_margin {
                            best_idx = Some(prev_pos);
                            best_score = prev_score;
                        }
                    }
                }
            }
        }
    }

    let idx = best_idx.unwrap_or(0); // fallback to master
    AngleChoice {
        member_index: idx,
        member_id: group.members[idx].source_id.clone(),
        local_file_time_secs: t_master + group.members[idx].offset_secs,
        score: if best_score.is_finite() { best_score } else { 0.0 },
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // local_file_time_secs / score are part of the choice record, surfaced via tests + future explain output
pub struct AngleChoice {
    /// Index into `group.members`.
    pub member_index: usize,
    pub member_id: String,
    /// Translated time in the chosen member's stitched timeline.
    pub local_file_time_secs: f64,
    pub score: f32,
}

/// Visual-quality proxy at virtual time `t` in the source's stitched timeline.
/// Uses sharpness × exposure (both in [0,1] after normalization upstream).
/// Returns 0 if the source has no visual telemetry at all.
fn visual_quality_at(tel: &SourceTelemetry, t: f32) -> f32 {
    if tel.visual_sharpness.is_empty() || tel.visual_time_secs.is_empty() {
        return 0.5; // neutral fallback so members without visual still rank
    }
    // Locate nearest sample.
    let i = nearest_index(&tel.visual_time_secs, t);
    let sh = tel.visual_sharpness.get(i).copied().unwrap_or(0.0);
    let ex = tel.visual_exposure.get(i).copied().unwrap_or(0.0);
    // Normalize sharpness to roughly [0,1] using a soft cap (Laplacian variances
    // typically run into hundreds–low-thousands; saturate at 1000).
    let sh_n = (sh / 1000.0).clamp(0.0, 1.0);
    sh_n * ex
}

fn nearest_index(times: &[f32], t: f32) -> usize {
    if times.is_empty() {
        return 0;
    }
    // Binary search by partition point.
    let pos = times.partition_point(|x| *x < t);
    if pos == 0 {
        0
    } else if pos >= times.len() {
        times.len() - 1
    } else {
        let lo = times[pos - 1];
        let hi = times[pos];
        if (t - lo).abs() <= (hi - t).abs() {
            pos - 1
        } else {
            pos
        }
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection_planner::Chapter;

    fn make_source(id: &str, duration: f64, creation: Option<&str>) -> Source {
        Source {
            id: id.into(),
            chapters: vec![Chapter {
                path: format!("{id}.MP4").into(),
                chapter_index: 1,
                duration_secs: duration,
                creation_time: creation.map(str::to_string),
                blake3: None,
            }],
            total_duration_secs: duration,
        }
    }

    #[test]
    fn parses_signed_humantime() {
        assert_eq!(parse_signed_humantime("+2.4s"), Some(2.4));
        assert_eq!(parse_signed_humantime("-800ms"), Some(-0.8));
        assert_eq!(parse_signed_humantime("500ms"), Some(0.5));
        assert_eq!(parse_signed_humantime(" -1s "), Some(-1.0));
        assert!(parse_signed_humantime("not a duration").is_none());
    }

    #[test]
    fn auto_groups_overlapping_sources() {
        // A and B overlap (same start ± 5s, both 60s long). C is unrelated.
        let sources = vec![
            make_source("A", 60.0, Some("2026-05-07T10:00:00.0Z")),
            make_source("B", 60.0, Some("2026-05-07T10:00:05.0Z")),
            make_source("C", 30.0, Some("2026-05-07T11:00:00.0Z")),
        ];
        let cfg = MultiAngle {
            enabled: true,
            ..Default::default()
        };
        let groups = discover_groups(&sources, &cfg, 1.0);
        assert_eq!(groups.len(), 1);
        let g = &groups[0];
        assert_eq!(g.master_id, "A"); // both A and B are 60s; first found wins via stable sort
        assert_eq!(g.members.len(), 2);
        let ids: Vec<_> = g.members.iter().map(|m| m.source_id.as_str()).collect();
        assert!(ids.contains(&"A"));
        assert!(ids.contains(&"B"));
    }

    #[test]
    fn auto_grouping_respects_min_overlap() {
        // 60s sources, second starts 59.5s after the first → 0.5s overlap.
        let sources = vec![
            make_source("A", 60.0, Some("2026-05-07T10:00:00.0Z")),
            make_source("B", 60.0, Some("2026-05-07T10:00:59.5Z")),
        ];
        let cfg = MultiAngle {
            enabled: true,
            ..Default::default()
        };
        let groups = discover_groups(&sources, &cfg, 1.0);
        assert!(groups.is_empty(), "0.5s overlap should not group");
    }

    #[test]
    fn manual_groups_override_discovery() {
        let sources = vec![
            make_source("A", 60.0, Some("2026-05-07T10:00:00.0Z")),
            make_source("B", 60.0, Some("2026-05-07T10:00:00.0Z")),
            make_source("C", 60.0, Some("2026-05-07T10:00:00.0Z")),
        ];
        let cfg = MultiAngle {
            enabled: true,
            groups: vec![AngleGroupCfg {
                master: "B".into(),
                members: vec!["C".into()],
                offsets: BTreeMap::new(),
            }],
            ..Default::default()
        };
        let groups = discover_groups(&sources, &cfg, 1.0);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].master_id, "B");
        let ids: Vec<_> = groups[0].members.iter().map(|m| m.source_id.as_str()).collect();
        assert_eq!(ids, vec!["B", "C"]);
    }

    #[test]
    fn creation_time_offset_member_started_later() {
        // Master at 10:00:00, member at 10:00:02 → member started 2s later.
        // master_scene_time T → member_local = T - 2 → offset = -2.
        let sources = vec![
            make_source("M", 60.0, Some("2026-05-07T10:00:00.0Z")),
            make_source("B", 60.0, Some("2026-05-07T10:00:02.0Z")),
        ];
        let cfg = MultiAngle {
            enabled: true,
            sync: SyncMethod::CreationTime,
            ..Default::default()
        };
        let mut groups = discover_groups(&sources, &cfg, 1.0);
        compute_offsets::<fn(&camino::Utf8Path, &camino::Utf8Path, f32, f32) -> Option<(f64, f32)>>(
            &mut groups,
            &sources,
            &cfg,
            None,
        );
        let g = &groups[0];
        let b = g.members.iter().find(|m| m.source_id == "B").unwrap();
        assert!((b.offset_secs - (-2.0)).abs() < 1e-6, "got {}", b.offset_secs);
        // B's recording window in master scene-time: [2.0, 62.0]
        assert!((b.start_in_master - 2.0).abs() < 1e-6);
        assert!((b.end_in_master - 62.0).abs() < 1e-6);
        assert_eq!(b.offset_source, OffsetSource::CreationTime);
    }

    #[test]
    fn manual_offset_overrides() {
        let sources = vec![
            make_source("M", 60.0, Some("2026-05-07T10:00:00.0Z")),
            make_source("B", 60.0, Some("2026-05-07T10:00:00.0Z")),
        ];
        let mut offsets = BTreeMap::new();
        offsets.insert("B".to_string(), "+1.5s".to_string());
        let cfg = MultiAngle {
            enabled: true,
            sync: SyncMethod::Manual,
            groups: vec![AngleGroupCfg {
                master: "M".into(),
                members: vec!["B".into()],
                offsets,
            }],
            ..Default::default()
        };
        let mut groups = discover_groups(&sources, &cfg, 1.0);
        compute_offsets::<fn(&camino::Utf8Path, &camino::Utf8Path, f32, f32) -> Option<(f64, f32)>>(
            &mut groups,
            &sources,
            &cfg,
            None,
        );
        let b = groups[0]
            .members
            .iter()
            .find(|m| m.source_id == "B")
            .unwrap();
        assert!((b.offset_secs - 1.5).abs() < 1e-6, "got {}", b.offset_secs);
        assert_eq!(b.offset_source, OffsetSource::Manual);
    }

    #[test]
    fn combine_curves_max_reduces_in_scene_time() {
        // Two members. Master is flat 0.2 throughout 10s.
        // Member B has offset = -2 (started 2s late) and a sharp peak at its
        // local t=3 → master scene-time t=5.
        let group = AngleGroup {
            master_id: "M".into(),
            members: vec![
                AngleMember {
                    source_id: "M".into(),
                    offset_secs: 0.0,
                    start_in_master: 0.0,
                    end_in_master: 10.0,
                    offset_source: OffsetSource::Self_,
                },
                AngleMember {
                    source_id: "B".into(),
                    offset_secs: -2.0,
                    start_in_master: 2.0,
                    end_in_master: 12.0,
                    offset_source: OffsetSource::CreationTime,
                },
            ],
        };
        let mut master = ScoreCurve::zeros(10.0, 4.0);
        for s in &mut master.samples {
            *s = 0.2;
        }
        let mut bcurve = ScoreCurve::zeros(10.0, 4.0);
        // peak at member-local t=3
        bcurve.add_dirac(3.0, 1.0);
        let combined = combine_curves_in_scene_time(&group, &[&master, &bcurve], 10.0, 4.0);
        // At master scene-time t=5 (= member local t=3) we should see ≥ 1.0 (peak from B).
        let i = combined.time_to_index(5.0);
        assert!(combined.samples[i] >= 0.95, "got {}", combined.samples[i]);
        // At master scene-time t=0 (master only, b not yet rolling) we should see 0.2.
        let i0 = combined.time_to_index(0.0);
        assert!((combined.samples[i0] - 0.2).abs() < 1e-3, "got {}", combined.samples[i0]);
    }

    #[test]
    fn pick_best_angle_chooses_higher_visual_quality() {
        let group = AngleGroup {
            master_id: "M".into(),
            members: vec![
                AngleMember {
                    source_id: "M".into(),
                    offset_secs: 0.0,
                    start_in_master: 0.0,
                    end_in_master: 10.0,
                    offset_source: OffsetSource::Self_,
                },
                AngleMember {
                    source_id: "B".into(),
                    offset_secs: 0.0,
                    start_in_master: 0.0,
                    end_in_master: 10.0,
                    offset_source: OffsetSource::CreationTime,
                },
            ],
        };
        // Master: blurry. B: sharp. Both well-exposed.
        let m_tel = SourceTelemetry {
            source_id: "M".into(),
            total_duration_secs: 10.0,
            visual_fps_hz: 1.0,
            visual_time_secs: vec![5.0],
            visual_sharpness: vec![100.0],
            visual_exposure: vec![1.0],
            visual_motion: vec![0.0],
            ..Default::default()
        };
        let b_tel = SourceTelemetry {
            source_id: "B".into(),
            total_duration_secs: 10.0,
            visual_fps_hz: 1.0,
            visual_time_secs: vec![5.0],
            visual_sharpness: vec![900.0],
            visual_exposure: vec![1.0],
            visual_motion: vec![0.0],
            ..Default::default()
        };
        let choice = pick_best_angle_for(&group, &[&m_tel, &b_tel], 5.0, None, 0.0, 0.0);
        assert_eq!(choice.member_id, "B");
    }

    #[test]
    fn pick_best_angle_respects_min_hold() {
        let group = AngleGroup {
            master_id: "M".into(),
            members: vec![
                AngleMember {
                    source_id: "M".into(),
                    offset_secs: 0.0,
                    start_in_master: 0.0,
                    end_in_master: 10.0,
                    offset_source: OffsetSource::Self_,
                },
                AngleMember {
                    source_id: "B".into(),
                    offset_secs: 0.0,
                    start_in_master: 0.0,
                    end_in_master: 10.0,
                    offset_source: OffsetSource::CreationTime,
                },
            ],
        };
        // B is slightly sharper than M (margin = 0.05 in normalized score).
        let m_tel = SourceTelemetry {
            source_id: "M".into(),
            total_duration_secs: 10.0,
            visual_fps_hz: 1.0,
            visual_time_secs: vec![5.0],
            visual_sharpness: vec![800.0],
            visual_exposure: vec![1.0],
            visual_motion: vec![0.0],
            ..Default::default()
        };
        let b_tel = SourceTelemetry {
            source_id: "B".into(),
            total_duration_secs: 10.0,
            visual_fps_hz: 1.0,
            visual_time_secs: vec![5.0],
            visual_sharpness: vec![850.0],
            visual_exposure: vec![1.0],
            visual_motion: vec![0.0],
            ..Default::default()
        };
        // Without min_hold: B wins.
        let no_hold = pick_best_angle_for(&group, &[&m_tel, &b_tel], 5.0, Some(("M", 4.0)), 0.0, 0.0);
        assert_eq!(no_hold.member_id, "B");
        // With min_hold = 5s and a 0.10 break margin, the 0.05 advantage of B
        // shouldn't be enough to switch off M.
        let with_hold = pick_best_angle_for(&group, &[&m_tel, &b_tel], 5.0, Some(("M", 4.0)), 5.0, 0.10);
        assert_eq!(with_hold.member_id, "M");
    }
}
