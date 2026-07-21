//! Bring footage back down that the cloud still holds but this machine doesn't.
//!
//! This is the way back from the two flows that free local disk: `archive`, which
//! deletes your raw once it's verified up, and clearing a pulled clip, which drops
//! the file but keeps the baseline row so it isn't misread as new footage to pull.
//! Both leave a clip as `L✗ B✓ R✓` — `sync::classify`'s `cloud_only` bucket — which
//! the engine has always described as "re-downloadable" without anything actually
//! implementing the download.
//!
//! Distinct from `pull`: Pull fetches footage that's *new to you* (no baseline
//! row), whole person-folders at a time. Restore fetches back exactly the files you
//! already accounted for and then freed, which is why it works file-by-file — a
//! folder copy would also drag down the clips you deliberately cleared.

use crate::config::Config;
use crate::model::{PullProgress, RestoreResult};
use crate::rclone;
use crate::store::temp_sibling;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::Path;

fn valid_trip(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != ".."
}

/// A rel must stay inside the trip: relative, forward slashes, no `..` segment.
/// The rels `reconcile` passes come from the engine's own classification, but this
/// is a `pub fn` reachable from the command layer, so it re-checks rather than
/// trusting the caller not to hand it `../../.ssh/id_rsa`.
fn safe_rel(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.starts_with('/')
        && !rel.contains('\\')
        && !rel
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
}

/// Download `rels` (paths relative to the trip root) from the trip's cloud folder
/// back into the local trip. `on` streams byte progress. Resumable — rclone skips
/// anything already down, so a half-finished restore just carries on.
///
/// The baseline is deliberately untouched: every one of these already has a row
/// (that's what put it in `cloud_only`), and it still matches the cloud.
pub fn restore(
    cfg: &Config,
    trip: &str,
    rels: &[String],
    mut on: impl FnMut(PullProgress),
) -> Result<RestoreResult, String> {
    if !valid_trip(trip) {
        return Err(format!("invalid trip name: {trip:?}"));
    }
    let bad: Vec<&String> = rels.iter().filter(|r| !safe_rel(r)).collect();
    if let Some(r) = bad.first() {
        return Err(format!("refusing to restore an unsafe path: {r:?}"));
    }
    if rels.is_empty() {
        return Ok(RestoreResult {
            trip: trip.to_string(),
            files: 0,
            bytes: 0,
        });
    }
    rclone::remote_ok(&cfg.remote)?;

    let dir = cfg.lib.join(trip);
    if !dir.is_dir() {
        return Err(format!("trip '{trip}' isn't in your library"));
    }

    // rclone takes the file list on disk. Keep it in the state dir, not the trip —
    // a stray .tmp under the trip would show up mid-scan.
    fs::create_dir_all(&cfg.state_dir).map_err(|e| format!("couldn't open the state dir: {e}"))?;
    let list = temp_sibling(&cfg.state_dir.join("restore.lst"));
    write_list(&list, rels).map_err(|e| format!("couldn't stage the restore list: {e}"))?;

    let src = format!("{}/{}", cfg.remote.trim_end_matches('/'), trip);
    let args: Vec<OsString> = vec![
        "copy".into(),
        "--files-from".into(),
        list.as_os_str().to_os_string(),
        "--transfers".into(),
        "4".into(),
        "--checkers".into(),
        "8".into(),
        "--retries".into(),
        "5".into(),
        "--low-level-retries".into(),
        "20".into(),
        src.into(),
        dir.as_os_str().to_os_string(),
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
    });
    let _ = fs::remove_file(&list);
    if !copied? {
        return Err("restore didn't finish — run it again to resume".into());
    }

    // Count what actually landed rather than what we asked for: a clip the cloud
    // lost between the listing and now simply doesn't come back, and saying "53
    // restored" when 52 arrived would be a lie the next sync immediately exposes.
    let mut files = 0usize;
    let mut bytes = 0u64;
    for rel in rels {
        if let Ok(m) = fs::metadata(dir.join(rel)) {
            files += 1;
            bytes += m.len();
        }
    }
    // Footage is back, so the trip isn't archived any more — clear the marker and
    // it returns to the dashboard. Done on any file landing, not just a full
    // restore: a partly-restored trip has raw you can act on, so leaving it filed
    // away would strand exactly the footage you just asked for.
    if files > 0 {
        let _ = crate::trips::set_trip_meta(&dir, "archived", "0");
    }
    Ok(RestoreResult {
        trip: trip.to_string(),
        files,
        bytes,
    })
}

fn write_list(path: &Path, rels: &[String]) -> std::io::Result<()> {
    let mut f = fs::File::create(path)?;
    for rel in rels {
        writeln!(f, "{rel}")?;
    }
    f.flush()
}
