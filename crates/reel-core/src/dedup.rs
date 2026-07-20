//! Global deduplication: find the same clip living in more than one place — copied
//! into two trips, or left behind as a cloud orphan by a rename/reorg — and, at the
//! user's pick, prune the redundant copies while keeping one canonical copy.
//!
//! This complements import-time dedup (the ledger), which only stops a clip *on a
//! card* re-entering a trip. Nothing there catches a clip that got copied into two
//! trips, or a whole cloud folder orphaned when a trip was renamed — that's what a
//! whole-library scan surfaces here.
//!
//! Identity is `(basename, byte size)` — the same rel+size trust the sync/push/pull
//! paths already run on. Camera filenames encode camera + timestamp + sequence, so
//! two clips share a name only when they're the same shot; requiring the size to
//! match too makes a false pair vanishingly unlikely, and a prune re-checks byte
//! identity by content id whenever both copies are local before deleting anything.
//!
//! Unlike a permanent delete (`remove.rs`), pruning a redundant copy does **not**
//! tombstone the content id: the clip lives on in the canonical copy, so it must
//! stay importable. The ledger pointer is moved to the survivor instead.

use crate::config::Config;
use crate::ledger::Ledger;
use crate::media::{fileid_of, has_ext, masters_in, rel_stem, under, CAPTURE_EXT};
use crate::model::{
    DupCopy, DupGroup, DupLoc, DupProgress, DupReport, DupResolution, DupResolveResult,
};
use crate::rclone;
use crate::store::{load_cloud_cache, save_cloud_cache, FileSet};
use crate::trips::trip_dirs;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn person_of(rel: &str) -> &str {
    rel.split('/').next().unwrap_or("")
}
fn base_of(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// A trip name that starts `YYYY-MM-DD` — reel's default when you don't name a
/// trip, so it's the less-meaningful home for a clip than a named one.
fn is_date_prefixed(t: &str) -> bool {
    let b = t.as_bytes();
    b.len() >= 10
        && b[0..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5].is_ascii_digit()
        && b[6].is_ascii_digit()
        && b[7] == b'-'
        && b[8].is_ascii_digit()
        && b[9].is_ascii_digit()
}
fn looks_defaultish(t: &str) -> bool {
    t.contains("_to_") || is_date_prefixed(t)
}

/// Rank a copy as the one to keep: a fully-synced copy (local **and** in the cloud)
/// beats a local-only one beats a cloud-only one, and a named trip beats a default
/// date-range name. Higher wins.
fn score(c: &DupCopy) -> i32 {
    let sync = if c.local && c.in_cloud {
        100
    } else if c.local {
        60
    } else {
        30
    };
    let named = if looks_defaultish(&c.trip) { 0 } else { 10 };
    sync + named
}

/// Index of the copy to keep by default (first of the top-scoring copies; `copies`
/// is pre-sorted, so the pick is stable across runs).
fn pick_canonical(copies: &[DupCopy]) -> usize {
    let mut best = 0;
    let mut best_score = i32::MIN;
    for (i, c) in copies.iter().enumerate() {
        let s = score(c);
        if s > best_score {
            best_score = s;
            best = i;
        }
    }
    best
}

/// One clip location while scanning — a `(trip, rel)` that may be on disk, in the
/// cloud, or both.
struct Loc {
    trip: String,
    rel: String,
    size: u64,
    local: bool,
    in_cloud: bool,
    path: Option<PathBuf>,
}

/// Scan the whole library and cloud for clips that exist in more than one place.
/// Local masters are a free directory walk; the cloud is one recursive `lsjson`.
/// When the cloud is unreachable the scan still runs over local footage and marks
/// itself `offline` (cloud-side pruning is then unavailable).
pub fn scan(cfg: &Config) -> Result<DupReport, String> {
    let mut locs: HashMap<(String, String), Loc> = HashMap::new();
    let mut scanned_local = 0usize;

    // Local: every master under every trip, keyed by (trip, cloud-relative path).
    for dir in trip_dirs(cfg) {
        let Some(trip) = dir.file_name().and_then(|s| s.to_str()).map(str::to_string) else {
            continue;
        };
        for m in masters_in(&dir) {
            let Some(rel) = m
                .strip_prefix(&dir)
                .ok()
                .and_then(|r| r.to_str())
                .map(|s| s.replace('\\', "/"))
            else {
                continue;
            };
            let size = std::fs::metadata(&m).map(|md| md.len()).unwrap_or(0);
            let loc = locs
                .entry((trip.clone(), rel.clone()))
                .or_insert_with(|| Loc {
                    trip: trip.clone(),
                    rel: rel.clone(),
                    size,
                    local: false,
                    in_cloud: false,
                    path: None,
                });
            loc.local = true;
            loc.size = size;
            loc.path = Some(m);
            scanned_local += 1;
        }
    }

    // Cloud: one recursive listing of the whole remote (`<trip>/<rel>`).
    let mut scanned_cloud = 0usize;
    let mut offline = false;
    if rclone::remote_ok(&cfg.remote).is_ok() {
        for e in rclone::lsjson(cfg.remote.trim_end_matches('/'))? {
            let path = e["Path"].as_str().unwrap_or("").replace('\\', "/");
            if path.is_empty() || !has_ext(Path::new(&path), CAPTURE_EXT) {
                continue;
            }
            let Some((trip, rel)) = path.split_once('/') else {
                continue; // a file at the cloud root, no trip — ignore
            };
            let size = e["Size"].as_u64().unwrap_or(0);
            let loc = locs
                .entry((trip.to_string(), rel.to_string()))
                .or_insert_with(|| Loc {
                    trip: trip.to_string(),
                    rel: rel.to_string(),
                    size,
                    local: false,
                    in_cloud: false,
                    path: None,
                });
            loc.in_cloud = true;
            if !loc.local {
                loc.size = size;
            }
            scanned_cloud += 1;
        }
    } else {
        offline = true;
    }

    // Group locations by content identity; a group of ≥2 distinct locations is a
    // set of duplicates.
    let mut groups: HashMap<(String, u64), Vec<Loc>> = HashMap::new();
    for (_, loc) in locs {
        let base = base_of(&loc.rel).to_string();
        groups.entry((base, loc.size)).or_default().push(loc);
    }

    let user = cfg.user.as_str();
    let mut out: Vec<DupGroup> = Vec::new();
    let mut total_reclaimable = 0u64;
    for ((base, size), mut group) in groups {
        if group.len() < 2 {
            continue;
        }
        group.sort_by(|a, b| a.trip.cmp(&b.trip).then_with(|| a.rel.cmp(&b.rel)));
        let copies: Vec<DupCopy> = group
            .iter()
            .map(|l| DupCopy {
                trip: l.trip.clone(),
                person: person_of(&l.rel).to_string(),
                rel: l.rel.clone(),
                name: base.clone(),
                bytes: l.size,
                local: l.local,
                in_cloud: l.in_cloud,
                mine: person_of(&l.rel) == user,
                path: l.path.as_ref().map(|p| p.display().to_string()),
            })
            .collect();
        let suggested_keep = pick_canonical(&copies);
        let reclaimable = size * (copies.len() as u64 - 1);
        total_reclaimable += reclaimable;
        out.push(DupGroup {
            key: format!("{base}|{size}"),
            name: base,
            bytes: size,
            copies,
            reclaimable,
            suggested_keep,
        });
    }
    // Biggest wins first, then by name for a stable order.
    out.sort_by(|a, b| {
        b.reclaimable
            .cmp(&a.reclaimable)
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(DupReport {
        groups_count: out.len(),
        total_reclaimable,
        scanned_local,
        scanned_cloud,
        offline,
        groups: out,
    })
}

fn keep_local_path(cfg: &Config, keep: &DupLoc) -> Option<PathBuf> {
    keep.local.then(|| cfg.lib.join(&keep.trip).join(&keep.rel))
}

/// Re-point any ledger row for `id` at the surviving copy, so a content id whose
/// recorded home we just pruned still resolves to a file that exists.
fn repoint_ledger(ledger: &mut Ledger, id: &str, keep_trip: &str, keep_rel: &str) {
    if let Some(row) = ledger.rows.iter_mut().find(|r| r.id == id) {
        let parts: Vec<&str> = keep_rel.split('/').collect();
        row.trip = keep_trip.to_string();
        row.person = parts.first().copied().unwrap_or("").to_string();
        row.base = parts.last().copied().unwrap_or("").to_string();
        row.camera = if parts.len() >= 3 {
            parts[1..parts.len() - 1].join("/")
        } else {
            String::new()
        };
    }
}

/// Prune the chosen duplicates: for each group keep `keep` and remove every copy in
/// `remove` — unlink the local master (+ its sidecars/proxy) and delete the cloud
/// copy. Never tombstones (the content survives in the canonical), and never prunes
/// a copy unless a survivor is provably present: the canonical's local file exists,
/// or it's in the cloud and the remote is reachable. A local↔local prune re-checks
/// content identity first, so a mere name+size collision can't cause a loss.
pub fn resolve(
    cfg: &Config,
    resolutions: Vec<DupResolution>,
    mut on: impl FnMut(DupProgress),
) -> Result<DupResolveResult, String> {
    let online = rclone::remote_ok(&cfg.remote).is_ok();
    let remote = cfg.remote.trim_end_matches('/');
    // (content id, surviving trip, surviving rel) for each pruned local copy. The
    // loop below deletes from the cloud over the network, so the ledger is merged
    // once at the end rather than held across every call.
    let mut repoints: Vec<(String, String, String)> = Vec::new();
    let mut res = DupResolveResult {
        cloud_ok: true,
        offline: !online,
        groups: resolutions.len(),
        ..Default::default()
    };
    let mut base_removes: HashMap<String, Vec<String>> = HashMap::new();
    let total: u64 = resolutions.iter().map(|r| r.remove.len() as u64).sum();
    let mut done = 0u64;

    for r in &resolutions {
        let keep_path = keep_local_path(cfg, &r.keep);
        let keep_local_ok = keep_path.as_ref().map(|p| p.is_file()).unwrap_or(false);
        let keep_id = keep_path
            .as_ref()
            .filter(|p| p.is_file())
            .and_then(|p| fileid_of(p).ok());
        // A survivor must be provably present before we delete anything else.
        let canonical_safe = keep_local_ok || (online && r.keep.in_cloud);

        for rm in &r.remove {
            done += 1;
            on(DupProgress {
                file: base_of(&rm.rel).to_string(),
                done,
                total,
            });
            if !canonical_safe || (rm.trip == r.keep.trip && rm.rel == r.keep.rel) {
                res.skipped += 1;
                continue;
            }

            let lp = cfg.lib.join(&rm.trip).join(&rm.rel);
            let lp_here = rm.local && lp.is_file() && under(&lp, &cfg.lib);
            let rm_id = if lp_here { fileid_of(&lp).ok() } else { None };

            // If both this copy and the canonical are on disk, they must be
            // byte-identical — a name+size match alone isn't enough to delete.
            if let (Some(k), Some(v)) = (&keep_id, &rm_id) {
                if k != v {
                    res.skipped += 1;
                    continue;
                }
            }

            let mut removed_any = false;

            if lp_here && std::fs::remove_file(&lp).is_ok() {
                res.removed_local += 1;
                removed_any = true;
                crate::remove::remove_sidecars(&lp);
                let stem = rel_stem(&lp, &cfg.lib.join(&rm.trip));
                let _ = std::fs::remove_file(
                    cfg.lib
                        .join(&rm.trip)
                        .join(".proxies")
                        .join(format!("{stem}.mp4")),
                );
                if let Some(id) = &rm_id {
                    repoints.push((id.clone(), r.keep.trip.clone(), r.keep.rel.clone()));
                }
            }

            // Only ever remove YOUR footage from the shared cloud. A duplicate
            // sitting under someone else's `person/` folder is their contribution
            // — pruning it would delete a collaborator's only cloud copy. Same rule
            // `remove::delete_clips` follows ("the cloud copy is theirs to keep");
            // the redundant local copy above is still pruned either way.
            let theirs_in_cloud = rm.in_cloud && person_of(&rm.rel) != cfg.user;
            if theirs_in_cloud {
                res.kept_cloud += 1;
            } else if rm.in_cloud {
                if online {
                    if rclone::delete_file(&format!("{remote}/{}/{}", rm.trip, rm.rel))
                        .unwrap_or(false)
                    {
                        res.removed_cloud += 1;
                        removed_any = true;
                    } else {
                        res.cloud_ok = false;
                    }
                } else {
                    res.cloud_ok = false;
                }
            }

            if removed_any {
                res.freed += rm.bytes;
                // Their cloud copy survives, so this rel is still legitimately in the
                // cloud: keep the baseline row. Dropping it would leave `L✗ B✗ R✓`
                // for someone else's footage, which `sync::classify` reads as "to
                // pull" — and a Sync would re-download what we just pruned.
                if !theirs_in_cloud {
                    base_removes
                        .entry(rm.trip.clone())
                        .or_default()
                        .push(rm.rel.clone());
                }
            }
        }
    }

    // The pruned copies are no longer intended in the cloud: drop them from each
    // trip's baseline and its cached listing (a no-op for rels not present).
    for (trip, rels) in &base_removes {
        let _ = FileSet::update(&cfg.base_path(trip), |base| {
            for rel in rels {
                base.remove(rel);
            }
        });

        let pc = cfg.cloud_cache_path(trip);
        let cache = load_cloud_cache(&pc);
        if let Some(checked) = cache.checked {
            let mut files = cache.files;
            for rel in rels {
                files.remove(rel);
            }
            let _ = save_cloud_cache(&pc, checked, &files);
        }
    }
    let _ = Ledger::update(&cfg.ledger_path(), |l| {
        for (id, trip, rel) in &repoints {
            repoint_ledger(l, id, trip, rel);
        }
    });

    Ok(res)
}
