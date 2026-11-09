//! The watcher end to end — folders appear, settle, queue, render through
//! a job, notify — with a stand-in [`ReelMaker`] and a real job registry.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::*;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("vibecut_watch_svc_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("footage")).unwrap();
    std::fs::create_dir_all(d.join("projects")).unwrap();
    d
}

fn put(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

fn settings(dir: &Path) -> WatchSettings {
    WatchSettings {
        roots: vec![dir.join("footage")],
        footage_root: dir.join("footage"),
        projects_root: dir.join("projects"),
        state_path: Some(dir.join("projects").join(STATE_FILE)),
        poll: Duration::from_millis(20),
        settle: Duration::from_secs(10),
        job_poll: Duration::from_millis(5),
        enabled_by_default: true,
        vibe: None,
        target_duration: None,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Outcome {
    Done,
    Fail,
    Stop,
}

/// Proposes instantly and "renders" in a few ms through a real job.
#[derive(Clone)]
struct FakeMaker {
    jobs: JobRegistry,
    /// The test's folder: footage in `footage/`, projects written to `projects/`.
    dir: PathBuf,
    proposals: Arc<AtomicUsize>,
    propose_error: Option<&'static str>,
    outcome: Outcome,
}

impl FakeMaker {
    fn new(jobs: &JobRegistry, dir: &Path) -> Self {
        FakeMaker {
            jobs: jobs.clone(),
            dir: dir.to_path_buf(),
            proposals: Arc::default(),
            propose_error: None,
            outcome: Outcome::Done,
        }
    }
    fn proposed(&self) -> usize {
        self.proposals.load(Ordering::SeqCst)
    }
}

impl ReelMaker for FakeMaker {
    /// Writes a real project, as `OpsReelMaker` does (`propose_vibe`):
    /// `projects/proposal_<name>_<n>.toml` whose `[input].folder` is the
    /// footage's absolute path — so the watcher meets its own projects.
    async fn propose(&self, entry: &WatchEntry) -> Result<Proposal, String> {
        let n = self.proposals.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(e) = self.propose_error {
            return Err(e.to_string());
        }
        let footage = self.dir.join("footage").join(&entry.folder);
        let config_path = self.dir.join("projects").join(format!("proposal_{}_{n}.toml", entry.name));
        let toml = format!("schema_version = 1\n[input]\nfolder = \"{}\"\n", footage.display());
        std::fs::write(&config_path, toml).map_err(|e| e.to_string())?;
        let config_path = config_path.to_string_lossy().into_owned();
        Ok(Proposal {
            summary: json!({ "title": format!("{} · 2026-11-09", entry.name), "config_path": config_path, "vibe": "kid_reel" }),
            config_path,
        })
    }

    async fn submit(&self, config_path: &str) -> Result<String, String> {
        let id = self.jobs.submit_for(config_path).await;
        let (jobs, job, outcome) = (self.jobs.clone(), id.clone(), self.outcome);
        tokio::spawn(async move {
            assert!(jobs.begin(&job, "t0").await);
            tokio::time::sleep(Duration::from_millis(15)).await;
            let state = match outcome {
                Outcome::Done => JobState::Done {
                    started_at: "t0".into(),
                    finished_at: "t1".into(),
                    elapsed_ms: 15,
                    result_json: json!({
                        "ok": true,
                        "output_path": "/projects/out/reel.mp4",
                        "version_path": "/projects/out/versions/reel__t_job.mp4",
                        "manifest_summary": { "actual_duration_secs": 30.0,
                            "beat_sync": { "n_cuts": 12, "within_tolerance": 12, "tempo_bpm": 132.0, "mean_abs_error_ms": 8.0 } }
                    }),
                },
                Outcome::Fail => JobState::Failed {
                    started_at: "t0".into(),
                    finished_at: "t1".into(),
                    elapsed_ms: 15,
                    error: "ffmpeg failed".into(),
                },
                Outcome::Stop => JobState::Cancelled { started_at: Some("t0".into()), finished_at: "t1".into(), elapsed_ms: 15 },
            };
            jobs.set(&job, state).await;
        });
        Ok(id)
    }

    async fn poster(&self, render: &Value) -> Option<String> {
        render.get("version_path").and_then(Value::as_str).map(maker::poster_path_for)
    }
}

async fn entries(svc: &WatchService) -> Vec<WatchEntry> {
    serde_json::from_value(svc.status_json().await["entries"].clone()).unwrap()
}

async fn entry(svc: &WatchService, name: &str) -> WatchEntry {
    entries(svc).await.into_iter().find(|e| e.name == name).unwrap_or_else(|| panic!("no entry {name}"))
}

fn drain(rx: &mut broadcast::Receiver<WatchEvent>) -> Vec<WatchEvent> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

/// The whole story: what is there at first is left alone; a new trip waits
/// while it is copied, queues once settled, renders once through a job,
/// and listeners hear every step and the finished reel.
#[tokio::test]
async fn a_new_trip_settles_queues_and_renders_once() {
    let dir = tmp("flow");
    let footage = dir.join("footage");
    put(&footage, "Old trip/GX010001.MP4", b"old");
    let jobs = JobRegistry::new();
    let maker = FakeMaker::new(&jobs, &dir);
    let svc = WatchService::new(settings(&dir), jobs.clone());
    let mut rx = svc.subscribe();
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);

    // First look: the baseline. Nothing listed, nothing rendered.
    svc.tick(at(0)).await;
    assert!(entries(&svc).await.is_empty());
    assert!(!svc.work_once(&maker).await);

    // A trip being copied in: waiting while it grows.
    put(&footage, "Coastline/GX010001.MP4", b"12");
    svc.tick(at(1)).await;
    let e = entry(&svc, "Coastline").await;
    assert_eq!((e.status, e.n_videos, e.total_bytes, e.folder.as_str()), (EntryStatus::Waiting, 1, 2, "Coastline"));
    put(&footage, "Coastline/GX010001.MP4", b"1234");
    put(&footage, "Coastline/GX020001.MP4", b"1");
    svc.tick(at(8)).await;
    svc.tick(at(15)).await;
    assert_eq!(entry(&svc, "Coastline").await.status, EntryStatus::Waiting, "settled 7 s after the last change");
    assert!(!svc.work_once(&maker).await, "rendered before it settled");
    svc.tick(at(19)).await;
    let e = entry(&svc, "Coastline").await;
    assert_eq!((e.status, e.n_videos, e.total_bytes), (EntryStatus::Queued, 2, 5));

    // Rendered as a job in the shared registry.
    assert!(svc.work_once(&maker).await);
    assert_eq!(maker.proposed(), 1);
    let e = entry(&svc, "Coastline").await;
    assert_eq!(e.status, EntryStatus::Done);
    assert_eq!(e.attempts, 1);
    let job = e.job_id.clone().unwrap();
    assert!(matches!(jobs.get(&job).await, Some(JobState::Done { .. })));
    assert_eq!(e.render.as_ref().unwrap()["output_path"], "/projects/out/reel.mp4");
    assert_eq!(e.proposal.as_ref().unwrap()["title"], "Coastline · 2026-11-09");
    assert_eq!(e.poster_path.as_deref(), Some("/projects/out/versions/reel__t_job.poster.jpg"));
    assert!(e.progress.is_none() && e.stage.is_none());

    // Nothing more to do, however often it looks.
    for s in [30, 60, 90] {
        svc.tick(at(s)).await;
    }
    assert!(!svc.work_once(&maker).await);
    assert_eq!(maker.proposed(), 1);

    // Listeners heard it all, ending with the reel.
    let events = drain(&mut rx);
    let statuses: Vec<EntryStatus> = events
        .iter()
        .filter_map(|ev| match ev {
            WatchEvent::Entry { entry } => Some(entry.status),
            _ => None,
        })
        .collect();
    for s in [EntryStatus::Waiting, EntryStatus::Queued, EntryStatus::Rendering, EntryStatus::Done] {
        assert!(statuses.contains(&s), "no {s:?} event in {statuses:?}");
    }
    match events.last() {
        Some(WatchEvent::ReelReady { entry }) => assert_eq!(entry.name, "Coastline"),
        other => panic!("last event: {other:?}"),
    }
    assert_eq!(events.iter().filter(|e| matches!(e, WatchEvent::ReelReady { .. })).count(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A restart reads what was handled: done folders aren't rendered again,
/// a folder still queued is rendered exactly once, baseline folders stay
/// alone.
#[tokio::test]
async fn a_restart_never_renders_a_folder_twice() {
    let dir = tmp("restart");
    let footage = dir.join("footage");
    put(&footage, "Old trip/GX010001.MP4", b"old");
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    {
        let jobs = JobRegistry::new();
        let maker = FakeMaker::new(&jobs, &dir);
        let svc = WatchService::new(settings(&dir), jobs);
        svc.tick(at(0)).await;
        put(&footage, "Alps/GX010001.MP4", b"1");
        put(&footage, "Beach/GX010001.MP4", b"1");
        svc.tick(at(1)).await;
        svc.tick(at(12)).await;
        let queued: Vec<String> =
            entries(&svc).await.iter().filter(|e| e.status == EntryStatus::Queued).map(|e| e.name.clone()).collect();
        assert_eq!(queued.len(), 2);
        // Renders one, then the server goes down with the other still queued.
        assert!(svc.work_once(&maker).await);
        assert_eq!(maker.proposed(), 1);
    }
    let file = WatchFile::load(&dir.join("projects").join(STATE_FILE));
    assert!(file.baseline_taken);
    assert_eq!(file.entries.iter().filter(|e| e.status == EntryStatus::Done).count(), 1);

    for restart in 0..2 {
        let jobs = JobRegistry::new();
        let maker = FakeMaker::new(&jobs, &dir);
        let svc = WatchService::new(settings(&dir), jobs);
        for s in [100, 120, 140] {
            svc.tick(at(s)).await;
        }
        while svc.work_once(&maker).await {}
        // First restart: only the one left queued; second: nothing at all.
        assert_eq!(maker.proposed(), usize::from(restart == 0), "restart {restart}");
        let all = entries(&svc).await;
        assert_eq!(all.len(), 2, "{all:?}");
        assert!(all.iter().all(|e| e.status == EntryStatus::Done));
        assert!(all.iter().all(|e| e.name != "Old trip"));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A server that died mid-render queues that folder again on restart;
/// the job it lost is not looked for.
#[tokio::test]
async fn a_render_cut_short_by_a_restart_is_made_again() {
    let dir = tmp("cutshort");
    put(&dir.join("footage"), "Alps/GX010001.MP4", b"1");
    let mut file = WatchFile { baseline_taken: true, enabled: Some(true), ..Default::default() };
    let unit = scan::find_units(&[dir.join("footage")]).remove(0);
    let mut e = WatchEntry::new(&unit, "", &dir.join("footage"), EntryStatus::Rendering);
    e.attempts = 1;
    e.job_id = Some("job_gone".into());
    file.entries.push(e);
    file.save(&dir.join("projects").join(STATE_FILE)).unwrap();

    let jobs = JobRegistry::new();
    let maker = FakeMaker::new(&jobs, &dir);
    let svc = WatchService::new(settings(&dir), jobs);
    let e = entry(&svc, "Alps").await;
    assert_eq!(e.status, EntryStatus::Queued);
    assert!(e.note.unwrap().contains("restart"));
    assert!(svc.work_once(&maker).await);
    let e = entry(&svc, "Alps").await;
    assert_eq!((e.status, e.attempts, maker.proposed()), (EntryStatus::Done, 2, 1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn folders_that_already_have_a_project_are_skipped() {
    let dir = tmp("skip");
    let footage = dir.join("footage");
    let jobs = JobRegistry::new();
    let maker = FakeMaker::new(&jobs, &dir);
    let svc = WatchService::new(settings(&dir), jobs);
    let t0 = Instant::now();
    svc.tick(t0).await; // baseline of an empty mount
    put(&footage, "Alps/GX010001.MP4", b"1");
    std::fs::write(
        dir.join("projects/proposal_alps_1.toml"),
        format!("schema_version = 1\n[input]\nfolder = \"{}\"\n", footage.join("Alps").display()),
    )
    .unwrap();
    svc.tick(t0 + Duration::from_secs(1)).await;
    svc.tick(t0 + Duration::from_secs(12)).await;
    let e = entry(&svc, "Alps").await;
    assert_eq!(e.status, EntryStatus::Skipped);
    assert!(e.note.as_deref().unwrap().contains("proposal_alps_1.toml"), "{:?}", e.note);
    assert!(!svc.work_once(&maker).await);
    // "Make it anyway".
    svc.retry(&e.id).await.unwrap().unwrap();
    assert!(svc.work_once(&maker).await);
    assert_eq!(entry(&svc, "Alps").await.status, EntryStatus::Done);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A card is handled per set of clips: the same card back with the same
/// clips is left alone, with new ones it makes a new reel.
#[tokio::test]
async fn a_card_back_with_new_clips_makes_a_new_reel() {
    let dir = tmp("card");
    let cards = dir.join("footage");
    let jobs = JobRegistry::new();
    let maker = FakeMaker::new(&jobs, &dir);
    let mut s = settings(&dir);
    s.settle = Duration::ZERO;
    let svc = WatchService::new(s, jobs);
    let t0 = Instant::now();
    let mut t = 0;
    let mut look = || {
        t += 1;
        t0 + Duration::from_secs(t)
    };
    svc.tick(look()).await; // baseline: no card yet
    put(&cards, "3436-6331/DCIM/100GOPRO/GX010001.MP4", b"1");
    put(&cards, "3436-6331/DCIM/100GOPRO/GL010001.LRV", b"1");
    svc.tick(look()).await;
    svc.tick(look()).await;
    let first = entry(&svc, "SD card").await;
    assert_eq!((first.kind, first.status, first.n_videos), (UnitKind::Card, EntryStatus::Queued, 1));
    assert!(first.folder.ends_with("3436-6331/DCIM"), "{}", first.folder);
    assert!(svc.work_once(&maker).await);

    // Pulled out and put back as it was: nothing new.
    std::fs::rename(cards.join("3436-6331"), dir.join("away")).unwrap();
    svc.tick(look()).await;
    std::fs::rename(dir.join("away"), cards.join("3436-6331")).unwrap();
    for _ in 0..3 {
        svc.tick(look()).await;
    }
    assert!(!svc.work_once(&maker).await);
    assert_eq!(entries(&svc).await.len(), 1);

    // Back after another day's filming: a new reel — the project the
    // watcher wrote for the first reel (same card folder) doesn't count
    // as "already in your projects".
    assert_eq!(std::fs::read_dir(dir.join("projects")).unwrap().flatten().filter(|e| e.path().extension().is_some_and(|x| x == "toml")).count(), 1);
    put(&cards, "3436-6331/DCIM/100GOPRO/GX010002.MP4", b"22");
    svc.tick(look()).await;
    svc.tick(look()).await;
    let second = entries(&svc).await.into_iter().find(|e| e.id != first.id).expect("a second entry");
    assert_eq!((second.status, second.n_videos, second.note.as_deref()), (EntryStatus::Queued, 2, None));
    assert!(svc.work_once(&maker).await);
    let all = entries(&svc).await;
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|e| e.status == EntryStatus::Done), "{all:?}");
    assert_ne!(all[0].id, all[1].id);
    assert_eq!(maker.proposed(), 2);

    // A third time, now with a project of the user's own for the card: still
    // a new reel only when none of the projects is someone else's.
    std::fs::write(
        dir.join("projects/my_edit.toml"),
        format!("schema_version = 1\n[input]\nfolder = \"{}\"\n", cards.join("3436-6331/DCIM").display()),
    )
    .unwrap();
    put(&cards, "3436-6331/DCIM/100GOPRO/GX010003.MP4", b"333");
    svc.tick(look()).await;
    svc.tick(look()).await;
    let third = entries(&svc).await.into_iter().find(|e| e.n_videos == 3).expect("a third entry");
    assert_eq!(third.status, EntryStatus::Skipped);
    assert!(third.note.as_deref().unwrap().contains("my_edit.toml"), "{:?}", third.note);
    let _ = std::fs::remove_dir_all(&dir);
}

/// While an earlier reel of the same card is still rendering — its project
/// already written, not yet recorded as the watcher's — new clips on the
/// card wait for it, rather than taking that project for someone else's.
#[tokio::test]
async fn new_clips_on_a_card_wait_for_its_earlier_reel_to_render() {
    let dir = tmp("cardbusy");
    let cards = dir.join("footage");
    let mut s = settings(&dir);
    s.settle = Duration::ZERO;
    let svc = WatchService::new(s, JobRegistry::new());
    let t0 = Instant::now();
    let mut t = 0;
    let mut look = || {
        t += 1;
        t0 + Duration::from_secs(t)
    };
    svc.tick(look()).await;
    put(&cards, "HERO12/DCIM/100GOPRO/GX010001.MP4", b"1");
    svc.tick(look()).await;
    svc.tick(look()).await;
    let first = entry(&svc, "HERO12").await;
    assert_eq!(first.status, EntryStatus::Queued);
    // The worker is on it: rendering, its project written, not recorded yet.
    svc.shared.inner.lock().await.file.get_mut(&first.id).unwrap().set(EntryStatus::Rendering, None);
    let project = dir.join("projects/proposal_dcim_1.toml");
    let toml = format!("schema_version = 1\n[input]\nfolder = \"{}\"\n", cards.join("HERO12/DCIM").display());
    std::fs::write(&project, toml).unwrap();
    put(&cards, "HERO12/DCIM/100GOPRO/GX010002.MP4", b"2");
    for _ in 0..3 {
        svc.tick(look()).await;
    }
    let second = entries(&svc).await.into_iter().find(|e| e.id != first.id).expect("a second entry");
    assert_eq!(second.status, EntryStatus::Waiting);
    // Proposal recorded, reel done: the new clips are queued, not skipped.
    {
        let mut g = svc.shared.inner.lock().await;
        let e = g.file.get_mut(&first.id).unwrap();
        e.proposal = Some(json!({ "config_path": project.to_string_lossy() }));
        e.set(EntryStatus::Done, None);
    }
    svc.tick(look()).await;
    let second = entries(&svc).await.into_iter().find(|e| e.id == second.id).unwrap();
    assert_eq!((second.status, second.note), (EntryStatus::Queued, None));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A state file written before entries had `follow_copy` / `gone` and
/// before trip folders' clips were recorded still loads; its folders learn
/// their clips on the next look and are still left alone.
#[tokio::test]
async fn older_state_files_still_load_and_learn_their_folders_clips() {
    let dir = tmp("legacy");
    let footage = dir.join("footage");
    put(&footage, "Alps/GX010001.MP4", b"1");
    let unit = scan::find_units(std::slice::from_ref(&footage)).remove(0);
    let file = WatchFile {
        baseline_taken: true,
        enabled: Some(true),
        entries: vec![WatchEntry::new(&unit, "", &footage, EntryStatus::Baseline)],
        ..Default::default()
    };
    let mut json = serde_json::to_value(&file).unwrap();
    for e in json["entries"].as_array_mut().unwrap() {
        let e = e.as_object_mut().unwrap();
        e.remove("follow_copy");
        e.remove("gone");
    }
    let state = dir.join("projects").join(STATE_FILE);
    std::fs::write(&state, json.to_string()).unwrap();
    let jobs = JobRegistry::new();
    let maker = FakeMaker::new(&jobs, &dir);
    let svc = WatchService::new(settings(&dir), jobs);
    let t0 = Instant::now();
    svc.tick(t0).await;
    svc.tick(t0 + Duration::from_secs(20)).await;
    assert!(!svc.work_once(&maker).await);
    assert!(entries(&svc).await.is_empty());
    let back = WatchFile::load(&state);
    assert_eq!(back.entries.len(), 1);
    assert_eq!(back.entries[0].content_id, snapshot(&footage.join("Alps")).content_id());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A card still being copied is one entry (one row in the UI) from its
/// first clip until it settles, however its clips change meanwhile.
#[tokio::test]
async fn a_card_being_copied_is_one_entry_until_it_settles() {
    let dir = tmp("cardcopy");
    let cards = dir.join("footage");
    let jobs = JobRegistry::new();
    let maker = FakeMaker::new(&jobs, &dir);
    let svc = WatchService::new(settings(&dir), jobs);
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    svc.tick(at(0)).await;
    let mut rx = svc.subscribe();
    put(&cards, "HERO12/DCIM/100GOPRO/GX010001.MP4", b"1");
    svc.tick(at(1)).await;
    let first = entry(&svc, "HERO12").await;
    assert_eq!((first.kind, first.status), (UnitKind::Card, EntryStatus::Waiting));
    for (t, clip) in [(3, "GX010002.MP4"), (5, "GX010003.MP4"), (7, "GX010004.MP4")] {
        put(&cards, &format!("HERO12/DCIM/100GOPRO/{clip}"), b"22");
        svc.tick(at(t)).await;
        let all = entries(&svc).await;
        assert_eq!(all.len(), 1, "{all:?}");
        assert_eq!((all[0].id.as_str(), all[0].status), (first.id.as_str(), EntryStatus::Waiting));
    }
    svc.tick(at(20)).await;
    let e = entry(&svc, "HERO12").await;
    assert_eq!((e.id, e.status, e.n_videos), (first.id.clone(), EntryStatus::Queued, 4));
    assert!(drain(&mut rx).iter().all(|ev| !matches!(ev, WatchEvent::Removed { .. })), "the row left and came back");
    assert!(svc.work_once(&maker).await);
    // Handled with all four clips: looking again changes nothing.
    svc.tick(at(30)).await;
    svc.tick(at(50)).await;
    assert!(!svc.work_once(&maker).await);
    assert_eq!(maker.proposed(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// "Don't make a reel of this" on a card that is still being copied holds
/// for the whole copy; footage added after it settled is new again.
#[tokio::test]
async fn a_card_dismissed_while_copying_is_not_rendered_when_the_copy_ends() {
    let dir = tmp("carddismiss");
    let cards = dir.join("footage");
    let jobs = JobRegistry::new();
    let maker = FakeMaker::new(&jobs, &dir);
    let svc = WatchService::new(settings(&dir), jobs);
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    svc.tick(at(0)).await;
    put(&cards, "HERO12 Ski/DCIM/100GOPRO/GX010001.MP4", b"1");
    svc.tick(at(1)).await;
    let e = entry(&svc, "HERO12 Ski").await;
    assert_eq!(e.status, EntryStatus::Waiting);
    let gone = svc.dismiss(&e.id).await.unwrap().unwrap();
    assert_eq!((gone.status, gone.dismissed), (EntryStatus::Skipped, true));
    let mut rx = svc.subscribe();
    // The copy goes on: more clips, a clip still growing.
    put(&cards, "HERO12 Ski/DCIM/100GOPRO/GX010002.MP4", b"2");
    svc.tick(at(3)).await;
    put(&cards, "HERO12 Ski/DCIM/100GOPRO/GX010002.MP4", b"2222");
    put(&cards, "HERO12 Ski/DCIM/100GOPRO/GX010003.MP4", b"3");
    svc.tick(at(5)).await;
    for t in [20, 40, 60] {
        svc.tick(at(t)).await;
    }
    assert!(!svc.work_once(&maker).await, "a dismissed card was rendered");
    assert_eq!(maker.proposed(), 0);
    assert!(entries(&svc).await.is_empty());
    assert!(drain(&mut rx).is_empty(), "a dismissed card came back to the list");
    // A restart (state file only) keeps it dismissed.
    let svc = WatchService::new(settings(&dir), JobRegistry::new());
    svc.tick(at(70)).await;
    svc.tick(at(90)).await;
    assert!(!svc.work_once(&maker).await);
    // After the copy settled, new footage on the card is a new reel.
    put(&cards, "HERO12 Ski/DCIM/100GOPRO/GX010004.MP4", b"4");
    svc.tick(at(100)).await;
    svc.tick(at(120)).await;
    assert_eq!(entry(&svc, "HERO12 Ski").await.status, EntryStatus::Queued);

    // Taken back ("Try again") while its copy is still going: it waits for
    // the copy to finish again rather than rendering half a card.
    put(&cards, "Mini 4/DCIM/100MEDIA/DJI_0001.MP4", b"1");
    svc.tick(at(130)).await;
    let dji = entry(&svc, "Mini 4").await;
    svc.dismiss(&dji.id).await.unwrap().unwrap();
    put(&cards, "Mini 4/DCIM/100MEDIA/DJI_0002.MP4", b"2");
    svc.tick(at(132)).await;
    let back = svc.retry(&dji.id).await.unwrap().unwrap();
    assert_eq!((back.status, back.dismissed), (EntryStatus::Waiting, false));
    svc.tick(at(134)).await;
    assert_eq!(entry(&svc, "Mini 4").await.status, EntryStatus::Waiting);
    svc.tick(at(150)).await;
    let dji = entry(&svc, "Mini 4").await;
    assert_eq!((dji.status, dji.n_videos), (EntryStatus::Queued, 2));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn off_means_no_looks_and_no_renders_and_the_toggle_is_remembered() {
    let dir = tmp("toggle");
    let footage = dir.join("footage");
    let mut s = settings(&dir);
    s.enabled_by_default = false;
    let jobs = JobRegistry::new();
    let maker = FakeMaker::new(&jobs, &dir);
    let svc = WatchService::new(s.clone(), jobs.clone());
    assert!(!svc.enabled().await);
    let t0 = Instant::now();
    put(&footage, "Alps/GX010001.MP4", b"1");
    svc.tick(t0).await;
    // Off: not even the baseline.
    assert!(!WatchFile::load(&dir.join("projects").join(STATE_FILE)).baseline_taken);
    let mut rx = svc.subscribe();
    let status = svc.set_enabled(true).await;
    assert_eq!(status["enabled"], true);
    assert!(matches!(drain(&mut rx)[..], [WatchEvent::Status { enabled: true }]));
    svc.tick(t0 + Duration::from_secs(1)).await; // baseline: Alps was already here
    put(&footage, "Beach/GX010001.MP4", b"1");
    svc.tick(t0 + Duration::from_secs(2)).await;
    svc.tick(t0 + Duration::from_secs(13)).await;
    assert_eq!(entry(&svc, "Beach").await.status, EntryStatus::Queued);
    // Switched off with a folder queued: it waits.
    svc.set_enabled(false).await;
    assert!(!svc.work_once(&maker).await);
    assert_eq!(maker.proposed(), 0);
    // The switch outlives a restart, over the environment's default.
    let mut on_by_env = s.clone();
    on_by_env.enabled_by_default = true;
    let again = WatchService::new(on_by_env, jobs.clone());
    assert!(!again.enabled().await);
    again.set_enabled(true).await;
    assert!(again.work_once(&maker).await);
    assert_eq!(entry(&again, "Beach").await.status, EntryStatus::Done);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn watched_reels_wait_for_renders_already_running() {
    let dir = tmp("quiet");
    let jobs = JobRegistry::new();
    let maker = FakeMaker::new(&jobs, &dir);
    let svc = WatchService::new(settings(&dir), jobs.clone());
    let t0 = Instant::now();
    svc.tick(t0).await;
    put(&dir.join("footage"), "Alps/GX010001.MP4", b"1");
    svc.tick(t0 + Duration::from_secs(1)).await;
    svc.tick(t0 + Duration::from_secs(12)).await;
    // Someone else's render is running.
    let theirs = jobs.submit_for("/projects/theirs.toml").await;
    assert!(jobs.begin(&theirs, "t0").await);
    let worker = {
        let (svc, maker) = (svc.clone(), maker.clone());
        tokio::spawn(async move { svc.work_once(&maker).await })
    };
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(maker.proposed(), 0, "started next to a running render");
    assert_eq!(entry(&svc, "Alps").await.status, EntryStatus::Queued);
    jobs.set(&theirs, JobState::Cancelled { started_at: Some("t0".into()), finished_at: "t1".into(), elapsed_ms: 1 })
        .await;
    assert!(worker.await.unwrap());
    assert_eq!((maker.proposed(), entry(&svc, "Alps").await.status), (1, EntryStatus::Done));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn failures_and_stops_are_reported_and_can_be_retried_or_dismissed() {
    let dir = tmp("fail");
    let footage = dir.join("footage");
    let jobs = JobRegistry::new();
    let svc = WatchService::new(settings(&dir), jobs.clone());
    let t0 = Instant::now();
    svc.tick(t0).await;
    for name in ["A", "B", "C"] {
        put(&footage, &format!("{name}/GX010001.MP4"), b"1");
    }
    svc.tick(t0 + Duration::from_secs(1)).await;
    svc.tick(t0 + Duration::from_secs(12)).await;

    let mut no_clips = FakeMaker::new(&jobs, &dir);
    no_clips.propose_error = Some("No clips Vibecut can read in this folder.");
    assert!(svc.work_once(&no_clips).await);
    let mut broken = FakeMaker::new(&jobs, &dir);
    broken.outcome = Outcome::Fail;
    assert!(svc.work_once(&broken).await);
    let mut stopped = FakeMaker::new(&jobs, &dir);
    stopped.outcome = Outcome::Stop;
    assert!(svc.work_once(&stopped).await);
    let by_status = |es: &[WatchEntry], s: EntryStatus| es.iter().filter(|e| e.status == s).count();
    let all = entries(&svc).await;
    assert_eq!((by_status(&all, EntryStatus::Failed), by_status(&all, EntryStatus::Stopped)), (2, 1), "{all:?}");
    let notes: Vec<&str> = all.iter().filter_map(|e| e.note.as_deref()).collect();
    assert!(notes.contains(&"No clips Vibecut can read in this folder."));
    assert!(notes.contains(&"ffmpeg failed"));

    // Retry the stopped one; dismiss a failed one.
    let stop_id = all.iter().find(|e| e.status == EntryStatus::Stopped).unwrap().id.clone();
    let fail_id = all.iter().find(|e| e.status == EntryStatus::Failed).unwrap().id.clone();
    assert_eq!(svc.retry(&stop_id).await.unwrap().unwrap().status, EntryStatus::Queued);
    assert!(svc.retry(&stop_id).await.is_err(), "a queued folder was retried");
    assert!(svc.work_once(&FakeMaker::new(&jobs, &dir)).await);
    svc.dismiss(&fail_id).await.unwrap().unwrap();
    let all = entries(&svc).await;
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|e| e.id != fail_id));
    assert!(svc.dismiss("w_nope").await.unwrap().is_none());
    // A dismissed folder stays handled.
    svc.tick(t0 + Duration::from_secs(40)).await;
    assert!(!svc.work_once(&FakeMaker::new(&jobs, &dir)).await);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_dismissed_waiting_folder_is_never_rendered() {
    let dir = tmp("dismiss");
    let jobs = JobRegistry::new();
    let maker = FakeMaker::new(&jobs, &dir);
    let svc = WatchService::new(settings(&dir), jobs);
    let t0 = Instant::now();
    svc.tick(t0).await;
    put(&dir.join("footage"), "Junk/GX010001.MP4", b"1");
    svc.tick(t0 + Duration::from_secs(1)).await;
    let e = entry(&svc, "Junk").await;
    assert_eq!(e.status, EntryStatus::Waiting);
    svc.dismiss(&e.id).await.unwrap();
    svc.tick(t0 + Duration::from_secs(20)).await;
    svc.tick(t0 + Duration::from_secs(40)).await;
    assert!(!svc.work_once(&maker).await);
    assert!(entries(&svc).await.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A copy that disappears before it settles (a card pulled mid-copy)
/// leaves the list; empty folders never join it.
#[tokio::test]
async fn vanished_and_empty_folders_leave_no_entry() {
    let dir = tmp("vanish");
    let footage = dir.join("footage");
    let svc = WatchService::new(settings(&dir), JobRegistry::new());
    let mut rx = svc.subscribe();
    let t0 = Instant::now();
    svc.tick(t0).await;
    std::fs::create_dir_all(footage.join("New folder")).unwrap();
    put(&footage, "Photos/a.jpg", b"1");
    put(&footage, "Partial/GX010001.MP4", b"1");
    svc.tick(t0 + Duration::from_secs(1)).await;
    let listed: Vec<String> = entries(&svc).await.into_iter().map(|e| e.name).collect();
    assert_eq!(listed, ["Partial"]);
    std::fs::remove_dir_all(footage.join("Partial")).unwrap();
    svc.tick(t0 + Duration::from_secs(2)).await;
    assert!(entries(&svc).await.is_empty());
    assert!(drain(&mut rx).iter().any(|e| matches!(e, WatchEvent::Removed { .. })));
    let _ = std::fs::remove_dir_all(&dir);
}

/// No watched folder there yet (a mount missing at first start): no
/// baseline is taken, so what is there once it appears counts as already
/// there, not as new footage.
#[tokio::test]
async fn the_baseline_waits_for_a_watched_folder_to_exist() {
    let dir = tmp("noroot");
    let footage = dir.join("footage");
    std::fs::remove_dir_all(&footage).unwrap();
    let jobs = JobRegistry::new();
    let maker = FakeMaker::new(&jobs, &dir);
    let svc = WatchService::new(settings(&dir), jobs);
    let state = dir.join("projects").join(STATE_FILE);
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    svc.tick(at(0)).await;
    assert!(!WatchFile::load(&state).baseline_taken);
    // The mount is fixed: the library is there, and it is left alone.
    for name in ["Alps", "Beach", "Coastline"] {
        put(&footage, &format!("{name}/GX010001.MP4"), b"1");
    }
    for s in [1, 12, 30] {
        svc.tick(at(s)).await;
    }
    assert!(WatchFile::load(&state).baseline_taken);
    assert!(entries(&svc).await.is_empty());
    assert!(!svc.work_once(&maker).await);
    // Footage that turns up after that is new.
    put(&footage, "Dunes/GX010001.MP4", b"1");
    svc.tick(at(31)).await;
    svc.tick(at(45)).await;
    assert_eq!(entry(&svc, "Dunes").await.status, EntryStatus::Queued);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The footage folder was empty at the baseline (a bind mount that pointed
/// at nothing), then a whole library turns up in one look: it is taken as
/// already there, not rendered folder by folder. A few at a time (a copy)
/// are still new footage.
#[tokio::test]
async fn a_library_turning_up_at_once_is_taken_as_already_there() {
    let dir = tmp("library");
    let footage = dir.join("footage");
    let jobs = JobRegistry::new();
    let maker = FakeMaker::new(&jobs, &dir);
    let svc = WatchService::new(settings(&dir), jobs);
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    svc.tick(at(0)).await;
    for i in 1..=MAX_NEW_AT_ONCE + 1 {
        put(&footage, &format!("Trip {i}/GX010001.MP4"), b"1");
    }
    for s in [1, 12, 30] {
        svc.tick(at(s)).await;
    }
    assert!(entries(&svc).await.is_empty());
    assert!(!svc.work_once(&maker).await);
    let file = WatchFile::load(&dir.join("projects").join(STATE_FILE));
    assert_eq!(file.entries.iter().filter(|e| e.status == EntryStatus::Baseline).count(), MAX_NEW_AT_ONCE + 1);
    // As many as MAX_NEW_AT_ONCE at once is still a copy.
    for i in 1..=MAX_NEW_AT_ONCE {
        put(&footage, &format!("New {i}/GX010001.MP4"), b"1");
    }
    svc.tick(at(31)).await;
    svc.tick(at(45)).await;
    let all = entries(&svc).await;
    assert_eq!(all.len(), MAX_NEW_AT_ONCE);
    assert!(all.iter().all(|e| e.status == EntryStatus::Queued), "{all:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A trip folder is handled once: clips added to it later make no new
/// reel. But a folder deleted and made again with other clips (a recurring
/// "Import" folder) is new footage — even though the watcher's own project
/// for the old one names the same folder — while the same clips moved away
/// and back are not. An empty folder at the baseline is not footage yet.
#[tokio::test]
async fn a_folder_made_again_with_other_clips_is_new_footage() {
    let dir = tmp("import");
    let footage = dir.join("footage");
    put(&footage, "GoPro/GX010001.MP4", b"old");
    std::fs::create_dir_all(footage.join("Import")).unwrap();
    let jobs = JobRegistry::new();
    let maker = FakeMaker::new(&jobs, &dir);
    let svc = WatchService::new(settings(&dir), jobs);
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    svc.tick(at(0)).await;
    // More clips in a folder that was already there: still handled.
    put(&footage, "GoPro/GX010002.MP4", b"more");
    svc.tick(at(1)).await;
    svc.tick(at(20)).await;
    assert!(entries(&svc).await.is_empty());

    // The week's card copied into the (empty at the baseline) Import folder.
    put(&footage, "Import/GX010001.MP4", b"week 1");
    svc.tick(at(21)).await;
    svc.tick(at(40)).await;
    assert!(svc.work_once(&maker).await);
    let first = entry(&svc, "Import").await;
    assert_eq!(first.status, EntryStatus::Done);

    // Moved away and back with the same clips: nothing new.
    std::fs::rename(footage.join("Import"), dir.join("Import")).unwrap();
    svc.tick(at(41)).await;
    std::fs::rename(dir.join("Import"), footage.join("Import")).unwrap();
    svc.tick(at(42)).await;
    svc.tick(at(60)).await;
    assert!(!svc.work_once(&maker).await);
    assert_eq!(entries(&svc).await.len(), 1);

    // Archived and deleted; next week's card copied into a new Import.
    std::fs::remove_dir_all(footage.join("Import")).unwrap();
    svc.tick(at(61)).await;
    put(&footage, "Import/GX010001.MP4", b"week 2!");
    svc.tick(at(62)).await;
    svc.tick(at(80)).await;
    let all = entries(&svc).await;
    assert_eq!(all.len(), 2, "{all:?}");
    let second = all.iter().find(|e| e.id != first.id).unwrap().clone();
    assert_eq!((second.status, second.note.as_deref()), (EntryStatus::Queued, None));
    assert!(svc.work_once(&maker).await);
    assert_eq!(entry(&svc, "Import").await.status, EntryStatus::Done);
    assert_eq!(maker.proposed(), 2);

    // A restart makes neither again.
    let jobs = JobRegistry::new();
    let maker = FakeMaker::new(&jobs, &dir);
    let svc = WatchService::new(settings(&dir), jobs);
    svc.tick(at(100)).await;
    svc.tick(at(120)).await;
    assert!(!svc.work_once(&maker).await);
    let all = entries(&svc).await;
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|e| e.status == EntryStatus::Done));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn settings_come_from_the_environment() {
    let vars = |pairs: &'static [(&'static str, &'static str)]| {
        move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    };
    let s = WatchSettings::from_vars(vars(&[]));
    assert!(!s.enabled_by_default);
    assert_eq!(s.roots, [PathBuf::from("/footage")]);
    assert_eq!(s.state_path.as_deref(), Some(Path::new("/projects/.vibecut-watch.json")));
    assert_eq!((s.poll, s.settle), (Duration::from_secs(7), Duration::from_secs(20)));
    let s = WatchSettings::from_vars(vars(&[
        ("VIBECUT_WATCH", "On"),
        ("VIBECUT_WATCH_DIRS", "/cards:relative:/footage:/media/usb"),
        ("VIBECUT_WATCH_INTERVAL", "3s"),
        ("VIBECUT_WATCH_SETTLE", "0"),
        ("VIBECUT_WATCH_VIBE", "action_highlight"),
        ("VIBECUT_WATCH_LENGTH", " 45s "),
    ]));
    assert!(s.enabled_by_default);
    let roots: Vec<&str> = s.roots.iter().map(|r| r.to_str().unwrap()).collect();
    assert_eq!(roots, ["/footage", "/cards", "/media/usb"]);
    assert_eq!((s.poll, s.settle), (Duration::from_secs(3), Duration::ZERO));
    assert_eq!((s.vibe.as_deref(), s.target_duration.as_deref()), (Some("action_highlight"), Some("45s")));
    let s = WatchSettings::from_vars(vars(&[("VIBECUT_WATCH", "0"), ("VIBECUT_WATCH_VIBE", "nope"), ("VIBECUT_WATCH_INTERVAL", "0")]));
    assert!(!s.enabled_by_default);
    assert_eq!((s.vibe, s.poll), (None, Duration::from_secs(1)));
    // compose.yml passes every variable through, empty when unset: the
    // defaults apply.
    let s = WatchSettings::from_vars(vars(&[
        ("VIBECUT_WATCH", ""),
        ("VIBECUT_WATCH_DIRS", ""),
        ("VIBECUT_WATCH_INTERVAL", ""),
        ("VIBECUT_WATCH_SETTLE", ""),
        ("VIBECUT_WATCH_VIBE", ""),
        ("VIBECUT_WATCH_LENGTH", ""),
    ]));
    assert!(!s.enabled_by_default);
    assert_eq!(s.roots, [PathBuf::from("/footage")]);
    assert_eq!((s.poll, s.settle), (Duration::from_secs(7), Duration::from_secs(20)));
    assert_eq!((s.vibe, s.target_duration), (None, None));
}

#[tokio::test]
async fn the_default_service_is_inert() {
    let svc = WatchService::default();
    let st = svc.status_json().await;
    assert_eq!((st["enabled"].as_bool(), st["available"].as_bool()), (Some(false), Some(false)));
    assert!(st["entries"].as_array().unwrap().is_empty());
    svc.tick(Instant::now()).await;
    assert!(!svc.work_once(&FakeMaker::new(&JobRegistry::new(), Path::new("/nonexistent"))).await);
}
