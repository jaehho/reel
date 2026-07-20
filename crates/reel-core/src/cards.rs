//! SD card discovery and the session survey (`scan`).

use crate::config::Config;
use crate::ledger::{Ledger, Tombstones};
use crate::media::{captured_at, fileid_of, has_ext, is_photo};
use crate::model::{CardInfo, ClipRef};
use crate::sessions::{cluster_sessions, ClipRec};
use std::fs;
use std::path::PathBuf;

/// DCIM roots on mounted cards, or the `DJI_SD` / `GOPRO_SD` overrides.
pub fn card_roots(cfg: &Config) -> Vec<PathBuf> {
    let mut r: Vec<PathBuf> = Vec::new();
    if let Some(p) = &cfg.dji_sd {
        r.push(p.clone());
    }
    if let Some(p) = &cfg.gopro_sd {
        r.push(p.clone());
    }
    if r.is_empty() {
        let media = PathBuf::from("/run/media").join(&cfg.media_user);
        if let Ok(rd) = fs::read_dir(&media) {
            for e in rd.filter_map(|e| e.ok()) {
                r.push(e.path().join("DCIM"));
            }
        }
    }
    r.into_iter().filter(|p| p.is_dir()).collect()
}

/// `(epoch, path)` for every capture on the card roots, capture-ordered — videos
/// and photos both, skipping the `PANORAMA/` source-frame subtree (the stitched
/// panorama sits beside the videos as an ordinary photo). Same walk as
/// `media::masters_under`; named for its card use.
pub(crate) fn card_masters(roots: &[PathBuf]) -> Vec<(i64, PathBuf)> {
    crate::media::masters_under(roots)
}

/// One panorama's raw source frames on the card: DJI writes them under
/// `PANORAMA/<seq>/PANO_*.JPG` (the finished, stitched panorama is a normal photo
/// beside the videos). reel imports the stitched photo, not these — they're only
/// listed so a reclaim can sweep them off the card once that photo is cloud-safe
/// (see `wipe`). Sorted by capture time.
pub(crate) struct RawPano {
    pub at: i64,
    pub seq: String,
    pub photos: Vec<PathBuf>,
    pub bytes: u64,
}

/// Every panorama's source-frame folder on the card (one per `PANORAMA/<seq>/`
/// that holds at least one JPG), capture-ordered by the first frame's time. Used
/// only by reclaim to clear the frames alongside their stitched photo.
pub(crate) fn card_panoramas(roots: &[PathBuf]) -> Vec<RawPano> {
    let mut out = Vec::new();
    for r in roots {
        let Ok(rd) = fs::read_dir(r.join("PANORAMA")) else {
            continue;
        };
        for seq_dir in rd.filter_map(|e| e.ok()).map(|e| e.path()) {
            if !seq_dir.is_dir() {
                continue;
            }
            let mut photos: Vec<PathBuf> = fs::read_dir(&seq_dir)
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file() && has_ext(p, &["jpg", "jpeg"]))
                .collect();
            if photos.is_empty() {
                continue;
            }
            photos.sort();
            let bytes = photos
                .iter()
                .map(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0))
                .sum();
            out.push(RawPano {
                at: captured_at(&photos[0]),
                seq: seq_dir
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string(),
                photos,
                bytes,
            });
        }
    }
    out.sort_by_key(|p| p.at);
    out
}

/// Survey the inserted card: cluster its captures (videos + photos) into sessions
/// and annotate each with the trip(s) that own its captures, how many are new, and
/// whether it's safe to clear. `None` if no card is mounted.
pub fn scan_card(cfg: &Config) -> Option<CardInfo> {
    let roots = card_roots(cfg);
    if roots.is_empty() {
        return None;
    }
    let captures = card_masters(&roots);
    let ledger = Ledger::load(&cfg.ledger_path());
    let tombs = Tombstones::load(&cfg.tombstones_path());

    let recs: Vec<ClipRec> = captures
        .iter()
        .map(|(ep, p)| {
            let bytes = fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            let id = fileid_of(p).ok();
            // A tombstoned capture is trash the user already deleted: no owner, so
            // it reads "discarded" rather than "new" or "imported in <trip>".
            let discarded = id.as_deref().map(|i| tombs.contains(i)).unwrap_or(false);
            let owner = if discarded {
                None
            } else {
                id.as_deref().and_then(|i| ledger.trip_of(i))
            };
            ClipRec {
                at: *ep,
                bytes,
                photo: is_photo(p),
                owner,
                discarded,
                clip: ClipRef {
                    path: p.display().to_string(),
                    fileid: id.unwrap_or_default(),
                },
            }
        })
        .collect();

    let mut sessions = cluster_sessions(&recs, cfg.session_gap);

    // Safe to clear = every owning trip has its footage provably in the cloud.
    // Uses the sync baseline (falling back to the `.reel share=` flag for trips
    // shared before that existed), so a post-share import correctly flips its
    // trip out of "safe" — the raw isn't up there yet.
    let in_cloud = crate::sync::trips_in_cloud(cfg);
    for s in &mut sessions {
        s.safe = s.imported
            && !s.owners.is_empty()
            && s.owners
                .iter()
                .all(|o| in_cloud.get(o).copied().unwrap_or(false));
    }

    let photos = recs.iter().filter(|r| r.photo).count();
    Some(CardInfo {
        roots: roots.iter().map(|p| p.display().to_string()).collect(),
        captures: captures.len(),
        bytes: recs.iter().map(|r| r.bytes).sum(),
        photos,
        sessions,
    })
}
