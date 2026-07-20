//! Free a trip's local raw footage once it's safe in the shared cloud: confirm
//! every master is in the cloud, then delete the per-person camera trees (plus
//! `_sheets/`/`.proxies/`), keeping the cut clips, marks, and `.reel`. The trip
//! then reads "Archived" and its raw can be re-pulled. Mirrors the script's
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

/// The top-level dirs that hold raw — everything under the trip but `clips/`: the
/// per-person camera trees, plus `_sheets/` and `.proxies/`. These are freed.
fn raw_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() && p.file_name().and_then(|s| s.to_str()) != Some("clips") {
                v.push(p);
            }
        }
    }
    v
}

/// Resolve and sanity-check a trip dir, returning it with its masters.
fn trip_masters(cfg: &Config, trip: &str) -> Result<(PathBuf, Vec<PathBuf>), String> {
    if !valid_trip(trip) {
        return Err(format!("invalid trip name: {trip:?}"));
    }
    let dir = cfg.lib.join(trip);
    if !dir.join(".reel").is_file() {
        return Err(format!("no such trip: {trip}"));
    }
    let masters = masters_in(&dir);
    if masters.is_empty() {
        return Err(format!("no raw footage in '{trip}' to free"));
    }
    Ok((dir, masters))
}

/// Confirm every master in `dir` hash-matches in `<remote>/<trip>`, streaming
/// rclone's per-file check progress. The `--files-from` list scopes it to exactly
/// the raw we're about to free.
fn verify_all_in_cloud(
    cfg: &Config,
    trip: &str,
    dir: &Path,
    masters: &[PathBuf],
    mut on: impl FnMut(ArchiveProgress),
) -> Result<(), String> {
    rclone::remote_ok(&cfg.remote)?;
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
    match ok {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!(
            "not all of '{trip}' is in the cloud — Share it first; nothing freed"
        )),
        Err(e) => Err(e),
    }
}

/// Work out what archiving `trip` would free, confirming every master is in the
/// cloud first. Nothing is deleted.
pub fn plan_archive(
    cfg: &Config,
    trip: &str,
    on: impl FnMut(ArchiveProgress),
) -> Result<ArchivePlan, String> {
    let (dir, masters) = trip_masters(cfg, trip)?;
    verify_all_in_cloud(cfg, trip, &dir, &masters, on)?;
    let bytes = raw_dirs(&dir).iter().map(|d| dir_bytes(d)).sum();
    Ok(ArchivePlan {
        trip: trip.to_string(),
        masters: masters.len(),
        bytes,
    })
}

/// Free `trip`'s local raw. Re-verifies against the cloud first — these are the
/// only local copies, so the backup is reconfirmed at delete time — then removes
/// the raw dirs, keeping `clips/`, `marks.tsv`, and `.reel`.
pub fn commit_archive(
    cfg: &Config,
    trip: &str,
    on: impl FnMut(ArchiveProgress),
) -> Result<ArchiveResult, String> {
    let (dir, masters) = trip_masters(cfg, trip)?;
    verify_all_in_cloud(cfg, trip, &dir, &masters, on)?;

    let mut freed = 0u64;
    for d in raw_dirs(&dir) {
        let b = dir_bytes(&d);
        fs::remove_dir_all(&d).map_err(|e| format!("couldn't free {}: {e}", d.display()))?;
        freed += b;
    }
    // a leftover play-state file the script also clears
    let _ = fs::remove_file(dir.join(".reel-play.tsv"));
    Ok(ArchiveResult {
        trip: trip.to_string(),
        freed,
        masters: masters.len(),
    })
}
