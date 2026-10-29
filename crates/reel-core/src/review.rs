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
use crate::media::{captured_at, is_photo, masters_in, native_proxy_of, quick_fileid, rel_stem};
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
    // Unique temp: a fixed `.partial` name lets two writers (the player autosaving
    // while a move migrates marks) publish each other's half-written body.
    let out = dir.join("marks.tsv");
    let tmp = crate::store::temp_sibling(&out);
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &out).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Read-modify-write a trip's `marks.tsv` under the state lock. The player
/// autosaves the whole list (`save_marks`) while a move, delete, or rename
/// filters or repoints it; done as two bare read/writes, the slower side's
/// write-back resurrects a mark the other just removed. Same rule as
/// `Ledger::update`: slow work first, then merge into fresh state here.
pub fn update_marks(dir: &Path, f: impl FnOnce(&mut Vec<Mark>)) -> std::io::Result<()> {
    let _g = crate::store::state_guard();
    let mut marks = read_marks(dir);
    f(&mut marks);
    write_marks(dir, &marks)
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
    // Per-master marks and cut counts ride on each clip for the organize
    // board's cells (#58) — the player keeps its own live mark count instead.
    let marks = read_marks(&dir);
    let mut marks_n: HashMap<String, usize> = HashMap::new();
    for m in &marks {
        *marks_n.entry(m.master.clone()).or_default() += 1;
    }
    let cuts_n = crate::cut::cuts_by_master(&dir.join("clips"));
    let clips = masters
        .iter()
        .map(|m| {
            let key = m.display().to_string();
            // Play a cached clean proxy if we've built one; never the raw native
            // proxy (its extra streams break the webview) or, ideally, a huge
            // master. `has_proxy` tells the UI a cheap source to build from is on
            // hand, so the build is a 720p re-encode rather than a 4K HEVC one.
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
            // Provenance: the person folder this master sits in (`<trip>/<person>/…`).
            // Via `parent()`, so a stray at the trip root gets no badge rather
            // than one naming itself.
            let person = m
                .strip_prefix(&dir)
                .ok()
                .and_then(|r| r.parent().map(|p| p.to_path_buf()))
                .as_deref()
                .and_then(|p| p.components().next())
                .and_then(|c| c.as_os_str().to_str())
                .unwrap_or("")
                .to_string();
            let mine = person == cfg.user;
            // A photo is shown directly as an image — no proxy, and its poster is
            // itself (thumbs downsizes the JPEG). Videos pull frames from the small
            // proxy when one exists.
            let photo = is_photo(m);
            let native = native_proxy_of(m);
            let poster = if proxied {
                play.clone()
            } else {
                native.clone().unwrap_or_else(|| m.clone())
            };
            ReviewClip {
                master: key.clone(),
                play: play.display().to_string(),
                name: m
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string(),
                fileid,
                captured: captured_at(m),
                bytes,
                poster: poster.display().to_string(),
                proxied,
                has_proxy: native.is_some(),
                // A real camera master is always multiple MB; a sub-512 KiB file
                // is a card stub (empty placeholder, no streams) that can't play.
                // Photos are never stubs — they display as images at any size.
                stub: !photo && !proxied && bytes < 512 * 1024,
                photo,
                person,
                mine,
                marks: marks_n.get(&key).copied().unwrap_or(0),
                cuts: cuts_n.get(&key).copied().unwrap_or(0),
            }
        })
        .collect();
    let st = read_review_state(&dir);
    Ok(Playlist {
        trip: trip.to_string(),
        clips,
        marks,
        last: st.last,
        watched: st.watched,
    })
}

/// Build a **read-only** preview playlist for the inserted card, optionally
/// limited to one capture session's `[start, end]` window. Card footage isn't a
/// trip, so there are no marks; `play` is a cached card proxy when one's been
/// built (see `proxy::ensure_card_proxy`), else the master. Ids are the cheap
/// stat-based `quick_fileid` — the card isn't on the ledger — so opening a session
/// stays instant no matter how many clips it holds.
pub fn card_playlist(cfg: &Config, window: Option<(i64, i64)>) -> Result<Playlist, String> {
    let roots = crate::cards::card_roots(cfg);
    if roots.is_empty() {
        return Err("no card inserted".into());
    }
    let mut masters = crate::cards::card_masters(&roots);
    if let Some((start, end)) = window {
        masters.retain(|(at, _)| *at >= start && *at <= end);
    }
    if masters.is_empty() {
        return Err("no footage on the card to preview".into());
    }
    let clips = masters
        .iter()
        .map(|(at, m)| {
            let fileid = quick_fileid(m);
            let cached = cfg.card_proxy_path(&fileid);
            let proxied = cached.is_file();
            let play = if proxied { &cached } else { m };
            let bytes = std::fs::metadata(m).map(|x| x.len()).unwrap_or(0);
            // A photo shows directly as an image (poster = itself). Video frames
            // come from the small proxy, never the multi-GB master: a built proxy
            // if we have one, else the native `.LRF`/`.LRV` on the card.
            let photo = is_photo(m);
            let native = native_proxy_of(m);
            let poster = if proxied {
                play.clone()
            } else {
                native.clone().unwrap_or_else(|| m.clone())
            };
            ReviewClip {
                master: m.display().to_string(),
                play: play.display().to_string(),
                name: m
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string(),
                fileid,
                captured: *at,
                bytes,
                poster: poster.display().to_string(),
                proxied,
                has_proxy: native.is_some(),
                // A real camera master is always multiple MB; a sub-512 KiB file
                // is a card stub (empty placeholder, no streams) that can't play.
                // Photos are never stubs — they display as images at any size.
                stub: !photo && !proxied && bytes < 512 * 1024,
                photo,
                // No provenance for card footage — it isn't in any trip yet.
                person: String::new(),
                mine: false,
                marks: 0,
                cuts: 0,
            }
        })
        .collect();
    Ok(Playlist {
        trip: "card".to_string(),
        clips,
        marks: Vec::new(),
        // No review bookkeeping for card footage — it isn't in a trip yet.
        last: None,
        watched: Vec::new(),
    })
}

/// Replace a trip's marks with `marks` (a full rewrite — the UI owns the whole
/// list), returning how many were written. Master paths are trusted as the UI
/// got them from `review_playlist`; nothing else on disk is touched.
pub fn save_marks(cfg: &Config, trip: &str, marks: Vec<Mark>) -> Result<usize, String> {
    let dir = trip_dir(cfg, trip)?;
    let n = marks.len();
    update_marks(&dir, |cur| *cur = marks).map_err(|e| format!("couldn't write marks: {e}"))?;
    Ok(n)
}

/// What the review has covered so far (#55): the master that was on screen when
/// the player closed, and every master that has been on screen. Stored beside
/// `marks.tsv` as `reviewed.tsv` — same TSV shape, so it greps and hand-edits
/// like the marks file, and a bad line is skipped rather than fatal.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct ReviewState {
    pub last: Option<String>,
    pub watched: Vec<String>,
}

pub fn read_review_state(dir: &Path) -> ReviewState {
    let mut st = ReviewState::default();
    let Ok(text) = std::fs::read_to_string(dir.join("reviewed.tsv")) else {
        return st;
    };
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.splitn(2, '\t');
        let (kind, val) = match (it.next(), it.next()) {
            (Some(k), Some(v)) => (k, v),
            _ => continue,
        };
        match kind {
            "last" => st.last = Some(val.to_string()),
            "watched" if !st.watched.iter().any(|w| w == val) => st.watched.push(val.to_string()),
            _ => {}
        }
    }
    st
}

/// Rewrite `reviewed.tsv` atomically (temp sibling + rename), like `write_marks`.
pub fn write_review_state(dir: &Path, st: &ReviewState) -> std::io::Result<()> {
    let mut body = String::new();
    if let Some(last) = &st.last {
        body.push_str(&format!("last\t{last}\n"));
    }
    for w in &st.watched {
        body.push_str(&format!("watched\t{w}\n"));
    }
    let out = dir.join("reviewed.tsv");
    let tmp = crate::store::temp_sibling(&out);
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &out).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Persist the review's watched set and resume point (#55). Master paths are
/// trusted as the UI got them from `review_playlist`.
pub fn save_review_state(
    cfg: &Config,
    trip: &str,
    last: Option<String>,
    watched: Vec<String>,
) -> Result<(), String> {
    let dir = trip_dir(cfg, trip)?;
    write_review_state(&dir, &ReviewState { last, watched })
        .map_err(|e| format!("couldn't write review state: {e}"))
}
