//! Import a card's capture session into a trip: copy its masters into
//! `<lib>/<trip>/<user>/<camera>/`, dedup against the content-id ledger, and
//! preserve each clip's capture time. Pure engine — progress is reported through
//! a callback, so the GUI layer can stream it without reel-core taking a UI dep.
//!
//! Scope note: this copies masters only. The cameras' native `.LRF`/`.LRV`
//! proxies stay a separate step; the shared-pool push that marks a trip "shared"
//! lives in `push.rs` and runs as its own action after import.

use crate::cards::card_roots;
use crate::config::Config;
use crate::ledger::{Ledger, LedgerRow};
use crate::media::{fileid_of, kind_of, masters_under};
use crate::model::{ImportProgress, ImportResult};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Read/write granularity, and how often progress ticks.
const COPY_CHUNK: usize = 4 * 1024 * 1024;

/// A trip name comes from the UI; keep it a single path segment so it can't climb
/// out of the library root.
fn valid_trip(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != ".."
}

/// Make `dir` a trip: create it and drop a `.reel` marker if absent (idempotent).
fn ensure_project(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let marker = dir.join(".reel");
    if !marker.exists() {
        fs::write(marker, "reel project\n")?;
    }
    Ok(())
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn ledger_row(
    id: &str,
    trip: &str,
    person: &str,
    cam: &str,
    base: &str,
    bytes: u64,
    at: i64,
) -> LedgerRow {
    LedgerRow {
        id: id.to_string(),
        trip: trip.to_string(),
        person: person.to_string(),
        camera: cam.to_string(),
        base: base.to_string(),
        bytes: bytes.to_string(),
        captured: at.to_string(),
        imported_at: now_epoch().to_string(),
    }
}

/// One clip to copy: source on the card, its destination in the trip, and the
/// bookkeeping needed to ledger it once the bytes land.
struct Job {
    src: PathBuf,
    dest: PathBuf,
    fileid: String,
    camera: &'static str,
    base: String,
    bytes: u64,
    at: i64,
}

/// Copy `src` → `dest` through a temp sibling (so an interrupted copy never looks
/// complete), preserving mtime, ticking `on` every chunk.
#[allow(clippy::too_many_arguments)]
fn copy_clip(
    job: &Job,
    file_index: usize,
    file_count: usize,
    base_done: u64,
    grand_total: u64,
    on: &mut impl FnMut(ImportProgress),
) -> io::Result<()> {
    if let Some(parent) = job.dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = job.dest.with_extension("partial");
    {
        let mut r = File::open(&job.src)?;
        let mut w = File::create(&tmp)?;
        let mut buf = vec![0u8; COPY_CHUNK];
        let mut done = 0u64;
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            w.write_all(&buf[..n])?;
            done += n as u64;
            on(ImportProgress {
                file: job.base.clone(),
                file_index,
                file_count,
                bytes_done: done,
                bytes_total: job.bytes,
                copied_bytes: base_done + done,
                total_bytes: grand_total,
            });
        }
        w.flush()?;
    }
    // Capture time rides on mtime; keep it so the imported clip still clusters and
    // sorts like it did on the card.
    if let Ok(mt) = fs::metadata(&job.src).and_then(|m| m.modified()) {
        let f = File::options().write(true).open(&tmp)?;
        let _ = f.set_modified(mt);
    }
    fs::rename(&tmp, &job.dest)
}

/// Copy every master captured in `[w0, w1]` on the inserted card into `trip`,
/// skipping clips already owned (here or elsewhere). Creates the trip if new.
/// Returns a summary; `on` streams per-chunk progress.
pub fn import_window(
    cfg: &Config,
    trip: &str,
    w0: i64,
    w1: i64,
    mut on: impl FnMut(ImportProgress),
) -> Result<ImportResult, String> {
    if !valid_trip(trip) {
        return Err(format!("invalid trip name: {trip:?}"));
    }
    let roots = card_roots(cfg);
    if roots.is_empty() {
        return Err("no card inserted".into());
    }
    let dir = cfg.lib.join(trip);
    ensure_project(&dir).map_err(|e| format!("couldn't create trip '{trip}': {e}"))?;

    let mut ledger = Ledger::load(&cfg.ledger_path());

    // Plan: which in-window masters are new here, already here, or another trip's.
    let mut jobs: Vec<Job> = Vec::new();
    let mut skipped_here = 0usize;
    let mut skipped_other = 0usize;

    for (at, p) in masters_under(&roots) {
        if at < w0 || at > w1 {
            continue;
        }
        let fileid = match fileid_of(&p) {
            Ok(id) => id,
            Err(_) => continue, // unreadable on the card; leave it
        };
        match ledger.trip_of(&fileid) {
            Some(o) if o == trip => {
                skipped_here += 1;
                continue;
            }
            Some(_) => {
                skipped_other += 1;
                continue;
            }
            None => {}
        }
        let kind = kind_of(&p);
        let base = p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let dest = dir.join(&cfg.user).join(kind.dir()).join(&base);
        let bytes = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        // A byte-identical copy already sitting in the trip is a pre-ledger import:
        // record it and move on, so the ledger self-heals without re-copying.
        if dest.is_file() && fs::metadata(&dest).map(|m| m.len()).unwrap_or(0) == bytes {
            ledger.upsert(ledger_row(
                &fileid,
                trip,
                &cfg.user,
                kind.dir(),
                &base,
                bytes,
                at,
            ));
            skipped_here += 1;
            continue;
        }
        jobs.push(Job {
            src: p,
            dest,
            fileid,
            camera: kind.dir(),
            base,
            bytes,
            at,
        });
    }

    let grand_total: u64 = jobs.iter().map(|j| j.bytes).sum();
    let file_count = jobs.len();
    let mut copied = 0usize;
    let mut copied_bytes = 0u64;

    for (i, job) in jobs.iter().enumerate() {
        copy_clip(job, i + 1, file_count, copied_bytes, grand_total, &mut on)
            .map_err(|e| format!("copy failed for {}: {e}", job.base))?;
        ledger.upsert(ledger_row(
            &job.fileid,
            trip,
            &cfg.user,
            job.camera,
            &job.base,
            job.bytes,
            job.at,
        ));
        copied += 1;
        copied_bytes += job.bytes;
    }

    ledger
        .save(&cfg.ledger_path())
        .map_err(|e| format!("couldn't write ledger: {e}"))?;

    Ok(ImportResult {
        trip: trip.to_string(),
        copied,
        bytes: copied_bytes,
        skipped_here,
        skipped_other,
    })
}
