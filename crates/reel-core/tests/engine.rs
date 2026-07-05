//! Headless engine tests — the Rust port of the original script's smoke tests.

use reel_core::config::Config;
use reel_core::ledger::Ledger;
use reel_core::media::{captured_at, fileid_of, kind_of, Kind};
use reel_core::model::{ClipRef, TripState};
use reel_core::sessions::{cluster_sessions, ClipRec};
use reel_core::{list_trips, scan_card};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

fn touch(path: &Path, fill: u8, len: usize, epoch: i64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![fill; len]).unwrap();
    let f = File::options().write(true).open(path).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(epoch as u64))
        .unwrap();
}

fn cfg(lib: &Path, state: &Path, dji_sd: Option<PathBuf>) -> Config {
    Config {
        lib: lib.to_path_buf(),
        remote: "test:pool".into(),
        user: "jaeho".into(),
        state_dir: state.to_path_buf(),
        cache_dir: state.join("cache"),
        session_gap: 21600,
        dji_sd,
        gopro_sd: None,
        media_user: "jaeho".into(),
    }
}

#[test]
fn classifies_cameras() {
    assert_eq!(kind_of(Path::new("/x/DJI_0001.MP4")), Kind::Dji);
    assert_eq!(kind_of(Path::new("/x/GX010001.MP4")), Kind::Gopro);
    assert_eq!(kind_of(Path::new("/x/GL010001.LRV")), Kind::Gopro);
    assert_eq!(kind_of(Path::new("/x/IMG_1234.MOV")), Kind::Iphone);
    assert_eq!(kind_of(Path::new("/x/random.mp4")), Kind::Misc);
}

#[test]
fn fileid_is_stable_and_content_addressed() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("a.mp4");
    let b = d.path().join("b.mp4");
    // > 4 MiB exercises the head+tail seek path.
    touch(&a, 0xAB, 5_000_000, 1_700_000_000);
    touch(&b, 0xCD, 5_000_000, 1_700_000_000);

    let id_a1 = fileid_of(&a).unwrap();
    let id_a2 = fileid_of(&a).unwrap();
    let id_b = fileid_of(&b).unwrap();

    assert_eq!(id_a1, id_a2, "same file must hash the same");
    assert_ne!(id_a1, id_b, "different content must differ");
    assert!(
        id_a1.starts_with("5000000-"),
        "id is prefixed with byte size"
    );
}

#[test]
fn captured_at_reads_mtime() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("DJI_0001.MP4");
    touch(&f, 1, 1000, 1_700_000_123);
    assert_eq!(captured_at(&f), 1_700_000_123);
}

#[test]
fn clusters_on_the_gap() {
    // two clips close, then one a day later → two sessions.
    let rec = |at: i64, owner: Option<&str>| ClipRec {
        at,
        bytes: 100,
        owner: owner.map(|s| s.to_string()),
        clip: ClipRef {
            path: format!("c{at}.mp4"),
            fileid: format!("id{at}"),
        },
    };
    let recs = vec![
        rec(1_700_000_000, None),
        rec(1_700_000_300, Some("ha-giang")),
        rec(1_700_100_000, None),
    ];
    let s = cluster_sessions(&recs, 21600);
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].clips, 2);
    assert_eq!(s[1].clips, 1);
    assert_eq!(s[0].owners, vec!["ha-giang".to_string()]);
    assert!(!s[0].imported, "only one of two clips owned");
    assert_eq!(s[0].new_clips, 1, "one clip still new");
    assert!(!s[0].strip.is_empty(), "session carries a contact strip");
}

#[test]
fn ledger_round_trips() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("imported.tsv");
    fs::write(
        &p,
        "9000000-deadbeef\tha-giang\tjaeho\tdji\tDJI_0001.MP4\t9000000\t1700000000\t2026-06-18 10:00:00\n",
    )
    .unwrap();
    let l = Ledger::load(&p);
    assert_eq!(l.trip_of("9000000-deadbeef").as_deref(), Some("ha-giang"));
    assert_eq!(l.trip_of("nope"), None);
}

#[test]
fn scans_card_into_sessions_with_owners() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let dcim = d.path().join("card/DCIM");
    fs::create_dir_all(&state).unwrap();

    touch(&dcim.join("DJI_0001.MP4"), 1, 9_000_000, 1_700_000_000);
    touch(&dcim.join("DJI_0002.MP4"), 2, 9_000_000, 1_700_000_300);
    touch(&dcim.join("GX010001.MP4"), 3, 9_000_000, 1_700_100_000);

    let c = cfg(&lib, &state, Some(dcim.clone()));

    // Seed the ledger so session A's first clip shows an owner.
    let id = fileid_of(&dcim.join("DJI_0001.MP4")).unwrap();
    fs::write(
        c.ledger_path(),
        format!("{id}\tbali\tjaeho\tdji\tDJI_0001.MP4\t9000000\t1700000000\tx\n"),
    )
    .unwrap();

    let card = scan_card(&c).expect("a card is present");
    assert_eq!(card.clips, 3);
    assert_eq!(card.sessions.len(), 2);
    assert_eq!(card.sessions[0].clips, 2);
    assert_eq!(card.sessions[1].clips, 1);
    assert_eq!(card.sessions[0].owners, vec!["bali".to_string()]);
    assert_eq!(
        card.sessions[0].new_clips, 1,
        "one clip in session A is new"
    );
    assert!(
        !card.sessions[0].safe,
        "owner trip has no share record → not safe to clear"
    );
}

#[test]
fn lists_trips_with_state() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let trip = lib.join("ha-giang");

    fs::create_dir_all(&trip).unwrap();
    fs::write(
        trip.join(".reel"),
        "reel project\nfrom=2026-06-18\nto=2026-06-20\n",
    )
    .unwrap();
    touch(&trip.join("jaeho/dji/DJI_0001.MP4"), 1, 1000, 1_700_000_000);
    fs::write(trip.join("marks.tsv"), "# header\nm1\ta\tb\nm2\tc\td\n").unwrap();
    touch(&trip.join("clips/DJI_0001_c01.mp4"), 1, 500, 1_700_000_000);

    let trips = list_trips(&cfg(&lib, &state, None));
    assert_eq!(trips.len(), 1);
    let t = &trips[0];
    assert_eq!(t.name, "ha-giang");
    assert_eq!(t.masters, 1);
    assert_eq!(t.marks, 2);
    assert_eq!(t.clips, 1);
    assert_eq!(t.from.as_deref(), Some("2026-06-18"));
    assert_eq!(
        t.start,
        Some(1_700_000_000),
        "range start from the master's mtime"
    );
    assert_eq!(t.end, Some(1_700_000_000));
    assert_eq!(t.state, TripState::Cut); // clips present → ready to edit
    assert_eq!(t.next, "edit");
    assert!(t.cover.is_some(), "a trip with masters has a cover clip");
    assert_eq!(t.mine, 1, "the lone master is yours");
    assert_eq!(t.pulled, 0);
    assert!(t.contributors.is_empty());
}

#[test]
fn splits_mine_from_pulled_footage() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let trip = lib.join("ha-giang");
    fs::create_dir_all(&trip).unwrap();
    fs::write(trip.join(".reel"), "reel project\n").unwrap();

    // two masters of yours (under jaeho/), one pulled from a friend (under alice/)
    touch(
        &trip.join("jaeho/dji/DJI_0001.MP4"),
        1,
        2_000_000,
        1_700_000_000,
    );
    touch(
        &trip.join("jaeho/gopro/GX010001.MP4"),
        2,
        2_000_000,
        1_700_000_300,
    );
    touch(
        &trip.join("alice/dji/DJI_0050.MP4"),
        3,
        2_000_000,
        1_700_000_600,
    );

    let trips = list_trips(&cfg(&lib, &state, None));
    let t = &trips[0];
    assert_eq!(t.masters, 3);
    assert_eq!(t.mine, 2, "two masters under jaeho/");
    assert_eq!(t.pulled, 1, "one master under alice/");
    assert_eq!(t.contributors, vec!["alice".to_string()]);
}

fn have_rclone() -> bool {
    std::process::Command::new("rclone")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn pushes_a_trip_and_marks_it_shared() {
    if !have_rclone() {
        eprintln!("rclone not found; skipping push test");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let pool = d.path().join("pool"); // a local path is a valid rclone "remote"
    fs::create_dir_all(&state).unwrap();

    let trip = lib.join("bali");
    fs::create_dir_all(&trip).unwrap();
    fs::write(trip.join(".reel"), "reel project\nfrom=2026-06-18\n").unwrap();
    // two masters of mine, a proxy that must stay local, and a pulled clip that
    // isn't mine to push.
    touch(
        &trip.join("jaeho/dji/DJI_0001.MP4"),
        1,
        2_000_000,
        1_700_000_000,
    );
    touch(
        &trip.join("jaeho/gopro/GX010001.MP4"),
        2,
        3_000_000,
        1_700_000_300,
    );
    touch(
        &trip.join("jaeho/gopro/GL010001.LRV"),
        3,
        40_000,
        1_700_000_300,
    );
    touch(
        &trip.join("alice/dji/DJI_0050.MP4"),
        4,
        2_000_000,
        1_700_000_600,
    );

    let mut c = cfg(&lib, &state, None);
    c.remote = pool.display().to_string();

    let mut phases = Vec::new();
    let res = reel_core::push_trip(&c, "bali", |p| phases.push(p.phase)).expect("push ok");

    assert_eq!(res.files, 2, "only my two masters are pushed");
    assert_eq!(res.bytes, 5_000_000);
    assert_eq!(res.uploaded, 5_000_000, "first push sends every byte");
    assert!(
        phases.contains(&reel_core::PushPhase::Verify),
        "the verify leg ran"
    );

    // Trip is now recorded shared, readable by both CLI and GUI.
    assert_eq!(
        reel_core::trips::trip_meta(&trip, "share").as_deref(),
        Some("shared")
    );
    assert_eq!(list_trips(&c)[0].share, reel_core::Share::Shared);

    // The pool holds my masters under <trip>/<me>/, but not the proxy or the
    // footage I pulled from someone else.
    assert!(pool.join("bali/jaeho/dji/DJI_0001.MP4").is_file());
    assert!(pool.join("bali/jaeho/gopro/GX010001.MP4").is_file());
    assert!(
        !pool.join("bali/jaeho/gopro/GL010001.LRV").exists(),
        "proxy excluded"
    );
    assert!(
        !pool.join("bali/alice").exists(),
        "only my contribution is pushed"
    );

    // Re-pushing is a no-op upload: everything's already up, still shared.
    let again = reel_core::push_trip(&c, "bali", |_| {}).expect("re-push ok");
    assert_eq!(again.uploaded, 0, "nothing new to send");
    assert_eq!(again.files, 2);
}

#[test]
fn pushing_an_unknown_trip_errors() {
    let d = tempfile::tempdir().unwrap();
    let c = cfg(&d.path().join("Videos"), &d.path().join("state"), None);
    assert!(reel_core::push_trip(&c, "ghost", |_| {}).is_err());
    assert!(reel_core::push_trip(&c, "../escape", |_| {}).is_err());
}

#[test]
fn shared_owner_makes_a_card_session_safe() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let dcim = d.path().join("card/DCIM");
    fs::create_dir_all(&state).unwrap();

    touch(&dcim.join("DJI_0001.MP4"), 1, 9_000_000, 1_700_000_000);
    let c = cfg(&lib, &state, Some(dcim.clone()));

    // The owning trip exists and is recorded shared.
    let owner = lib.join("ha-giang");
    fs::create_dir_all(&owner).unwrap();
    fs::write(owner.join(".reel"), "reel project\nshare=shared\n").unwrap();

    // Its sole clip is already imported there.
    let id = fileid_of(&dcim.join("DJI_0001.MP4")).unwrap();
    fs::write(
        c.ledger_path(),
        format!("{id}\tha-giang\tjaeho\tdji\tDJI_0001.MP4\t9000000\t1700000000\tx\n"),
    )
    .unwrap();

    let card = scan_card(&c).expect("a card is present");
    assert_eq!(card.sessions.len(), 1);
    assert_eq!(card.sessions[0].new_clips, 0, "already imported");
    assert!(
        card.sessions[0].safe,
        "owner trip is shared → safe to clear the card"
    );
}

#[test]
fn generates_and_caches_a_poster() {
    // Engine degrades gracefully without ffmpeg; skip rather than fail if absent.
    if std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_err()
    {
        eprintln!("ffmpeg not found; skipping poster test");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let clip = d.path().join("DJI_test.mp4");
    let made = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=15:duration=1",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&clip)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(made && clip.is_file(), "ffmpeg should synth a test clip");

    let c = cfg(&d.path().join("Videos"), &d.path().join("state"), None);

    let uri = reel_core::thumbs::poster_data_uri(&c, &clip, "testid-1").expect("a poster");
    assert!(uri.starts_with("data:image/jpeg;base64,"));
    assert!(uri.len() > 100, "non-trivial image payload");

    // Cached now, keyed by content id.
    assert!(
        reel_core::thumbs::poster_path(&c, "testid-1").is_file(),
        "poster cached"
    );
}

#[test]
fn imports_a_window_into_a_trip() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let dcim = d.path().join("card/DCIM");
    fs::create_dir_all(&state).unwrap();

    // Two clips inside the window; one a day later, outside it.
    touch(&dcim.join("DJI_0001.MP4"), 1, 2_000_000, 1_700_000_000);
    touch(&dcim.join("GX010001.MP4"), 2, 3_000_000, 1_700_000_300);
    touch(&dcim.join("DJI_0009.MP4"), 9, 1_000_000, 1_700_100_000);

    let c = cfg(&lib, &state, Some(dcim.clone()));

    let mut ticks = 0usize;
    let res = reel_core::import_window(&c, "bali", 1_700_000_000, 1_700_000_300, |_| ticks += 1)
        .expect("import ok");

    assert_eq!(res.copied, 2, "both in-window clips copied");
    assert_eq!(res.bytes, 5_000_000);
    assert_eq!(res.skipped_here, 0);
    assert_eq!(res.skipped_other, 0);
    assert!(ticks > 0, "progress streamed at least once");

    // Files land under <trip>/<user>/<camera>/, capture time preserved.
    let dji = lib.join("bali/jaeho/dji/DJI_0001.MP4");
    let gopro = lib.join("bali/jaeho/gopro/GX010001.MP4");
    assert!(
        dji.is_file() && gopro.is_file(),
        "clips copied into camera dirs"
    );
    assert_eq!(captured_at(&dji), 1_700_000_000, "mtime preserved on copy");
    assert!(
        !lib.join("bali/jaeho/dji/DJI_0009.MP4").exists(),
        "out-of-window clip stays on the card"
    );
    assert!(lib.join("bali/.reel").is_file(), "trip marker created");
    assert!(
        !dji.with_extension("partial").exists(),
        "no temp left behind"
    );

    // Ledger now records both clips as bali's.
    let led = Ledger::load(&c.ledger_path());
    let id = fileid_of(&dcim.join("DJI_0001.MP4")).unwrap();
    assert_eq!(led.trip_of(&id).as_deref(), Some("bali"));

    // Re-import is idempotent: nothing new, both already here.
    let again = reel_core::import_window(&c, "bali", 1_700_000_000, 1_700_000_300, |_| {})
        .expect("re-import ok");
    assert_eq!(again.copied, 0);
    assert_eq!(again.skipped_here, 2);
}

#[test]
fn import_skips_clips_owned_by_another_trip() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let dcim = d.path().join("card/DCIM");
    fs::create_dir_all(&state).unwrap();

    touch(&dcim.join("DJI_0001.MP4"), 1, 2_000_000, 1_700_000_000);
    touch(&dcim.join("DJI_0002.MP4"), 2, 2_000_000, 1_700_000_300);
    let c = cfg(&lib, &state, Some(dcim.clone()));

    // The second clip already belongs to another trip.
    let id2 = fileid_of(&dcim.join("DJI_0002.MP4")).unwrap();
    fs::write(
        c.ledger_path(),
        format!("{id2}\tha-giang\tjaeho\tdji\tDJI_0002.MP4\t2000000\t1700000300\tx\n"),
    )
    .unwrap();

    let res = reel_core::import_window(&c, "bali", 1_700_000_000, 1_700_000_300, |_| {})
        .expect("import ok");
    assert_eq!(res.copied, 1, "only the unowned clip is taken");
    assert_eq!(res.skipped_other, 1);
    assert!(lib.join("bali/jaeho/dji/DJI_0001.MP4").is_file());
    assert!(!lib.join("bali/jaeho/dji/DJI_0002.MP4").exists());

    // The other trip's ledger row survives the rewrite.
    let led = Ledger::load(&c.ledger_path());
    assert_eq!(led.trip_of(&id2).as_deref(), Some("ha-giang"));
}

// Lay down a card master, an identical local copy under <trip>/jaeho/<cam>/, and
// (unless `pool` is None) an identical pool copy, plus the ledger row tying them
// together. Same fill+len everywhere → byte-identical, so rclone check matches.
fn seed_imported(
    c: &Config,
    dcim: &Path,
    pool: Option<&Path>,
    trip: &str,
    cam: &str,
    name: &str,
    fill: u8,
    len: usize,
    at: i64,
) {
    let card = dcim.join(name);
    touch(&card, fill, len, at);
    touch(
        &c.lib.join(trip).join("jaeho").join(cam).join(name),
        fill,
        len,
        at,
    );
    if let Some(p) = pool {
        touch(
            &p.join(trip).join("jaeho").join(cam).join(name),
            fill,
            len,
            at,
        );
    }
    let id = fileid_of(&card).unwrap();
    let mut row = fs::read_to_string(c.ledger_path()).unwrap_or_default();
    row.push_str(&format!(
        "{id}\t{trip}\tjaeho\t{cam}\t{name}\t{len}\t{at}\tx\n"
    ));
    fs::create_dir_all(c.ledger_path().parent().unwrap()).unwrap();
    fs::write(c.ledger_path(), row).unwrap();
}

#[test]
fn reclaims_verified_pooled_card_files() {
    if !have_rclone() {
        eprintln!("rclone not found; skipping reclaim test");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let pool = d.path().join("pool");
    let dcim = d.path().join("card/DCIM");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&lib.join("bali")).unwrap();
    fs::write(lib.join("bali/.reel"), "reel project\nshare=shared\n").unwrap();

    let mut c = cfg(&lib, &state, Some(dcim.clone()));
    c.remote = pool.display().to_string();

    seed_imported(
        &c,
        &dcim,
        Some(&pool),
        "bali",
        "dji",
        "DJI_0001.MP4",
        0xA1,
        2_000_000,
        1_700_000_000,
    );
    seed_imported(
        &c,
        &dcim,
        Some(&pool),
        "bali",
        "gopro",
        "GX010001.MP4",
        0xB2,
        3_000_000,
        1_700_000_300,
    );

    let mut phases = Vec::new();
    let plan = reel_core::plan_reclaim(&c, None, false, |p| phases.push(p.phase)).expect("plan ok");
    assert_eq!(
        plan.files.len(),
        2,
        "both verified+pooled masters are eligible"
    );
    assert_eq!(plan.bytes, 5_000_000);
    assert_eq!(plan.trips, vec!["bali".to_string()]);
    assert_eq!(plan.not_imported, 0);
    assert_eq!(plan.not_verified, 0);
    assert!(
        phases.contains(&reel_core::WipePhase::Verify),
        "the pool check ran"
    );

    // Card files are still here until commit — planning never deletes.
    assert!(dcim.join("DJI_0001.MP4").exists());

    let res = reel_core::commit_reclaim(&c, &plan.files).expect("commit ok");
    assert_eq!(res.deleted, 2);
    assert_eq!(res.bytes, 5_000_000);
    assert!(!dcim.join("DJI_0001.MP4").exists(), "card freed");
    assert!(!dcim.join("GX010001.MP4").exists());
    assert!(
        lib.join("bali/jaeho/dji/DJI_0001.MP4").is_file(),
        "local copy kept"
    );
    assert!(
        pool.join("bali/jaeho/dji/DJI_0001.MP4").is_file(),
        "pool copy kept"
    );
}

#[test]
fn reclaim_skips_unimported_and_mismatched() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let dcim = d.path().join("card/DCIM");
    fs::create_dir_all(&state).unwrap();

    let c = cfg(&lib, &state, Some(dcim.clone()));

    // (1) verified+present, (2) on the card but never imported (no ledger),
    // (3) imported but the local copy is a different size. Offline skips the pool
    // check, so we're exercising the match/verify-by-size logic alone.
    seed_imported(
        &c,
        &dcim,
        None,
        "bali",
        "dji",
        "DJI_0001.MP4",
        0xA1,
        2_000_000,
        1_700_000_000,
    );
    touch(&dcim.join("DJI_0002.MP4"), 0xC3, 1_500_000, 1_700_000_300);
    seed_imported(
        &c,
        &dcim,
        None,
        "bali",
        "dji",
        "DJI_0003.MP4",
        0xD4,
        1_000_000,
        1_700_000_600,
    );
    // shrink the local copy of #3 so its size no longer matches the card
    touch(
        &lib.join("bali/jaeho/dji/DJI_0003.MP4"),
        0xD4,
        900_000,
        1_700_000_600,
    );

    let plan = reel_core::plan_reclaim(&c, None, true, |_| {}).expect("plan ok");
    assert_eq!(plan.files.len(), 1, "only the matching master is eligible");
    assert!(plan.files[0].ends_with("DJI_0001.MP4"));
    assert_eq!(plan.not_imported, 1, "DJI_0002 has no ledger row");
    assert_eq!(
        plan.not_verified, 1,
        "DJI_0003's local copy is a different size"
    );
    assert!(plan.offline);
}

#[test]
fn reclaim_aborts_when_master_missing_from_pool() {
    if !have_rclone() {
        eprintln!("rclone not found; skipping reclaim-abort test");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let pool = d.path().join("pool");
    let dcim = d.path().join("card/DCIM");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&pool).unwrap(); // pool exists but is empty

    let mut c = cfg(&lib, &state, Some(dcim.clone()));
    c.remote = pool.display().to_string();

    // Imported locally, but never pushed — the pool has nothing.
    seed_imported(
        &c,
        &dcim,
        None,
        "bali",
        "dji",
        "DJI_0001.MP4",
        0xA1,
        2_000_000,
        1_700_000_000,
    );

    let plan = reel_core::plan_reclaim(&c, None, false, |_| {});
    assert!(plan.is_err(), "an unpooled master aborts the whole reclaim");
    assert!(
        dcim.join("DJI_0001.MP4").exists(),
        "nothing deleted on abort"
    );
}

#[test]
fn archives_a_pooled_trip() {
    if !have_rclone() {
        eprintln!("rclone not found; skipping archive test");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let pool = d.path().join("pool");
    fs::create_dir_all(&state).unwrap();

    let trip = lib.join("bali");
    fs::create_dir_all(&trip).unwrap();
    fs::write(trip.join(".reel"), "reel project\nshare=shared\n").unwrap();

    // raw (freed): yours, a proxy beside it, and footage pulled from alice; plus a
    // contact sheet. clips/ and marks.tsv are kept.
    touch(
        &trip.join("jaeho/dji/DJI_0001.MP4"),
        0xA1,
        2_000_000,
        1_700_000_000,
    );
    touch(
        &trip.join("jaeho/gopro/GX010001.MP4"),
        0xB2,
        3_000_000,
        1_700_000_300,
    );
    touch(
        &trip.join("jaeho/gopro/GL010001.LRV"),
        0xC3,
        100_000,
        1_700_000_300,
    );
    touch(
        &trip.join("alice/dji/DJI_0050.MP4"),
        0xD4,
        2_000_000,
        1_700_000_600,
    );
    touch(
        &trip.join("_sheets/contact.jpg"),
        0xE5,
        50_000,
        1_700_000_000,
    );
    touch(
        &trip.join("clips/DJI_0001_c01.mp4"),
        0xF6,
        500_000,
        1_700_000_000,
    );
    fs::write(trip.join("marks.tsv"), "# header\nm1\ta\tb\n").unwrap();

    // pool holds every master, byte-identical (same fill+len → same hash).
    touch(
        &pool.join("bali/jaeho/dji/DJI_0001.MP4"),
        0xA1,
        2_000_000,
        1,
    );
    touch(
        &pool.join("bali/jaeho/gopro/GX010001.MP4"),
        0xB2,
        3_000_000,
        1,
    );
    touch(
        &pool.join("bali/alice/dji/DJI_0050.MP4"),
        0xD4,
        2_000_000,
        1,
    );

    let mut c = cfg(&lib, &state, None);
    c.remote = pool.display().to_string();

    let plan = reel_core::plan_archive(&c, "bali", |_| {}).expect("plan ok");
    assert_eq!(plan.masters, 3, "every master, all contributors");
    assert_eq!(
        plan.bytes, 7_150_000,
        "raw trees + proxy + sheet, not clips"
    );

    // Still all here until commit.
    assert!(trip.join("jaeho/dji/DJI_0001.MP4").exists());

    let res = reel_core::commit_archive(&c, "bali", |_| {}).expect("commit ok");
    assert_eq!(res.freed, 7_150_000);
    assert_eq!(res.masters, 3);

    // Raw trees, proxy, and sheets are gone…
    assert!(!trip.join("jaeho").exists(), "your raw freed");
    assert!(!trip.join("alice").exists(), "pulled raw freed");
    assert!(!trip.join("_sheets").exists(), "sheets freed");
    // …but the cut clips, marks, and marker stay.
    assert!(trip.join("clips/DJI_0001_c01.mp4").is_file(), "clips kept");
    assert!(trip.join("marks.tsv").is_file(), "marks kept");
    assert!(trip.join(".reel").is_file(), "trip marker kept");

    // The trip now reads Archived (no local raw, clips present).
    assert_eq!(list_trips(&c)[0].state, TripState::Archived);
}

#[test]
fn archive_aborts_when_raw_not_in_pool() {
    if !have_rclone() {
        eprintln!("rclone not found; skipping archive-abort test");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let pool = d.path().join("pool");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&pool).unwrap(); // pool exists but is empty

    let trip = lib.join("bali");
    fs::create_dir_all(&trip).unwrap();
    fs::write(trip.join(".reel"), "reel project\n").unwrap();
    touch(
        &trip.join("jaeho/dji/DJI_0001.MP4"),
        0xA1,
        2_000_000,
        1_700_000_000,
    );

    let mut c = cfg(&lib, &state, None);
    c.remote = pool.display().to_string();

    assert!(
        reel_core::plan_archive(&c, "bali", |_| {}).is_err(),
        "unpooled raw can't be archived"
    );
    assert!(
        reel_core::commit_archive(&c, "bali", |_| {}).is_err(),
        "commit re-verifies and also refuses"
    );
    assert!(
        trip.join("jaeho/dji/DJI_0001.MP4").exists(),
        "nothing freed"
    );
}

#[test]
fn archive_errs_on_empty_or_unknown_trip() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let c = cfg(&lib, &d.path().join("state"), None);

    // unknown trip
    assert!(reel_core::plan_archive(&c, "ghost", |_| {}).is_err());

    // a trip with no raw left (already archived) — nothing to free, no pool call
    let trip = lib.join("done");
    fs::create_dir_all(&trip).unwrap();
    fs::write(trip.join(".reel"), "reel project\n").unwrap();
    touch(&trip.join("clips/c01.mp4"), 1, 500, 1_700_000_000);
    assert!(reel_core::plan_archive(&c, "done", |_| {}).is_err());
}

#[test]
fn commit_reclaim_only_deletes_card_files() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let dcim = d.path().join("card/DCIM");
    fs::create_dir_all(&state).unwrap();

    let c = cfg(&lib, &state, Some(dcim.clone()));
    let on_card = dcim.join("DJI_0001.MP4");
    touch(&on_card, 1, 1000, 1_700_000_000);
    let off_card = lib.join("precious.mp4");
    touch(&off_card, 1, 1000, 1_700_000_000);

    let res = reel_core::commit_reclaim(
        &c,
        &[
            off_card.display().to_string(),
            on_card.display().to_string(),
        ],
    )
    .expect("commit ok");
    assert_eq!(res.deleted, 1, "only the card file is removed");
    assert!(!on_card.exists(), "card file deleted");
    assert!(off_card.exists(), "a path off the card is never touched");
}

// ---- review: playlist, marks, clip server ----

fn make_trip(lib: &Path, name: &str) -> PathBuf {
    let trip = lib.join(name);
    fs::create_dir_all(&trip).unwrap();
    fs::write(trip.join(".reel"), "reel project\n").unwrap();
    trip
}

#[test]
fn playlist_flags_a_native_proxy_source() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let trip = make_trip(&lib, "road");

    // DJI master + its native .LRF proxy (earlier), iPhone master with none (later)
    touch(
        &trip.join("jaeho/dji/DJI_0001.MP4"),
        0x11,
        2_000_000,
        1_700_000_000,
    );
    touch(
        &trip.join("jaeho/dji/DJI_0001.LRF"),
        0x22,
        50_000,
        1_700_000_000,
    );
    touch(
        &trip.join("jaeho/iphone/IMG_2000.MOV"),
        0x33,
        2_000_000,
        1_700_000_600,
    );

    let c = cfg(&lib, &state, None);
    let pl = reel_core::review_playlist(&c, "road").expect("playlist");
    assert_eq!(pl.clips.len(), 2);
    // The native .LRF is never played raw (its extra streams break the webview):
    // play falls back to the master, but has_proxy flags the fast remux source.
    assert!(pl.clips[0].master.ends_with("DJI_0001.MP4"));
    assert_eq!(pl.clips[0].play, pl.clips[0].master);
    assert!(!pl.clips[0].proxied, "no cached clean proxy yet");
    assert!(
        pl.clips[0].has_proxy,
        "a native .LRF is a fast build source"
    );
    // iPhone clip: no fast source at all
    assert!(pl.clips[1].master.ends_with("IMG_2000.MOV"));
    assert_eq!(pl.clips[1].play, pl.clips[1].master);
    assert!(!pl.clips[1].proxied);
    assert!(!pl.clips[1].has_proxy);
}

#[test]
fn playlist_flags_a_stub_clip() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let trip = make_trip(&lib, "road");

    // A card stub written first: a couple-hundred-byte placeholder the camera
    // left with no video (plus its equally-empty .LRF).
    touch(
        &trip.join("jaeho/dji/DJI_0001.MP4"),
        0x00,
        1_241,
        1_700_000_000,
    );
    touch(
        &trip.join("jaeho/dji/DJI_0001.LRF"),
        0x00,
        1_241,
        1_700_000_000,
    );
    // A real clip afterwards.
    touch(
        &trip.join("jaeho/dji/DJI_0002.MP4"),
        0x11,
        3_000_000,
        1_700_000_600,
    );

    let c = cfg(&lib, &state, None);
    let pl = reel_core::review_playlist(&c, "road").expect("playlist");
    assert_eq!(pl.clips.len(), 2);
    assert!(pl.clips[0].master.ends_with("DJI_0001.MP4"));
    assert!(pl.clips[0].stub, "a sub-512 KiB master is a card stub");
    assert!(!pl.clips[1].stub, "a multi-MB master is real footage");
}

#[test]
fn playlist_plays_a_cached_proxy_when_present() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let trip = make_trip(&lib, "trip");
    touch(
        &trip.join("jaeho/iphone/IMG_3000.MOV"),
        0x33,
        2_000_000,
        1_700_000_000,
    );
    // rel_stem(<trip>/jaeho/iphone/IMG_3000.MOV) == "jaeho__iphone__IMG_3000"
    touch(
        &trip.join(".proxies/jaeho__iphone__IMG_3000.mp4"),
        0x44,
        9000,
        1_700_000_000,
    );

    let c = cfg(&lib, &d.path().join("state"), None);
    let pl = reel_core::review_playlist(&c, "trip").unwrap();
    assert_eq!(pl.clips.len(), 1, ".proxies content is not itself a clip");
    assert!(pl.clips[0]
        .play
        .ends_with(".proxies/jaeho__iphone__IMG_3000.mp4"));
    assert!(pl.clips[0].proxied, "a cached clean proxy loads directly");
    assert!(!pl.clips[0].has_proxy);
}

fn have_ffmpeg() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn clip_health_flags_brief_and_unreadable() {
    let d = tempfile::tempdir().unwrap();

    // A garbage file can't be probed → unreadable (no ffmpeg needed).
    let junk = d.path().join("junk.mp4");
    fs::write(&junk, b"not a video at all").unwrap();
    let h = reel_core::media::clip_health(&junk);
    assert!(!h.ok);
    assert_eq!(h.tag, "unreadable");

    if !have_ffmpeg() {
        eprintln!("skipping the encoded-clip half: ffmpeg not installed");
        return;
    }
    let enc = |path: &Path, dur: &str| {
        std::process::Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg(format!("testsrc=size=320x240:rate=30:duration={dur}"))
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };

    // A sub-second clip is an accidental blip → flagged "brief".
    let brief = d.path().join("brief.mp4");
    if enc(&brief, "0.2") {
        let h = reel_core::media::clip_health(&brief);
        assert!(!h.ok, "a 0.2s clip should be flagged");
        assert_eq!(h.tag, "brief");
    }

    // A normal clip is healthy.
    let good = d.path().join("good.mp4");
    if enc(&good, "2") {
        let h = reel_core::media::clip_health(&good);
        assert!(h.ok, "a 2s clip should be playable: {}", h.reason);
    }
}

#[test]
fn ensure_proxy_remuxes_a_native_proxy() {
    if !have_ffmpeg() {
        eprintln!("skipping ensure_proxy_remuxes_a_native_proxy: ffmpeg not installed");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let trip = make_trip(&lib, "trip");
    let cam = trip.join("jaeho/dji");
    fs::create_dir_all(&cam).unwrap();

    // a real H.264 mp4 standing in for the native .LRF proxy
    let lrf = cam.join("DJI_0001.LRF");
    let encoded = std::process::Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=640x360:rate=15:duration=1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&lrf)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !encoded {
        eprintln!("skipping: this ffmpeg can't encode H.264");
        return;
    }
    // a stub master beside it — ensure_proxy must build from the LRF, not this
    touch(&cam.join("DJI_0001.MP4"), 0, 1000, 1_700_000_000);

    let c = cfg(&lib, &d.path().join("state"), None);
    let master = cam.join("DJI_0001.MP4");
    let out = reel_core::ensure_proxy(&c, "trip", &master).expect("build proxy");
    assert!(out.ends_with(".proxies/jaeho__dji__DJI_0001.mp4"));
    assert!(out.is_file());
    assert!(
        fs::metadata(&out).unwrap().len() > 1000,
        "the remuxed proxy has real content"
    );
    // idempotent: a second call returns the cache
    assert_eq!(reel_core::ensure_proxy(&c, "trip", &master).unwrap(), out);
}

#[test]
fn marks_round_trip_cut_compatibly() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let trip = make_trip(&lib, "trip");
    let c = cfg(&lib, &d.path().join("state"), None);
    let master = trip.join("jaeho/dji/DJI_0001.MP4").display().to_string();

    let marks = vec![
        reel_core::Mark {
            master: master.clone(),
            start: 1.5,
            end: 12.0,
            label: "jump".into(),
        },
        reel_core::Mark {
            master: master.clone(),
            start: 30.25,
            end: 41.0,
            label: "hl".into(),
        },
        // a tab inside a label must be flattened so the row keeps four fields
        reel_core::Mark {
            master: master.clone(),
            start: 5.0,
            end: 6.0,
            label: "a\tb".into(),
        },
    ];
    assert_eq!(reel_core::save_marks(&c, "trip", marks).unwrap(), 3);

    // raw bytes are exactly what `reel cut` reads: master<TAB>%.3f<TAB>%.3f<TAB>label
    let raw = fs::read_to_string(trip.join("marks.tsv")).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines[0], format!("{master}\t1.500\t12.000\tjump"));
    assert_eq!(lines[1], format!("{master}\t30.250\t41.000\thl"));
    assert_eq!(lines[2], format!("{master}\t5.000\t6.000\ta b"));

    // and it parses back to the same segments
    let back = reel_core::review::read_marks(&trip);
    assert_eq!(back.len(), 3);
    assert_eq!(back[0].start, 1.5);
    assert_eq!(back[0].label, "jump");
}

#[test]
fn read_marks_skips_comments_and_tolerates_missing_label() {
    let d = tempfile::tempdir().unwrap();
    let trip = d.path().join("trip");
    fs::create_dir_all(&trip).unwrap();
    let body = "# header\n\n/m/DJI_0001.MP4\t1.000\t2.000\n/m/DJI_0002.MP4\t3.0\t4.0\twin\njunk\n";
    fs::write(trip.join("marks.tsv"), body).unwrap();

    let m = reel_core::review::read_marks(&trip);
    assert_eq!(
        m.len(),
        2,
        "comment, blank and the malformed line are skipped"
    );
    assert_eq!(m[0].master, "/m/DJI_0001.MP4");
    assert_eq!(m[0].label, "", "a missing fourth field is an empty label");
    assert_eq!(m[1].label, "win");
}

#[test]
fn finds_gopro_and_dji_native_proxies() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    let gx = p.join("GX010007.MP4");
    let gl = p.join("GL010007.LRV");
    fs::write(&gx, b"m").unwrap();
    fs::write(&gl, b"p").unwrap();
    assert_eq!(reel_core::media::native_proxy_of(&gx), Some(gl));

    let dji = p.join("DJI_0042.MP4");
    let lrf = p.join("DJI_0042.LRF");
    fs::write(&dji, b"m").unwrap();
    fs::write(&lrf, b"p").unwrap();
    assert_eq!(reel_core::media::native_proxy_of(&dji), Some(lrf));

    // a master with no sibling proxy resolves to nothing
    let solo = p.join("DJI_0099.MP4");
    fs::write(&solo, b"m").unwrap();
    assert_eq!(reel_core::media::native_proxy_of(&solo), None);
}

#[test]
fn serves_byte_ranges() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path();
    let f = lib.join("clip.mp4");
    let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    fs::write(&f, &data).unwrap();
    let len = data.len() as u64;

    // explicit small range → exactly those bytes
    let r = reel_core::serve::serve_clip(lib, &f, Some("bytes=0-3"));
    assert_eq!(r.status, 206);
    assert_eq!(r.send, Some((0, 3)));
    assert!(r.accept_ranges);
    assert_eq!(
        r.content_range.as_deref(),
        Some(format!("bytes 0-3/{len}").as_str())
    );

    // open-ended range streams the whole rest — no cap now (the server streams
    // from disk, so a long range costs no memory)
    let r2 = reel_core::serve::serve_clip(lib, &f, Some("bytes=0-"));
    assert_eq!(r2.status, 206);
    assert_eq!(r2.send, Some((0, len - 1)));
    assert_eq!(
        r2.content_range.as_deref(),
        Some(format!("bytes 0-{}/{len}", len - 1).as_str())
    );

    // suffix range: the last 4 bytes
    let r3 = reel_core::serve::serve_clip(lib, &f, Some("bytes=-4"));
    assert_eq!(r3.status, 206);
    assert_eq!(r3.send, Some((len - 4, len - 1)));
}

#[test]
fn serve_guards_scope_and_missing() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("lib");
    fs::create_dir_all(&lib).unwrap();
    let inside = lib.join("a.mp4");
    fs::write(&inside, vec![1u8, 2, 3, 4]).unwrap();
    let outside = d.path().join("secret.txt");
    fs::write(&outside, b"nope").unwrap();

    assert_eq!(
        reel_core::serve::serve_clip(&lib, &outside, None).status,
        403
    );
    assert_eq!(
        reel_core::serve::serve_clip(&lib, &lib.join("ghost.mp4"), None).status,
        404
    );

    // a small file with no range comes back whole (streamed 0..=len-1)
    let r = reel_core::serve::serve_clip(&lib, &inside, None);
    assert_eq!(r.status, 200);
    assert_eq!(r.send, Some((0, 3)));
    assert!(r.content_range.is_none());
}

#[test]
fn mime_forces_video_for_camera_proxies() {
    assert_eq!(
        reel_core::serve::mime_for(Path::new("/x/DJI_0001.LRF")),
        "video/mp4"
    );
    assert_eq!(
        reel_core::serve::mime_for(Path::new("/x/GL010001.LRV")),
        "video/mp4"
    );
    assert_eq!(
        reel_core::serve::mime_for(Path::new("/x/IMG_1.MOV")),
        "video/mp4"
    );
    assert_eq!(
        reel_core::serve::mime_for(Path::new("/x/poster.jpg")),
        "image/jpeg"
    );
}

#[test]
fn cut_errors_without_marks() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    make_trip(&lib, "trip"); // a real project, but no marks.tsv
    let c = cfg(&lib, &d.path().join("state"), None);
    assert!(reel_core::cut_trip(&c, "trip", |_| {}).is_err());
    // an unknown trip errors too, without touching the disk
    assert!(reel_core::cut_trip(&c, "ghost", |_| {}).is_err());
}

#[test]
fn cuts_marked_ranges_into_clips() {
    if !have_ffmpeg() {
        eprintln!("skipping cuts_marked_ranges_into_clips: ffmpeg not installed");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let trip = make_trip(&lib, "trip");
    let cam = trip.join("jaeho/dji");
    fs::create_dir_all(&cam).unwrap();

    // A real ~3s H.264 master; -g 15 gives frequent keyframes so a stream-copy
    // cut lands close to the marks.
    let master = cam.join("DJI_0001.MP4");
    let made = std::process::Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=30:duration=3",
            "-c:v",
            "libx264",
            "-g",
            "15",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&master)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !made {
        eprintln!("skipping: this ffmpeg can't encode H.264");
        return;
    }

    let c = cfg(&lib, &d.path().join("state"), None);
    let m = |s: f64, e: f64, label: &str| reel_core::Mark {
        master: master.display().to_string(),
        start: s,
        end: e,
        label: label.into(),
    };
    // A labelled segment and a highlight — exercises slug naming and c-numbering.
    reel_core::save_marks(
        &c,
        "trip",
        vec![m(0.5, 1.5, "great jump"), m(1.0, 2.0, "hl")],
    )
    .unwrap();

    let r = reel_core::cut_trip(&c, "trip", |_| {}).expect("cut");
    assert_eq!((r.made, r.skipped, r.failed), (2, 0, 0));

    // labels slugify the same way the script's `tr -cs 'a-zA-Z0-9' '-'` does, and
    // the mark's file-order index becomes the c<NN> suffix.
    let clips = trip.join("clips");
    let a = clips.join("jaeho__dji__DJI_0001_c01_great-jump.mp4");
    let b = clips.join("jaeho__dji__DJI_0001_c02_hl.mp4");
    assert!(a.is_file(), "labelled cut written");
    assert!(b.is_file(), "highlight cut written");
    assert!(
        fs::metadata(&a).unwrap().len() > 1000,
        "cut has real content"
    );

    // additive + re-runnable: a second cut writes nothing new, skips both.
    let r2 = reel_core::cut_trip(&c, "trip", |_| {}).expect("re-cut");
    assert_eq!((r2.made, r2.skipped, r2.failed), (0, 2, 0));
}
