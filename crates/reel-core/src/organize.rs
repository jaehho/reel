//! Reorganize local footage: move clips between trips, rename a trip, and merge
//! one trip into another — the "wrong trip" fixes, the counterpart to import.
//!
//! A relocation is only safe if it keeps five things in step, so each of these
//! does all five together:
//!   - **files** — the master, its native `.LRF`/`.LRV`/`.THM`, any cached
//!     `.proxies/` proxy, and any cut `clips/*` derived from it, all moved as a
//!     unit. The `<person>/<camera>` subpath is preserved, so provenance and the
//!     cloud layout ride along unchanged.
//!   - **ledger** — each moved clip's `trip` is rewritten, so dedup and card
//!     reclaim keep pointing at the real owner.
//!   - **marks** — segments keyed on a moved master migrate to the destination
//!     trip's `marks.tsv`, repointed at the new path.
//!   - **cloud** — when the remote is reachable the same move is mirrored there
//!     (`rclone moveto`), so a shared trip stays provably complete; when it isn't,
//!     the destination's share is dropped to `unknown` instead of overstating
//!     safety (PRODUCT principle 3).
//!   - **share state** — the source keeps a valid `shared` claim (its remaining
//!     locals are still in the cloud); only the destination can regress.
//!
//! Pure engine — no GUI dep, so it's exercised headlessly in `tests/engine.rs`.

use crate::config::Config;
use crate::ledger::Ledger;
use crate::media::{masters_in, native_proxy_of, rel_stem, under};
use crate::model::{Mark, MoveResult};
use crate::rclone;
use crate::review::{read_marks, write_marks};
use crate::store::{now_epoch, FileSet, Pending, PendingOp};
use crate::trips::{set_trip_meta, trip_meta};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

/// A trip name from the UI must stay a single path segment under the library, so
/// it can't climb out of the root.
fn valid_trip(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != ".."
}

/// Make `dir` a trip: create it and drop a `.reel` marker if absent (idempotent).
/// Mirrors `import::ensure_project` so a move into a brand-new trip works.
fn ensure_project(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let marker = dir.join(".reel");
    if !marker.exists() {
        fs::write(marker, "reel project\n")?;
    }
    Ok(())
}

/// Is this trip's footage recorded in the shared cloud? (Same reading as
/// `trips::share_of`, kept local to avoid widening that module's surface.)
fn is_shared(dir: &Path) -> bool {
    matches!(
        trip_meta(dir, "share").as_deref(),
        Some("shared" | "verified" | "done" | "yes")
    )
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

/// `person/camera` for a master under `trip_dir` (`<trip>/<person>/<camera>/file`)
/// — the subpath preserved across a move so provenance and cloud paths hold.
fn person_camera(master: &Path, trip_dir: &Path) -> Option<(String, String)> {
    let rel = master.strip_prefix(trip_dir).ok()?;
    let mut it = rel.components();
    let person = it.next()?.as_os_str().to_str()?.to_string();
    let camera = it.next()?.as_os_str().to_str()?.to_string();
    Some((person, camera))
}

/// Move a file, preferring a rename (same filesystem — the whole library is one
/// tree) and falling back to copy+remove across devices, preserving mtime so the
/// clip's capture time survives.
fn relocate(src: &Path, dst: &Path) -> io::Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    fs::copy(src, dst)?;
    if let Ok(mt) = fs::metadata(src).and_then(|m| m.modified()) {
        if let Ok(f) = File::options().write(true).open(dst) {
            let _ = f.set_modified(mt);
        }
    }
    fs::remove_file(src)
}

/// Move the sidecars that live beside a master and share its stem: the camera's
/// native proxy (`.LRF`/`.LRV`) and thumbnail (`.THM`). Best-effort — a missing
/// sidecar is normal.
fn move_sidecars(master_src: &Path, master_dst: &Path) {
    if let Some(np) = native_proxy_of(master_src) {
        if let (Some(base), Some(dstdir)) = (np.file_name(), master_dst.parent()) {
            let _ = relocate(&np, &dstdir.join(base));
        }
    }
    if let (Some(stem), Some(srcdir), Some(dstdir)) = (
        master_src.file_stem().and_then(|s| s.to_str()),
        master_src.parent(),
        master_dst.parent(),
    ) {
        for ext in ["THM", "thm"] {
            let thm = srcdir.join(format!("{stem}.{ext}"));
            if thm.is_file() {
                let _ = relocate(&thm, &dstdir.join(format!("{stem}.{ext}")));
            }
        }
    }
}

/// Move a master's cached review proxy, if one was built. `rel_stem` is identical
/// in source and destination (same `<person>/<camera>/<base>`), so the cache name
/// carries over untouched. Returns whether one moved (unused today, handy later).
fn move_cached_proxy(src_dir: &Path, dst_dir: &Path, stem: &str) -> bool {
    let from = src_dir.join(".proxies").join(format!("{stem}.mp4"));
    if from.is_file() {
        let _ = relocate(&from, &dst_dir.join(".proxies").join(format!("{stem}.mp4")));
        true
    } else {
        false
    }
}

/// Move the cut clips derived from one master (`clips/<stem>_c*`) into the
/// destination's `clips/`, so a move after a cut keeps the cut intact. Returns
/// how many moved.
fn move_cut_clips(src_dir: &Path, dst_dir: &Path, stem: &str) -> usize {
    let prefix = format!("{stem}_c");
    let mut moved = 0;
    if let Ok(rd) = fs::read_dir(src_dir.join("clips")) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with(&prefix) && relocate(&p, &dst_dir.join("clips").join(name)).is_ok()
            {
                moved += 1;
            }
        }
    }
    moved
}

/// One master queued for a move: where it is, where it's going, and the cloud
/// bookkeeping needed once its bytes land.
struct Plan {
    src_dir: PathBuf,
    src_trip: String,
    master_src: PathBuf,
    master_dst: PathBuf,
    rel: String,       // person/camera/base — the cloud-relative path
    stem: String,      // rel_stem, for proxies/clips
    needs_cloud: bool, // this clip has a copy in the cloud that must follow
    old_abs: String,   // marks key on this
    new_abs: String,
}

/// Move `masters` (absolute library paths) into `dest`, creating `dest` if new.
/// The selection may span several source trips; each clip's `<person>/<camera>`
/// subpath is preserved. Clips already present in `dest` are left alone (dedup).
pub fn move_clips(cfg: &Config, masters: &[String], dest: &str) -> Result<MoveResult, String> {
    if !valid_trip(dest) {
        return Err(format!("invalid trip name: {dest:?}"));
    }
    let dst_dir = cfg.lib.join(dest);
    ensure_project(&dst_dir).map_err(|e| format!("couldn't open trip '{dest}': {e}"))?;

    // ---- plan every move up front (no disk changes yet) ----
    let mut plans: Vec<Plan> = Vec::new();
    let mut skipped = 0usize;
    for m in masters {
        let src = PathBuf::from(m);
        if !under(&src, &cfg.lib) || !src.is_file() {
            continue; // never touch anything outside the library
        }
        let Some(src_trip) = trip_of(&cfg.lib, &src) else {
            continue;
        };
        if src_trip == dest {
            skipped += 1; // already here
            continue;
        }
        let src_dir = cfg.lib.join(&src_trip);
        let Some((person, camera)) = person_camera(&src, &src_dir) else {
            continue;
        };
        let base = src.file_name().and_then(|s| s.to_str()).unwrap_or_default();
        let rel = format!("{person}/{camera}/{base}");
        let master_dst = dst_dir.join(&person).join(&camera).join(base);
        if master_dst.exists() {
            skipped += 1; // the destination already owns this clip
            continue;
        }
        let needs_cloud = person != cfg.user || is_shared(&src_dir);
        plans.push(Plan {
            src_dir: src_dir.clone(),
            src_trip,
            stem: rel_stem(&src, &src_dir),
            master_src: src,
            master_dst: master_dst.clone(),
            rel,
            needs_cloud,
            old_abs: m.clone(),
            new_abs: master_dst.display().to_string(),
        });
    }

    if plans.is_empty() {
        return Ok(MoveResult {
            dest: dest.to_string(),
            moved: 0,
            clips: 0,
            marks: 0,
            skipped,
            cloud_synced: true,
        });
    }

    // ---- move the files ----
    // (src_trip, rel) of each clip whose bytes actually landed — the ledger rows to
    // repoint. Applied after the loop, not during: `relocate` falls back to a full
    // copy across devices, and holding the ledger across that would let a concurrent
    // import's rows disappear under us.
    let mut repoint: Vec<(String, String)> = Vec::new();
    let mut moved = 0usize;
    let mut clips = 0usize;
    let mut migrations: HashMap<PathBuf, Vec<(String, String)>> = HashMap::new();
    let mut moved_plans: Vec<&Plan> = Vec::new();

    for p in &plans {
        if relocate(&p.master_src, &p.master_dst).is_err() {
            continue; // leave everything else consistent; just don't count it
        }
        move_sidecars(&p.master_src, &p.master_dst);
        move_cached_proxy(&p.src_dir, &dst_dir, &p.stem);
        clips += move_cut_clips(&p.src_dir, &dst_dir, &p.stem);
        // ledger: repoint this clip's owner to the destination trip (applied below).
        repoint.push((p.src_trip.clone(), p.rel.clone()));
        migrations
            .entry(p.src_dir.clone())
            .or_default()
            .push((p.old_abs.clone(), p.new_abs.clone()));
        moved += 1;
        moved_plans.push(p);
    }
    Ledger::update(&cfg.ledger_path(), |l| {
        for (src_trip, rel) in &repoint {
            if let Some(row) = l.rows.iter_mut().find(|r| {
                &r.trip == src_trip && rel == &format!("{}/{}/{}", r.person, r.camera, r.base)
            }) {
                row.trip = dest.to_string();
            }
        }
    })
    .map_err(|e| format!("moved the files, but couldn't update the ledger: {e}"))?;

    // ---- migrate marks: pull moved masters' segments out of each source trip,
    // repoint them, and append to the destination's marks.tsv ----
    let mut dst_marks = read_marks(&dst_dir);
    let before = dst_marks.len();
    for (src_dir, pairs) in &migrations {
        let mut kept = read_marks(src_dir);
        kept.retain(|m| {
            if let Some((_, np)) = pairs.iter().find(|(old, _)| old == &m.master) {
                dst_marks.push(Mark {
                    master: np.clone(),
                    start: m.start,
                    end: m.end,
                    label: m.label.clone(),
                });
                false
            } else {
                true
            }
        });
        let _ = write_marks(src_dir, &kept);
    }
    let marks = dst_marks.len() - before;
    if marks > 0 {
        let _ = write_marks(&dst_dir, &dst_marks);
    }

    // ---- mirror in the cloud, or drop the destination's share claim ----
    let any_cloud = moved_plans.iter().any(|p| p.needs_cloud);
    let cloud_synced = sync_cloud_moves(cfg, &moved_plans, dest, any_cloud);
    if any_cloud && !cloud_synced {
        let _ = set_trip_meta(&dst_dir, "share", "unknown");
    }

    // ---- keep the baseline pointing at where each in_cloud clip now lives ----
    if any_cloud {
        if cloud_synced {
            retarget_baseline(cfg, &moved_plans, dest);
        } else {
            // Couldn't move in the cloud now — offline, or a move failed. Queue each
            // so a later sync replays it (server-side) rather than re-uploading
            // under the new trip. The baseline stays at the source and self-heals
            // once the queued move replays.
            let _ = Pending::update(&cfg.pending_path(), |pending| {
                for p in moved_plans.iter().filter(|p| p.needs_cloud) {
                    pending.push(
                        now_epoch(),
                        PendingOp::Move {
                            from: p.src_trip.clone(),
                            to: dest.to_string(),
                            rel: p.rel.clone(),
                        },
                    );
                }
            });
        }
    }

    Ok(MoveResult {
        dest: dest.to_string(),
        moved,
        clips,
        marks,
        skipped,
        cloud_synced,
    })
}

/// Mirror the moved masters in the cloud. Only clips that actually have a cloud copy
/// (`needs_cloud`) are moved; if the remote is unreachable, or any required move
/// fails, we report `false` so the caller drops the destination's share to
/// `unknown`. When nothing needed the cloud, it's trivially in sync.
fn sync_cloud_moves(cfg: &Config, plans: &[&Plan], dest: &str, any_cloud: bool) -> bool {
    if !any_cloud {
        return true;
    }
    if rclone::remote_ok(&cfg.remote).is_err() {
        return false; // offline / misconfigured — can't prove the cloud followed
    }
    let remote = cfg.remote.trim_end_matches('/');
    let mut ok = true;
    for p in plans.iter().filter(|p| p.needs_cloud) {
        let from = format!("{}/{}/{}", remote, p.src_trip, p.rel);
        let to = format!("{}/{}/{}", remote, dest, p.rel);
        ok &= rclone::move_path(&from, &to).unwrap_or(false);
    }
    ok
}

/// After a successful cloud move, move each in_cloud clip's baseline row from its
/// source trip's file to the destination's, so sync keeps reading it as synced.
fn retarget_baseline(cfg: &Config, plans: &[&Plan], dest: &str) {
    // Work out the edits first (stat included), then apply each file under the
    // state lock so a concurrent push/pull's rows survive.
    let mut adds: Vec<(String, u64)> = Vec::new();
    let mut removes: HashMap<String, Vec<String>> = HashMap::new();
    for p in plans.iter().filter(|p| p.needs_cloud) {
        removes
            .entry(p.src_trip.clone())
            .or_default()
            .push(p.rel.clone());
        let size = std::fs::metadata(&p.master_dst)
            .map(|m| m.len())
            .unwrap_or(0);
        adds.push((p.rel.clone(), size));
    }
    let _ = FileSet::update(&cfg.base_path(dest), |b| {
        for (rel, size) in &adds {
            b.insert(rel, *size);
        }
    });
    for (trip, rels) in removes {
        let _ = FileSet::update(&cfg.base_path(&trip), |b| {
            for rel in &rels {
                b.remove(rel);
            }
        });
    }
}

/// Rename a trip. The directory move carries its footage, cached proxies, cut,
/// and `.reel` (share state included); we then repoint the ledger and marks, and
/// rename the trip's cloud folder so a shared trip stays complete. Refuses if a
/// trip named `new` already exists — that's a merge, not a rename.
pub fn rename_trip(cfg: &Config, old: &str, new: &str) -> Result<MoveResult, String> {
    if !valid_trip(old) || !valid_trip(new) {
        return Err("invalid trip name".into());
    }
    if old == new {
        return Err("that's already its name".into());
    }
    let old_dir = cfg.lib.join(old);
    let new_dir = cfg.lib.join(new);
    if !old_dir.join(".reel").is_file() {
        return Err(format!("no such trip: {old}"));
    }
    if new_dir.exists() {
        return Err(format!(
            "a trip named '{new}' already exists — merge into it instead"
        ));
    }

    let masters_v = masters_in(&old_dir);
    // Only your own subtree is ever moved in the cloud, so the rename only needs
    // the cloud when your footage is up there (`shared`); a friend's pulled
    // footage stays under the old cloud name — see the cloud block below.
    let needs_cloud = is_shared(&old_dir);

    fs::rename(&old_dir, &new_dir).map_err(|e| format!("couldn't rename '{old}': {e}"))?;

    // ledger: every row that named the old trip now names the new one.
    Ledger::update(&cfg.ledger_path(), |l| {
        for row in l.rows.iter_mut() {
            if row.trip == old {
                row.trip = new.to_string();
            }
        }
    })
    .map_err(|e| format!("renamed, but couldn't update the ledger: {e}"))?;

    // marks: repoint absolute paths from the old dir to the new one.
    let old_prefix = old_dir.display().to_string();
    let new_prefix = new_dir.display().to_string();
    let mut marks = read_marks(&new_dir);
    for m in marks.iter_mut() {
        if let Some(rest) = m.master.strip_prefix(&old_prefix) {
            m.master = format!("{new_prefix}{rest}");
        }
    }
    let n_marks = marks.len();
    let _ = write_marks(&new_dir, &marks);

    // baseline + cloud cache ride along as file renames (rows are trip-local).
    let _ = fs::rename(cfg.base_path(old), cfg.base_path(new));
    let _ = fs::rename(cfg.cloud_cache_path(old), cfg.cloud_cache_path(new));

    // cloud: move only YOUR subtree `<old>/<user>` → `<new>/<user>`, so a rename
    // never drags other contributors' footage with it. Offline, queue it.
    let mut cloud_synced = true;
    if needs_cloud {
        let remote = cfg.remote.trim_end_matches('/');
        cloud_synced = rclone::remote_ok(&cfg.remote).is_ok()
            && rclone::move_path(
                &format!("{remote}/{old}/{}", cfg.user),
                &format!("{remote}/{new}/{}", cfg.user),
            )
            .unwrap_or(false);
        if !cloud_synced {
            // Couldn't move your cloud folder now — offline, OR the move failed.
            // Queue the rename so a later sync replays it, and DON'T leave the trip
            // looking like fresh footage to upload (that would re-upload under the
            // new name and orphan the old one). Sync suppresses re-upload while a
            // rename is queued and replays it first.
            let _ = Pending::update(&cfg.pending_path(), |p| {
                p.push(
                    now_epoch(),
                    PendingOp::Rename {
                        old: old.to_string(),
                        new: new.to_string(),
                    },
                )
            });
            let _ = set_trip_meta(&new_dir, "share", "unknown");
        }
    }

    Ok(MoveResult {
        dest: new.to_string(),
        moved: masters_v.len(),
        clips: 0,
        marks: n_marks,
        skipped: 0,
        cloud_synced,
    })
}

/// Merge every clip of `src` into `dst`, then remove `src` once it's empty — the
/// whole-session "wrong trip" fix. Builds on `move_clips`, so files, ledger,
/// marks, cut, and cloud all move with the footage.
pub fn merge_trips(cfg: &Config, src: &str, dst: &str) -> Result<MoveResult, String> {
    if !valid_trip(src) || !valid_trip(dst) {
        return Err("invalid trip name".into());
    }
    if src == dst {
        return Err("a trip can't merge into itself".into());
    }
    let src_dir = cfg.lib.join(src);
    if !src_dir.join(".reel").is_file() {
        return Err(format!("no such trip: {src}"));
    }
    let masters: Vec<String> = masters_in(&src_dir)
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    if masters.is_empty() {
        return Err(format!("'{src}' has no footage to merge"));
    }

    let result = move_clips(cfg, &masters, dst)?;

    // Fold the emptied source away: once no masters remain, drop its now-empty
    // marks/proxies and the directory itself (its share proof left with the raw).
    if masters_in(&src_dir).is_empty() {
        let _ = fs::remove_dir_all(&src_dir);
    }
    Ok(result)
}
