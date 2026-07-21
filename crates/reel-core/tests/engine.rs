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
        remote: "test:cloud".into(),
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
        photo: false,
        owner: owner.map(|s| s.to_string()),
        discarded: false,
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
    assert_eq!(s[0].captures, 2);
    assert_eq!(s[1].captures, 1);
    assert_eq!(s[0].owners, vec!["ha-giang".to_string()]);
    assert!(!s[0].imported, "only one of two clips owned");
    assert_eq!(s[0].new_captures, 1, "one clip still new");
    assert!(!s[0].strip.is_empty(), "session carries a contact strip");
}

#[test]
fn photos_cluster_with_videos_by_time() {
    // A video with a photo shot during it, plus a lone photo a day later → two
    // sessions: one video+photo, one photo-only. Photos cluster exactly like video.
    let cap = |at: i64, photo: bool, owner: Option<&str>| ClipRec {
        at,
        bytes: 100,
        photo,
        owner: owner.map(|s| s.to_string()),
        discarded: false,
        clip: ClipRef {
            path: format!("c{at}.{}", if photo { "jpg" } else { "mp4" }),
            fileid: format!("id{at}"),
        },
    };
    let caps = vec![
        cap(1_700_000_000, false, None),         // video
        cap(1_700_000_200, true, Some("kyoto")), // photo, rides the same session
        cap(1_700_100_000, true, None),          // lone photo → its own session
    ];
    let s = cluster_sessions(&caps, 21600);
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].captures, 2, "video + photo in one session");
    assert_eq!(s[0].photos, 1, "one of the two is a photo");
    assert_eq!(
        s[0].owners,
        vec!["kyoto".to_string()],
        "the photo's owner surfaces"
    );
    assert_eq!(s[0].new_captures, 1, "the video is still new");
    assert_eq!(s[1].captures, 1);
    assert_eq!(s[1].photos, 1, "photo-only session");
    assert_eq!(s[1].new_captures, 1, "the lone photo is new");
    assert!(!s[1].imported);
}

#[test]
fn stitched_panorama_imports_as_a_photo_and_sweeps_source_frames() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let dcim = d.path().join("card/DCIM");
    fs::create_dir_all(&state).unwrap();

    // A video, and — beside it — DJI's finished panorama as one wide photo. Its raw
    // source frames sit under PANORAMA/<seq>/ (the negatives we don't import); the
    // seq (0001) is embedded in both the photo name and the folder name.
    touch(
        &dcim.join("DJI_001/DJI_0001.MP4"),
        1,
        9_000_000,
        1_700_000_000,
    );
    touch(
        &dcim.join("DJI_001/DJI_20260101_0001_D.JPG"),
        2,
        6_000_000,
        1_700_000_100,
    );
    touch(
        &dcim.join("PANORAMA/001_0001/PANO_0001.JPG"),
        3,
        5_000_000,
        1_700_000_090,
    );
    touch(
        &dcim.join("PANORAMA/001_0001/PANO_0002.JPG"),
        4,
        5_000_000,
        1_700_000_091,
    );

    let c = cfg(&lib, &state, Some(dcim.clone()));

    // Survey: two captures (a video + the stitched photo). The PANORAMA/ source
    // frames are ignored — they're not importable captures.
    let card = scan_card(&c).expect("card present");
    assert_eq!(card.captures, 2, "video + stitched panorama photo");
    assert_eq!(card.photos, 1, "the panorama is one photo");
    assert_eq!(card.sessions.len(), 1);
    let sess = card.sessions[0].clone();

    // Import → the video and the one photo copy in (not the source frames), and no
    // panoramas/ folder is created.
    let r = reel_core::import_window(&c, "kyoto", sess.start, sess.end, |_| {}).unwrap();
    assert_eq!(r.copied, 2);
    assert_eq!(r.photos, 1);
    assert!(lib
        .join("kyoto/jaeho/dji/DJI_20260101_0001_D.JPG")
        .is_file());
    assert!(
        !lib.join("kyoto/jaeho/panoramas").exists(),
        "source frames not imported"
    );

    // Offline reclaim: plans the video and the photo, and sweeps the panorama's two
    // source frames too — they ride the now-cleared-safe photo's fate.
    let plan = reel_core::plan_reclaim(&c, Some((sess.start, sess.end)), true, |_| {}).unwrap();
    assert!(plan.offline);
    assert!(plan.files.iter().any(|f| f.ends_with("DJI_0001.MP4")));
    assert!(plan
        .files
        .iter()
        .any(|f| f.ends_with("DJI_20260101_0001_D.JPG")));
    assert_eq!(
        plan.files.iter().filter(|f| f.contains("PANORAMA")).count(),
        2,
        "both raw source frames swept with the stitched photo"
    );

    // Commit removes them and tidies the emptied PANORAMA/<seq>/ folder.
    reel_core::commit_reclaim(&c, &plan.files).unwrap();
    assert!(!dcim.join("PANORAMA/001_0001/PANO_0001.JPG").exists());
    assert!(
        !dcim.join("PANORAMA/001_0001").exists(),
        "emptied pano folder tidied"
    );
}

#[test]
fn source_frames_stay_until_their_stitched_photo_is_safe() {
    // Import only the video (window excludes the panorama photo). A reclaim of that
    // window must NOT touch the source frames — they ride the photo, which isn't
    // imported/safe. Losing negatives whose deliverable isn't secured is the hazard.
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let dcim = d.path().join("card/DCIM");
    fs::create_dir_all(&state).unwrap();

    touch(
        &dcim.join("DJI_001/DJI_0001.MP4"),
        1,
        9_000_000,
        1_700_000_000,
    );
    touch(
        &dcim.join("DJI_001/DJI_20260101_0001_D.JPG"),
        2,
        6_000_000,
        1_700_050_000,
    );
    touch(
        &dcim.join("PANORAMA/001_0001/PANO_0001.JPG"),
        3,
        5_000_000,
        1_700_050_010,
    );

    let c = cfg(&lib, &state, Some(dcim.clone()));

    // Import just the video's instant — the photo (much later) stays on the card.
    reel_core::import_window(&c, "kyoto", 1_700_000_000, 1_700_000_000, |_| {}).unwrap();

    let plan =
        reel_core::plan_reclaim(&c, Some((1_700_000_000, 1_700_000_000)), true, |_| {}).unwrap();
    assert!(plan.files.iter().any(|f| f.ends_with("DJI_0001.MP4")));
    assert_eq!(
        plan.files.iter().filter(|f| f.contains("PANORAMA")).count(),
        0,
        "frames stay while their panorama photo is unimported"
    );
    assert!(dcim.join("PANORAMA/001_0001/PANO_0001.JPG").exists());
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
    assert_eq!(card.captures, 3);
    assert_eq!(card.sessions.len(), 2);
    assert_eq!(card.sessions[0].captures, 2);
    assert_eq!(card.sessions[1].captures, 1);
    assert_eq!(card.sessions[0].owners, vec!["bali".to_string()]);
    assert_eq!(
        card.sessions[0].new_captures, 1,
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
    assert_eq!(t.state, TripState::Marked); // reviewed; the cut doesn't advance it
    assert_eq!(t.next, "edit");
    assert!(t.cover.is_some(), "a trip with masters has a cover clip");
    assert_eq!(t.mine, 1, "the lone master is yours");
    assert_eq!(t.pulled, 0);
    assert!(t.contributors.is_empty());
}

/// Cutting is an export, not a stage: a trip reads the same before and after, and
/// the next step stays Edit either way. It used to flip Marked → Cut, so the card
/// kept offering `Cut →` as the thing the trip was waiting on and a cut trip
/// looked further along than an uncut one — neither of which is true now that
/// marks open in an editor directly.
#[test]
fn cutting_does_not_advance_a_trip() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let trip = make_trip(&lib, "koh-samui");
    let c = cfg(&lib, &state, None);

    touch(&trip.join("jaeho/dji/DJI_0001.MP4"), 1, 1000, 1_700_000_000);
    fs::write(trip.join("marks.tsv"), "# header\nm1\ta\tb\n").unwrap();

    let before = &list_trips(&c)[0];
    assert_eq!(before.state, TripState::Marked);
    assert_eq!(before.next, "edit");
    assert_eq!(before.clips, 0);

    touch(&trip.join("clips/DJI_0001_c01.mp4"), 1, 500, 1_700_000_000);

    let after = &list_trips(&c)[0];
    assert_eq!(after.state, TripState::Marked, "still just a reviewed trip");
    assert_eq!(after.next, "edit");
    assert_eq!(after.clips, 1, "the clip count is where cutting shows up");
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
    let cloud = d.path().join("cloud"); // a local path is a valid rclone "remote"
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
    c.remote = cloud.display().to_string();

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

    // The cloud holds my masters under <trip>/<me>/, but not the proxy or the
    // footage I pulled from someone else.
    assert!(cloud.join("bali/jaeho/dji/DJI_0001.MP4").is_file());
    assert!(cloud.join("bali/jaeho/gopro/GX010001.MP4").is_file());
    assert!(
        !cloud.join("bali/jaeho/gopro/GL010001.LRV").exists(),
        "proxy excluded"
    );
    assert!(
        !cloud.join("bali/alice").exists(),
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
    assert_eq!(card.sessions[0].new_captures, 0, "already imported");
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
// (unless `cloud` is None) an identical cloud copy, plus the ledger row tying them
// together. Same fill+len everywhere → byte-identical, so rclone check matches.
#[allow(clippy::too_many_arguments)]
fn seed_imported(
    c: &Config,
    dcim: &Path,
    cloud: Option<&Path>,
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
    if let Some(p) = cloud {
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
    let cloud = d.path().join("cloud");
    let dcim = d.path().join("card/DCIM");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(lib.join("bali")).unwrap();
    fs::write(lib.join("bali/.reel"), "reel project\nshare=shared\n").unwrap();

    let mut c = cfg(&lib, &state, Some(dcim.clone()));
    c.remote = cloud.display().to_string();

    seed_imported(
        &c,
        &dcim,
        Some(&cloud),
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
        Some(&cloud),
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
        "both verified+in_cloud masters are eligible"
    );
    assert_eq!(plan.bytes, 5_000_000);
    assert_eq!(plan.trips, vec!["bali".to_string()]);
    assert_eq!(plan.not_imported, 0);
    assert_eq!(plan.not_verified, 0);
    assert!(
        phases.contains(&reel_core::WipePhase::Verify),
        "the cloud check ran"
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
        cloud.join("bali/jaeho/dji/DJI_0001.MP4").is_file(),
        "cloud copy kept"
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
    // (3) imported but the local copy is a different size. Offline skips the cloud
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
    let cloud = d.path().join("cloud");
    let dcim = d.path().join("card/DCIM");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&cloud).unwrap(); // cloud exists but is empty

    let mut c = cfg(&lib, &state, Some(dcim.clone()));
    c.remote = cloud.display().to_string();

    // Imported locally, but never pushed — the cloud has nothing.
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
    let cloud = d.path().join("cloud");
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

    // cloud holds every master, byte-identical (same fill+len → same hash).
    touch(
        &cloud.join("bali/jaeho/dji/DJI_0001.MP4"),
        0xA1,
        2_000_000,
        1,
    );
    touch(
        &cloud.join("bali/jaeho/gopro/GX010001.MP4"),
        0xB2,
        3_000_000,
        1,
    );
    touch(
        &cloud.join("bali/alice/dji/DJI_0050.MP4"),
        0xD4,
        2_000_000,
        1,
    );

    let mut c = cfg(&lib, &state, None);
    c.remote = cloud.display().to_string();

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
    let cloud = d.path().join("cloud");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&cloud).unwrap(); // cloud exists but is empty

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
    c.remote = cloud.display().to_string();

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

    // a trip with no raw left (already archived) — nothing to free, no cloud call
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
    let roots = [lib.to_path_buf()];

    // explicit small range → exactly those bytes
    let r = reel_core::serve::serve_clip(&roots, &f, Some("bytes=0-3"));
    assert_eq!(r.status, 206);
    assert_eq!(r.send, Some((0, 3)));
    assert!(r.accept_ranges);
    assert_eq!(
        r.content_range.as_deref(),
        Some(format!("bytes 0-3/{len}").as_str())
    );

    // open-ended range streams the whole rest — no cap now (the server streams
    // from disk, so a long range costs no memory)
    let r2 = reel_core::serve::serve_clip(&roots, &f, Some("bytes=0-"));
    assert_eq!(r2.status, 206);
    assert_eq!(r2.send, Some((0, len - 1)));
    assert_eq!(
        r2.content_range.as_deref(),
        Some(format!("bytes 0-{}/{len}", len - 1).as_str())
    );

    // suffix range: the last 4 bytes
    let r3 = reel_core::serve::serve_clip(&roots, &f, Some("bytes=-4"));
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
    let roots = [lib.clone()];

    assert_eq!(
        reel_core::serve::serve_clip(&roots, &outside, None).status,
        403
    );
    assert_eq!(
        reel_core::serve::serve_clip(&roots, &lib.join("ghost.mp4"), None).status,
        404
    );

    // a small file with no range comes back whole (streamed 0..=len-1)
    let r = reel_core::serve::serve_clip(&roots, &inside, None);
    assert_eq!(r.status, 200);
    assert_eq!(r.send, Some((0, 3)));
    assert!(r.content_range.is_none());

    // a second root (a mounted card) is admitted too, while a file under neither
    // still 403s — the scope stays a whitelist, not "any path".
    let card = d.path().join("card");
    fs::create_dir_all(&card).unwrap();
    let on_card = card.join("DJI_0001.MP4");
    fs::write(&on_card, vec![9u8, 9, 9, 9]).unwrap();
    let multi = [lib.clone(), card.clone()];
    assert_eq!(
        reel_core::serve::serve_clip(&multi, &on_card, None).status,
        200
    );
    assert_eq!(
        reel_core::serve::serve_clip(&roots, &on_card, None).status,
        403
    );
}

#[test]
fn card_proxy_cache_is_within_clip_scope() {
    // Regression: card-preview proxies are cached under cache_dir/proxies, which
    // must be one of the clip server's allowed roots — else <video> gets a 403 and
    // the clip reads "won't play, even as a proxy" (trip proxies live under lib).
    let d = tempfile::tempdir().unwrap();
    let cfg = reel_core::Config {
        lib: d.path().join("Videos"),
        remote: "x:".into(),
        user: "me".into(),
        state_dir: d.path().join("state"),
        cache_dir: d.path().join("cache"),
        session_gap: 21600,
        dji_sd: None,
        gopro_sd: None,
        media_user: "me".into(),
    };
    let proxy = cfg.card_proxy_path("qdeadbeef");
    fs::create_dir_all(proxy.parent().unwrap()).unwrap();
    fs::write(&proxy, vec![1u8, 2, 3, 4]).unwrap();
    let roots = cfg.clip_roots();
    assert!(roots.contains(&cfg.cache_dir.join("proxies")));
    assert_eq!(
        reel_core::serve::serve_clip(&roots, &proxy, None).status,
        200
    );
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

// ---- edit: hand-off to the external editor ----

/// Edit builds a timeline or it errors — it never opens something else instead.
/// The loose-file hand-off used to catch every build failure, so a trip with no
/// marks and an archived trip both quietly opened their masters or their `clips/`.
/// That's the confusing outcome: not the timeline, and not an error either. Now the
/// build's own reason is what comes back, and for archived raw it says to restore.
#[test]
fn edit_errs_rather_than_opening_loose_files() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let c = cfg(&lib, &d.path().join("state"), None);
    let trip = make_trip(&lib, "trip");

    // Masters and a finished cut — everything the fallback used to hand over — but
    // no marks, so there's no timeline and that's what you're told.
    touch(&trip.join("jaeho/dji/DJI_0001.MP4"), 1, 1000, 1_700_000_000);
    touch(&trip.join("clips/DJI_0001_c01.mp4"), 1, 500, 1_700_000_000);
    assert!(
        reel_core::open_in_editor(&c, "trip")
            .unwrap_err()
            .contains("no marks"),
        "an unreviewed trip says so instead of opening its clips"
    );

    // An archived trip: marks and clips/ survive, the raw doesn't. This is the case
    // the fallback most obscured — it opened the leftover cut, which looks enough
    // like a working edit to be worth mistaking for one. Say the raw is gone instead.
    fs::write(
        trip.join("marks.tsv"),
        "# header\njaeho/dji/DJI_0009.MP4\t1.0\t2.0\tgone\n",
    )
    .unwrap();
    let err = reel_core::open_in_editor(&c, "trip").unwrap_err();
    assert!(
        err.contains("archived or missing") && err.contains("Restore it first"),
        "archived raw names itself and says what to do: {err}"
    );

    assert!(
        reel_core::open_in_editor(&c, "../etc").is_err(),
        "a path-y name is rejected before any launch"
    );
    assert!(
        reel_core::open_in_editor(&c, "ghost")
            .unwrap_err()
            .contains("no such trip"),
        "a trip with no .reel marker is rejected"
    );
}

// ---- organize: move clips, rename, merge ----

/// A friend's footage pulled down as `person/file` (no camera level) used to have
/// its *filename* read as the camera folder, so the move built
/// `alice/CLIP.MP4/CLIP.MP4` — a directory named after the clip, with the clip
/// inside it — and wrote that shape into the baseline and the cloud.
#[test]
fn moving_a_friends_clip_with_no_camera_folder_keeps_its_shape() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let a = make_trip(&lib, "a");
    make_trip(&lib, "b");

    // two components, the shape `pull` writes for a friend with no camera level
    let master = a.join("alice/CLIP_0050.MP4");
    touch(&master, 2, 1_000_000, 1_700_000_000);

    let c = cfg(&lib, &state, None);
    let r = reel_core::move_clips(&c, &[master.display().to_string()], "b").expect("move ok");
    assert_eq!(r.moved, 1);

    assert!(!master.exists(), "gone from source");
    assert!(
        lib.join("b/alice/CLIP_0050.MP4").is_file(),
        "lands at person/file, the shape it arrived in"
    );
    assert!(
        !lib.join("b/alice/CLIP_0050.MP4/CLIP_0050.MP4").exists(),
        "the filename must never become a directory"
    );
}

/// Anything deeper than `person/camera` was truncated onto those two levels,
/// which can drop two distinct clips onto one destination path.
#[test]
fn a_deeper_subpath_survives_a_move_intact() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let a = make_trip(&lib, "a");
    make_trip(&lib, "b");

    let master = a.join("jaeho/dji/day2/DJI_0007.MP4");
    touch(&master, 3, 1_000_000, 1_700_000_000);

    let c = cfg(&lib, &state, None);
    let r = reel_core::move_clips(&c, &[master.display().to_string()], "b").expect("move ok");
    assert_eq!(r.moved, 1);
    assert!(
        lib.join("b/jaeho/dji/day2/DJI_0007.MP4").is_file(),
        "the whole subpath rides along, not just the first two levels"
    );
}

#[test]
fn moves_a_clip_between_trips() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let a = make_trip(&lib, "a");
    make_trip(&lib, "b");

    let master = a.join("jaeho/dji/DJI_0001.MP4");
    touch(&master, 1, 2_000_000, 1_700_000_000);

    let c = cfg(&lib, &state, None);
    // a mark keyed on the master, and a ledger row owning it for trip "a"
    fs::write(
        a.join("marks.tsv"),
        format!("{}\t1.000\t2.000\tjump\n", master.display()),
    )
    .unwrap();
    let id = fileid_of(&master).unwrap();
    fs::write(
        c.ledger_path(),
        format!("{id}\ta\tjaeho\tdji\tDJI_0001.MP4\t2000000\t1700000000\tx\n"),
    )
    .unwrap();

    let r = reel_core::move_clips(&c, &[master.display().to_string()], "b").expect("move ok");
    assert_eq!(r.moved, 1);
    assert_eq!(r.marks, 1);
    assert!(r.cloud_synced, "unshared footage needs no cloud sync");

    // file moved, provenance subpath (person/camera) preserved
    assert!(!master.exists(), "gone from source");
    assert!(
        lib.join("b/jaeho/dji/DJI_0001.MP4").is_file(),
        "under dest, same person/camera"
    );

    // ledger now points at "b"
    assert_eq!(
        Ledger::load(&c.ledger_path()).trip_of(&id).as_deref(),
        Some("b")
    );

    // mark migrated to b, repointed at the new path; a's marks emptied
    let bm = reel_core::review::read_marks(&lib.join("b"));
    assert_eq!(bm.len(), 1);
    assert!(bm[0].master.ends_with("b/jaeho/dji/DJI_0001.MP4"));
    assert_eq!(bm[0].label, "jump");
    assert!(
        reel_core::review::read_marks(&a).is_empty(),
        "source marks emptied"
    );
}

#[test]
fn move_carries_proxy_and_cut_clips() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let a = make_trip(&lib, "a");
    make_trip(&lib, "b");
    let master = a.join("jaeho/dji/DJI_0001.MP4");
    touch(&master, 1, 2_000_000, 1_700_000_000);
    // rel_stem(<a>/jaeho/dji/DJI_0001.MP4) == "jaeho__dji__DJI_0001"
    touch(
        &a.join(".proxies/jaeho__dji__DJI_0001.mp4"),
        2,
        9000,
        1_700_000_000,
    );
    touch(
        &a.join("clips/jaeho__dji__DJI_0001_c01_win.mp4"),
        3,
        5000,
        1_700_000_000,
    );
    // a cut clip from a DIFFERENT master must stay put
    touch(
        &a.join("clips/jaeho__dji__DJI_0002_c01.mp4"),
        4,
        5000,
        1_700_000_000,
    );

    let c = cfg(&lib, &d.path().join("state"), None);
    let r = reel_core::move_clips(&c, &[master.display().to_string()], "b").unwrap();
    assert_eq!(r.moved, 1);
    assert_eq!(r.clips, 1, "only this master's cut clip follows it");
    assert!(lib.join("b/.proxies/jaeho__dji__DJI_0001.mp4").is_file());
    assert!(lib
        .join("b/clips/jaeho__dji__DJI_0001_c01_win.mp4")
        .is_file());
    assert!(
        a.join("clips/jaeho__dji__DJI_0002_c01.mp4").is_file(),
        "another master's clip is not swept along"
    );
}

#[test]
fn move_skips_a_clip_the_dest_already_has() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let a = make_trip(&lib, "a");
    make_trip(&lib, "b");
    let master = a.join("jaeho/dji/DJI_0001.MP4");
    touch(&master, 1, 2_000_000, 1_700_000_000);
    touch(
        &lib.join("b/jaeho/dji/DJI_0001.MP4"),
        1,
        2_000_000,
        1_700_000_000,
    );

    let c = cfg(&lib, &d.path().join("state"), None);
    let r = reel_core::move_clips(&c, &[master.display().to_string()], "b").unwrap();
    assert_eq!(r.moved, 0);
    assert_eq!(r.skipped, 1);
    assert!(
        master.is_file(),
        "source clip left in place, never clobbered"
    );
}

#[test]
fn rename_moves_dir_ledger_and_marks() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let a = lib.join("a");
    fs::create_dir_all(&a).unwrap();
    fs::write(a.join(".reel"), "reel project\nfrom=2026-06-18\n").unwrap();
    let master = a.join("jaeho/dji/DJI_0001.MP4");
    touch(&master, 1, 1000, 1_700_000_000);
    fs::write(
        a.join("marks.tsv"),
        format!("{}\t1.000\t2.000\thl\n", master.display()),
    )
    .unwrap();

    let c = cfg(&lib, &state, None);
    let id = fileid_of(&master).unwrap();
    fs::write(
        c.ledger_path(),
        format!("{id}\ta\tjaeho\tdji\tDJI_0001.MP4\t1000\t1700000000\tx\n"),
    )
    .unwrap();

    let r = reel_core::rename_trip(&c, "a", "trip-b").expect("rename ok");
    assert_eq!(r.dest, "trip-b");
    assert!(r.cloud_synced, "unshared, so no cloud involvement");
    assert!(!a.exists(), "old dir gone");
    assert!(lib.join("trip-b/jaeho/dji/DJI_0001.MP4").is_file());
    assert_eq!(
        reel_core::trips::trip_meta(&lib.join("trip-b"), "from").as_deref(),
        Some("2026-06-18"),
        ".reel metadata carried over"
    );
    assert_eq!(
        Ledger::load(&c.ledger_path()).trip_of(&id).as_deref(),
        Some("trip-b")
    );
    let m = reel_core::review::read_marks(&lib.join("trip-b"));
    assert_eq!(m.len(), 1);
    assert!(
        m[0].master.ends_with("trip-b/jaeho/dji/DJI_0001.MP4"),
        "mark path repointed to the new dir"
    );
}

#[test]
fn rename_refuses_bad_targets() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    make_trip(&lib, "a");
    make_trip(&lib, "b");
    let c = cfg(&lib, &d.path().join("state"), None);
    assert!(
        reel_core::rename_trip(&c, "a", "b").is_err(),
        "won't clobber an existing trip — that's a merge"
    );
    assert!(
        reel_core::rename_trip(&c, "ghost", "x").is_err(),
        "no such source trip"
    );
    assert!(
        reel_core::rename_trip(&c, "a", "../escape").is_err(),
        "a path-y name is rejected"
    );
}

#[test]
fn merge_folds_source_into_dest() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let a = make_trip(&lib, "a");
    let b = make_trip(&lib, "b");
    touch(
        &a.join("jaeho/dji/DJI_0001.MP4"),
        1,
        2_000_000,
        1_700_000_000,
    );
    touch(
        &a.join("jaeho/gopro/GX010001.MP4"),
        2,
        2_000_000,
        1_700_000_300,
    );
    touch(
        &b.join("jaeho/dji/DJI_0009.MP4"),
        3,
        2_000_000,
        1_700_000_600,
    );

    let c = cfg(&lib, &state, None);
    let id1 = fileid_of(&a.join("jaeho/dji/DJI_0001.MP4")).unwrap();
    fs::write(
        c.ledger_path(),
        format!("{id1}\ta\tjaeho\tdji\tDJI_0001.MP4\t2000000\t1700000000\tx\n"),
    )
    .unwrap();

    let r = reel_core::merge_trips(&c, "a", "b").expect("merge ok");
    assert_eq!(r.moved, 2);
    assert!(!a.exists(), "emptied source is removed");
    assert!(lib.join("b/jaeho/dji/DJI_0001.MP4").is_file());
    assert!(lib.join("b/jaeho/gopro/GX010001.MP4").is_file());
    assert!(
        lib.join("b/jaeho/dji/DJI_0009.MP4").is_file(),
        "dest's own footage stays"
    );
    assert_eq!(
        Ledger::load(&c.ledger_path()).trip_of(&id1).as_deref(),
        Some("b")
    );
}

#[test]
fn moving_shared_footage_offline_unshares_the_dest() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let a = lib.join("a");
    fs::create_dir_all(&a).unwrap();
    fs::write(a.join(".reel"), "reel project\nshare=shared\n").unwrap();
    make_trip(&lib, "b");
    let master = a.join("jaeho/dji/DJI_0001.MP4");
    touch(&master, 1, 2_000_000, 1_700_000_000);

    // A remote name that isn't configured → the cloud is unreachable, so the move
    // can't be mirrored there and the destination must not claim to be shared.
    let mut c = cfg(&lib, &state, None);
    c.remote = "reel_absent_remote:cloud".into();

    let r = reel_core::move_clips(&c, &[master.display().to_string()], "b").expect("move ok");
    assert_eq!(r.moved, 1);
    assert!(!r.cloud_synced, "the move couldn't follow into the cloud");
    assert_eq!(
        reel_core::trips::trip_meta(&lib.join("b"), "share").as_deref(),
        Some("unknown"),
        "dest share dropped rather than overstating safety"
    );
}

// ---- permanent delete + tombstones ----

#[test]
fn delete_removes_pulled_footage_locally_and_tombstones() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let trip = make_trip(&lib, "bali");

    // footage pulled from a friend, plus a cut derived from it and a mark
    let master = trip.join("alice/dji/DJI_0050.MP4");
    touch(&master, 1, 2_000_000, 1_700_000_000);
    touch(
        &trip.join("clips/alice__dji__DJI_0050_c01.mp4"),
        2,
        5000,
        1_700_000_000,
    );
    fs::write(
        trip.join("marks.tsv"),
        format!("{}\t1.000\t2.000\tx\n", master.display()),
    )
    .unwrap();

    let c = cfg(&lib, &state, None);
    let id = fileid_of(&master).unwrap();
    fs::write(
        c.ledger_path(),
        format!("{id}\tbali\talice\tdji\tDJI_0050.MP4\t2000000\t1700000000\tx\n"),
    )
    .unwrap();

    let r = reel_core::delete_clips(&c, &[master.display().to_string()]).expect("delete ok");
    assert_eq!(r.deleted, 1);
    assert_eq!(r.kept_cloud, 1, "a friend's cloud copy is never removed");
    assert_eq!(r.in_cloud, 0);
    assert!(r.cloud_ok, "no cloud call was needed for pulled footage");
    assert!(!master.exists(), "local copy gone");
    assert!(
        trip.join("clips/alice__dji__DJI_0050_c01.mp4").is_file(),
        "a finished cut is kept, not swept away"
    );
    // tombstoned, ledger row dropped, dangling mark stripped
    assert!(reel_core::ledger::Tombstones::load(&c.tombstones_path()).contains(&id));
    assert_eq!(Ledger::load(&c.ledger_path()).trip_of(&id), None);
    assert!(
        reel_core::review::read_marks(&trip).is_empty(),
        "the mark that pointed at the deleted clip is gone"
    );
}

#[test]
fn delete_your_footage_offline_owes_pool_cleanup() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let trip = make_trip(&lib, "bali");
    let master = trip.join("jaeho/dji/DJI_0001.MP4");
    touch(&master, 1, 2_000_000, 1_700_000_000);

    let mut c = cfg(&lib, &state, None);
    c.remote = "reel_absent_remote:cloud".into(); // unreachable

    let r = reel_core::delete_clips(&c, &[master.display().to_string()]).expect("delete ok");
    assert_eq!(r.deleted, 1, "still erased locally");
    assert!(!r.cloud_ok, "couldn't reach the cloud to erase your copy");
    assert_eq!(r.in_cloud, 0);
    assert!(!master.exists());
}

#[test]
fn deleted_clip_reads_discarded_on_the_card() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let dcim = d.path().join("card/DCIM");
    fs::create_dir_all(&state).unwrap();
    touch(&dcim.join("DJI_0001.MP4"), 1, 9_000_000, 1_700_000_000);

    let c = cfg(&lib, &state, Some(dcim.clone()));
    // this card clip was permanently deleted earlier — tombstone its id
    let id = fileid_of(&dcim.join("DJI_0001.MP4")).unwrap();
    fs::write(c.tombstones_path(), format!("{id}\n")).unwrap();

    let card = scan_card(&c).expect("a card is present");
    assert_eq!(card.sessions.len(), 1);
    assert_eq!(
        card.sessions[0].discarded, 1,
        "a tombstoned clip counts as discarded"
    );
    assert_eq!(
        card.sessions[0].new_captures, 0,
        "and is never offered as new to import"
    );
}

#[test]
fn import_skips_a_tombstoned_clip() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let dcim = d.path().join("card/DCIM");
    fs::create_dir_all(&state).unwrap();
    touch(&dcim.join("DJI_0001.MP4"), 1, 2_000_000, 1_700_000_000);

    let c = cfg(&lib, &state, Some(dcim.clone()));
    let id = fileid_of(&dcim.join("DJI_0001.MP4")).unwrap();
    fs::write(c.tombstones_path(), format!("{id}\n")).unwrap();

    let res = reel_core::import_window(&c, "bali", 1_700_000_000, 1_700_000_000, |_| {})
        .expect("import ok");
    assert_eq!(res.copied, 0, "a clip you deleted is not re-imported");
    assert!(!lib.join("bali/jaeho/dji/DJI_0001.MP4").exists());
}

#[test]
fn clears_only_tombstoned_card_files() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let dcim = d.path().join("card/DCIM");
    fs::create_dir_all(&state).unwrap();
    touch(&dcim.join("DJI_0001.MP4"), 1, 2_000_000, 1_700_000_000); // deleted → trash
    touch(&dcim.join("DJI_0002.MP4"), 2, 2_000_000, 1_700_000_300); // keep

    let c = cfg(&lib, &state, Some(dcim.clone()));
    let trash = fileid_of(&dcim.join("DJI_0001.MP4")).unwrap();
    fs::write(c.tombstones_path(), format!("{trash}\n")).unwrap();

    let r = reel_core::clear_discarded(&c, None).expect("clear ok");
    assert_eq!(r.deleted, 1, "only the tombstoned card file is removed");
    assert!(!dcim.join("DJI_0001.MP4").exists(), "trash cleared");
    assert!(
        dcim.join("DJI_0002.MP4").exists(),
        "a clip that isn't trash stays on the card"
    );
}

#[test]
fn delete_trip_removes_dir_and_tombstones() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let trip = make_trip(&lib, "bali");
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

    let c = cfg(&lib, &state, None);
    let id1 = fileid_of(&trip.join("jaeho/dji/DJI_0001.MP4")).unwrap();
    let id2 = fileid_of(&trip.join("jaeho/gopro/GX010001.MP4")).unwrap();
    fs::write(
        c.ledger_path(),
        format!(
            "{id1}\tbali\tjaeho\tdji\tDJI_0001.MP4\t2000000\t1700000000\tx\n\
             {id2}\tbali\tjaeho\tgopro\tGX010001.MP4\t2000000\t1700000300\tx\n"
        ),
    )
    .unwrap();

    let r = reel_core::delete_trip(&c, "bali").expect("delete trip ok");
    assert_eq!(r.deleted, 2);
    assert!(!trip.exists(), "local trip directory removed");
    let tombs = reel_core::ledger::Tombstones::load(&c.tombstones_path());
    assert!(
        tombs.contains(&id1) && tombs.contains(&id2),
        "every clip the trip owned is tombstoned"
    );
    let led = Ledger::load(&c.ledger_path());
    assert_eq!(led.trip_of(&id1), None);
    assert_eq!(led.trip_of(&id2), None);
}

#[test]
fn delete_erases_your_pool_copy() {
    if !have_rclone() {
        eprintln!("rclone not found; skipping delete-cloud test");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let cloud = d.path().join("cloud");
    fs::create_dir_all(&state).unwrap();
    let trip = make_trip(&lib, "bali");
    let master = trip.join("jaeho/dji/DJI_0001.MP4");
    touch(&master, 0xA1, 2_000_000, 1_700_000_000);
    touch(
        &cloud.join("bali/jaeho/dji/DJI_0001.MP4"),
        0xA1,
        2_000_000,
        1,
    );

    let mut c = cfg(&lib, &state, None);
    c.remote = cloud.display().to_string();
    let id = fileid_of(&master).unwrap();
    fs::write(
        c.ledger_path(),
        format!("{id}\tbali\tjaeho\tdji\tDJI_0001.MP4\t2000000\t1700000000\tx\n"),
    )
    .unwrap();

    let r = reel_core::delete_clips(&c, &[master.display().to_string()]).expect("delete ok");
    assert_eq!(r.deleted, 1);
    assert_eq!(r.in_cloud, 1, "your cloud copy is erased too");
    assert!(r.cloud_ok);
    assert!(!master.exists());
    assert!(
        !cloud.join("bali/jaeho/dji/DJI_0001.MP4").exists(),
        "gone from the cloud"
    );
}

#[test]
fn delete_trip_purges_only_your_pool_subtree() {
    if !have_rclone() {
        eprintln!("rclone not found; skipping delete-trip-cloud test");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let cloud = d.path().join("cloud");
    fs::create_dir_all(&state).unwrap();
    let trip = make_trip(&lib, "bali");
    fs::write(trip.join(".reel"), "reel project\nshare=shared\n").unwrap();
    touch(
        &trip.join("jaeho/dji/DJI_0001.MP4"),
        1,
        2_000_000,
        1_700_000_000,
    );
    // the cloud holds your footage AND a friend's under the same trip
    touch(&cloud.join("bali/jaeho/dji/DJI_0001.MP4"), 1, 2_000_000, 1);
    touch(&cloud.join("bali/alice/dji/DJI_0050.MP4"), 2, 2_000_000, 1);

    let mut c = cfg(&lib, &state, None);
    c.remote = cloud.display().to_string();

    let r = reel_core::delete_trip(&c, "bali").expect("delete trip ok");
    assert!(r.cloud_ok);
    assert!(
        !cloud.join("bali/jaeho").exists(),
        "your cloud subtree is purged"
    );
    assert!(
        cloud.join("bali/alice/dji/DJI_0050.MP4").is_file(),
        "a friend's cloud footage is left untouched"
    );
}

// ---- pull: bring others' footage down from the cloud ----

#[test]
fn lists_pool_contributors() {
    if !have_rclone() {
        eprintln!("rclone not found; skipping contributors test");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let cloud = d.path().join("cloud");
    fs::create_dir_all(&state).unwrap();
    // trip "bali" in the cloud: your footage, plus alice's and ben's
    touch(&cloud.join("bali/jaeho/dji/DJI_0001.MP4"), 1, 2_000_000, 1);
    touch(&cloud.join("bali/alice/dji/DJI_0050.MP4"), 2, 2_000_000, 1);
    touch(
        &cloud.join("bali/alice/gopro/GX010001.MP4"),
        3,
        3_000_000,
        1,
    );
    touch(&cloud.join("bali/ben/iphone/IMG_2000.MOV"), 4, 1_000_000, 1);

    let mut c = cfg(&lib, &state, None);
    c.remote = cloud.display().to_string();

    let people = reel_core::cloud_contributors(&c, "bali").expect("contributors");
    let names: Vec<&str> = people.iter().map(|p| p.person.as_str()).collect();
    assert_eq!(names, vec!["alice", "ben"], "yours excluded, others sorted");
    assert_eq!(people[0].clips, 2, "alice's two masters");
    assert_eq!(people[0].bytes, 5_000_000);
    assert!(!people[0].pulled, "nothing pulled locally yet");
}

#[test]
fn pulls_a_person_into_a_trip() {
    if !have_rclone() {
        eprintln!("rclone not found; skipping pull test");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let cloud = d.path().join("cloud");
    fs::create_dir_all(&state).unwrap();
    touch(&cloud.join("bali/alice/dji/DJI_0050.MP4"), 2, 2_000_000, 1);
    touch(
        &cloud.join("bali/alice/gopro/GX010001.MP4"),
        3,
        3_000_000,
        1,
    );

    let mut c = cfg(&lib, &state, None);
    c.remote = cloud.display().to_string();

    let r = reel_core::pull_person(&c, "bali", "alice", |_| {}).expect("pull ok");
    assert_eq!(r.files, 2);
    assert_eq!(r.bytes, 5_000_000);
    assert!(
        lib.join("bali/alice/dji/DJI_0050.MP4").is_file(),
        "pulled under <trip>/<person>/, provenance intact"
    );
    assert!(lib.join("bali/alice/gopro/GX010001.MP4").is_file());
    assert!(lib.join("bali/.reel").is_file(), "trip created locally");

    // provenance now reads it as pulled from alice
    let t = &list_trips(&c)[0];
    assert_eq!(t.pulled, 2);
    assert_eq!(t.mine, 0);
    assert_eq!(t.contributors, vec!["alice".to_string()]);

    // pulling your own footage is refused (that's a Share)
    assert!(reel_core::pull_person(&c, "bali", "jaeho", |_| {}).is_err());

    // and alice now reads as already pulled
    let people = reel_core::cloud_contributors(&c, "bali").expect("contributors");
    assert!(
        people.iter().find(|p| p.person == "alice").unwrap().pulled,
        "her footage is local now"
    );
}

// ============================ cloud sync (Phase 7) ============================

/// A `bali` trip with one 2 MB master of mine and a local-path cloud, not yet
/// pushed. Returns (lib, cloud, cfg, trip_dir).
fn sync_env(d: &Path) -> (PathBuf, PathBuf, Config, PathBuf) {
    let lib = d.join("Videos");
    let state = d.join("state");
    let cloud = d.join("cloud");
    fs::create_dir_all(&state).unwrap();
    let trip = lib.join("bali");
    fs::create_dir_all(&trip).unwrap();
    fs::write(trip.join(".reel"), "reel project\n").unwrap();
    touch(
        &trip.join("jaeho/dji/DJI_0001.MP4"),
        1,
        2_000_000,
        1_700_000_000,
    );
    let mut c = cfg(&lib, &state, None);
    c.remote = cloud.display().to_string();
    (lib, cloud, c, trip)
}

#[test]
fn store_roundtrips() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("synced/bali.tsv");
    let mut f = reel_core::store::FileSet::default();
    f.insert("jaeho/dji/A.MP4", 100);
    f.insert("alice/gopro/B.MP4", 200);
    f.save(&p).unwrap();
    let back = reel_core::store::FileSet::load(&p);
    assert_eq!(back.get("jaeho/dji/A.MP4"), Some(100));
    assert_eq!(back.get("alice/gopro/B.MP4"), Some(200));

    let pend = d.path().join("pending.tsv");
    let mut q = reel_core::store::Pending::default();
    q.push(
        1,
        reel_core::store::PendingOp::Purge {
            trip: "bali".into(),
        },
    );
    q.push(
        2,
        reel_core::store::PendingOp::Move {
            from: "a".into(),
            to: "b".into(),
            rel: "jaeho/dji/A.MP4".into(),
        },
    );
    // a duplicate op isn't queued twice
    q.push(
        3,
        reel_core::store::PendingOp::Move {
            from: "a".into(),
            to: "b".into(),
            rel: "jaeho/dji/A.MP4".into(),
        },
    );
    q.save(&pend).unwrap();
    let back = reel_core::store::Pending::load(&pend);
    assert_eq!(back.rows.len(), 2);
    assert_eq!(back.count_for("b"), 1);
    assert_eq!(back.count_for("bali"), 1);
}

#[test]
fn sync_tier1_flags_to_share_without_network() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let trip = lib.join("bali");
    fs::create_dir_all(&trip).unwrap();
    fs::write(trip.join(".reel"), "reel project\n").unwrap();
    touch(&trip.join("jaeho/dji/DJI_0001.MP4"), 1, 1000, 1_700_000_000);
    touch(&trip.join("jaeho/dji/DJI_0002.MP4"), 2, 1000, 1_700_000_300);
    let c = cfg(&lib, &state, None);

    // no baseline yet → every master of mine is "to share", no network touched
    let s = reel_core::sync_status(&c, "bali", false).unwrap();
    assert_eq!(s.to_push.len(), 2);
    assert!(!s.in_sync);
    assert_eq!(s.last_cloud_check, None);

    // seed a baseline with one already synced → only the other is left to share
    let mut b = reel_core::store::FileSet::default();
    b.insert("jaeho/dji/DJI_0001.MP4", 1000);
    b.save(&c.base_path("bali")).unwrap();
    let s = reel_core::sync_status(&c, "bali", false).unwrap();
    assert_eq!(s.to_push.len(), 1);
    assert_eq!(s.to_push[0].name, "DJI_0002.MP4");
    // the same count rides along on the trip card, network-free
    assert_eq!(list_trips(&c)[0].sync.to_push, 1);
}

#[test]
fn push_writes_baseline_and_post_share_import_unsyncs() {
    if !have_rclone() {
        eprintln!("rclone not found; skipping sync push test");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (_lib, _pool, c, trip) = sync_env(d.path());
    reel_core::push_trip(&c, "bali", |_| {}).expect("push");

    // share records the verified upload as the baseline
    let b = reel_core::store::FileSet::load(&c.base_path("bali"));
    assert_eq!(b.get("jaeho/dji/DJI_0001.MP4"), Some(2_000_000));
    assert_eq!(reel_core::trips_in_cloud(&c).get("bali"), Some(&true));

    // a post-share import: a new local master that isn't in the cloud
    touch(
        &trip.join("jaeho/dji/DJI_0002.MP4"),
        5,
        1_000_000,
        1_700_000_500,
    );
    assert_eq!(
        reel_core::trips_in_cloud(&c).get("bali"),
        Some(&false),
        "a post-share import is not safe to clear"
    );
    let s = reel_core::sync_status(&c, "bali", true).expect("status");
    assert_eq!(s.to_push.len(), 1);
    assert_eq!(s.to_push[0].name, "DJI_0002.MP4");
    assert!(!s.in_sync);
}

#[test]
fn sync_pulls_new_pool_footage() {
    if !have_rclone() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (_lib, cloud, c, trip) = sync_env(d.path());
    reel_core::push_trip(&c, "bali", |_| {}).expect("push");
    // a friend drops footage straight into the cloud
    touch(
        &cloud.join("bali/alice/dji/DJI_0050.MP4"),
        7,
        1_500_000,
        1_700_000_600,
    );

    let s = reel_core::sync_status(&c, "bali", true).expect("status");
    assert_eq!(s.to_pull.len(), 1);
    assert_eq!(s.to_pull[0].person, "alice");
    assert!(s.deleted_local.is_empty());

    let r = reel_core::reconcile(
        &c,
        "bali",
        reel_core::SyncActions {
            push: false,
            pull: true,
            push_deletions: false,
            restore_cloud: false,
        },
        |_| {},
    )
    .expect("reconcile");
    assert_eq!(r.pulled, 1);
    assert!(trip.join("alice/dji/DJI_0050.MP4").is_file());
    assert!(reel_core::sync_status(&c, "bali", true).unwrap().in_sync);
}

#[test]
fn archive_keeps_a_trip_in_sync() {
    if !have_rclone() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (_lib, _pool, c, trip) = sync_env(d.path());
    reel_core::push_trip(&c, "bali", |_| {}).expect("push");
    reel_core::commit_archive(&c, "bali", |_| {}).expect("archive");
    assert!(
        !trip.join("jaeho/dji/DJI_0001.MP4").exists(),
        "raw was freed"
    );

    // archived footage is intended-in-cloud and present → still in sync, NOT an
    // owed cleanup (the invariant: deletion is driven by intent, not local absence)
    let s = reel_core::sync_status(&c, "bali", true).expect("status");
    assert!(
        s.deleted_local.is_empty(),
        "archived raw must not read as a zombie"
    );
    assert!(s.to_push.is_empty());
    assert!(s.in_sync);
    // ...but it IS surfaced as cloud-only (in the cloud, not on this machine)
    assert_eq!(s.cloud_only.len(), 1, "archived raw reads as cloud-only");
    let bali = list_trips(&c)
        .into_iter()
        .find(|x| x.name == "bali")
        .unwrap();
    assert_eq!(bali.sync.cloud_only, 1, "the card chip shows it too");
}

/// A friend whose cloud folder has no camera level (`person/base`, not
/// `person/camera/base`) still counts as local footage. `push`/`pull` write those
/// rels into the baseline, so a stricter reader in `sync` left them classified
/// `L✗ B✓ R✓` — cloud-only forever, sitting on disk the whole time. This was worth
/// 303 phantom clips on a real trip.
#[test]
fn a_friends_footage_with_no_camera_folder_still_reads_as_local() {
    if !have_rclone() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (_lib, cloud, c, trip) = sync_env(d.path());
    reel_core::push_trip(&c, "bali", |_| {}).expect("push");
    // alice uploaded straight into her folder — no camera directory
    touch(
        &cloud.join("bali/alice/CLIP_0050.MP4"),
        7,
        1_500_000,
        1_700_000_600,
    );

    let r = reel_core::reconcile(
        &c,
        "bali",
        reel_core::SyncActions {
            push: false,
            pull: true,
            push_deletions: false,
            restore_cloud: false,
        },
        |_| {},
    )
    .expect("reconcile");
    assert_eq!(r.pulled, 1);
    let local = trip.join("alice/CLIP_0050.MP4");
    assert!(local.is_file(), "it came down");

    let s = reel_core::sync_status(&c, "bali", true).expect("status");
    assert!(
        s.cloud_only.is_empty(),
        "a clip that's on disk must never read as cloud-only: {:?}",
        s.cloud_only.iter().map(|i| &i.rel).collect::<Vec<_>>()
    );
    assert!(
        s.to_pull.is_empty(),
        "and it must not offer to pull it again"
    );
    assert!(s.in_sync);
}

/// The way back from archiving. `cloud_only` was surfaced from the start and
/// described in the model as "re-downloadable", but nothing implemented the
/// download: archive your raw to free the disk and the footage was stranded in the
/// cloud. Restore closes that, and stays opt-in so freeing disk isn't undone by a
/// stray click of Sync.
#[test]
fn archived_footage_can_be_brought_back_down() {
    if !have_rclone() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (_lib, _pool, c, trip) = sync_env(d.path());
    reel_core::push_trip(&c, "bali", |_| {}).expect("push");
    reel_core::commit_archive(&c, "bali", |_| {}).expect("archive");
    let raw = trip.join("jaeho/dji/DJI_0001.MP4");
    assert!(!raw.exists(), "raw was freed");
    assert_eq!(
        reel_core::sync_status(&c, "bali", true)
            .unwrap()
            .cloud_only
            .len(),
        1
    );

    // Every other leg ticked must still leave the freed disk freed.
    let r = reel_core::reconcile(
        &c,
        "bali",
        reel_core::SyncActions {
            push: true,
            pull: true,
            push_deletions: true,
            restore_cloud: false,
        },
        |_| {},
    )
    .expect("reconcile");
    assert_eq!(r.restored, 0);
    assert!(
        !raw.exists(),
        "restore is opt-in — a plain Sync must never re-fill disk you freed"
    );

    // Ticking it brings the footage back down.
    let r = reel_core::reconcile(
        &c,
        "bali",
        reel_core::SyncActions {
            push: false,
            pull: false,
            push_deletions: false,
            restore_cloud: true,
        },
        |_| {},
    )
    .expect("reconcile");
    assert_eq!(r.restored, 1);
    assert!(raw.is_file(), "the archived master is back on this machine");
    let s = reel_core::sync_status(&c, "bali", true).expect("status");
    assert!(s.cloud_only.is_empty(), "nothing is cloud-only any more");
    assert!(s.in_sync);
}

/// A rel is a path into the trip, so it must not be able to climb out of it. The
/// rels `reconcile` passes are engine-derived, but `restore` is reachable from the
/// command layer, where the list crosses the IPC boundary.
#[test]
fn restore_refuses_to_climb_out_of_the_trip() {
    let d = tempfile::tempdir().unwrap();
    let (_lib, _pool, c, _trip) = sync_env(d.path());
    for bad in [
        "../../.ssh/id_rsa",
        "/etc/passwd",
        "jaeho/../../escape.MP4",
        "",
    ] {
        let e =
            reel_core::restore(&c, "bali", &[bad.to_string()], |_| {}).expect_err("must refuse");
        assert!(e.contains("unsafe path"), "{bad:?} gave: {e}");
    }
}

#[test]
fn global_sync_pushes_and_pulls_every_trip() {
    if !have_rclone() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (_lib, cloud, c, trip) = sync_env(d.path());
    // bali has my unshared master (from sync_env), and a friend has footage in the
    // cloud that I don't have locally yet.
    touch(
        &cloud.join("bali/alice/dji/DJI_0050.MP4"),
        7,
        1_000_000,
        1_700_000_600,
    );

    let r = reel_core::reconcile_all(&c, |_| {}).expect("global sync");
    assert_eq!(r.pushed, 1, "my unshared footage was uploaded");
    assert_eq!(r.pulled, 1, "the friend's footage was pulled");
    assert!(
        cloud.join("bali/jaeho/dji/DJI_0001.MP4").is_file(),
        "mine is now in the cloud"
    );
    assert!(
        trip.join("alice/dji/DJI_0050.MP4").is_file(),
        "the friend's is now local"
    );
    // a second run has nothing additive left to do
    let again = reel_core::reconcile_all(&c, |_| {}).expect("global sync again");
    assert!(again.in_sync);
}

#[test]
fn premigration_archived_reads_as_cloud_only_not_zombie() {
    if !have_rclone() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (_lib, _pool, c, trip) = sync_env(d.path());
    reel_core::push_trip(&c, "bali", |_| {}).expect("push");
    // simulate a trip shared BEFORE this feature (no baseline file) whose raw was
    // then archived (freed locally), footage still in the cloud
    std::fs::remove_file(c.base_path("bali")).ok();
    std::fs::remove_dir_all(trip.join("jaeho")).unwrap();

    // the migration seed must adopt your cloud footage into the baseline so it reads
    // as cloud-only, NOT as an offline-delete zombie to clean
    let s = reel_core::sync_status(&c, "bali", true).expect("status");
    assert!(
        s.deleted_local.is_empty(),
        "archived-without-baseline must not read as a zombie"
    );
    assert_eq!(s.cloud_only.len(), 1, "it reads as cloud-only");
}

#[test]
fn offline_delete_surfaces_then_reconcile_cleans_zombie() {
    if !have_rclone() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (_lib, cloud, c, trip) = sync_env(d.path());
    reel_core::push_trip(&c, "bali", |_| {}).expect("push");
    let master = trip.join("jaeho/dji/DJI_0001.MP4");

    // delete while the remote is unreachable
    let mut c_off = c.clone();
    c_off.remote = "reel_absent_remote_x:cloud".into();
    reel_core::delete_clips(&c_off, &[master.display().to_string()]).expect("delete");
    assert!(!master.exists(), "removed locally");
    assert!(
        cloud.join("bali/jaeho/dji/DJI_0001.MP4").is_file(),
        "the cloud copy survives an offline delete"
    );

    // back online, sync sees the zombie and can clear it
    let s = reel_core::sync_status(&c, "bali", true).expect("status");
    assert_eq!(s.deleted_local.len(), 1);
    let r = reel_core::reconcile(
        &c,
        "bali",
        reel_core::SyncActions {
            push: false,
            pull: false,
            push_deletions: true,
            restore_cloud: false,
        },
        |_| {},
    )
    .expect("reconcile");
    assert_eq!(r.deleted, 1);
    assert!(
        !cloud.join("bali/jaeho/dji/DJI_0001.MP4").exists(),
        "the zombie is gone from the cloud"
    );
    assert!(reel_core::sync_status(&c, "bali", true).unwrap().in_sync);
}

#[test]
fn rename_moves_only_your_pool_subtree() {
    if !have_rclone() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (_lib, cloud, c, _trip) = sync_env(d.path());
    reel_core::push_trip(&c, "bali", |_| {}).expect("push");
    // a friend's footage sits in the cloud under the old name — not ours to move
    touch(
        &cloud.join("bali/alice/dji/DJI_0050.MP4"),
        7,
        100_000,
        1_700_000_600,
    );

    reel_core::rename_trip(&c, "bali", "sumatra").expect("rename");
    assert!(
        cloud.join("sumatra/jaeho/dji/DJI_0001.MP4").is_file(),
        "your subtree moved to the new name"
    );
    assert!(
        !cloud.join("bali/jaeho").exists(),
        "your old subtree is gone"
    );
    assert!(
        cloud.join("bali/alice/dji/DJI_0050.MP4").is_file(),
        "the friend's footage is left untouched"
    );
    assert!(
        !cloud.join("sumatra/alice").exists(),
        "the friend isn't dragged along"
    );
    assert!(c.base_path("sumatra").exists());
    assert!(!c.base_path("bali").exists());
}

#[test]
fn offline_move_queues_then_reconcile_applies() {
    if !have_rclone() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (lib, cloud, c, trip) = sync_env(d.path());
    reel_core::push_trip(&c, "bali", |_| {}).expect("push");
    let master = trip.join("jaeho/dji/DJI_0001.MP4");

    // move to a new trip while offline
    let mut c_off = c.clone();
    c_off.remote = "reel_absent_remote_y:cloud".into();
    reel_core::move_clips(&c_off, &[master.display().to_string()], "java").expect("move");
    assert!(
        lib.join("java/jaeho/dji/DJI_0001.MP4").is_file(),
        "moved locally"
    );
    assert!(
        cloud.join("bali/jaeho/dji/DJI_0001.MP4").is_file(),
        "the cloud copy stays at the source while offline"
    );
    assert_eq!(
        reel_core::store::Pending::load(&c.pending_path()).count_for("java"),
        1
    );

    // reconcile the destination online → the queued move replays server-side
    let r = reel_core::reconcile(&c, "java", reel_core::SyncActions::default(), |_| {})
        .expect("reconcile");
    assert_eq!(r.replayed, 1);
    assert!(
        cloud.join("java/jaeho/dji/DJI_0001.MP4").is_file(),
        "the cloud move was applied"
    );
    assert!(
        !cloud.join("bali/jaeho/dji/DJI_0001.MP4").exists(),
        "the old cloud copy is gone"
    );
    assert_eq!(
        reel_core::store::Pending::load(&c.pending_path()).count_for("java"),
        0
    );
}

#[test]
fn offline_rename_queues_and_does_not_offer_reupload() {
    if !have_rclone() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (lib, cloud, c, _trip) = sync_env(d.path());
    reel_core::push_trip(&c, "bali", |_| {}).expect("push");

    // rename while the remote is unreachable — the cloud move can't happen now
    let mut c_off = c.clone();
    c_off.remote = "reel_absent_remote_r:cloud".into();
    reel_core::rename_trip(&c_off, "bali", "flores").expect("rename");
    assert!(
        lib.join("flores/jaeho/dji/DJI_0001.MP4").is_file(),
        "renamed locally"
    );
    assert!(
        cloud.join("bali/jaeho/dji/DJI_0001.MP4").is_file(),
        "the cloud copy is still under the old name"
    );
    assert_eq!(
        reel_core::store::Pending::load(&c.pending_path()).count_for("flores"),
        1,
        "the rename is queued"
    );

    // the crux: status must NOT offer to re-upload — the footage is already up,
    // under the old name; a re-upload would duplicate it and orphan "bali"
    let s = reel_core::sync_status(&c, "flores", true).expect("status");
    assert!(s.to_push.is_empty(), "a queued rename suppresses re-upload");
    assert!(!s.in_sync);
    let flores = list_trips(&c)
        .into_iter()
        .find(|x| x.name == "flores")
        .unwrap();
    assert_eq!(flores.sync.to_push, 0, "the card chip agrees, network-free");
    assert_eq!(flores.sync.pending, 1);

    // reconcile replays the rename server-side — no bytes re-uploaded
    let r = reel_core::reconcile(&c, "flores", reel_core::SyncActions::default(), |_| {})
        .expect("reconcile");
    assert_eq!(r.replayed, 1);
    assert!(
        cloud.join("flores/jaeho/dji/DJI_0001.MP4").is_file(),
        "the cloud folder moved to the new name"
    );
    assert!(
        !cloud.join("bali/jaeho").exists(),
        "the old cloud name is gone"
    );
    assert!(reel_core::sync_status(&c, "flores", true).unwrap().in_sync);
}

#[test]
fn offline_delete_trip_queues_purge_then_reconcile_all_applies() {
    if !have_rclone() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (lib, cloud, c, _trip) = sync_env(d.path());
    reel_core::push_trip(&c, "bali", |_| {}).expect("push");

    // delete the whole trip while offline — its local dir is gone, so only a
    // queued purge can clean the cloud later
    let mut c_off = c.clone();
    c_off.remote = "reel_absent_remote_z:cloud".into();
    reel_core::delete_trip(&c_off, "bali").expect("delete trip");
    assert!(!lib.join("bali").exists(), "local trip removed");
    assert!(
        cloud.join("bali/jaeho/dji/DJI_0001.MP4").is_file(),
        "the cloud copy survives the offline delete"
    );
    assert_eq!(
        reel_core::store::Pending::load(&c.pending_path()).count_for("bali"),
        1
    );

    let r = reel_core::reconcile_all(&c, |_| {}).expect("reconcile_all");
    assert_eq!(r.replayed, 1);
    assert!(
        !cloud.join("bali/jaeho").exists(),
        "your cloud subtree is purged"
    );
    assert_eq!(
        reel_core::store::Pending::load(&c.pending_path()).count_for("bali"),
        0
    );
}

fn dl(trip: &str, rel: &str, local: bool, in_cloud: bool, bytes: u64) -> reel_core::model::DupLoc {
    reel_core::model::DupLoc {
        trip: trip.into(),
        rel: rel.into(),
        local,
        in_cloud,
        bytes,
    }
}

#[test]
fn dedup_scan_finds_cross_trip_dupes_and_prefers_named_trip() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let c = cfg(&lib, &state, None); // remote "test:cloud" is unreachable → local-only scan

    let sz = 6_000_000usize;
    touch(&lib.join("party/.reel"), 0, 1, 1);
    touch(&lib.join("2026-05-26_to_06-12/.reel"), 0, 1, 1);
    // the same clip in a named trip and in a default date-range trip
    touch(
        &lib.join("party/jaeho/dji/DJI_0004.MP4"),
        0xAA,
        sz,
        1_700_000_000,
    );
    touch(
        &lib.join("2026-05-26_to_06-12/jaeho/dji/DJI_0004.MP4"),
        0xAA,
        sz,
        1_700_000_000,
    );
    // a clip that lives in only one trip must NOT be flagged
    touch(
        &lib.join("party/jaeho/dji/DJI_0009.MP4"),
        0xBB,
        1000,
        1_700_000_100,
    );

    let rep = reel_core::dedup::scan(&c).unwrap();
    assert_eq!(rep.groups.len(), 1, "one duplicate group");
    let g = &rep.groups[0];
    assert_eq!(g.copies.len(), 2);
    assert_eq!(g.bytes, sz as u64);
    assert_eq!(g.reclaimable, sz as u64, "one redundant copy's worth");
    assert_eq!(rep.total_reclaimable, sz as u64);
    assert_eq!(
        g.copies[g.suggested_keep].trip, "party",
        "the named trip is the canonical keep, not the date-range default"
    );
}

#[test]
fn dedup_prune_keeps_canonical_drops_baseline_and_never_tombstones() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let c = cfg(&lib, &state, None);

    let sz = 5_000_000usize;
    let rel = "jaeho/dji/DJI_0004.MP4";
    touch(&lib.join("party/.reel"), 0, 1, 1);
    touch(&lib.join("2026-05-26_to_06-12/.reel"), 0, 1, 1);
    let keep = lib.join("party").join(rel);
    let dup = lib.join("2026-05-26_to_06-12").join(rel);
    touch(&keep, 0xAA, sz, 1_700_000_000);
    touch(&dup, 0xAA, sz, 1_700_000_000);
    // the redundant trip's baseline lists the clip — pruning must drop that row
    fs::create_dir_all(state.join("synced")).unwrap();
    fs::write(
        state.join("synced/2026-05-26_to_06-12.tsv"),
        format!("{rel}\t{sz}\n"),
    )
    .unwrap();

    let res = reel_core::dedup::resolve(
        &c,
        vec![reel_core::model::DupResolution {
            keep: dl("party", rel, true, false, sz as u64),
            remove: vec![dl("2026-05-26_to_06-12", rel, true, false, sz as u64)],
        }],
        |_| {},
    )
    .unwrap();

    assert_eq!(res.removed_local, 1);
    assert_eq!(res.freed, sz as u64);
    assert!(keep.is_file(), "canonical copy kept");
    assert!(!dup.is_file(), "redundant copy pruned");
    // pruning is NOT a permanent delete: the content survives, so no tombstone
    let tombs = state.join("deleted.tsv");
    assert!(
        !tombs.exists() || fs::read_to_string(&tombs).unwrap().trim().is_empty(),
        "a prune must not tombstone the content id"
    );
    let base = fs::read_to_string(state.join("synced/2026-05-26_to_06-12.tsv")).unwrap();
    assert!(!base.contains("DJI_0004"), "baseline drops the pruned clip");
}

#[test]
fn dedup_prune_wont_delete_when_canonical_unverifiable() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let c = cfg(&lib, &state, None); // remote unreachable → the cloud copy can't be confirmed

    let rel = "jaeho/dji/DJI_0004.MP4";
    touch(&lib.join("party/.reel"), 0, 1, 1);
    let only_local = lib.join("party").join(rel);
    touch(&only_local, 0xAA, 4_000_000, 1_700_000_000);

    // keep is cloud-only while offline → the survivor is unverifiable, so nothing
    // may be deleted.
    let res = reel_core::dedup::resolve(
        &c,
        vec![reel_core::model::DupResolution {
            keep: dl("some-cloud-trip", rel, false, true, 4_000_000),
            remove: vec![dl("party", rel, true, false, 4_000_000)],
        }],
        |_| {},
    )
    .unwrap();

    assert_eq!(res.removed_local, 0);
    assert_eq!(res.skipped, 1);
    assert!(
        only_local.is_file(),
        "the only reachable copy is left intact"
    );
}

#[test]
fn dedup_prune_skips_name_size_collision_with_different_content() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let c = cfg(&lib, &state, None);

    let sz = 4_000_000usize;
    let rel = "jaeho/dji/DJI_0004.MP4";
    touch(&lib.join("party/.reel"), 0, 1, 1);
    touch(&lib.join("grad/.reel"), 0, 1, 1);
    let keep = lib.join("party").join(rel);
    let other = lib.join("grad").join(rel);
    touch(&keep, 0xAA, sz, 1_700_000_000); // same name + size…
    touch(&other, 0xBB, sz, 1_700_000_000); // …but different content

    let res = reel_core::dedup::resolve(
        &c,
        vec![reel_core::model::DupResolution {
            keep: dl("party", rel, true, false, sz as u64),
            remove: vec![dl("grad", rel, true, false, sz as u64)],
        }],
        |_| {},
    )
    .unwrap();

    assert_eq!(
        res.removed_local, 0,
        "content differs → not a real duplicate"
    );
    assert_eq!(res.skipped, 1);
    assert!(other.is_file(), "the differing file is kept");
}

// A permanent delete of *pulled* footage must not undo itself on the next sync.
// The owner's cloud copy legitimately stays up there, so the clip has to keep its
// baseline row: drop it and the compare reads `L✗ B✗ R✓` for someone else's
// footage, which classifies as "to pull" — and `reconcile_all` (the topbar Sync)
// pulls without asking, silently re-downloading what you just deleted.
#[test]
fn deleting_pulled_footage_does_not_offer_to_pull_it_back() {
    if !have_rclone() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let (_lib, cloud, c, trip) = sync_env(d.path());

    // a friend's clip: in the cloud, and pulled down locally
    let master = trip.join("alice/dji/DJI_0050.MP4");
    touch(&master, 7, 1_000_000, 1_700_000_100);
    touch(
        &cloud.join("bali/alice/dji/DJI_0050.MP4"),
        7,
        1_000_000,
        1_700_000_100,
    );
    reel_core::push_trip(&c, "bali", |_| {}).expect("push");
    // a live refresh backfills alice's clip into the baseline (present both sides)
    let s = reel_core::sync_status(&c, "bali", true).expect("status");
    assert!(s.in_sync, "starts in sync");

    let r = reel_core::delete_clips(&c, &[master.display().to_string()]).expect("delete");
    assert_eq!(r.kept_cloud, 1, "a friend's cloud copy is never removed");
    assert!(!master.exists(), "local copy gone");
    assert!(
        cloud.join("bali/alice/dji/DJI_0050.MP4").is_file(),
        "her cloud copy survives"
    );

    let s = reel_core::sync_status(&c, "bali", true).expect("status");
    assert!(
        s.to_pull.is_empty(),
        "a permanently-deleted pulled clip must not come back as 'to pull'"
    );
    assert_eq!(
        s.cloud_only.len(),
        1,
        "it reads as cloud-only — in the cloud, not on this machine"
    );
}

// Dedup prunes your redundant copies, but a duplicate under someone else's
// `person/` folder is their contribution: the local copy goes, their cloud copy
// stays (the rule `remove::delete_clips` already follows). Its baseline row must
// survive too, or the next sync would offer to pull the clip back.
#[test]
fn dedup_prune_never_removes_a_friends_pool_copy() {
    if !have_rclone() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let cloud = d.path().join("cloud");
    fs::create_dir_all(&state).unwrap();
    let mut c = cfg(&lib, &state, None);
    c.remote = cloud.display().to_string(); // a reachable cloud, so a delete would land

    let sz = 3_000_000usize;
    let rel = "alice/dji/DJI_0050.MP4";
    make_trip(&lib, "party");
    make_trip(&lib, "bali");
    let keep = lib.join("party").join(rel);
    let dup = lib.join("bali").join(rel);
    touch(&keep, 0xBB, sz, 1_700_000_000);
    touch(&dup, 0xBB, sz, 1_700_000_000);
    // her copy really is up in the cloud
    let in_cloud = cloud.join("bali").join(rel);
    touch(&in_cloud, 0xBB, sz, 1_700_000_000);
    fs::create_dir_all(state.join("synced")).unwrap();
    fs::write(state.join("synced/bali.tsv"), format!("{rel}\t{sz}\n")).unwrap();

    let res = reel_core::dedup::resolve(
        &c,
        vec![reel_core::model::DupResolution {
            keep: dl("party", rel, true, false, sz as u64),
            // the redundant copy is local AND in the cloud, under alice's folder
            remove: vec![dl("bali", rel, true, true, sz as u64)],
        }],
        |_| {},
    )
    .unwrap();

    assert_eq!(res.removed_local, 1, "the redundant local copy is pruned");
    assert_eq!(res.removed_cloud, 0, "her cloud copy is never deleted");
    assert_eq!(res.kept_cloud, 1, "and it's reported as kept");
    assert!(
        in_cloud.is_file(),
        "her actual cloud bytes survive the prune"
    );
    assert!(!dup.is_file(), "local duplicate gone");
    assert!(keep.is_file(), "canonical kept");
    let base = fs::read_to_string(state.join("synced/bali.tsv")).unwrap();
    assert!(
        base.contains("DJI_0050"),
        "a friend's in_cloud clip keeps its baseline row, so sync won't re-pull it"
    );
}

// Every Tauri command runs on its own blocking thread with no coordination, so a
// background sweep and a user action read-modify-write the same state files at the
// same time. The atomic write stops a *torn* file but not a lost update: both load,
// both mutate, the last save wins. A dropped baseline row self-heals on the next
// refresh — a dropped tombstone lets deleted footage re-import, and a dropped
// ledger row loses an import. This is the guard for that.
#[test]
fn concurrent_state_updates_dont_lose_rows() {
    use reel_core::ledger::{Ledger, LedgerRow, Tombstones};

    let d = tempfile::tempdir().unwrap();
    let state = d.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let c = cfg(&d.path().join("Videos"), &state, None);

    const N: usize = 24;
    std::thread::scope(|s| {
        for i in 0..N {
            let c = &c;
            s.spawn(move || {
                let id = format!("id-{i:03}");
                Tombstones::update(&c.tombstones_path(), |t| t.insert(&id)).unwrap();
                Ledger::update(&c.ledger_path(), |l| {
                    l.upsert(LedgerRow {
                        id: id.clone(),
                        trip: "bali".into(),
                        person: "jaeho".into(),
                        camera: "dji".into(),
                        base: format!("DJI_{i:04}.MP4"),
                        bytes: "1".into(),
                        captured: "0".into(),
                        imported_at: "0".into(),
                    })
                })
                .unwrap();
            });
        }
    });

    let tombs = Tombstones::load(&c.tombstones_path());
    let lost_tombs: Vec<String> = (0..N)
        .map(|i| format!("id-{i:03}"))
        .filter(|id| !tombs.contains(id))
        .collect();
    assert!(
        lost_tombs.is_empty(),
        "{} of {N} concurrent tombstones were lost: {lost_tombs:?}",
        lost_tombs.len()
    );

    let ledger = Ledger::load(&c.ledger_path());
    let lost_rows: Vec<String> = (0..N)
        .map(|i| format!("id-{i:03}"))
        .filter(|id| ledger.trip_of(id).is_none())
        .collect();
    assert!(
        lost_rows.is_empty(),
        "{} of {N} concurrent ledger rows were lost: {lost_rows:?}",
        lost_rows.len()
    );
}

// ---- stills: a grabbed frame is an ordinary capture ----

/// The ffmpeg decode is skipped when the frame already exists (the grab is
/// idempotent per millisecond), which lets the rest of the contract — ledger row,
/// filmstrip ordering, discovery as a capture — be tested without a real video.
#[test]
fn a_grabbed_still_joins_the_trip_as_a_capture() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("lib");
    let state = d.path().join("state");
    let c = cfg(&lib, &state, None);

    let master = lib.join("DOHA/jaeho/DJI/DJI_0030_D.MP4");
    touch(&master, 0xAB, 5_000_000, 1_700_000_000);
    // stand in for the ffmpeg output at exactly the path grab_still derives
    let still = lib.join("DOHA/jaeho/DJI/DJI_0030_D_t12480.jpg");
    touch(&still, 0x11, 4096, 1);

    let r = reel_core::grab_still(&c, &master, 12.48).unwrap();
    assert_eq!(r.name, "DJI_0030_D_t12480.jpg");
    assert_eq!(r.path, still.to_string_lossy());

    // ledgered under the master's own trip/person/camera, so it's tracked like an import
    let row = Ledger::load(&c.ledger_path()).row_of(&r.fileid).cloned();
    let row = row.expect("a still must be recorded on the ledger");
    assert_eq!(
        (row.trip.as_str(), row.person.as_str(), row.camera.as_str()),
        ("DOHA", "jaeho", "DJI")
    );

    // mtime = the moment in the footage, so masters_in sorts it right after its source
    assert_eq!(captured_at(&still), 1_700_000_012);
    let found = reel_core::media::masters_in(&lib.join("DOHA"));
    assert_eq!(
        found,
        vec![master.clone(), still.clone()],
        "the still must be discovered as a capture, ordered after its source"
    );

    // a repeat grab of the same frame is the same file, not a second picture
    let again = reel_core::grab_still(&c, &master, 12.4801).unwrap();
    assert_eq!(again.path, r.path);
    assert_eq!(reel_core::media::masters_in(&lib.join("DOHA")).len(), 2);
}

#[test]
fn a_still_is_refused_outside_the_library() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("lib");
    let state = d.path().join("state");
    fs::create_dir_all(&lib).unwrap();
    let c = cfg(&lib, &state, None);

    let outside = d.path().join("elsewhere/DJI_0001.MP4");
    touch(&outside, 0xAB, 1000, 1_700_000_000);
    let err = reel_core::grab_still(&c, &outside, 1.0).unwrap_err();
    assert!(err.contains("isn't in the library"), "got: {err}");
}

// ---- archived is an intent, not an inference ----

/// The case the old "no masters + some clips" guess could never see: freeing a
/// trip you never cut. That read as `Empty` — indistinguishable from a trip with
/// nothing in it — which, now that archived trips are filed off the dashboard,
/// would quietly lose the trip instead of just mislabelling it.
#[test]
fn an_archive_with_no_cut_clips_still_reads_as_archived() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let c = cfg(&lib, &state, None);

    let trip = lib.join("bali");
    fs::create_dir_all(&trip).unwrap();
    fs::write(trip.join(".reel"), "reel project\narchived=1\n").unwrap();

    assert_eq!(list_trips(&c)[0].state, TripState::Archived);
}

/// A library archived before the marker existed must not read as `Empty` after the
/// upgrade — the old inference stays as a fallback.
#[test]
fn a_trip_archived_before_the_marker_still_reads_as_archived() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let c = cfg(&lib, &state, None);

    let trip = lib.join("bali");
    fs::create_dir_all(&trip).unwrap();
    fs::write(trip.join(".reel"), "reel project\n").unwrap(); // no archived= line
    touch(&trip.join("clips/DJI_0001_c01.mp4"), 7, 2000, 1_700_000_000);

    assert_eq!(list_trips(&c)[0].state, TripState::Archived);
}

/// A stale marker must not strand a trip whose footage is back: `restore` clears
/// it, but the state ignores it while masters exist either way.
#[test]
fn footage_on_disk_overrides_a_stale_archived_marker() {
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let c = cfg(&lib, &state, None);

    let trip = lib.join("bali");
    fs::create_dir_all(&trip).unwrap();
    fs::write(trip.join(".reel"), "reel project\narchived=1\n").unwrap();
    touch(
        &trip.join("jaeho/dji/DJI_0001.MP4"),
        1,
        2_000_000,
        1_700_000_000,
    );

    let t = &list_trips(&c)[0];
    assert_eq!(t.state, TripState::Imported, "raw is back — not archived");
}

/// An archived trip kept its cover and dates: the ledger remembers every capture
/// and the poster cache is keyed by content id *outside* the trip, so freeing the
/// disk shouldn't blank the card as though the footage never existed.
#[test]
fn an_archived_trip_keeps_its_cover_and_dates() {
    use reel_core::ledger::LedgerRow;
    let d = tempfile::tempdir().unwrap();
    let lib = d.path().join("Videos");
    let state = d.path().join("state");
    let c = cfg(&lib, &state, None);

    let trip = lib.join("bali");
    fs::create_dir_all(&trip).unwrap();
    fs::write(trip.join(".reel"), "reel project\narchived=1\n").unwrap();

    // what the ledger still knows about footage that's no longer on disk
    Ledger::update(&c.ledger_path(), |l| {
        for (i, at) in [1_700_000_900i64, 1_700_000_100, 1_700_000_500]
            .into_iter()
            .enumerate()
        {
            l.upsert(LedgerRow {
                id: format!("id-{i}"),
                trip: "bali".into(),
                person: "jaeho".into(),
                camera: "dji".into(),
                base: format!("DJI_000{i}.MP4"),
                bytes: "2000000".into(),
                captured: at.to_string(),
                imported_at: "0".into(),
            });
        }
    })
    .unwrap();

    let t = &list_trips(&c)[0];
    assert_eq!(t.state, TripState::Archived);
    assert_eq!(
        t.cover.as_ref().map(|x| x.fileid.as_str()),
        Some("id-1"),
        "cover is the earliest capture, by the id its poster is cached under"
    );
    assert_eq!(
        t.start,
        Some(1_700_000_100),
        "trip still knows when it began"
    );
    assert_eq!(t.end, Some(1_700_000_900), "and when it ended");
}

// ---- kdenlive timeline: every mark on one editable project ----

/// The arithmetic the whole export rests on. Seconds in `marks.tsv` become frame
/// numbers in MLT, and MLT's `out` is *inclusive* — so a naive `end - start`
/// loses a frame on every segment, and a naive `fps = 30` misplaces every mark on
/// footage that isn't 30 fps. Both are silent failures: the timeline still opens,
/// it's just wrong. Rate here is 24000/1001, which is what every DJI master in
/// this library actually reports.
#[test]
fn a_marks_seconds_become_inclusive_frames_at_the_masters_real_rate() {
    use reel_core::timeline::segment_of;
    let fps = 24000.0 / 1001.0; // 23.976…
    let m = Path::new("/lib/party/jaeho/dji/DJI_0004.MP4");
    let s = segment_of(m, "hl", 70.920, 80.920, fps, 30_000);

    assert_eq!(
        (s.in_f, s.out_f),
        (1700, 1939),
        "start/end land on the frames the player was showing"
    );
    assert_eq!(
        s.frames(),
        240,
        "inclusive out: a 10s mark is 240 frames at 23.976, not 239"
    );
}

/// A mark can outlive its source — the trip's raw was replaced by a shorter
/// re-encode, or the mark was written against a file the camera never finished.
/// Clamping keeps the segment inside the media; extrapolating past it produces a
/// project Kdenlive opens with a red "invalid duration" clip.
#[test]
fn a_mark_past_the_end_of_its_master_is_clamped_not_extrapolated() {
    use reel_core::timeline::segment_of;
    let m = Path::new("/lib/t/p/c.MP4");
    let s = segment_of(m, "", 1.0, 999.0, 30.0, 100); // source is frames 0..99
    assert_eq!(s.out_f, 99, "never past the last frame");
    assert!(s.in_f <= s.out_f, "and never inverted");

    // A zero-width mark is still a clip, not a zero-frame hole in the playlist.
    let z = segment_of(m, "", 5.0, 5.0, 30.0, 1000);
    assert_eq!(z.frames(), 1);
}

/// `marks.tsv` is in whatever order the player last wrote it. A timeline that
/// jumps around in time reads as a bug even when every segment is correct, so the
/// export re-sorts by when the footage was shot.
#[test]
fn the_timeline_runs_in_capture_order_not_file_order() {
    use reel_core::model::Mark;
    use reel_core::timeline::order_marks;
    use std::collections::HashMap;

    let mk = |p: &str, start: f64| Mark {
        master: p.to_string(),
        start,
        end: start + 5.0,
        label: "hl".into(),
    };
    // written newest-first, and the middle clip has two marks out of order
    let mut marks = vec![
        mk("/lib/t/late.MP4", 1.0),
        mk("/lib/t/mid.MP4", 40.0),
        mk("/lib/t/mid.MP4", 10.0),
        mk("/lib/t/early.MP4", 1.0),
    ];
    let captured: HashMap<PathBuf, i64> = [
        (PathBuf::from("/lib/t/early.MP4"), 1_700_000_000),
        (PathBuf::from("/lib/t/mid.MP4"), 1_700_000_500),
        (PathBuf::from("/lib/t/late.MP4"), 1_700_000_900),
    ]
    .into_iter()
    .collect();

    order_marks(&mut marks, &captured);
    let order: Vec<(&str, f64)> = marks
        .iter()
        .map(|m| (m.master.rsplit('/').next().unwrap(), m.start))
        .collect();
    assert_eq!(
        order,
        vec![
            ("early.MP4", 1.0),
            ("mid.MP4", 10.0),
            ("mid.MP4", 40.0),
            ("late.MP4", 1.0)
        ],
        "by capture time, then by position within the clip"
    );
}

/// A trip mixes formats for real — 4K drone at 23.976 next to a 30 fps phone clip.
/// One project has one profile, so it goes to whichever format most of the
/// *timeline* is in.
#[test]
fn the_project_profile_follows_the_majority_of_the_marked_footage() {
    use reel_core::timeline::{choose_profile, Profile};
    let drone = Profile {
        width: 3840,
        height: 2160,
        fps_num: 24000,
        fps_den: 1001,
        sar_num: 1,
        sar_den: 1,
    };
    let phone = Profile {
        width: 1280,
        height: 720,
        fps_num: 30,
        fps_den: 1,
        sar_num: 1,
        sar_den: 1,
    };
    let picked = choose_profile(&[phone.clone(), drone.clone(), drone.clone()]);
    assert_eq!(picked, drone, "two drone marks outvote one phone mark");
    assert_eq!(picked.describe(), "3840×2160 · 23.98 fps");

    // Portrait drone footage is in this library too; a hardcoded 16:9 would squash it.
    let portrait = Profile {
        width: 1080,
        height: 1920,
        ..drone.clone()
    };
    assert_eq!(portrait.dar(), (9, 16), "display aspect follows the pixels");

    // Nothing probed at all still yields a usable project rather than a panic.
    assert_eq!(choose_profile(&[]), Profile::default());
}

/// The regression that makes the whole approach worth it. A bin clip must span the
/// **whole master**, not the span of its marks — Kdenlive won't let you drag an
/// edge past the end of the media, so a clip capped at the last mark is a clip
/// whose edges only move inward. That is precisely the limitation of the cut files
/// this export exists to avoid, and it renders identically either way, so nothing
/// but reading the XML catches it.
#[test]
fn a_bin_clip_spans_its_whole_master_so_edges_stay_draggable() {
    use reel_core::timeline::{segment_of, timeline_xml, Profile, Source};
    let m = PathBuf::from("/lib/party/jaeho/dji/DJI_0004.MP4");
    let fps = 24000.0 / 1001.0;
    // a real one: 2228-frame master, mark covering frames 1700..1939
    let seg = segment_of(&m, "hl", 70.920, 80.920, fps, 2228);
    let src = Source {
        master: m.clone(),
        name: "DJI_0004.MP4".into(),
        length_f: 2228,
        has_audio: true,
        proxy: None,
    };
    let xml = timeline_xml(
        "reel: party",
        Path::new("/lib/party"),
        &Profile::default(),
        None,
        &[src],
        &[seg],
    );

    assert!(
        xml.contains(r#"<entry producer="bin2" in="0" out="2227"/>"#),
        "the bin clip must reach the end of the media, not the end of the mark"
    );
    assert!(
        xml.contains(r#"<property name="length">2228</property>"#),
        "and the producer must declare the master's real length"
    );
    // while the timeline segment stays exactly where the mark was
    assert!(
        xml.contains(r#"in="1700" out="1939""#),
        "mark placement unchanged"
    );
}

/// Labels are user text and this library really does hold masters with spaces in
/// their names. An unescaped `&` in a label is a file no XML parser will open —
/// the timeline would simply fail to load, with the label as the only clue.
#[test]
fn labels_and_filenames_survive_the_xml_and_json_layers() {
    use reel_core::timeline::{segment_of, timeline_xml, Profile, Source};
    let m = PathBuf::from("/lib/t/jack/26-06-20 14-36-45 4297.mov");
    let seg = segment_of(&m, r#"Tyler & "Jack" <best>"#, 1.0, 3.0, 30.0, 900);
    let src = Source {
        master: m.clone(),
        name: "26-06-20 14-36-45 4297.mov".into(),
        length_f: 900,
        has_audio: true,
        proxy: None,
    };
    let xml = timeline_xml(
        "reel: t",
        Path::new("/lib/t"),
        &Profile::default(),
        None,
        &[src],
        &[seg],
    );

    // no raw markup escaped into the document
    assert!(
        !xml.contains("& \""),
        "a raw ampersand would break the parse"
    );
    assert!(xml.contains("Tyler &amp;"), "ampersand escaped for XML");
    assert!(xml.contains("&lt;best&gt;"), "angle brackets escaped");
    // The guide rides inside a JSON string inside an XML text node, so BOTH layers
    // apply and in that order: JSON turns `"` into `\"`, then XML turns the quote
    // itself into `&quot;`, leaving `\&quot;`. Unescaping the XML hands a parser
    // back valid JSON. Kdenlive's own files look exactly like this.
    assert!(
        xml.contains(r#"\&quot;Jack\&quot;"#),
        "quotes escaped for JSON first, then for XML"
    );
    assert!(
        xml.contains(r#"&quot;comment&quot;"#),
        "the JSON structure itself is XML-escaped, as Kdenlive writes it"
    );
    // the space in the path is fine unescaped, but must be present and intact
    assert!(xml.contains("26-06-20 14-36-45 4297.mov"));
}

/// A silent master (a still grabbed with `s`, or a clip whose audio track the
/// camera never wrote) must leave a **gap** on the audio track, not be skipped.
/// Skipping slides every later audio clip earlier, and the whole track drifts out
/// of sync with the picture — a failure you'd hear but never see in the XML.
#[test]
fn a_silent_source_leaves_a_gap_rather_than_desyncing_the_audio_track() {
    use reel_core::timeline::{segment_of, timeline_xml, Profile, Source};
    let quiet = PathBuf::from("/lib/t/p/quiet.MP4");
    let loud = PathBuf::from("/lib/t/p/loud.MP4");
    let segs = vec![
        segment_of(&quiet, "", 0.0, 2.0, 30.0, 900), // 60 frames, no audio
        segment_of(&loud, "", 0.0, 1.0, 30.0, 900),  // 30 frames, with audio
    ];
    let sources = vec![
        Source {
            master: quiet,
            name: "quiet.MP4".into(),
            length_f: 900,
            has_audio: false,
            proxy: None,
        },
        Source {
            master: loud,
            name: "loud.MP4".into(),
            length_f: 900,
            has_audio: true,
            proxy: None,
        },
    ];
    let xml = timeline_xml(
        "reel: t",
        Path::new("/lib/t"),
        &Profile::default(),
        None,
        &sources,
        &segs,
    );

    assert!(
        xml.contains(r#"<blank length="60"/>"#),
        "the silent clip holds its 60 frames open on the audio track"
    );
    assert!(
        !xml.contains(r#"producer="a2""#),
        "and no audio producer is emitted for a source that has none"
    );
    assert!(
        xml.contains(r#"producer="a3""#),
        "while the source that does have audio still gets its clip"
    );
}

/// Kdenlive 23.08+ opens a *sequence*: a tractor keyed by UUID, tagged
/// `producer_type=17`, listed in the bin and pointed at by `activetimeline`. The
/// older flat layout is perfectly valid MLT — `melt` renders it without a
/// complaint — but Kdenlive hangs on load rather than opening it. So `melt` is not
/// an acceptance test for this file, and these are the markers that distinguish
/// "renders" from "opens".
#[test]
fn the_project_is_a_sequence_which_is_what_kdenlive_actually_opens() {
    use reel_core::timeline::{segment_of, timeline_xml, Profile, Source};
    let m = PathBuf::from("/lib/t/p/c.MP4");
    let seg = segment_of(&m, "sunset", 1.0, 3.0, 30.0, 900);
    let src = Source {
        master: m.clone(),
        name: "c.MP4".into(),
        length_f: 900,
        has_audio: true,
        proxy: None,
    };
    let xml = timeline_xml(
        "reel: t",
        Path::new("/lib/t"),
        &Profile::default(),
        None,
        &[src],
        &[seg],
    );

    assert!(xml.contains(r#"<property name="kdenlive:producer_type">17</property>"#));
    assert!(xml.contains(r#"<property name="kdenlive:docproperties.version">1.1</property>"#));
    // the sequence uuid has to be the same string in all three places or Kdenlive
    // opens a project whose active timeline points at nothing
    let uuid = xml
        .split(r#"<property name="kdenlive:uuid">"#)
        .nth(1)
        .and_then(|s| s.split('<').next())
        .expect("a sequence uuid")
        .to_string();
    assert!(
        uuid.starts_with('{') && uuid.ends_with('}') && uuid.len() == 38,
        "got {uuid}"
    );
    assert!(
        xml.contains(&format!(r#"<tractor id="{uuid}""#)),
        "sequence element"
    );
    assert!(
        xml.contains(&format!(
            r#"<property name="kdenlive:docproperties.activetimeline">{uuid}</property>"#
        )),
        "activetimeline points at the sequence"
    );
    assert!(
        xml.contains(&format!(r#"<entry producer="{uuid}""#)),
        "sequence is a bin item"
    );
    assert!(
        xml.contains(&format!(r#"<track producer="{uuid}""#)),
        "project tractor plays it"
    );

    // Guides belong to the SEQUENCE. `docproperties.guides` is a legacy upgrade
    // target: writing there loses every guide silently, which is the whole
    // user-visible point of the export.
    assert!(
        xml.contains(
            r#"kdenlive:sequenceproperties.guides">[{&quot;comment&quot;:&quot;sunset&quot;"#
        ),
        "guides ride on the sequence, XML-escaped"
    );
    assert!(
        !xml.contains("docproperties.guides"),
        "never the legacy location"
    );
}

/// UUIDs are derived from a stable seed, not randomised or clock-based, so
/// rebuilding a trip's timeline is byte-identical. Re-running `edit` shouldn't
/// rewrite the project into something a diff calls "changed everywhere".
#[test]
fn rebuilding_the_same_timeline_produces_the_same_file() {
    use reel_core::timeline::{segment_of, timeline_xml, Profile, Source};
    let m = PathBuf::from("/lib/t/p/c.MP4");
    let seg = segment_of(&m, "hl", 1.0, 3.0, 30.0, 900);
    let src = Source {
        master: m.clone(),
        name: "c.MP4".into(),
        length_f: 900,
        has_audio: true,
        proxy: None,
    };
    let once = timeline_xml(
        "reel: t",
        Path::new("/lib/t"),
        &Profile::default(),
        None,
        std::slice::from_ref(&src),
        std::slice::from_ref(&seg),
    );
    let twice = timeline_xml(
        "reel: t",
        Path::new("/lib/t"),
        &Profile::default(),
        None,
        &[src],
        &[seg],
    );
    assert_eq!(once, twice);
    // and a different trip is genuinely a different project
    let other = timeline_xml(
        "reel: other",
        Path::new("/lib/o"),
        &Profile::default(),
        None,
        &[],
        &[],
    );
    assert_ne!(once, other);
}

/// `kdenlive:docproperties.profile` wants a profile *identifier* (`atsc_1080p_2398`),
/// not a human description — and there is no stock profile for most of what these
/// cameras shoot (3840x2160 or 1080x1920 at 24000/1001). An unresolvable name makes
/// Kdenlive block on a "choose a profile" modal at load: offscreen that looks like a
/// hang, in the GUI it's a dialog standing between you and your project.
///
/// The reason this is a test and not a comment: the failure is *conditional*. A
/// bogus name is harmless at 30/1, because Kdenlive quietly matches a stock profile,
/// and fatal at 24000/1001. Every synthetic 30 fps fixture passed while every real
/// trip in this library — all DJI, all 24000/1001 — would have hung.
#[test]
fn the_project_never_claims_a_profile_name_kdenlive_cannot_resolve() {
    use reel_core::timeline::{segment_of, timeline_xml, Profile, Source};
    let m = PathBuf::from("/lib/party/jaeho/dji/DJI_0004.MP4");
    let drone = Profile {
        width: 3840,
        height: 2160,
        fps_num: 24000,
        fps_den: 1001,
        sar_num: 1,
        sar_den: 1,
    };
    let seg = segment_of(&m, "hl", 1.0, 3.0, drone.fps(), 5000);
    let src = Source {
        master: m,
        name: "DJI_0004.MP4".into(),
        length_f: 5000,
        has_audio: true,
        proxy: None,
    };
    let xml = timeline_xml(
        "reel: party",
        Path::new("/lib/party"),
        &drone,
        None,
        &[src],
        &[seg],
    );

    assert!(
        !xml.contains("docproperties.profile"),
        "never write a profile identifier that may not resolve"
    );
    // The <profile> element is what actually states the format, and must carry the
    // real fractional rate rather than a rounded one.
    assert!(
        xml.contains(r#"frame_rate_num="24000" frame_rate_den="1001""#),
        "the real rate, unrounded"
    );
    assert!(xml.contains(r#"width="3840" height="2160""#));
}

/// `kdenlive:docproperties.profile` must name a profile Kdenlive can resolve, so
/// the name is looked up in MLT's profile set rather than invented. The matching
/// rule is exact — a profile that agrees on size but not frame rate would conform
/// the footage and change its duration, which is not a near-miss worth taking.
#[test]
fn the_profile_id_is_an_exact_match_or_nothing() {
    use reel_core::timeline::{match_profile, Profile};
    let drone = Profile {
        width: 3840,
        height: 2160,
        fps_num: 24000,
        fps_den: 1001,
        sar_num: 1,
        sar_den: 1,
    };
    let candidates = vec![
        ("uhd_2160p_2398".to_string(), drone.clone()),
        (
            "uhd_2160p_30".to_string(),
            Profile {
                fps_num: 30,
                fps_den: 1,
                ..drone.clone()
            },
        ),
        (
            "atsc_1080p_2398".to_string(),
            Profile {
                width: 1920,
                height: 1080,
                ..drone.clone()
            },
        ),
    ];
    assert_eq!(
        match_profile(&candidates, &drone).as_deref(),
        Some("uhd_2160p_2398")
    );

    // Portrait drone footage at 23.98 matches no stock profile. `None` is the right
    // answer: the caller then tells the user to set it, rather than emitting a name
    // that hangs Kdenlive on a modal or letting it default to PAL in silence.
    let portrait = Profile {
        width: 1080,
        height: 1920,
        ..drone.clone()
    };
    assert_eq!(match_profile(&candidates, &portrait), None);

    // Same size, different rate is NOT a match.
    let same_size_other_rate = Profile {
        fps_num: 25,
        fps_den: 1,
        ..drone
    };
    assert_eq!(match_profile(&candidates, &same_size_other_rate), None);
}

/// Kdenlive **segfaults** on load if `kdenlive:proxy` names a file that isn't
/// there — measured, reproducibly, and it happens whether `resource` points at the
/// proxy or the master. Proxies are a cache (`<trip>/.proxies/`, swept by archive,
/// deletable by hand), so the only safe rule is: never write the property unless
/// the file was present when the project was built. `build_timeline` sets
/// `Source.proxy` from an `is_file()` check; this pins the emitter to it.
#[test]
fn a_proxy_is_only_referenced_when_it_actually_exists() {
    use reel_core::timeline::{segment_of, timeline_xml, Profile, Source};
    let m = PathBuf::from("/lib/t/jaeho/dji/clip.MP4");
    let seg = segment_of(&m, "hl", 1.0, 3.0, 30.0, 900);
    let bare = Source {
        master: m.clone(),
        name: "clip.MP4".into(),
        length_f: 900,
        has_audio: true,
        proxy: None,
    };
    let xml = timeline_xml(
        "reel: t",
        Path::new("/lib/t"),
        &Profile::default(),
        None,
        std::slice::from_ref(&bare),
        std::slice::from_ref(&seg),
    );
    assert!(
        !xml.contains("kdenlive:proxy"),
        "no proxy property without a proxy"
    );
    assert!(
        !xml.contains("kdenlive:originalurl"),
        "and no originalurl either"
    );
    assert!(
        xml.contains(r#"name="resource">/lib/t/jaeho/dji/clip.MP4"#),
        "resource is the master"
    );
    assert!(
        xml.contains(r#"kdenlive:docproperties.enableproxy">0"#),
        "proxies off when there are none"
    );

    // With one: resource swaps to the proxy, the master rides in originalurl so
    // Kdenlive renders full quality from it, and proxying is switched on.
    let proxied = Source {
        proxy: Some(PathBuf::from("/lib/t/.proxies/jaeho__dji__clip.mp4")),
        ..bare
    };
    let xml = timeline_xml(
        "reel: t",
        Path::new("/lib/t"),
        &Profile::default(),
        None,
        &[proxied],
        &[seg],
    );
    assert!(xml.contains(r#"name="resource">/lib/t/.proxies/jaeho__dji__clip.mp4"#));
    assert!(xml.contains(r#"kdenlive:originalurl">/lib/t/jaeho/dji/clip.MP4"#));
    assert!(xml.contains(r#"kdenlive:docproperties.enableproxy">1"#));
}

/// Kdenlive refuses a fractional frame rate on non-standard geometry — portrait
/// drone footage at 1080x1920 @ 23.98 — with a modal on *every* open, and no custom
/// profile avoids it (tested in Kdenlive's own file format, in its own profile
/// directory, under several names). So when nothing standard matches, the rate is
/// rounded to the nearest whole number and every frame position is computed against
/// that, keeping clips in step with each other.
///
/// Footage that *does* match a stock profile must be left exactly alone — 4K
/// landscape at 23.98 is `uhd_2160p_2398` and conforming it would be a pointless
/// loss of precision.
#[test]
fn only_footage_kdenlive_cannot_open_gets_its_rate_conformed() {
    use reel_core::timeline::{conform, Profile};

    // Portrait at 23.98: no stock profile, fractional rate → rounded to 24.
    let portrait = Profile {
        width: 1080,
        height: 1920,
        fps_num: 24000,
        fps_den: 1001,
        sar_num: 1,
        sar_den: 1,
    };
    let (got, id, conformed) = conform(&portrait);
    assert!(
        conformed,
        "portrait 23.98 has to be conformed or it won't open"
    );
    assert_eq!(
        (got.fps_num, got.fps_den),
        (24, 1),
        "nearest whole rate, not 25 or 30"
    );
    assert_eq!(
        (got.width, got.height),
        (1080, 1920),
        "geometry is never touched"
    );
    assert!(
        id.is_none(),
        "still no stock profile — but an integer rate opens clean"
    );

    // 4K landscape at 23.98 IS stock. Leave it exactly as shot.
    let uhd = Profile {
        width: 3840,
        height: 2160,
        ..portrait.clone()
    };
    let (got, id, conformed) = conform(&uhd);
    assert!(
        !conformed,
        "never conform footage Kdenlive already understands"
    );
    assert_eq!(
        (got.fps_num, got.fps_den),
        (24000, 1001),
        "left frame-exact"
    );
    assert_eq!(id.as_deref(), Some("uhd_2160p_2398"));

    // An integer rate on odd geometry is already fine — Kdenlive invents a profile.
    let odd = Profile {
        width: 640,
        height: 480,
        fps_num: 30,
        fps_den: 1,
        sar_num: 1,
        sar_den: 1,
    };
    let (got, _, conformed) = conform(&odd);
    assert!(!conformed);
    assert_eq!(got.fps(), 30.0);
}
