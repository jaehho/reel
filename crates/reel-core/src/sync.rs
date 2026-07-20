//! Cloud sync: compute a trip's drift from the shared cloud and reconcile it.
//!
//! A clip's identity across the three sets is its cloud-relative path
//! `person/camera/base` + byte size:
//!   - **L** — local masters now (`masters_in` + `stat`, free)
//!   - **B** — the baseline: clips this machine considers correctly synced with
//!     the cloud (your uploaded-and-verified masters ∪ others' footage you pulled).
//!     Persisted per trip; maintained inline by every cloud op.
//!   - **R** — the live cloud listing (one `rclone lsjson`, cached with a stamp).
//!
//! Status is set arithmetic over (L, B, R). The one subtlety worth stating: a
//! **delete drops the clip from B** (online or offline), while **archive leaves B
//! alone** (the raw is freed locally but still belongs in the cloud). So an
//! archived clip is `L✗ B✓ R✓` → *in sync*, and an offline-deleted clip is
//! `L✗ B✗ R✓` → a zombie to clean. Deriving deletion from B (intent), never from
//! local absence, is what keeps the two apart — the load-bearing invariant.

use crate::config::Config;
use crate::media::{has_ext, masters_in, CAPTURE_EXT};
use crate::model::{
    SyncActions, SyncBrief, SyncItem, SyncPhase, SyncProgress, SyncResult, TripSync,
};
use crate::rclone;
use crate::store::{load_cloud_cache, now_epoch, save_cloud_cache, FileSet, Pending, PendingOp};
use crate::trips::{trip_dirs, trip_meta};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Cloud-relative path for a master under `trip_dir` (`person/…/base`), or `None`
/// for a stray sitting loose in the trip root with no owner folder. `/`-joined so
/// it matches rclone's `Path` on any platform.
///
/// Only the *owner* segment is required. This used to demand `person/camera/base`,
/// which is the shape reel's own import writes — but `pull` copies a friend's cloud
/// folder down verbatim, and a friend whose footage sits at `person/base` (no
/// camera level) got rels this rejected while `push`/`pull` happily wrote them into
/// the baseline. The reader being stricter than every writer meant those clips were
/// never in L, so `L✗ B✓ R✓` classified them cloud-only **forever**: present on
/// disk, permanently reported as missing, and impossible to clear.
fn rel_of(master: &Path, trip_dir: &Path) -> Option<String> {
    let rel = master.strip_prefix(trip_dir).ok()?;
    if rel.components().count() < 2 {
        return None; // no owner folder → no provenance, not a in_cloud clip
    }
    Some(rel.to_str()?.replace('\\', "/"))
}

fn person_of(rel: &str) -> &str {
    rel.split('/').next().unwrap_or("")
}
fn base_of(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// Local masters of a trip as rel → size (skips derived dirs via `masters_in`).
pub fn local_set(cfg: &Config, trip: &str) -> FileSet {
    let trip_dir = cfg.lib.join(trip);
    local_set_from(&masters_in(&trip_dir), &trip_dir)
}

/// Build the local set from an already-walked master list — lets `build_trip`
/// reuse its `masters_in` result instead of walking twice.
pub fn local_set_from(masters: &[PathBuf], trip_dir: &Path) -> FileSet {
    let mut fs = FileSet::default();
    for m in masters {
        if let Some(rel) = rel_of(m, trip_dir) {
            let size = std::fs::metadata(m).map(|md| md.len()).unwrap_or(0);
            fs.insert(&rel, size);
        }
    }
    fs
}

/// The live cloud listing for a trip as rel → size (masters only). An absent path
/// reads as empty (`lsjson` returns nothing), not an error.
pub fn cloud_set(cfg: &Config, trip: &str) -> Result<FileSet, String> {
    let root = format!("{}/{}", cfg.remote.trim_end_matches('/'), trip);
    let entries = rclone::lsjson(&root)?;
    let mut fs = FileSet::default();
    for e in &entries {
        let path = e["Path"].as_str().unwrap_or("");
        if path.is_empty() || !has_ext(Path::new(path), CAPTURE_EXT) {
            continue;
        }
        fs.insert(&path.replace('\\', "/"), e["Size"].as_u64().unwrap_or(0));
    }
    Ok(fs)
}

fn is_shared_flag(dir: &Path) -> bool {
    matches!(
        trip_meta(dir, "share").as_deref(),
        Some("shared" | "verified" | "done" | "yes")
    )
}

/// Per-trip "is your footage here provably in the cloud?" — pure filesystem, no
/// network, so `scan_card` can gate "safe to clear" truthfully. A post-share
/// import (L grows past B) flips the trip out of the set; card reclaim's own
/// per-file `rclone check` remains the hard delete gate on top of this. Trips
/// with no baseline yet fall back to the `.reel share=` flag (no regression for
/// footage shared before this feature).
pub fn trips_in_cloud(cfg: &Config) -> HashMap<String, bool> {
    let user = cfg.user.as_str();
    let mut out = HashMap::new();
    for dir in trip_dirs(cfg) {
        let Some(name) = dir.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // Only your own subtree decides whether your footage is up — walking it
        // alone (not the whole trip, which may hold big pulled footage) keeps this
        // cheap enough to run on every card scan.
        let mine = local_set_from(&masters_in(&dir.join(user)), &dir);
        let base_p = cfg.base_path(name);
        let in_cloud = if base_p.exists() {
            let b = FileSet::load(&base_p);
            mine.files.iter().all(|(rel, sz)| b.get(rel) == Some(*sz))
        } else {
            mine.files.is_empty() || is_shared_flag(&dir)
        };
        out.insert(name.to_string(), in_cloud);
    }
    out
}

/// The buckets a (L, B, R) comparison sorts clips into, plus the baseline edits
/// the comparison implies (backfills and orphan-drops) — applied only when R was
/// fetched live.
#[derive(Default)]
struct Diff {
    to_push: Vec<SyncItem>,
    to_pull: Vec<SyncItem>,
    deleted_local: Vec<SyncItem>,
    deleted_upstream: Vec<SyncItem>,
    cloud_only: Vec<SyncItem>,
    conflicts: Vec<SyncItem>,
    b_updates: Vec<(String, u64)>,
    b_removes: Vec<String>,
}

fn item(rel: &str, size: u64, mine: bool) -> SyncItem {
    SyncItem {
        rel: rel.to_string(),
        person: person_of(rel).to_string(),
        name: base_of(rel).to_string(),
        bytes: size,
        mine,
    }
}

/// Sort every clip across L/B/R into a bucket. `skip` holds rels whose cloud copy
/// is mid-move (a queued `Move`), so they aren't misread as push/zombie.
fn classify(user: &str, l: &FileSet, b: &FileSet, r: &FileSet, skip: &HashSet<String>) -> Diff {
    let mut d = Diff::default();
    let keys: BTreeSet<&String> = l
        .files
        .keys()
        .chain(b.files.keys())
        .chain(r.files.keys())
        .collect();
    for rel in keys {
        if skip.contains(rel) {
            continue;
        }
        let mine = person_of(rel) == user;
        match (l.get(rel), b.get(rel), r.get(rel)) {
            // present all three — synced, or a size disagreement, or a stale B.
            (Some(ls), Some(bs), Some(rs)) => {
                if ls == rs {
                    if bs != ls {
                        d.b_updates.push((rel.clone(), ls)); // refresh stale B
                    }
                } else {
                    d.conflicts.push(item(rel, ls, mine));
                }
            }
            // local + intended, but the cloud lost it.
            (Some(ls), Some(_), None) => {
                if mine {
                    d.to_push.push(item(rel, ls, mine)); // you still hold it → re-share
                } else {
                    d.deleted_upstream.push(item(rel, ls, mine)); // friend removed it
                }
            }
            // local + cloud, no baseline row — backfill (first run / other machine).
            (Some(ls), None, Some(rs)) => {
                if ls == rs {
                    d.b_updates.push((rel.clone(), ls));
                } else {
                    d.conflicts.push(item(rel, ls, mine));
                }
            }
            // local only.
            (Some(ls), None, None) => {
                if mine {
                    d.to_push.push(item(rel, ls, mine)); // local-new, never shared
                }
                // else: a stray under a friend's folder that isn't in_cloud — no action.
            }
            // not local, but intended and present — footage that's in the cloud but
            // not on this machine: your archived raw (freed locally), or a pulled
            // clip you cleared. Safe and re-downloadable — surface it, keep B.
            (None, Some(bs), Some(_)) => d.cloud_only.push(item(rel, bs, mine)),
            // intended but gone both places — the delete completed; forget it.
            (None, Some(_), None) => d.b_removes.push(rel.clone()),
            // cloud only.
            (None, None, Some(rs)) => {
                if mine {
                    // In the cloud, not intended (a delete dropped it from B), not
                    // local → an offline-delete zombie to clean. (Single-writer: a
                    // clip you shared from another machine also lands here; it's
                    // surfaced, never auto-removed.)
                    d.deleted_local.push(item(rel, rs, mine));
                } else {
                    d.to_pull.push(item(rel, rs, mine)); // someone else's new footage
                }
            }
            (None, None, None) => {}
        }
    }
    d
}

/// Rels of this trip's queued moves, to skip while classifying.
fn pending_move_rels(pending: &Pending, trip: &str) -> HashSet<String> {
    pending
        .rows
        .iter()
        .filter_map(|(_, o)| match o {
            PendingOp::Move { to, rel, .. } if to == trip => Some(rel.clone()),
            _ => None,
        })
        .collect()
}

/// A trip whose footage is being relocated in the cloud by a queued rename or
/// whole-trip purge — the clips are up there under the OLD name until the op
/// replays. While migrating we must NOT offer to re-upload/pull them (that would
/// duplicate them under the new name and orphan the old); a reconcile replays the
/// queued op first, after which the normal diff takes over.
fn is_migrating(pending: &Pending, trip: &str) -> bool {
    pending.rows.iter().any(|(_, o)| match o {
        PendingOp::Rename { new, .. } => new == trip,
        PendingOp::Purge { trip: t } => t == trip,
        _ => false,
    })
}

/// Compute a trip's sync status. `refresh` fetches the cloud live (and rewrites
/// the cache + persists any baseline backfills); otherwise the cached listing is
/// used, or tier-1 only (push/deletions unknown) if the cloud was never fetched.
pub fn sync_status(cfg: &Config, trip: &str, refresh: bool) -> Result<TripSync, String> {
    let l = local_set(cfg, trip);
    let base_p = cfg.base_path(trip);
    // Whether a baseline was ever written — distinguishes a pre-feature trip (no
    // file → migrate-seed) from one a delete emptied (empty file → don't re-adopt
    // the clip you just deleted, so it reads as a zombie to clean).
    let base_existed = base_p.exists();
    let mut b = FileSet::load(&base_p);
    let pending = Pending::load(&cfg.pending_path());
    let skip = pending_move_rels(&pending, trip);

    // Rows adopted by the first-run migration below, replayed against fresh state
    // when we persist (the cloud fetch in between is a network round trip).
    let mut b_adopt: Vec<(String, u64)> = Vec::new();
    // Resolve R: live, or the cache, tracking whether we have one and its age.
    let mut offline = false;
    let (r, last_check, live): (Option<FileSet>, Option<i64>, bool) = if refresh {
        if rclone::remote_ok(&cfg.remote).is_ok() {
            let fetched = cloud_set(cfg, trip)?;
            let now = now_epoch();
            let _ = save_cloud_cache(&cfg.cloud_cache_path(trip), now, &fetched);
            // Migration: the first time we see a trip's cloud with no baseline file
            // yet, adopt what's already synced — clips present both sides, plus your
            // own cloud footage (e.g. archived raw) — so footage shared/archived
            // before this feature doesn't read as new-to-upload or a zombie. (An
            // *empty* baseline file means a delete emptied it — never re-adopt.)
            if !base_existed {
                for (rel, size) in &fetched.files {
                    if l.contains(rel) || person_of(rel) == cfg.user {
                        b.insert(rel, *size);
                        b_adopt.push((rel.clone(), *size));
                    }
                }
            }
            (Some(fetched), Some(now), true)
        } else {
            offline = true;
            let c = load_cloud_cache(&cfg.cloud_cache_path(trip));
            (c.checked.map(|_| c.files), c.checked, false)
        }
    } else {
        let c = load_cloud_cache(&cfg.cloud_cache_path(trip));
        (c.checked.map(|_| c.files), c.checked, false)
    };

    let mut ts = TripSync {
        trip: trip.to_string(),
        last_cloud_check: last_check,
        offline,
        pending: pending.count_for(trip),
        ..Default::default()
    };

    if let Some(r) = &r {
        let d = classify(&cfg.user, &l, &b, r, &skip);
        ts.to_push = d.to_push;
        ts.to_pull = d.to_pull;
        ts.deleted_local = d.deleted_local;
        ts.deleted_upstream = d.deleted_upstream;
        ts.cloud_only = d.cloud_only;
        ts.conflicts = d.conflicts;
        if live {
            // Replay the edits against fresh state under the lock — a push, pull or
            // delete may have written this baseline while the cloud listing above was
            // in flight, and a wholesale save of our snapshot would drop it.
            let _ = FileSet::update(&base_p, |base| {
                for (rel, size) in &b_adopt {
                    base.insert(rel, *size);
                }
                for (rel, size) in &d.b_updates {
                    base.insert(rel, *size);
                }
                for rel in &d.b_removes {
                    base.remove(rel);
                }
            });
        }
    } else {
        // Tier-1: only "to share" is knowable without the cloud. Deletions/pulls
        // stay hidden rather than guessed (an archived clip must not read as a
        // zombie).
        ts.to_push = tier1_to_push(cfg, &l, &b, trip);
    }

    // A queued rename/purge means this footage is already in the cloud (under the
    // old name); don't offer to re-upload or pull it — the reconcile replays the
    // move first.
    if is_migrating(&pending, trip) {
        ts.to_push.clear();
        ts.to_pull.clear();
        ts.deleted_local.clear();
        ts.conflicts.clear();
    }

    ts.in_sync = ts.to_push.is_empty()
        && ts.to_pull.is_empty()
        && ts.deleted_local.is_empty()
        && ts.conflicts.is_empty()
        && ts.pending == 0;
    Ok(ts)
}

/// Your local masters not yet in the baseline — the network-free "to share" set.
/// With no baseline at all, fall back to the `.reel share=` flag so a trip shared
/// before this feature doesn't read as entirely unshared.
fn tier1_to_push(cfg: &Config, l: &FileSet, b: &FileSet, trip: &str) -> Vec<SyncItem> {
    let user = cfg.user.as_str();
    let base_exists = cfg.base_path(trip).exists();
    if !base_exists && is_shared_flag(&cfg.lib.join(trip)) {
        return Vec::new();
    }
    l.files
        .iter()
        .filter(|(rel, sz)| person_of(rel) == user && b.get(rel) != Some(**sz))
        .map(|(rel, sz)| item(rel, *sz, true))
        .collect()
}

/// Compact status for the dashboard card — tier-1 "to share" always, plus the
/// cloud-side buckets from the cache when one exists. Network-free.
pub fn brief(cfg: &Config, trip: &str, masters: &[PathBuf], trip_dir: &Path) -> SyncBrief {
    let l = local_set_from(masters, trip_dir);
    let b = FileSet::load(&cfg.base_path(trip));
    let pending = Pending::load(&cfg.pending_path());
    let skip = pending_move_rels(&pending, trip);
    let c = load_cloud_cache(&cfg.cloud_cache_path(trip));
    // A queued rename/purge: the footage is in the cloud under the old name, so
    // don't count it as "to share"/"to pull" — the chip shows the owed op instead.
    let migrating = is_migrating(&pending, trip);

    let to_push = if migrating {
        0
    } else {
        tier1_to_push(cfg, &l, &b, trip).len()
    };
    let (mut to_pull, mut deleted_local, mut deleted_upstream, mut conflicts) = (0, 0, 0, 0);
    let mut cloud_only = 0;
    if !migrating && c.checked.is_some() {
        let d = classify(&cfg.user, &l, &b, &c.files, &skip);
        to_pull = d.to_pull.len();
        deleted_local = d.deleted_local.len();
        deleted_upstream = d.deleted_upstream.len();
        cloud_only = d.cloud_only.len();
        conflicts = d.conflicts.len();
    } else if !migrating {
        // No cloud listing cached yet: baseline entries you don't have locally are
        // cloud-only (archived/freed raw) — a good network-free estimate, so an
        // archived trip reads "☁ N in cloud" without a refresh first.
        cloud_only = b.files.keys().filter(|rel| !l.contains(rel)).count();
    }
    let pending = pending.count_for(trip);
    SyncBrief {
        to_push,
        to_pull,
        deleted_local,
        deleted_upstream,
        cloud_only,
        conflicts,
        pending,
        last_cloud_check: c.checked,
        in_sync: to_push == 0
            && to_pull == 0
            && deleted_local == 0
            && conflicts == 0
            && pending == 0,
    }
}

/// Replay one owed cloud op, returning whether it landed (a failure keeps it
/// queued). The baseline self-heals on the next `sync_status` refresh, so replay
/// only needs to touch the cloud.
fn replay(cfg: &Config, op: &PendingOp) -> bool {
    let remote = cfg.remote.trim_end_matches('/');
    match op {
        PendingOp::Move { from, to, rel } => rclone::move_path(
            &format!("{remote}/{from}/{rel}"),
            &format!("{remote}/{to}/{rel}"),
        )
        .unwrap_or(false),
        PendingOp::Rename { old, new } => rclone::move_path(
            &format!("{remote}/{old}/{}", cfg.user),
            &format!("{remote}/{new}/{}", cfg.user),
        )
        .unwrap_or(false),
        PendingOp::Purge { trip } => {
            rclone::purge(&format!("{remote}/{trip}/{}", cfg.user)).unwrap_or(false)
        }
    }
}

/// Reconcile one trip: replay its owed ops, refresh the cloud, then apply the
/// chosen push/pull/cleanup. Each leg persists its own baseline slice on success,
/// so a mid-reconcile stop leaves the baseline consistent with what actually ran.
pub fn reconcile(
    cfg: &Config,
    trip: &str,
    actions: SyncActions,
    mut on: impl FnMut(SyncProgress),
) -> Result<SyncResult, String> {
    rclone::remote_ok(&cfg.remote)?;

    // 1) replay this trip's owed ops first (a queued rename must run before a push
    // that would otherwise upload to the new name).
    let pending = Pending::load(&cfg.pending_path());
    let mut replayed = 0usize;
    let mut applied: Vec<PendingOp> = Vec::new();
    for (_, op) in pending.rows.iter().filter(|(_, o)| o.trip() == trip) {
        on(SyncProgress {
            phase: SyncPhase::Replay,
            file: op.trip().to_string(),
            done: replayed as u64,
            total: 0,
            trip: trip.to_string(),
            ..Default::default()
        });
        if replay(cfg, op) {
            replayed += 1;
            applied.push(op.clone());
        }
    }
    // Drop only what actually landed, re-read under the lock: replay is a network
    // call, so an op queued by another command meanwhile must survive (the old
    // wholesale rewrite of the queue would have erased it).
    if !applied.is_empty() {
        let _ = Pending::update(&cfg.pending_path(), |p| {
            p.rows.retain(|(_, o)| !applied.contains(o))
        });
    }

    // 2) refresh + classify (persists baseline backfills, rewrites the cache).
    on(SyncProgress {
        phase: SyncPhase::Check,
        trip: trip.to_string(),
        ..Default::default()
    });
    let status = sync_status(cfg, trip, true)?;

    // 3) apply chosen actions.
    let mut pushed = 0usize;
    let mut pulled = 0usize;
    let mut restored = 0usize;
    let mut deleted = 0usize;

    if actions.push && !status.to_push.is_empty() {
        let r = crate::push::push_trip(cfg, trip, |p| {
            on(SyncProgress {
                phase: SyncPhase::Push,
                file: p.file,
                done: p.done,
                total: p.total,
                trip: trip.to_string(),
                ..Default::default()
            })
        })?;
        pushed = r.files;
    }

    if actions.pull {
        let people: BTreeSet<String> = status.to_pull.iter().map(|i| i.person.clone()).collect();
        for person in people {
            let r = crate::pull::pull_person(cfg, trip, &person, |p| {
                on(SyncProgress {
                    phase: SyncPhase::Pull,
                    file: p.file,
                    done: p.done,
                    total: p.total,
                    trip: trip.to_string(),
                    ..Default::default()
                })
            })?;
            pulled += r.files;
        }
    }

    // Bring back what you archived or cleared. After push/pull so a restore always
    // lands against the freshest cloud listing, and before the deletion leg — the two
    // touch disjoint sets (`cloud_only` is L✗B✓R✓, `deleted_local` is L✗B✗R✓), but
    // ordering it this way means a run that both restores and cleans can't have the
    // clean racing a download.
    if actions.restore_cloud && !status.cloud_only.is_empty() {
        let rels: Vec<String> = status.cloud_only.iter().map(|i| i.rel.clone()).collect();
        let r = crate::restore::restore(cfg, trip, &rels, |p| {
            on(SyncProgress {
                phase: SyncPhase::Restore,
                file: p.file,
                done: p.done,
                total: p.total,
                trip: trip.to_string(),
                ..Default::default()
            })
        })?;
        restored = r.files;
    }

    if actions.push_deletions && !status.deleted_local.is_empty() {
        let remote = cfg.remote.trim_end_matches('/');
        let total = status.deleted_local.len() as u64;
        for (i, it) in status.deleted_local.iter().enumerate() {
            on(SyncProgress {
                phase: SyncPhase::Delete,
                file: it.name.clone(),
                done: i as u64,
                total,
                trip: trip.to_string(),
                ..Default::default()
            });
            if rclone::delete_file(&format!("{remote}/{trip}/{}", it.rel)).unwrap_or(false) {
                deleted += 1; // B already lacks it (dropped at delete time)
            }
        }
    }

    let still_pending = Pending::load(&cfg.pending_path()).count_for(trip);
    let final_status = sync_status(cfg, trip, true)?;
    Ok(SyncResult {
        trip: trip.to_string(),
        pushed,
        pulled,
        restored,
        deleted,
        replayed,
        still_pending,
        in_sync: final_status.in_sync,
    })
}

/// Global sync: replay every owed op, then fetch/diff/push/pull each trip. The
/// additive legs run for all trips (upload your unshared footage, pull what others
/// added); removing cloud footage and resolving conflicts stay a per-trip, opt-in
/// decision, so a whole-library sync can never delete anything.
///
/// The replay comes first and sweeps ALL trips — including a purge owed from a
/// trip you deleted offline, whose local dir is gone so no per-trip reconcile would
/// reach it.
pub fn reconcile_all(cfg: &Config, mut on: impl FnMut(SyncProgress)) -> Result<SyncResult, String> {
    rclone::remote_ok(&cfg.remote)?;

    // 1) flush the whole owed-op queue (covers orphaned purges).
    let pending = Pending::load(&cfg.pending_path());
    let mut replayed = 0usize;
    let total = pending.rows.len() as u64;
    let mut applied: Vec<PendingOp> = Vec::new();
    for (_, op) in pending.rows.iter() {
        on(SyncProgress {
            phase: SyncPhase::Replay,
            file: op.trip().to_string(),
            done: replayed as u64,
            total,
            trip: op.trip().to_string(),
            ..Default::default()
        });
        if replay(cfg, op) {
            replayed += 1;
            applied.push(op.clone());
        }
    }
    // Drop only what landed, re-read under the lock (see `reconcile`).
    if !applied.is_empty() {
        let _ = Pending::update(&cfg.pending_path(), |p| {
            p.rows.retain(|(_, o)| !applied.contains(o))
        });
    }

    // 2) fetch/diff/push/pull each existing trip (additive only).
    // Additive legs only. `restore_cloud` is additive too, but it re-fills disk you
    // deliberately freed — a whole-library sync that silently pulled back every
    // archived master would undo an archive in one click, so it stays per-trip.
    let acts = SyncActions {
        push: true,
        pull: true,
        push_deletions: false,
        restore_cloud: false,
    };
    let mut pushed = 0usize;
    let mut pulled = 0usize;
    let dirs = trip_dirs(cfg);
    let trip_count = dirs.len() as u32;
    for (i, dir) in dirs.iter().enumerate() {
        let Some(name) = dir.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let trip_index = i as u32 + 1;
        // One trip failing (a bad remote path, say) shouldn't abort the sweep.
        // Each event is stamped with this trip's place in the sweep so the UI can
        // show an overall "trip X of Y" alongside the per-trip line.
        if let Ok(r) = reconcile(cfg, name, acts, |mut p| {
            p.trip_index = trip_index;
            p.trip_count = trip_count;
            on(p);
        }) {
            pushed += r.pushed;
            pulled += r.pulled;
        }
    }

    let still_pending = Pending::load(&cfg.pending_path()).rows.len();
    Ok(SyncResult {
        trip: String::new(),
        pushed,
        pulled,
        restored: 0,
        deleted: 0,
        replayed,
        still_pending,
        in_sync: pushed == 0 && pulled == 0 && still_pending == 0,
    })
}
