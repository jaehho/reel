//! Free a trip's local raw footage once it's safe in the shared cloud: confirm
//! every master is in the cloud, then delete the per-person camera trees (plus
//! `_sheets/`/`.proxies/`), keeping the cut clips, marks, and `.reel`. The trip
//! then reads "Archived" (recorded in `.reel`, see `commit_archive`), is filed
//! away from the dashboard, and its raw can be re-pulled. Mirrors the script's
//! `archive`.
//!
//! Two steps, like `wipe`: `plan_archive` verifies and reports what would be
//! freed (no deletion); `commit_archive` re-verifies — because it's deleting the
//! only local copies — and then frees the raw. The cloud check aborts the whole
//! operation on the first miss, so raw is never freed unless it's recoverable.

use crate::config::Config;
use crate::media::masters_in;
use crate::model::{ArchivePlan, ArchiveProgress, ArchiveResult};
use crate::rclone;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

fn valid_trip(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != ".."
}

/// Total size of the files under `dir`.
fn dir_bytes(dir: &Path) -> u64 {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// The top-level dirs that hold raw — the per-person camera trees, plus the
/// regenerable `_sheets/` and `.proxies/` caches. These are freed.
///
/// Deliberately *not* "everything but `clips/`". Only capture-extension files are
/// ever cloud-verified (`verify_all_in_cloud` checks `masters_in`), so a blanket
/// sweep deleted whatever else lived in the trip with no proof of a backup at all —
/// and since `build_timeline` writes its project into the trip root, keeping an
/// `audio/`, `titles/` or `renders/` folder beside it is the natural way to work.
/// A directory holding anything that isn't a master or a known cache stays.
fn raw_dirs(dir: &Path) -> Vec<PathBuf> {
    const CACHES: [&str; 2] = ["_sheets", ".proxies"];
    let mut v = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name == "clips" {
                continue;
            }
            if CACHES.contains(&name) || holds_only_masters(&p) {
                v.push(p);
            }
        }
    }
    v
}

/// The camera's own sidecars: its native proxy and thumbnail. Worthless without
/// the master they shadow, and the exact set `push` refuses to upload — so unlike
/// arbitrary files, "not in the cloud" is their normal state, not a warning.
const SIDECAR_EXT: &[&str] = &["lrf", "lrv", "thm"];

/// Is every file in this tree either a capture (so `verify_all_in_cloud` has
/// vouched for it) or one of those sidecars? An empty directory counts — there's
/// nothing in it to lose.
fn holds_only_masters(dir: &Path) -> bool {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .all(|e| {
            crate::media::has_ext(e.path(), crate::media::CAPTURE_EXT)
                || crate::media::has_ext(e.path(), SIDECAR_EXT)
        })
}

/// Resolve and sanity-check a trip dir, returning it with the masters archive is
/// actually concerned with: the ones inside the directories it would free.
///
/// Verified set and deleted set are the same set, deliberately. Checking *every*
/// master in the trip meant a stray capture somewhere push never uploads — a
/// `renders/final_v3.mov` beside the Kdenlive project — made the cloud check fail
/// forever, so the trip could never be archived at all. Scoping both to `raw_dirs`
/// keeps the guarantee exact in both directions: nothing is freed without proof,
/// and nothing unprovable blocks freeing what is proved.
fn trip_masters(cfg: &Config, trip: &str) -> Result<(PathBuf, Vec<PathBuf>), String> {
    if !valid_trip(trip) {
        return Err(format!("invalid trip name: {trip:?}"));
    }
    let dir = cfg.lib.join(trip);
    if !dir.join(".reel").is_file() {
        return Err(format!("no such trip: {trip}"));
    }
    let freed = raw_dirs(&dir);
    let masters: Vec<PathBuf> = masters_in(&dir)
        .into_iter()
        .filter(|m| freed.iter().any(|d| m.starts_with(d)))
        .collect();
    if masters.is_empty() {
        return Err(format!("no raw footage in '{trip}' to free"));
    }
    Ok((dir, masters))
}

/// Whether every master in `masters` hash-matches in `<remote>/<trip>`, streaming
/// rclone's per-file check progress. The `--files-from` list scopes it to exactly
/// the raw being considered. `Ok(false)` means "some are missing" — a normal
/// answer, not a failure; only a check that couldn't run is `Err`.
fn all_in_cloud(
    cfg: &Config,
    trip: &str,
    dir: &Path,
    masters: &[PathBuf],
    mut on: impl FnMut(ArchiveProgress),
) -> Result<bool, String> {
    fs::create_dir_all(&cfg.state_dir).ok();
    let rels: Vec<String> = masters
        .iter()
        .filter_map(|m| m.strip_prefix(dir).ok())
        .map(|r| r.to_string_lossy().into_owned())
        .collect();
    let list = cfg.state_dir.join(format!(".archive-check-{trip}.lst"));
    fs::write(&list, format!("{}\n", rels.join("\n")))
        .map_err(|e| format!("couldn't stage cloud check: {e}"))?;

    let remote_trip = format!("{}/{}", cfg.remote.trim_end_matches('/'), trip);
    let args: Vec<OsString> = vec![
        "check".into(),
        "--files-from".into(),
        list.as_os_str().to_os_string(),
        "--one-way".into(),
        dir.as_os_str().to_os_string(),
        remote_trip.into(),
    ];
    let ok = rclone::stream(args, |v| {
        let s = &v["stats"];
        on(ArchiveProgress {
            done: s["checks"].as_u64().unwrap_or(0),
            total: s["totalChecks"].as_u64().unwrap_or(0),
        });
    });
    let _ = fs::remove_file(&list);
    ok
}

/// The raw dirs safe to free: caches (regenerable) plus every per-person tree
/// whose masters are all confirmed in the cloud. A tree that *isn't* fully up
/// there — footage that was never shared, a local `renders/` push never touched —
/// is left in place rather than deleted unbacked (the old blanket sweep) or made
/// to block the whole archive (the old all-or-nothing check). Errors if nothing is
/// backed, so a plain "share it first" still surfaces.
fn verified_raw_dirs(
    cfg: &Config,
    trip: &str,
    dir: &Path,
    mut on: impl FnMut(ArchiveProgress),
) -> Result<Vec<PathBuf>, String> {
    rclone::remote_ok(&cfg.remote)?;
    let mut freeable = Vec::new();
    let mut any_backed = false;
    for d in raw_dirs(dir) {
        let name = d.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name == "_sheets" || name == ".proxies" {
            freeable.push(d);
            continue;
        }
        let masters = masters_in(&d);
        if masters.is_empty() {
            freeable.push(d); // sidecars only — nothing to lose
        } else if all_in_cloud(cfg, trip, dir, &masters, &mut on)? {
            freeable.push(d);
            any_backed = true;
        }
    }
    if !any_backed {
        return Err(format!(
            "not all of '{trip}' is in the cloud — Share it first; nothing freed"
        ));
    }
    Ok(freeable)
}

/// The masters inside a set of dirs, for the count in the result.
fn masters_within(dir: &Path, dirs: &[PathBuf]) -> usize {
    masters_in(dir)
        .into_iter()
        .filter(|m| dirs.iter().any(|d| m.starts_with(d)))
        .count()
}

/// Work out what archiving `trip` would free, confirming the raw is in the cloud
/// first. Nothing is deleted.
pub fn plan_archive(
    cfg: &Config,
    trip: &str,
    on: impl FnMut(ArchiveProgress),
) -> Result<ArchivePlan, String> {
    let (dir, _) = trip_masters(cfg, trip)?;
    let dirs = verified_raw_dirs(cfg, trip, &dir, on)?;
    let bytes = dirs.iter().map(|d| dir_bytes(d)).sum();
    Ok(ArchivePlan {
        trip: trip.to_string(),
        masters: masters_within(&dir, &dirs),
        bytes,
    })
}

/// Free `trip`'s local raw. Re-verifies against the cloud first — these are the
/// only local copies, so the backup is reconfirmed at delete time — then removes
/// the backed raw dirs, keeping `clips/`, `marks.tsv`, `.reel`, and any unshared
/// work.
pub fn commit_archive(
    cfg: &Config,
    trip: &str,
    on: impl FnMut(ArchiveProgress),
) -> Result<ArchiveResult, String> {
    let (dir, _) = trip_masters(cfg, trip)?;
    let dirs = verified_raw_dirs(cfg, trip, &dir, on)?;
    let masters = masters_within(&dir, &dirs);

    let mut freed = 0u64;
    for d in &dirs {
        let b = dir_bytes(d);
        fs::remove_dir_all(d).map_err(|e| format!("couldn't free {}: {e}", d.display()))?;
        freed += b;
    }
    // a leftover play-state file the script also clears
    let _ = fs::remove_file(dir.join(".reel-play.tsv"));
    // Record the archive as an *intent*. Inferring it from "no masters + some
    // clips" can't tell a deliberate archive from footage lost by accident, reads a
    // trip archived before it was ever cut as merely Empty, and would be wrong
    // again for any trip holding a still or a photo. Since the dashboard now files
    // archived trips away, that guess is load-bearing — so it gets written down.
    let _ = crate::trips::set_trip_meta(&dir, "archived", "1");
    crate::log::event(
        crate::log::Level::Info,
        "engine",
        "archive",
        Some(serde_json::json!({ "trip": trip, "masters": masters, "freed": freed })),
    );
    Ok(ArchiveResult {
        trip: trip.to_string(),
        freed,
        masters,
    })
}
