//! Push your raw masters in a trip up to the shared pool with rclone, verify the
//! upload, then record `share=shared` in the trip's `.reel`. Pure engine —
//! progress is reported through a callback, so the GUI streams it without
//! reel-core taking a UI dep.
//!
//! Mirrors the script's `push_user_raw`: it uploads only `<trip>/<you>/`, masters
//! only (the cameras' `.LRF`/`.LRV`/`.THM` proxies stay local), and refuses to
//! mark a trip shared unless rclone's own `check` confirms every byte landed —
//! the share state is what lets the UI call a card "safe to clear", so it must
//! never be claimed without proof.

use crate::config::Config;
use crate::media::masters_in;
use crate::model::{PushPhase, PushProgress, PushResult};
use crate::rclone;
use crate::trips::set_trip_meta;
use std::ffi::OsString;
use std::path::Path;

/// Camera proxies/thumbs sit beside masters but never go to the pool.
const EXCLUDES: &[&str] = &["*.LRF", "*.LRV", "*.THM"];

fn valid_trip(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != ".."
}

/// Build a copy/check arg list: the verb and its flags, then the proxy excludes,
/// then src and dest. Shared stats flags are added by `rclone::stream`.
fn pool_args(lead: &[&str], src: &Path, dest: &str) -> Vec<OsString> {
    let mut a: Vec<OsString> = lead.iter().map(OsString::from).collect();
    for ex in EXCLUDES {
        a.push("--exclude".into());
        a.push((*ex).into());
    }
    a.push(src.as_os_str().to_os_string());
    a.push(dest.into());
    a
}

/// Upload your masters in `trip` to `<remote>/<trip>/<you>`, verify them, and on
/// success mark the trip shared. `on` streams progress for both legs. Errors
/// leave the trip *not* marked shared — local copies are never touched, so a
/// failed or half-finished push is always safe to retry.
pub fn push_trip(
    cfg: &Config,
    trip: &str,
    mut on: impl FnMut(PushProgress),
) -> Result<PushResult, String> {
    if !valid_trip(trip) {
        return Err(format!("invalid trip name: {trip:?}"));
    }
    let dir = cfg.lib.join(trip);
    if !dir.join(".reel").is_file() {
        return Err(format!("no such trip: {trip}"));
    }
    let src = dir.join(&cfg.user);
    let mine = masters_in(&src);
    if mine.is_empty() {
        return Err(format!("nothing of yours to share in '{trip}'"));
    }
    rclone::remote_ok(&cfg.remote)?;
    let dest = format!("{}/{}/{}", cfg.remote.trim_end_matches('/'), trip, cfg.user);

    // ---- upload (resumable: rclone skips files already in the pool) ----
    let mut uploaded = 0u64;
    let copied = rclone::stream(
        pool_args(
            &[
                "copy",
                "--transfers",
                "2",
                "--checkers",
                "4",
                "--retries",
                "5",
                "--low-level-retries",
                "20",
            ],
            &src,
            &dest,
        ),
        |v| {
            let s = &v["stats"];
            let done = s["bytes"].as_u64().unwrap_or(0);
            uploaded = uploaded.max(done);
            let file = s["transferring"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|t| t["name"].as_str())
                .unwrap_or("")
                .to_string();
            on(PushProgress {
                phase: PushPhase::Upload,
                file,
                done,
                total: s["totalBytes"].as_u64().unwrap_or(0),
            });
        },
    )?;
    if !copied {
        return Err(
            "upload didn't finish — your local copies are safe; Share again to resume".into(),
        );
    }

    // ---- verify every master is byte-present before claiming the trip shared ----
    on(PushProgress {
        phase: PushPhase::Verify,
        file: String::new(),
        done: 0,
        total: mine.len() as u64,
    });
    let verified = rclone::stream(pool_args(&["check", "--one-way"], &src, &dest), |v| {
        let s = &v["stats"];
        on(PushProgress {
            phase: PushPhase::Verify,
            file: String::new(),
            done: s["checks"].as_u64().unwrap_or(0),
            total: s["totalChecks"].as_u64().unwrap_or(0),
        });
    })?;
    if !verified {
        return Err(
            "pool check failed — not marking shared; your local copies are safe. Share again to retry"
                .into(),
        );
    }

    // ---- record: now it's provably in the pool ----
    set_trip_meta(&dir, "share", "shared")
        .map_err(|e| format!("uploaded and verified, but couldn't record share state: {e}"))?;

    let bytes = mine
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    Ok(PushResult {
        trip: trip.to_string(),
        files: mine.len(),
        bytes,
        uploaded,
    })
}
