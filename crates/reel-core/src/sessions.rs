//! Cluster capture-ordered clips into sessions on time gaps.

use crate::model::{ClipRef, Session};

/// One card clip, as clustering needs it: capture time, size, the trip that
/// already owns it (if any), and a pointer for its thumbnail.
pub struct ClipRec {
    pub at: i64,
    pub bytes: u64,
    pub owner: Option<String>,
    pub clip: ClipRef,
}

/// Up to this many frames make a session's contact strip.
const STRIP_MAX: usize = 4;

/// Input **sorted by `at` ascending**. A gap strictly greater than `gap` seconds
/// between consecutive clips starts a new session.
pub fn cluster_sessions(recs: &[ClipRec], gap: i64) -> Vec<Session> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < recs.len() {
        let mut j = i;
        while j + 1 < recs.len() && recs[j + 1].at - recs[j].at <= gap {
            j += 1;
        }
        let slice = &recs[i..=j];

        let mut owners: Vec<String> = Vec::new();
        let mut owned = 0usize;
        for r in slice {
            if let Some(o) = &r.owner {
                owned += 1;
                if !owners.contains(o) {
                    owners.push(o.clone());
                }
            }
        }

        out.push(Session {
            index: out.len() + 1,
            start: slice[0].at,
            end: slice[slice.len() - 1].at,
            clips: slice.len(),
            bytes: slice.iter().map(|r| r.bytes).sum(),
            owners,
            imported: owned == slice.len(),
            new_clips: slice.len() - owned,
            strip: contact_strip(slice, STRIP_MAX),
            safe: false, // filled in by the caller once trip share state is known
        });
        i = j + 1;
    }
    out
}

/// Frames spread evenly across the session (not just the opening clips), so the
/// strip reads like a real contact sheet of what the session contains.
fn contact_strip(slice: &[ClipRec], n: usize) -> Vec<ClipRef> {
    let take = n.min(slice.len());
    (0..take)
        .map(|k| {
            let idx = if take == 1 {
                0
            } else {
                k * (slice.len() - 1) / (take - 1)
            };
            slice[idx].clip.clone()
        })
        .collect()
}
