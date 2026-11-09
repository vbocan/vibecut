use super::*;

/// A fresh temp folder for one test.
fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("vibecut_browse_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn touch(p: &Path) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, b"x").unwrap();
}

fn is_invalid(r: &Result<Resolved, OpsError>) -> bool {
    matches!(r, Err(OpsError::InvalidInput { .. }))
}

fn is_not_found(r: &Result<Resolved, OpsError>) -> bool {
    matches!(r, Err(OpsError::NotFound { .. }))
}

/// Roots on paths that need not exist: Footage, C:, D: and a posix
/// folder shown under another name.
fn paper_roots() -> Vec<BrowseRoot> {
    let drive = |l: &str| BrowseRoot {
        label: format!("{}:", l.to_uppercase()),
        container: PathBuf::from(format!("/host/{l}")),
        display: format!("{}:\\", l.to_uppercase()),
        kind: RootKind::Drive,
    };
    vec![
        BrowseRoot { label: "Footage".into(), container: "/footage".into(), display: "/footage".into(), kind: RootKind::Footage },
        drive("c"),
        drive("d"),
        BrowseRoot { label: "Home".into(), container: "/host/home".into(), display: "/home".into(), kind: RootKind::Folder },
    ]
}

#[test]
fn auto_detects_drive_letters_and_hides_docker_desktop_folders() {
    let base = temp("auto");
    let host = base.join("host");
    for d in ["c", "d", "e", "wsl", "wslg", "docker"] {
        std::fs::create_dir_all(host.join(d)).unwrap();
    }
    touch(&host.join("f")); // a file named like a drive is not one
    let footage = base.join("footage");
    let roots = roots_from(None, &footage, &host);
    let labels: Vec<_> = roots.iter().map(|r| (r.label.as_str(), r.display.as_str(), r.kind)).collect();
    assert_eq!(
        labels,
        vec![
            ("Footage", footage.to_str().unwrap(), RootKind::Footage),
            ("C:", "C:\\", RootKind::Drive),
            ("D:", "D:\\", RootKind::Drive),
            ("E:", "E:\\", RootKind::Drive),
        ]
    );
    assert_eq!(roots[2].container, host.join("d"));
    // `auto` spelled out is the same; `off` leaves only the footage mount.
    assert_eq!(roots_from(Some(" auto "), &footage, &host), roots);
    assert_eq!(roots_from(Some("off"), &footage, &host).len(), 1);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn host_without_drive_letters_is_one_computer_root_and_a_missing_host_none() {
    let base = temp("linuxhost");
    let host = base.join("host");
    std::fs::create_dir_all(host.join("home")).unwrap();
    std::fs::create_dir_all(host.join("media")).unwrap();
    let roots = roots_from(None, Path::new("/footage"), &host);
    assert_eq!(roots.len(), 2);
    assert_eq!((roots[1].label.as_str(), roots[1].display.as_str()), ("Computer", "/"));
    assert_eq!(roots[1].container, host);
    // No /host at all (the default compose on Linux, or a plain docker run).
    let roots = roots_from(None, Path::new("/footage"), &base.join("absent"));
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].label, "Footage");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn parses_configured_roots_and_ignores_bad_entries() {
    let absent = Path::new("/definitely/not/here");
    let roots = roots_from(
        Some("Card=/media/card; /mnt/nas/ ;Home=/host/home=/home;bad;rel/path;X=/a/../b;=/;/;Y=/ok=;/media/card;off"),
        Path::new("/footage"),
        absent,
    );
    let got: Vec<_> = roots.iter().map(|r| (r.label.as_str(), r.container.to_str().unwrap(), r.display.as_str())).collect();
    assert_eq!(
        got,
        vec![
            ("Footage", "/footage", "/footage"),
            ("Card", "/media/card", "/media/card"),
            ("nas", "/mnt/nas", "/mnt/nas"),
            ("Home", "/host/home", "/home"),
        ]
    );
    assert!(roots[1..].iter().all(|r| r.kind == RootKind::Folder));
    // A Windows display path given by hand behaves like a drive's.
    let roots = roots_from(Some("Card=/media/card=E:\\Card"), Path::new("/footage"), absent);
    assert!(roots[1].windows());
    assert_eq!(roots[1].display_of(&["DCIM".into()]), "E:\\Card\\DCIM");
}

#[test]
fn display_and_container_paths_map_both_ways() {
    let roots = paper_roots();
    let c = |s: &str| display_to_container(&roots, s).map(|p| p.to_string_lossy().into_owned());
    assert_eq!(c("D:\\GoPro\\Alps").as_deref(), Some("/host/d/GoPro/Alps"));
    assert_eq!(c("d:/GoPro/Alps/").as_deref(), Some("/host/d/GoPro/Alps"));
    assert_eq!(c("D:").as_deref(), Some("/host/d"));
    assert_eq!(c("D:\\").as_deref(), Some("/host/d"));
    // The drive letter is any case; the rest is kept as typed.
    assert_eq!(c("c:\\USERS\\Me").as_deref(), Some("/host/c/USERS/Me"));
    assert_eq!(c("C:\\a\\.\\b").as_deref(), Some("/host/c/a/b"));
    assert_eq!(c("/host/d/GoPro").as_deref(), Some("/host/d/GoPro"));
    assert_eq!(c("/footage/trip").as_deref(), Some("/footage/trip"));
    assert_eq!(c("/home/me/Videos").as_deref(), Some("/host/home/me/Videos"));
    // A deeper root wins over a shallower one; container paths over display paths.
    assert_eq!(c("/host/home/me").as_deref(), Some("/host/home/me"));
    for bad in ["E:\\x", "C:\\a\\..\\..\\etc", "/etc/passwd", "trip", "\\\\nas\\share", "C:x", "C:\\a\0b", ""] {
        assert_eq!(c(bad), None, "{bad:?}");
    }

    let d = |s: &str| container_to_display(&roots, s);
    assert_eq!(d("/host/d/GoPro/Alps"), "D:\\GoPro\\Alps");
    assert_eq!(d("/host/c"), "C:\\");
    assert_eq!(d("/footage/x"), "/footage/x");
    assert_eq!(d("/host/home/me"), "/home/me");
    assert_eq!(d("/elsewhere/x"), "/elsewhere/x");
    // Round trip.
    for p in ["D:\\GoPro\\Alps", "C:\\", "/footage/a b/c", "/home/me"] {
        assert_eq!(d(&c(p).unwrap()), p);
    }
}

/// C: and Footage on real temp folders.
fn real_roots(base: &Path) -> Vec<BrowseRoot> {
    let host = base.join("host");
    std::fs::create_dir_all(host.join("c")).unwrap();
    let footage = base.join("footage");
    std::fs::create_dir_all(&footage).unwrap();
    roots_from(None, &footage, &host)
}

#[test]
fn resolve_rejects_traversal_nul_outside_and_hidden() {
    let base = temp("safety");
    let roots = real_roots(&base);
    let c = base.join("host/c");
    std::fs::create_dir_all(c.join("GoPro/Alps")).unwrap();
    std::fs::create_dir_all(c.join("$RECYCLE.BIN/S-1")).unwrap();
    std::fs::create_dir_all(c.join(".git")).unwrap();
    std::fs::create_dir_all(base.join("secret")).unwrap();
    touch(&c.join("file.txt"));

    let ok = resolve_in(&roots, "C:\\GoPro\\Alps").unwrap();
    assert_eq!(ok, Resolved { root: 1, rel: vec!["GoPro".into(), "Alps".into()] });
    assert_eq!(resolve_in(&roots, "c:/gopro/../GoPro").ok(), None, "`..` is refused even when it lands inside");
    let container = format!("{}/GoPro", c.display());
    assert_eq!(resolve_in(&roots, &container).unwrap().rel, vec!["GoPro".to_string()]);
    assert_eq!(resolve_in(&roots, "C:\\").unwrap().rel, Vec::<String>::new());

    for bad in [
        "C:\\..",
        "C:\\GoPro\\..\\..\\secret",
        &format!("{}/../../secret", c.display()),
        "C:\\GoPro\0",
        "/etc",
        "GoPro",
        "",
        "C:\\$RECYCLE.BIN",
        "C:\\$RECYCLE.BIN\\S-1",
        "C:\\.git",
        "C:\\file.txt",
        "Z:\\GoPro",
    ] {
        let r = resolve_in(&roots, bad);
        assert!(is_invalid(&r), "{bad:?} → {r:?}");
    }
    assert!(is_invalid(&resolve_in(&roots, &"a/".repeat(3000))));
    assert!(is_not_found(&resolve_in(&roots, "C:\\Nope")));
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn symlinks_out_of_a_root_are_refused_and_never_listed() {
    let base = temp("symlink");
    let roots = real_roots(&base);
    let c = base.join("host/c");
    std::fs::create_dir_all(base.join("secret/inner")).unwrap();
    std::fs::create_dir_all(c.join("Real")).unwrap();
    std::os::unix::fs::symlink(base.join("secret"), c.join("Escape")).unwrap();
    std::os::unix::fs::symlink(c.join("Real"), c.join("Inside")).unwrap();
    for bad in ["C:\\Escape", "C:\\Escape\\inner"] {
        let r = resolve_in(&roots, bad);
        assert!(is_invalid(&r), "{bad} → {r:?}");
    }
    // A link that stays inside resolves to where it points.
    assert_eq!(resolve_in(&roots, "C:\\Inside").unwrap().rel, vec!["Real".to_string()]);
    let listing = browse_in(&roots, "C:\\", TIME_BUDGET).unwrap();
    let names: Vec<_> = listing.folders.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["Real"]);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn listing_counts_videos_hides_system_folders_and_shows_display_paths() {
    let base = temp("listing");
    let roots = real_roots(&base);
    let c = base.join("host/c");
    let trip = c.join("Users/me/GoPro/Alps");
    for f in ["GX010001.MP4", "GX020001.MP4", "GL010001.LRV", "GX010001.THM", "GL010002.MP4", "phone.mov", "notes.txt"] {
        touch(&trip.join(f));
    }
    std::fs::create_dir_all(trip.join("day2")).unwrap();
    touch(&c.join("Users/me/GoPro/Coast/Card A/DCIM/GX010009.MP4"));
    for hidden in ["$RECYCLE.BIN", "System Volume Information", "wsl", "wslg", "node_modules", ".git", "lost+found"] {
        std::fs::create_dir_all(c.join("Users/me/GoPro").join(hidden)).unwrap();
    }

    let l = browse_in(&roots, "c:\\users\\..\\Users\\me", TIME_BUDGET);
    assert!(l.is_err(), "`..` again");
    let l = browse_in(&roots, "C:\\Users\\me\\GoPro", TIME_BUDGET).unwrap();
    assert_eq!(l.path, "C:\\Users\\me\\GoPro");
    assert_eq!(l.container_path, c.join("Users/me/GoPro").to_string_lossy());
    assert_eq!(l.name, "GoPro");
    assert_eq!(l.root, "C:");
    assert_eq!(l.parent.as_deref(), Some("C:\\Users\\me"));
    let crumbs: Vec<_> = l.crumbs.iter().map(|c| (c.name.as_str(), c.path.as_str())).collect();
    assert_eq!(
        crumbs,
        vec![("C:", "C:\\"), ("Users", "C:\\Users"), ("me", "C:\\Users\\me"), ("GoPro", "C:\\Users\\me\\GoPro")]
    );
    assert!(l.can_use);
    assert_eq!(l.n_videos, 0);
    assert!(!l.truncated);
    let names: Vec<_> = l.folders.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["Alps", "Coast"]);
    let alps = &l.folders[0];
    assert_eq!(alps.path, "C:\\Users\\me\\GoPro\\Alps");
    assert_eq!(alps.container_path, trip.to_string_lossy());
    assert_eq!(alps.n_videos, 3, "two GoPro chapters and a phone clip; proxies and thumbnails don't count");
    assert!(alps.has_subfolders);
    assert!(alps.complete);
    assert_eq!(alps.sample_video.as_deref(), Some(trip.join("GX010001.MP4").to_str().unwrap()));
    let coast = &l.folders[1];
    assert_eq!((coast.n_videos, coast.has_subfolders, coast.sample_video.as_deref()), (0, true, None));

    // Inside the trip: its own videos.
    let l = browse_in(&roots, &trip.to_string_lossy(), TIME_BUDGET).unwrap();
    assert_eq!((l.n_videos, l.path.as_str()), (3, "C:\\Users\\me\\GoPro\\Alps"));
    // A whole drive can be browsed but not used; the footage mount can.
    let l = browse_in(&roots, "C:\\", TIME_BUDGET).unwrap();
    assert!(!l.can_use);
    assert_eq!(l.parent, None);
    let footage = roots[0].display.clone();
    assert!(browse_in(&roots, &footage, TIME_BUDGET).unwrap().can_use);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn huge_folders_are_capped_and_slow_ones_left_unscanned() {
    let base = temp("caps");
    let roots = real_roots(&base);
    let big = base.join("host/c/big");
    for i in 0..MAX_FOLDERS + 3 {
        std::fs::create_dir_all(big.join(format!("f{i:04}"))).unwrap();
    }
    touch(&big.join("f0000/GX010001.MP4"));
    let l = browse_in(&roots, "C:\\big", TIME_BUDGET).unwrap();
    assert_eq!(l.folders.len(), MAX_FOLDERS);
    assert!(l.truncated);
    assert_eq!(l.folders[0].n_videos, 1);
    // No time left: names only, marked incomplete.
    let l = browse_in(&roots, "C:\\big", Duration::ZERO).unwrap();
    assert!(l.folders.iter().all(|f| !f.complete && f.n_videos == 0));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn whole_drives_are_not_footage_folders() {
    let roots = paper_roots();
    for bad in ["/host/c", "/host/d/", "/host", "/"] {
        assert!(check_footage_folder_in(&roots, Path::new(bad)).is_err(), "{bad}");
    }
    for ok in ["/host/c/GoPro", "/footage", "/host/home", "/elsewhere"] {
        assert!(check_footage_folder_in(&roots, Path::new(ok)).is_ok(), "{ok}");
    }
}

#[test]
fn whole_drives_are_refused_however_the_path_is_spelled() {
    let base = temp("whole");
    let roots = real_roots(&base);
    let c = base.join("host/c");
    std::fs::create_dir_all(c.join("Trip")).unwrap();
    for bad in [c.clone(), c.join("Trip/.."), base.join("host"), c.join("..")] {
        assert!(check_footage_folder_in(&roots, &bad).is_err(), "{bad:?}");
    }
    assert!(check_footage_folder_in(&roots, &c.join("Trip")).is_ok());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn hidden_names() {
    for h in [".git", ".Trash-1000", "$RECYCLE.BIN", "$WinREAgent", "System Volume Information", "WSL", "wslg", "node_modules", "lost+found", "@eaDir"] {
        assert!(is_hidden_name(h), "{h}");
    }
    for v in ["GoPro", "DCIM", "100GOPRO", "Users", "wslconfig-backup", "my.trip"] {
        assert!(!is_hidden_name(v), "{v}");
    }
}
