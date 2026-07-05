//! SD card discovery and the session survey (`scan`).

use crate::config::Config;
use crate::ledger::Ledger;
use crate::media::{captured_at, fileid_of, has_ext, MASTER_EXT};
use crate::model::{CardInfo, ClipRef, Share};
use crate::sessions::{cluster_sessions, ClipRec};
use crate::trips::trip_shares;
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

/// `(epoch, path)` for every master on the card roots, capture-ordered.
fn card_masters(roots: &[PathBuf]) -> Vec<(i64, PathBuf)> {
    let mut v = Vec::new();
    for r in roots {
        for e in walkdir::WalkDir::new(r).into_iter().filter_map(|e| e.ok()) {
            if e.file_type().is_file() && has_ext(e.path(), MASTER_EXT) {
                v.push((captured_at(e.path()), e.path().to_path_buf()));
            }
        }
    }
    v.sort();
    v
}

/// Survey the inserted card: cluster its masters into sessions and annotate each
/// with the trip(s) that own its clips, how many clips are new, and whether it's
/// safe to clear. `None` if no card is mounted.
pub fn scan_card(cfg: &Config) -> Option<CardInfo> {
    let roots = card_roots(cfg);
    if roots.is_empty() {
        return None;
    }
    let masters = card_masters(&roots);
    let ledger = Ledger::load(&cfg.ledger_path());

    let recs: Vec<ClipRec> = masters
        .iter()
        .map(|(ep, p)| {
            let bytes = fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            let id = fileid_of(p).ok();
            let owner = id.as_deref().and_then(|i| ledger.trip_of(i));
            ClipRec {
                at: *ep,
                bytes,
                owner,
                clip: ClipRef {
                    path: p.display().to_string(),
                    fileid: id.unwrap_or_default(),
                },
            }
        })
        .collect();

    let mut sessions = cluster_sessions(&recs, cfg.session_gap);

    // Safe to clear = every clip imported into a trip whose footage is shared.
    let shares = trip_shares(cfg);
    for s in &mut sessions {
        s.safe = s.imported
            && !s.owners.is_empty()
            && s.owners
                .iter()
                .all(|o| matches!(shares.get(o), Some(Share::Shared)));
    }

    Some(CardInfo {
        roots: roots.iter().map(|p| p.display().to_string()).collect(),
        clips: masters.len(),
        bytes: recs.iter().map(|r| r.bytes).sum(),
        sessions,
    })
}
