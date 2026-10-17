//! Reclaim space on the card: delete the masters that are verified-imported AND
//! confirmed in the shared cloud, leaving anything unproven untouched. Two steps,
//! mirroring the script's `wipe`: `plan_reclaim` matches card files to their
//! local copies and cloud-checks them (no deletion), then `commit_reclaim` removes
//! exactly the planned files. Pure engine — progress streams through a callback.
//!
//! Only ledger-known masters with a matching local copy and a verified cloud copy
//! are ever deleted; a card file that's unimported, mismatched, or missing from
//! the cloud stays put. Losing the only copy of footage is unrecoverable, so the
//! cloud check aborts the whole reclaim on the first miss.

use crate::cards::{card_panoramas, card_roots, RawPano};
use crate::config::Config;
use crate::ledger::Ledger;
use crate::media::{fileid_of, is_photo, masters_under};
use crate::model::{ReclaimPlan, ReclaimResult, WipePhase, WipeProgress};
use crate::rclone;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

fn base_of(p: &Path) -> String {
    p.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

/// Confirm `<lib>/<trip>/<rels…>` all hash-match in `<remote>/<trip>`, streaming
/// rclone's per-file check progress. The `--files-from` list scopes the check to
/// exactly the masters we're about to delete off the card.
fn verify_in_cloud(
    cfg: &Config,
    trip: &str,
    rels: &[String],
    on: &mut impl FnMut(WipeProgress),
) -> Result<(), String> {
    // `trip` here is a ledger `row.trip`, read from a TSV shared with the legacy
    // script and never validated on the way in. Unlike every other entry point in
    // these modules it isn't guarded, yet it lands in a `.wipe-check-{trip}.lst`
    // filename and in `lib`/`remote` paths — so a crafted `../../x` would escape the
    // state dir and misdirect the check. Refuse it; a reclaim that can't prove a
    // backup deletes nothing.
    if trip.is_empty() || trip.contains(['/', '\\']) || trip == "." || trip == ".." {
        return Err(format!("ledger has a bad trip name: {trip:?}"));
    }
    rclone::remote_ok(&cfg.remote)?;
    fs::create_dir_all(&cfg.state_dir).ok();
    let list = cfg.state_dir.join(format!(".wipe-check-{trip}.lst"));
    fs::write(&list, format!("{}\n", rels.join("\n")))
        .map_err(|e| format!("couldn't stage cloud check: {e}"))?;

    let lib_trip = cfg.lib.join(trip);
    let remote_trip = format!("{}/{}", cfg.remote.trim_end_matches('/'), trip);
    let args: Vec<OsString> = vec![
        "check".into(),
        "--files-from".into(),
        list.as_os_str().to_os_string(),
        "--one-way".into(),
        lib_trip.as_os_str().to_os_string(),
        remote_trip.into(),
    ];
    let ok = rclone::stream(args, |v| {
        let s = &v["stats"];
        on(WipeProgress {
            phase: WipePhase::Verify,
            done: s["checks"].as_u64().unwrap_or(0),
            total: s["totalChecks"].as_u64().unwrap_or(0),
            label: trip.to_string(),
        });
    });
    let _ = fs::remove_file(&list);
    match ok {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!(
            "'{trip}' isn't fully in the cloud — Share it first; nothing deleted"
        )),
        Err(e) => Err(e),
    }
}

/// Work out what a reclaim would delete. `window` scopes it to one capture
/// session (`[w0, w1]`, as the card panel offers per session); `None` considers
/// every master on the card. With `offline` true the cloud check is skipped — the
/// card's footage then rests on a single local copy, so the caller must warn.
pub fn plan_reclaim(
    cfg: &Config,
    window: Option<(i64, i64)>,
    offline: bool,
    mut on: impl FnMut(WipeProgress),
) -> Result<ReclaimPlan, String> {
    let roots = card_roots(cfg);
    if roots.is_empty() {
        return Err("no card inserted".into());
    }
    // Captures eligible for reclaim — videos and photos both. `masters_under`
    // skips the PANORAMA/ source frames; those are swept below, riding the fate of
    // their stitched photo (which we did import) rather than matched on their own.
    let mut files = masters_under(&roots);
    if let Some((w0, w1)) = window {
        files.retain(|(at, _)| *at >= w0 && *at <= w1);
    }
    if files.is_empty() {
        return Err("no footage on the card for this selection".into());
    }

    let ledger = Ledger::load(&cfg.ledger_path());
    let mut ok: Vec<(PathBuf, u64)> = Vec::new();
    let mut cloud_rels: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut not_imported = 0usize;
    let mut not_verified = 0usize;

    // Match each card capture to its local copy via the ledger. Only files we can
    // place AND whose local size agrees are eligible; the rest stay on the card.
    // The local path and cloud rel use the ledger's recorded `person/camera/base`.
    let total = files.len() as u64;
    for (i, (_, path)) in files.iter().enumerate() {
        on(WipeProgress {
            phase: WipePhase::Match,
            done: i as u64,
            total,
            label: base_of(path),
        });
        let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let Ok(fid) = fileid_of(path) else {
            not_imported += 1;
            continue;
        };
        let Some(row) = ledger.row_of(&fid) else {
            not_imported += 1;
            continue;
        };
        let localf = cfg
            .lib
            .join(&row.trip)
            .join(&row.person)
            .join(&row.camera)
            .join(&row.base);
        if fs::metadata(&localf).map(|m| m.len()).ok() != Some(size) {
            not_verified += 1;
            continue;
        }
        ok.push((path.clone(), size));
        cloud_rels
            .entry(row.trip.clone())
            .or_default()
            .push(format!("{}/{}/{}", row.person, row.camera, row.base));
    }

    if ok.is_empty() {
        return Err(
            "nothing here is verified-imported yet — import and share these clips first".into(),
        );
    }

    // Cloud gate: every master must hash-match in its trip's cloud, or we plan no
    // deletion at all. Conservative on purpose.
    if !offline {
        for (trip, rels) in &cloud_rels {
            verify_in_cloud(cfg, trip, rels, &mut on)?;
        }
    }

    let mut bytes: u64 = ok.iter().map(|(_, s)| *s).sum();
    let mut files: Vec<String> = ok.iter().map(|(p, _)| p.display().to_string()).collect();

    // Sweep each cleared panorama's raw source frames off the card too. DJI keeps a
    // panorama's frames under PANORAMA/<seq>/ and the finished, stitched image as an
    // ordinary photo beside the videos — we imported the photo, so the frames would
    // otherwise linger. They ride the photo's fate: swept only once that photo is
    // itself cleared-safe (in `ok`, so it passed import + local + the cloud gate).
    //
    // The pairing is checked against *every* photo on the card, not just the ones
    // being cleared: the 4-digit seq is a per-camera counter, so it recurs (another
    // card, an ordinary photo numbered the same), and a cleared photo must not vouch
    // for some other capture's frames.
    let safe_photos: BTreeSet<&Path> = ok
        .iter()
        .map(|(p, _)| p.as_path())
        .filter(|p| is_photo(p))
        .collect();
    if !safe_photos.is_empty() {
        let card_photos: Vec<(i64, PathBuf)> = masters_under(&roots)
            .into_iter()
            .filter(|(_, p)| is_photo(p))
            .collect();
        for raw in card_panoramas(&roots) {
            if pano_cleared(&raw, &card_photos, &safe_photos) {
                for photo in raw.photos {
                    bytes += fs::metadata(&photo).map(|m| m.len()).unwrap_or(0);
                    files.push(photo.display().to_string());
                }
            }
        }
    }

    Ok(ReclaimPlan {
        bytes,
        files,
        trips: cloud_rels.keys().cloned().collect(),
        not_imported,
        not_verified,
        offline,
    })
}

/// The panorama sequence number in a DJI name — the last underscore-separated run
/// of exactly four digits. A stitched photo `DJI_<ts>_0039_D.JPG` and its source
/// folder `001_0039` both yield `0039`, so one maps to the other. `None` when
/// there's no such group (an ordinary photo with no matching folder just won't
/// match, which is what we want).
fn seq_num(name: &str) -> Option<String> {
    name.split('_')
        .rfind(|c| c.len() == 4 && c.bytes().all(|b| b.is_ascii_digit()))
        .map(str::to_string)
}

/// A raw panorama source frame — anything under a `PANORAMA/` component. The
/// ledger never holds these (reel imports the stitched photo, not the frames), so
/// the commit-time identity check has to reach them by their sequence instead.
fn is_pano_frame(p: &Path) -> bool {
    p.components()
        .any(|c| c.as_os_str().to_str() == Some("PANORAMA"))
}

/// The sequence a source frame belongs to, from its `PANORAMA/<seq>/` folder.
fn seq_of_pano_frame(p: &Path) -> Option<String> {
    p.parent()
        .and_then(|d| d.file_name())
        .and_then(|s| s.to_str())
        .and_then(seq_num)
}

/// How far apart (seconds) a stitched photo and its source frames can be captured
/// and still be one panorama. The stitch lands within moments of the last frame.
const PANO_SLOP_SECS: i64 = 900;

/// Is this panorama's raw-frame folder safe to sweep? Its candidates are the card
/// photos carrying the same `seq` *and* captured close in time; the frames go only
/// when there is at least one and every one of them is cleared. A seq with no
/// close photo (never stitched, or stitched elsewhere) and a seq shared with a
/// photo that isn't being cleared both keep the frames.
fn pano_cleared(raw: &RawPano, card_photos: &[(i64, PathBuf)], safe: &BTreeSet<&Path>) -> bool {
    let Some(seq) = seq_num(&raw.seq) else {
        return false;
    };
    let mut near = card_photos
        .iter()
        .filter(|(at, p)| {
            (at - raw.at).abs() <= PANO_SLOP_SECS
                && p.file_name()
                    .and_then(|s| s.to_str())
                    .and_then(seq_num)
                    .is_some_and(|s| s == seq)
        })
        .peekable();
    near.peek().is_some() && near.all(|(_, p)| safe.contains(p.as_path()))
}

/// True if `p` resolves to a location inside `root` (symlinks resolved), so a
/// delete can't escape the card.
fn under(p: &Path, root: &Path) -> bool {
    match (p.canonicalize(), root.canonicalize()) {
        (Ok(pc), Ok(rc)) => pc.starts_with(rc),
        _ => p.starts_with(root),
    }
}

/// Delete the planned card files. Each path is re-checked to live under a current
/// card root before removal, so a stale or stray path can never touch anything
/// off the card. Returns what actually left the card.
pub fn commit_reclaim(cfg: &Config, files: &[String]) -> Result<ReclaimResult, String> {
    let roots = card_roots(cfg);
    if roots.is_empty() {
        return Err("no card inserted".into());
    }
    let mut deleted = 0usize;
    let mut bytes = 0u64;
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();

    // Re-establish *identity*, not just location. Plan and commit are two separate
    // user-driven calls with a confirm dialog between them, and a card mount path
    // (`/run/media/<user>/GOPRO/DCIM/100GOPRO/…`) is stable across cards from the
    // same camera — so swapping cards at the dialog would otherwise delete
    // never-imported footage off the new one. `clear_discarded` already re-derives
    // the content id before each delete; this is the same rule, against the ledger.
    let ledger = Ledger::load(&cfg.ledger_path());
    let verified = |p: &Path| -> bool {
        let Ok(fid) = fileid_of(p) else { return false };
        let Some(row) = ledger.row_of(&fid) else {
            return false;
        };
        let localf = cfg
            .lib
            .join(&row.trip)
            .join(&row.person)
            .join(&row.camera)
            .join(&row.base);
        fs::metadata(&localf).map(|m| m.len()).ok() == fs::metadata(p).map(|m| m.len()).ok()
    };
    // Panorama source frames are raw stock the ledger never holds — they ride their
    // stitched photo's fate, so re-derive which sequences are still safe from the
    // photos that just passed above, exactly as `plan_reclaim` does.
    let safe_seqs: BTreeSet<String> = files
        .iter()
        .map(PathBuf::from)
        .filter(|p| !is_pano_frame(p) && is_photo(p) && verified(p))
        .filter_map(|p| p.file_name().and_then(|s| s.to_str()).and_then(seq_num))
        .collect();

    for f in files {
        let p = Path::new(f);
        if !roots.iter().any(|r| under(p, r)) {
            continue; // never delete anything that isn't on the card
        }
        let safe = if is_pano_frame(p) {
            seq_of_pano_frame(p).is_some_and(|s| safe_seqs.contains(&s))
        } else {
            verified(p)
        };
        if !safe {
            continue; // not the file that was planned — a different card, or changed
        }
        let size = fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        if fs::remove_file(p).is_ok() {
            deleted += 1;
            bytes += size;
            if let Some(parent) = p.parent() {
                dirs.insert(parent.to_path_buf());
            }
        }
    }
    // Tidy up directories we just emptied — a panorama's `<seq>/` source folder,
    // and its `PANORAMA/` parent once that folder held the last one. `remove_dir`
    // only removes an empty directory, so this never touches a folder with files
    // left (the camera's DCIM/<model>/ dirs stay unless every file in them went).
    for d in &dirs {
        if fs::remove_dir(d).is_ok() {
            if let Some(parent) = d.parent() {
                if parent.file_name().and_then(|s| s.to_str()) == Some("PANORAMA") {
                    let _ = fs::remove_dir(parent);
                }
            }
        }
    }
    crate::log::event(
        crate::log::Level::Info,
        "engine",
        "reclaim",
        Some(serde_json::json!({ "deleted": deleted, "bytes": bytes })),
    );
    Ok(ReclaimResult { deleted, bytes })
}
