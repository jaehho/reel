//! Local trips: discovery, `.reel` metadata, and pipeline-state detection.

use crate::config::Config;
use crate::media::{captured_at, fileid_of, masters_in};
use crate::model::{ClipRef, Share, Trip, TripState};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A trip is a dir under the library holding a `.reel` marker (depth 1 or 2).
pub fn trip_dirs(cfg: &Config) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&cfg.lib) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            if p.join(".reel").is_file() {
                v.push(p.clone());
            }
            if let Ok(rd2) = std::fs::read_dir(&p) {
                for e2 in rd2.filter_map(|e| e.ok()) {
                    let p2 = e2.path();
                    if p2.is_dir() && p2.join(".reel").is_file() {
                        v.push(p2);
                    }
                }
            }
        }
    }
    v.sort();
    v.dedup();
    v
}

/// Read a `key=value` from a trip's `.reel`.
pub fn trip_meta(dir: &Path, key: &str) -> Option<String> {
    let txt = std::fs::read_to_string(dir.join(".reel")).ok()?;
    let prefix = format!("{key}=");
    txt.lines()
        .find_map(|l| l.strip_prefix(&prefix).map(|v| v.to_string()))
}

/// Write `key=value` into a trip's `.reel`, replacing that key's line or
/// appending one, and leaving every other line intact. Matches the script's
/// `trip_meta_set` so the CLI and GUI read each other's metadata. Written through
/// a temp sibling so a crash never leaves a half-rewritten marker.
pub fn set_trip_meta(dir: &Path, key: &str, value: &str) -> std::io::Result<()> {
    let path = dir.join(".reel");
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .ok()
        .map(|t| t.lines().map(str::to_string).collect())
        .unwrap_or_default();
    if lines.is_empty() {
        lines.push("reel project".to_string());
    }
    let prefix = format!("{key}=");
    let row = format!("{key}={value}");
    match lines.iter_mut().find(|l| l.starts_with(&prefix)) {
        Some(slot) => *slot = row,
        None => lines.push(row),
    }
    let mut body = lines.join("\n");
    body.push('\n');
    let tmp = dir.join(".reel.partial");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)
}

/// Share state from the `.reel` `share=` line. `Unknown` unless explicitly
/// recorded, so the UI never claims footage is safe without proof.
fn share_of(dir: &Path) -> Share {
    match trip_meta(dir, "share").as_deref() {
        Some("shared" | "verified" | "done" | "yes") => Share::Shared,
        Some("local" | "no" | "pending") => Share::Local,
        _ => Share::Unknown,
    }
}

/// name → share state for every trip, reading only each `.reel` (cheap; used to
/// decide whether a card session is safe to clear).
pub fn trip_shares(cfg: &Config) -> HashMap<String, Share> {
    trip_dirs(cfg)
        .into_iter()
        .filter_map(|d| {
            let name = d.file_name()?.to_str()?.to_string();
            Some((name, share_of(&d)))
        })
        .collect()
}

/// The person a master belongs to: the first path segment under the trip dir
/// (`<trip>/<person>/<camera>/file`). `None` for a stray master at the root.
fn person_of(master: &Path, trip: &Path) -> Option<String> {
    master
        .strip_prefix(trip)
        .ok()?
        .components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .map(|s| s.to_string())
}

fn count_marks(marks: &Path) -> usize {
    std::fs::read_to_string(marks)
        .map(|t| {
            t.lines()
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .count()
        })
        .unwrap_or(0)
}

fn count_clips(clips_dir: &Path) -> usize {
    std::fs::read_dir(clips_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .and_then(|x| x.to_str())
                        .map(|x| x.eq_ignore_ascii_case("mp4"))
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

fn dir_bytes(dir: &Path) -> u64 {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

fn derive_state(masters: usize, marks: usize, clips: usize) -> TripState {
    match (masters, marks, clips) {
        (0, _, c) if c > 0 => TripState::Archived,
        (0, _, _) => TripState::Empty,
        (_, _, c) if c > 0 => TripState::Cut,
        (_, m, _) if m > 0 => TripState::Marked,
        _ => TripState::Imported,
    }
}

fn build_trip(me: &str, dir: &Path) -> Trip {
    let masters_v = masters_in(dir);
    let masters = masters_v.len();
    let marks = count_marks(&dir.join("marks.tsv"));
    let clips = count_clips(&dir.join("clips"));
    let state = derive_state(masters, marks, clips);
    // Provenance: yours (under `<trip>/<you>/`) vs. footage pulled from others.
    let mut mine = 0usize;
    let mut contributors: Vec<String> = Vec::new();
    for p in &masters_v {
        match person_of(p, dir) {
            Some(person) if person == me => mine += 1,
            Some(person) => {
                if !contributors.contains(&person) {
                    contributors.push(person);
                }
            }
            None => {}
        }
    }
    contributors.sort();
    // Prefer the first real-sized clip for the cover; skip tiny/corrupt files
    // (a stray 1 KB "master" has no decodable frame, so its poster would fail).
    let cover = masters_v
        .iter()
        .find(|p| {
            std::fs::metadata(p)
                .map(|m| m.len() > 1_048_576)
                .unwrap_or(false)
        })
        .or_else(|| masters_v.first())
        .and_then(|p| {
            Some(ClipRef {
                path: p.display().to_string(),
                fileid: fileid_of(p).ok()?,
            })
        });
    // Capture window straight from the footage: masters are capture-ordered, so
    // the first and last clips' timestamps bound the trip.
    let start = masters_v.first().map(|p| captured_at(p)).filter(|&t| t > 0);
    let end = masters_v.last().map(|p| captured_at(p)).filter(|&t| t > 0);
    Trip {
        name: dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string(),
        path: dir.display().to_string(),
        masters,
        marks,
        clips,
        bytes: dir_bytes(dir),
        from: trip_meta(dir, "from"),
        to: trip_meta(dir, "to"),
        start,
        end,
        state,
        next: state.next().to_string(),
        cover,
        share: share_of(dir),
        mine,
        pulled: masters - mine,
        contributors,
    }
}

pub fn list_trips(cfg: &Config) -> Vec<Trip> {
    trip_dirs(cfg)
        .iter()
        .map(|d| build_trip(&cfg.user, d))
        .collect()
}
