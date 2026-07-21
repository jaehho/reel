//! Grab a frame from a master and keep it as an ordinary photo capture.
//!
//! A still is not a derived artifact like a poster or a proxy — it's a picture the
//! user *chose*, so it lands beside its source in the same
//! `<trip>/<person>/<camera>/` folder and is thereafter indistinguishable from a
//! photo the camera itself wrote: discovered by `masters_in`, shown in the
//! filmstrip, pushed to the cloud, deduped by content id, and freed by `archive`
//! along with everything else. Nothing downstream needs to know it was extracted.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, UNIX_EPOCH};

use crate::config::Config;
use crate::ledger::{Ledger, LedgerRow};
use crate::media::{captured_at, fileid_of, under};
use crate::model::StillResult;
use crate::store::now_epoch;

const FF_BASE: &[&str] = &["-nostdin", "-hide_banner", "-loglevel", "error", "-y"];

fn nonempty_file(p: &Path) -> bool {
    fs::metadata(p)
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}

/// Where a still for `master` at `t` seconds lands. The offset is milliseconds:
/// unique to the frame, and *deterministic*, so grabbing the same moment twice
/// resolves to the same file rather than to a second near-identical picture.
pub fn still_path(master: &Path, t: f64) -> PathBuf {
    let ms = (t.max(0.0) * 1000.0).round() as i64;
    let stem = master
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("frame");
    master.with_file_name(format!("{stem}_t{ms}.jpg"))
}

/// `<trip>/<person>/<camera…>/<base>` → the ledger's trip / person / camera fields.
/// Levels below the person are joined rather than truncated, so a friend's deeper
/// tree rides along intact; a stray at the trip root has neither owner nor camera
/// (the same reading `trips::person_of` settled on).
fn ledger_place(rel: &Path) -> Option<(String, String, String)> {
    let c: Vec<String> = rel
        .components()
        .map(|x| x.as_os_str().to_string_lossy().into_owned())
        .collect();
    match c.len() {
        0 | 1 => None,                                           // no trip, or bare file
        2 => Some((c[0].clone(), String::new(), String::new())), // trip/base — a stray
        _ => Some((c[0].clone(), c[1].clone(), c[2..c.len() - 1].join("/"))),
    }
}

/// Extract the frame at `t` seconds from `master` as a full-resolution JPEG beside
/// it, and record it on the ledger so it's tracked like any other capture.
///
/// Idempotent: a repeat grab of the same frame returns the existing file untouched.
pub fn grab_still(cfg: &Config, master: &Path, t: f64) -> Result<StillResult, String> {
    if !under(master, &cfg.lib) {
        return Err("that clip isn't in the library".into());
    }
    if !master.is_file() {
        return Err(format!("no such clip: {}", master.display()));
    }
    if !t.is_finite() || t < 0.0 {
        return Err(format!("bad timestamp: {t}"));
    }
    let out = still_path(master, t);
    let name = out
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    if !out.is_file() {
        write_frame(master, t, &out)?;
    }

    // Sort the still into the filmstrip where the moment actually happened:
    // `masters_in` orders by mtime, so the source clip's capture time plus the
    // offset puts the picture immediately after the footage it came from.
    let when = captured_at(master).saturating_add(t as i64);
    if when > 0 {
        if let Ok(f) = fs::File::options().write(true).open(&out) {
            let _ = f.set_modified(UNIX_EPOCH + Duration::from_secs(when as u64));
        }
    }

    let bytes = fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    let id = fileid_of(&out).map_err(|e| format!("couldn't identify the still: {e}"))?;

    // Ledger last, and only the row edit inside the lock — the ffmpeg run above is
    // the slow part, and holding the ledger across it drops the other side's rows.
    if let Some(rel) = out.strip_prefix(&cfg.lib).ok().and_then(ledger_place) {
        let (trip, person, camera) = rel;
        let row = LedgerRow {
            id: id.clone(),
            trip,
            person,
            camera,
            base: name.clone(),
            bytes: bytes.to_string(),
            captured: when.to_string(),
            imported_at: now_epoch().to_string(),
        };
        let _ = Ledger::update(&cfg.ledger_path(), |l| l.upsert(row));
    }

    Ok(StillResult {
        path: out.to_string_lossy().into_owned(),
        name,
        fileid: id,
        bytes,
    })
}

/// One decoded frame → JPEG, written to a temp sibling and renamed so a failed or
/// interrupted run can't leave a half-written picture in the library.
fn write_frame(master: &Path, t: f64, out: &Path) -> Result<(), String> {
    let tmp = out.with_extension("partial.jpg");
    let o = Command::new("ffmpeg")
        .args(FF_BASE)
        // Seeking *before* `-i` is both fast and exact here. The keyframe caveat in
        // `cut` is a property of `-c copy`, which has no way to re-encode its way to
        // an in-between frame; this decodes, so ffmpeg's accurate seek lands on the
        // frame that was actually on screen.
        .arg("-ss")
        .arg(format!("{t:.3}"))
        .arg("-i")
        .arg(master)
        // No scale filter: a poster is deliberately small, a still is the picture.
        .args(["-map", "0:v:0", "-frames:v", "1", "-q:v", "2"])
        .arg(&tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("couldn't run ffmpeg: {e}"))?;

    if !o.status.success() || !nonempty_file(&tmp) {
        let _ = fs::remove_file(&tmp);
        // Unlike a poster — which silently degrades to a placeholder — a still was
        // asked for by hand, so say why it didn't happen.
        let why = String::from_utf8_lossy(&o.stderr)
            .lines()
            .last()
            .unwrap_or("ffmpeg couldn't decode that frame")
            .to_string();
        return Err(format!("couldn't grab that frame: {why}"));
    }
    fs::rename(&tmp, out).map_err(|e| format!("couldn't save the still: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn still_name_carries_the_source_and_the_offset() {
        let p = Path::new("/lib/DOHA/jaeho/DJI/DJI_0030_D.MP4");
        assert_eq!(
            still_path(p, 12.48),
            Path::new("/lib/DOHA/jaeho/DJI/DJI_0030_D_t12480.jpg")
        );
        // deterministic to the millisecond, so a repeat grab is the same file
        assert_eq!(still_path(p, 12.48), still_path(p, 12.4801));
        // and a negative/zero playhead still yields a sane name
        assert_eq!(
            still_path(p, -1.0),
            Path::new("/lib/DOHA/jaeho/DJI/DJI_0030_D_t0.jpg")
        );
    }

    #[test]
    fn ledger_place_reads_owner_and_camera_at_any_depth() {
        let f = |s: &str| ledger_place(Path::new(s));
        assert_eq!(
            f("DOHA/jaeho/DJI/clip.jpg"),
            Some(("DOHA".into(), "jaeho".into(), "DJI".into()))
        );
        // a friend's flat tree has no camera level
        assert_eq!(
            f("DOHA/alice/clip.jpg"),
            Some(("DOHA".into(), "alice".into(), String::new()))
        );
        // deeper trees are joined, never truncated onto person/camera
        assert_eq!(
            f("DOHA/alice/DJI/2026/clip.jpg"),
            Some(("DOHA".into(), "alice".into(), "DJI/2026".into()))
        );
        // a stray at the trip root has no owner — the filename is not a person
        assert_eq!(
            f("DOHA/clip.jpg"),
            Some(("DOHA".into(), String::new(), String::new()))
        );
        assert_eq!(f("clip.jpg"), None);
    }
}
