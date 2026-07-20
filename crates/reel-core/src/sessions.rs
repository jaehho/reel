//! Cluster capture-ordered card captures (videos and photos) into sessions on
//! time gaps.

use crate::model::{ClipRef, Session};

/// One card capture, as clustering needs it: capture time, size, whether it's a
/// photo (vs a video), the trip that already owns it (if any), whether it was
/// permanently deleted (tombstoned), and a pointer for its thumbnail.
pub struct ClipRec {
    pub at: i64,
    pub bytes: u64,
    pub photo: bool,
    pub owner: Option<String>,
    pub discarded: bool,
    pub clip: ClipRef,
}

/// Up to this many frames make a session's contact strip.
const STRIP_MAX: usize = 4;

/// Captures, **sorted by `at` ascending**, clustered by time: a gap strictly
/// greater than `gap` seconds between consecutive captures starts a new session.
/// Videos and photos cluster together, so a photo or panorama shot mid-session
/// rides along with it, and a run of photos with no video forms its own session.
pub fn cluster_sessions(caps: &[ClipRec], gap: i64) -> Vec<Session> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < caps.len() {
        let mut j = i;
        while j + 1 < caps.len() && caps[j + 1].at - caps[j].at <= gap {
            j += 1;
        }
        let slice = &caps[i..=j];

        // Owners, how many are already imported, how many are trash, how many photos.
        let mut owners: Vec<String> = Vec::new();
        let mut owned = 0usize;
        let mut discarded = 0usize;
        let mut photos = 0usize;
        for r in slice {
            if r.photo {
                photos += 1;
            }
            if r.discarded {
                discarded += 1;
            } else if let Some(o) = &r.owner {
                owned += 1;
                if !owners.contains(o) {
                    owners.push(o.clone());
                }
            }
        }

        let captures = slice.len();
        out.push(Session {
            index: out.len() + 1,
            start: slice[0].at,
            end: slice[slice.len() - 1].at,
            captures,
            photos,
            bytes: slice.iter().map(|r| r.bytes).sum(),
            owners,
            // Fully imported = every capture already owned (a discarded one, being
            // unowned, keeps a session out of "imported" until it's cleared).
            imported: owned == captures,
            // Discarded items are handled (thrown away), so they aren't "new".
            new_captures: captures - owned - discarded,
            discarded,
            strip: contact_strip(slice, STRIP_MAX),
            safe: false, // filled in by the caller once trip share state is known
        });
        i = j + 1;
    }
    out
}

/// Frames spread evenly across the session's captures (not just the opening ones),
/// so the strip reads like a real contact sheet of what the session contains.
fn contact_strip(caps: &[ClipRec], n: usize) -> Vec<ClipRef> {
    let take = n.min(caps.len());
    (0..take)
        .map(|k| {
            let idx = if take == 1 {
                0
            } else {
                k * (caps.len() - 1) / (take - 1)
            };
            caps[idx].clip.clone()
        })
        .collect()
}
