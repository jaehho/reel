//! Permanent delete: erase footage for good. Unlike card reclaim (`wipe.rs`),
//! which only frees a card once a clip is safely in the cloud, this is the user
//! deciding a clip is garbage — so it goes everywhere.
//!
//! Two entry points, `delete_clips` and `delete_trip`. Both:
//!   - remove the local master(s) and their derived junk (native proxy, `.THM`,
//!     cached review proxy), leaving finished `clips/*` cuts alone;
//!   - erase **your** footage from the shared cloud too (footage *pulled* from
//!     someone else is only removed locally — the cloud copy is theirs to keep);
//!   - drop the clip's ledger row and **tombstone** its content id, so a copy
//!     still on a card reads "discarded" and is never re-imported;
//!   - strip any now-dangling marks.
//!
//! Guards: only paths under the library are ever removed locally, and a cloud
//! `purge` is scoped to a single person's subtree of one trip — never a whole
//! trip, which would take other contributors' footage with it. Losing footage is
//! irreversible, so the callers gate this behind an explicit confirm.

use crate::cards::card_roots;
use crate::config::Config;
use crate::ledger::{Ledger, Tombstones};
use crate::media::{fileid_of, masters_in, masters_under, native_proxy_of, rel_stem, under};
use crate::model::{DeleteResult, ReclaimResult};
use crate::rclone;
use crate::review::{read_marks, write_marks};
use crate::store::{now_epoch, FileSet, Pending, PendingOp};
use crate::trips::trip_meta;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn valid_trip(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != ".."
}

/// The trip a library path sits in: its first component under `lib`.
fn trip_of(lib: &Path, master: &Path) -> Option<String> {
    master
        .strip_prefix(lib)
        .ok()?
        .components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .map(str::to_string)
}

/// `(person, "person/camera/base")` for a master under its trip dir — the owner
/// and the cloud-relative tail used for both the ledger match and the cloud path.
fn rel_of(master: &Path, trip_dir: &Path) -> Option<(String, String)> {
    let rel = master.strip_prefix(trip_dir).ok()?;
    let person = rel.components().next()?.as_os_str().to_str()?.to_string();
    Some((person, rel.to_str()?.to_string()))
}

/// Is this trip's footage recorded in the cloud? (Then your subtree is up there.)
fn is_shared(dir: &Path) -> bool {
    matches!(
        trip_meta(dir, "share").as_deref(),
        Some("shared" | "verified" | "done" | "yes")
    )
}

/// Remove the raw sidecars beside a master (native `.LRF`/`.LRV`, `.THM`) — the
/// unusable leftovers of a clip that's going away. The master itself is deleted
/// by the caller; this reads only its name, so it's fine to call afterward.
pub(crate) fn remove_sidecars(master: &Path) {
    if let Some(np) = native_proxy_of(master) {
        let _ = std::fs::remove_file(np);
    }
    if let (Some(stem), Some(dir)) = (master.file_stem().and_then(|s| s.to_str()), master.parent())
    {
        for ext in ["THM", "thm", "LRF", "lrf", "LRV", "lrv"] {
            let _ = std::fs::remove_file(dir.join(format!("{stem}.{ext}")));
        }
    }
}

/// One clip resolved for deletion, captured while its bytes still exist.
struct Doomed {
    path: PathBuf,
    trip: String,
    person: String,
    rel: String, // person/camera/base
    id: Option<String>,
    bytes: u64,
}

/// Permanently delete `masters` (absolute library paths). Yours go from the cloud
/// too; pulled footage is only removed locally. Every clip is tombstoned. The
/// selection may span trips.
pub fn delete_clips(cfg: &Config, masters: &[String]) -> Result<DeleteResult, String> {
    // ---- resolve targets while the files are still here (need size + id) ----
    let ledger_pre = Ledger::load(&cfg.ledger_path());
    let mut doomed: Vec<Doomed> = Vec::new();
    for m in masters {
        let p = PathBuf::from(m);
        if !under(&p, &cfg.lib) || !p.is_file() {
            continue; // never touch anything outside the library
        }
        let Some(trip) = trip_of(&cfg.lib, &p) else {
            continue;
        };
        let trip_dir = cfg.lib.join(&trip);
        let Some((person, rel)) = rel_of(&p, &trip_dir) else {
            continue;
        };
        let bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        // Prefer the ledger's stored id (no re-hash); fall back to hashing.
        let id = ledger_pre
            .rows
            .iter()
            .find(|r| r.trip == trip && rel == format!("{}/{}/{}", r.person, r.camera, r.base))
            .map(|r| r.id.clone())
            .or_else(|| fileid_of(&p).ok());
        doomed.push(Doomed {
            path: p,
            trip,
            person,
            rel,
            id,
            bytes,
        });
    }

    if doomed.is_empty() {
        return Ok(DeleteResult {
            deleted: 0,
            bytes: 0,
            in_cloud: 0,
            kept_cloud: 0,
            cloud_ok: true,
        });
    }

    // Check the cloud once, and only if we might touch it (any of your footage).
    let touches_cloud = doomed.iter().any(|d| d.person == cfg.user);
    let online = touches_cloud && rclone::remote_ok(&cfg.remote).is_ok();
    let remote = cfg.remote.trim_end_matches('/');

    // Ids whose row goes and whose content gets tombstoned. Collected here and
    // applied once at the end: the loop below makes a network call per clip, and
    // holding the ledger across that would let a concurrent command's rows vanish.
    let mut gone_ids: Vec<String> = Vec::new();
    let mut deleted = 0usize;
    let mut bytes = 0u64;
    let mut in_cloud = 0usize;
    let mut kept_cloud = 0usize;
    let mut cloud_ok = true;
    let mut removed_by_trip: HashMap<String, Vec<String>> = HashMap::new();
    let mut base_removes: HashMap<String, Vec<String>> = HashMap::new();

    for d in &doomed {
        // local delete (double-checked to sit under the library)
        if !under(&d.path, &cfg.lib) || std::fs::remove_file(&d.path).is_err() {
            continue;
        }
        deleted += 1;
        bytes += d.bytes;
        remove_sidecars(&d.path);
        let trip_dir = cfg.lib.join(&d.trip);
        let stem = rel_stem(&d.path, &trip_dir);
        let _ = std::fs::remove_file(trip_dir.join(".proxies").join(format!("{stem}.mp4")));

        // cloud: your own footage is erased there too; a friend's is left alone.
        if d.person == cfg.user {
            if online {
                if rclone::delete_file(&format!("{remote}/{}/{}", d.trip, d.rel)).unwrap_or(false) {
                    in_cloud += 1;
                } else {
                    cloud_ok = false;
                }
            } else {
                cloud_ok = false;
            }
        } else {
            kept_cloud += 1;
        }

        // ledger + tombstone, and note the dangling mark to strip
        if let Some(id) = &d.id {
            gone_ids.push(id.clone());
        }
        removed_by_trip
            .entry(d.trip.clone())
            .or_default()
            .push(d.path.display().to_string());
        // Baseline drop is for YOUR footage only. A pulled clip's cloud copy belongs
        // to its owner and stays up there, so the rel is still legitimately in the
        // cloud — dropping it would leave `L✗ B✗ R✓`, which `sync::classify` reads as
        // "to pull", and `reconcile_all` (the topbar Sync) pulls without asking. The
        // clip you just permanently deleted would silently come back. Left in the
        // baseline it reads as cloud-only ("in cloud"), which sync never re-pulls.
        if d.person == cfg.user {
            base_removes
                .entry(d.trip.clone())
                .or_default()
                .push(d.rel.clone());
        }
    }

    let doomed_ids: std::collections::HashSet<&String> = gone_ids.iter().collect();
    Ledger::update(&cfg.ledger_path(), |l| {
        l.rows.retain(|r| !doomed_ids.contains(&r.id))
    })
    .map_err(|e| format!("deleted the files, but couldn't update the ledger: {e}"))?;
    Tombstones::update(&cfg.tombstones_path(), |t| {
        for id in &gone_ids {
            t.insert(id);
        }
    })
    .map_err(|e| format!("deleted the files, but couldn't record the tombstone: {e}"))?;

    // strip marks that pointed at anything we removed
    for (trip, gone) in &removed_by_trip {
        let dir = cfg.lib.join(trip);
        let kept: Vec<_> = read_marks(&dir)
            .into_iter()
            .filter(|m| !gone.contains(&m.master))
            .collect();
        let _ = write_marks(&dir, &kept);
    }

    // Baseline: deleting your own clip means it should no longer be in the cloud.
    // Drop it from B whether or not the cloud removal ran — offline, the cloud copy
    // survives and the next sync flags it as an owed cleanup. (Pulled footage is
    // excluded above: its cloud copy legitimately stays.)
    for (trip, rels) in &base_removes {
        let _ = FileSet::update(&cfg.base_path(trip), |base| {
            for rel in rels {
                base.remove(rel);
            }
        });
    }

    Ok(DeleteResult {
        deleted,
        bytes,
        in_cloud,
        kept_cloud,
        cloud_ok,
    })
}

fn dir_bytes(dir: &Path) -> u64 {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// Permanently delete a whole trip: remove its local directory outright, tombstone
/// every clip it owned, and — if it was shared — purge *your* contribution from
/// the cloud (other contributors' cloud footage stays). Refuses a bad/unknown trip.
pub fn delete_trip(cfg: &Config, trip: &str) -> Result<DeleteResult, String> {
    if !valid_trip(trip) {
        return Err(format!("invalid trip name: {trip:?}"));
    }
    let dir = cfg.lib.join(trip);
    if !dir.join(".reel").is_file() {
        return Err(format!("no such trip: {trip}"));
    }
    if !under(&dir, &cfg.lib) {
        return Err("refusing to delete outside the library".into());
    }

    let masters = masters_in(&dir).len();
    let bytes = dir_bytes(&dir);
    let shared = is_shared(&dir);

    // Content ids this trip owns, so a copy still on a card reads as discarded.
    // Read now, applied after the directory goes — each under the state lock, so a
    // concurrent import can't lose its rows to our wholesale rewrite.
    let owned_ids: Vec<String> = Ledger::load(&cfg.ledger_path())
        .rows
        .iter()
        .filter(|r| r.trip == trip)
        .map(|r| r.id.clone())
        .collect();

    // Remove the local trip wholesale.
    std::fs::remove_dir_all(&dir).map_err(|e| format!("couldn't delete '{trip}': {e}"))?;
    Ledger::update(&cfg.ledger_path(), |l| l.rows.retain(|r| r.trip != trip))
        .map_err(|e| format!("deleted locally, but couldn't update the ledger: {e}"))?;
    Tombstones::update(&cfg.tombstones_path(), |t| {
        for id in &owned_ids {
            t.insert(id);
        }
    })
    .map_err(|e| format!("deleted locally, but couldn't record tombstones: {e}"))?;

    // Baseline + cloud cache for this trip leave with it.
    let _ = std::fs::remove_file(cfg.base_path(trip));
    let _ = std::fs::remove_file(cfg.cloud_cache_path(trip));

    // Cloud: only your own subtree of this trip, and only if it was up there.
    let mut in_cloud = 0usize;
    let mut cloud_ok = true;
    if shared {
        if rclone::remote_ok(&cfg.remote).is_ok() {
            let path = format!("{}/{}/{}", cfg.remote.trim_end_matches('/'), trip, cfg.user);
            if rclone::purge(&path).unwrap_or(false) {
                in_cloud = masters;
            } else {
                cloud_ok = false;
            }
        } else {
            // Offline: the local trip is gone, so no later per-trip sync could
            // re-derive that your subtree is still up there. Queue the purge.
            let _ = Pending::update(&cfg.pending_path(), |p| {
                p.push(
                    now_epoch(),
                    PendingOp::Purge {
                        trip: trip.to_string(),
                    },
                )
            });
            cloud_ok = false;
        }
    }

    Ok(DeleteResult {
        deleted: masters,
        bytes,
        in_cloud,
        kept_cloud: 0,
        cloud_ok,
    })
}

/// Clear the tombstoned files still sitting on the card — trash you already
/// permanently deleted. `window` scopes it to one session. Only files under a
/// current card root, and only tombstoned ones, are removed; anything else on the
/// card is left untouched (no cloud check — you already decided these are gone).
pub fn clear_discarded(cfg: &Config, window: Option<(i64, i64)>) -> Result<ReclaimResult, String> {
    let roots = card_roots(cfg);
    if roots.is_empty() {
        return Err("no card inserted".into());
    }
    let tombs = Tombstones::load(&cfg.tombstones_path());
    let mut masters = masters_under(&roots);
    if let Some((w0, w1)) = window {
        masters.retain(|(at, _)| *at >= w0 && *at <= w1);
    }
    let mut deleted = 0usize;
    let mut bytes = 0u64;
    for (_, p) in &masters {
        let Ok(id) = fileid_of(p) else { continue };
        if !tombs.contains(&id) || !roots.iter().any(|r| under(p, r)) {
            continue; // not trash, or somehow not on the card — leave it
        }
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        if std::fs::remove_file(p).is_ok() {
            deleted += 1;
            bytes += size;
        }
    }
    Ok(ReclaimResult { deleted, bytes })
}
