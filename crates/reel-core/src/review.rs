//! Review: build a trip's skim playlist and read/write its marks.
//!
//! The player plays a cached clean proxy when one exists (built on demand from
//! the native `.LRF`/`.LRV`; see `proxy.rs`) but records marks against the
//! *master*, which shares the proxy's timeline — so the marks file stays
//! byte-compatible with the script's `reel review` / `reel cut`. That file is
//! `marks.tsv`: tab-separated `master<TAB>start<TAB>end<TAB>label`, one segment
//! per line, `#`/blank lines ignored, times plain seconds (`%.3f`).

use crate::config::Config;
use crate::ledger::Ledger;
use crate::media::{captured_at, masters_in, native_proxy_of, quick_fileid, rel_stem};
use crate::model::{Mark, Playlist, ReviewClip};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A trip name from the UI must stay a single path segment under the library.
fn valid_trip(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != ".."
}

/// Resolve a trip arg to its directory, erroring if it isn't a project.
fn trip_dir(cfg: &Config, trip: &str) -> Result<PathBuf, String> {
    if !valid_trip(trip) {
        return Err(format!("invalid trip name: {trip:?}"));
    }
    let dir = cfg.lib.join(trip);
    if !dir.join(".reel").is_file() {
        return Err(format!("no trip '{trip}'"));
    }
    Ok(dir)
}

/// Parse one `marks.tsv` line into a segment, or `None` for comments/blanks and
/// malformed rows (a bad line is skipped, never aborts the read).
fn parse_mark(line: &str) -> Option<Mark> {
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut it = line.splitn(4, '\t');
    let master = it.next()?;
    let start: f64 = it.next()?.trim().parse().ok()?;
    let end: f64 = it.next()?.trim().parse().ok()?;
    let label = it.next().unwrap_or("").to_string();
    if master.is_empty() {
        return None;
    }
    Some(Mark {
        master: master.to_string(),
        start,
        end,
        label,
    })
}

/// Read a trip's marks in file order (missing file → no marks).
pub fn read_marks(dir: &Path) -> Vec<Mark> {
    std::fs::read_to_string(dir.join("marks.tsv"))
        .map(|t| t.lines().filter_map(parse_mark).collect())
        .unwrap_or_default()
}

/// Rewrite `marks.tsv` from `marks`, atomically (temp sibling + rename). Emits
/// exactly the format `cut` reads; a tab/newline in a label is flattened to a
/// space so it can't split a row. Times to millisecond precision, matching the
/// mpv script.
pub fn write_marks(dir: &Path, marks: &[Mark]) -> std::io::Result<()> {
    let mut body = String::new();
    for m in marks {
        let label = m.label.replace(['\t', '\n', '\r'], " ");
        body.push_str(&format!(
            "{}\t{:.3}\t{:.3}\t{}\n",
            m.master, m.start, m.end, label
        ));
    }
    let tmp = dir.join("marks.tsv.partial");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, dir.join("marks.tsv"))
}

/// Build the review playlist for a trip: every master in capture order, each
/// paired with the file to actually play (proxy when available) and its poster
/// id, plus the marks saved so far.
pub fn review_playlist(cfg: &Config, trip: &str) -> Result<Playlist, String> {
    let dir = trip_dir(cfg, trip)?;
    let masters = masters_in(&dir);
    if masters.is_empty() {
        return Err(format!("no footage in '{trip}' to review yet"));
    }
    // fileids key the thumbnail cache. Computing one reads ~8 MiB of each master
    // (see `fileid_of`), which made opening a big trip slow. The ledger already
    // stored the id for every imported clip, so look it up by path and only fall
    // back to hashing for anything not on the ledger.
    let ledger = Ledger::load(&cfg.ledger_path());
    let id_by_path: HashMap<PathBuf, &str> = ledger
        .rows
        .iter()
        .map(|r| {
            let p = cfg
                .lib
                .join(&r.trip)
                .join(&r.person)
                .join(&r.camera)
                .join(&r.base);
            (p, r.id.as_str())
        })
        .collect();
    let clips = masters
        .iter()
        .map(|m| {
            // Play a cached clean proxy if we've built one; never the raw native
            // proxy (its extra streams break the webview) or, ideally, a huge
            // master. `has_proxy` tells the UI a fast remux source is on hand.
            let cached = dir
                .join(".proxies")
                .join(format!("{}.mp4", rel_stem(m, &dir)));
            let proxied = cached.is_file();
            let play = if proxied { &cached } else { m };
            let bytes = std::fs::metadata(m).map(|x| x.len()).unwrap_or(0);
            // Ledger id (content-addressed, shares posters with the card copy)
            // when we have it; otherwise a cheap stat-based id so an un-ledgered
            // trip still opens instantly.
            let fileid = id_by_path
                .get(m)
                .map(|s| s.to_string())
                .unwrap_or_else(|| quick_fileid(m));
            ReviewClip {
                master: m.display().to_string(),
                play: play.display().to_string(),
                name: m
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string(),
                fileid,
                captured: captured_at(m),
                bytes,
                proxied,
                has_proxy: native_proxy_of(m).is_some(),
                // A real camera master is always multiple MB; a sub-512 KiB file
                // is a card stub (empty placeholder, no streams) that can't play.
                stub: !proxied && bytes < 512 * 1024,
            }
        })
        .collect();
    Ok(Playlist {
        trip: trip.to_string(),
        clips,
        marks: read_marks(&dir),
    })
}

/// Replace a trip's marks with `marks` (a full rewrite — the UI owns the whole
/// list), returning how many were written. Master paths are trusted as the UI
/// got them from `review_playlist`; nothing else on disk is touched.
pub fn save_marks(cfg: &Config, trip: &str, marks: Vec<Mark>) -> Result<usize, String> {
    let dir = trip_dir(cfg, trip)?;
    write_marks(&dir, &marks).map_err(|e| format!("couldn't write marks: {e}"))?;
    Ok(marks.len())
}
