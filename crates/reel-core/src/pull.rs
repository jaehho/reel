//! Pull footage down from the shared cloud: the inverse of `push`. Where Share
//! uploads *your* masters (`<trip>/<you>/`), Pull brings *other people's* masters
//! into a trip so you can review and cut with them.
//!
//! Provenance is the folder, both ways: a person's clips live under
//! `<trip>/<person>/` in the cloud and land at the same path locally, so once
//! they're down `list_trips` already reads them as "pulled from <person>" with no
//! extra bookkeeping. Pure engine — download progress streams through a callback.

use crate::config::Config;
use crate::media::{has_ext, masters_in, CAPTURE_EXT};
use crate::model::{Contributor, PullProgress, PullResult};
use crate::rclone;
use crate::store::FileSet;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;

fn valid_trip(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != ".."
}

/// A person folder name must be a single path segment, like a trip name.
fn valid_person(name: &str) -> bool {
    valid_trip(name)
}

/// Make `dir` a trip locally (create it + a `.reel` marker) so a pull into a trip
/// you don't have yet still works. Mirrors `import::ensure_project`.
fn ensure_project(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let marker = dir.join(".reel");
    if !marker.exists() {
        fs::write(marker, "reel project\n")?;
    }
    Ok(())
}

/// Who has footage in this trip's cloud that you could pull. Surveys
/// `<remote>/<trip>/` once, groups masters by their top-level person folder, and
/// drops your own (that's a Share, not a pull). `pulled` marks people whose
/// footage you already have locally.
pub fn cloud_contributors(cfg: &Config, trip: &str) -> Result<Vec<Contributor>, String> {
    if !valid_trip(trip) {
        return Err(format!("invalid trip name: {trip:?}"));
    }
    rclone::remote_ok(&cfg.remote)?;
    let root = format!("{}/{}", cfg.remote.trim_end_matches('/'), trip);
    let entries = rclone::lsjson(&root)?;

    // person → (master count, bytes)
    let mut by: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    for e in &entries {
        let path = e["Path"].as_str().unwrap_or("");
        if !has_ext(Path::new(path), CAPTURE_EXT) {
            continue; // videos and photos count; the cloud holds no proxies anyway
        }
        let Some(person) = path.split('/').next().filter(|p| !p.is_empty()) else {
            continue;
        };
        let slot = by.entry(person.to_string()).or_default();
        slot.0 += 1;
        slot.1 += e["Size"].as_u64().unwrap_or(0);
    }

    let mut out = Vec::new();
    for (person, (clips, bytes)) in by {
        if person == cfg.user {
            continue; // your own contribution — use Share to put it up, not Pull
        }
        let local = masters_in(&cfg.lib.join(trip).join(&person)).len();
        out.push(Contributor {
            person,
            clips,
            bytes,
            pulled: clips > 0 && local >= clips,
        });
    }
    Ok(out)
}

/// Copy one person's masters from the cloud into `<trip>/<person>/`, creating the
/// trip locally if new. `on` streams byte progress. Refuses your own name (that's
/// what Share is for). Resumable — rclone skips files already down.
pub fn pull_person(
    cfg: &Config,
    trip: &str,
    person: &str,
    mut on: impl FnMut(PullProgress),
) -> Result<PullResult, String> {
    if !valid_trip(trip) {
        return Err(format!("invalid trip name: {trip:?}"));
    }
    if !valid_person(person) {
        return Err(format!("invalid person: {person:?}"));
    }
    if person == cfg.user {
        return Err("that's your own footage — Share uploads it, no need to pull".into());
    }
    rclone::remote_ok(&cfg.remote)?;

    let dir = cfg.lib.join(trip);
    ensure_project(&dir).map_err(|e| format!("couldn't open trip '{trip}': {e}"))?;
    let dst = dir.join(person);
    let src = format!("{}/{}/{}", cfg.remote.trim_end_matches('/'), trip, person);

    let args: Vec<OsString> = vec![
        "copy".into(),
        "--transfers".into(),
        "4".into(),
        "--checkers".into(),
        "8".into(),
        "--retries".into(),
        "5".into(),
        "--low-level-retries".into(),
        "20".into(),
        src.into(),
        dst.as_os_str().to_os_string(),
    ];
    let copied = rclone::stream(args, |v| {
        let s = &v["stats"];
        let file = s["transferring"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|t| t["name"].as_str())
            .unwrap_or("")
            .to_string();
        on(PullProgress {
            file,
            done: s["bytes"].as_u64().unwrap_or(0),
            total: s["totalBytes"].as_u64().unwrap_or(0),
        });
    })?;
    if !copied {
        return Err("pull didn't finish — run it again to resume".into());
    }

    let masters = masters_in(&dst);
    // Baseline: this person's footage is now present locally and synced with the
    // cloud, so later sync can tell if either side changes.
    // Stat outside the lock, then merge — the download above is the slow part.
    let rows: Vec<(String, u64)> = masters
        .iter()
        .filter_map(|p| {
            let rel = p.strip_prefix(&dir).ok()?.to_str()?.replace('\\', "/");
            Some((rel, fs::metadata(p).map(|m| m.len()).unwrap_or(0)))
        })
        .collect();
    let _ = FileSet::update(&cfg.base_path(trip), |base| {
        for (rel, size) in &rows {
            base.insert(rel, *size);
        }
    });

    let bytes = masters
        .iter()
        .filter_map(|p| fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    Ok(PullResult {
        trip: trip.to_string(),
        person: person.to_string(),
        files: masters.len(),
        bytes,
    })
}
