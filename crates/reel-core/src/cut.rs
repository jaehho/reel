//! Cut a trip's marked ranges into `clips/` — lossless, one ffmpeg stream-copy
//! per mark. Pure engine: progress streams through a callback so the GUI reports
//! it without reel-core taking a UI dep.
//!
//! Ports the script's `reel cut`. Marks live in `marks.tsv` keyed on the MASTER
//! (a proxy shares its timeline), so GUI marks and the script's cut read the same
//! file. Each output is `clips/<stem>_c<NN>[_<slug>].mp4`; the seek is `-ss/-to`
//! *before* `-i` (starts on the keyframe at or just before the mark — never late,
//! no re-encode), and only the primary video + optional audio are mapped, since
//! DJI/GoPro masters carry telemetry/timecode/thumbnail tracks mp4 can't hold. An
//! output that already exists is left untouched, so a cut is safe to re-run.

use crate::config::Config;
use crate::media::{masters_in, rel_stem};
use crate::model::{CutProgress, CutResult};
use crate::review::read_marks;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn valid_trip(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != ".."
}

/// Slugify a mark label for a filename: runs of non-alphanumerics collapse to a
/// single `-`, leading/trailing dashes trimmed. Matches the script's
/// `tr -cs 'a-zA-Z0-9' '-'` then trim, so both tools name clips identically.
fn slugify(label: &str) -> String {
    let mut s = String::new();
    let mut dash = false;
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            s.push(ch);
            dash = false;
        } else if !dash {
            s.push('-');
            dash = true;
        }
    }
    s.trim_matches('-').to_string()
}

/// Resolve a mark's master to a file that exists. The stored path is normally
/// current, but marks written before a trip was renamed carry the old absolute
/// path; fall back to a master under `dir` with the same basename (unique per
/// camera), so a moved trip still cuts. Ambiguous → give up rather than guess.
///
/// Shared with `timeline`, so a moved trip builds a Kdenlive project exactly as
/// well as it cuts — one recovery rule, not two that can drift apart.
pub(crate) fn locate(master: &Path, dir: &Path) -> Option<PathBuf> {
    if master.is_file() {
        return Some(master.to_path_buf());
    }
    let base = master.file_name()?;
    let mut hit = None;
    for m in masters_in(dir) {
        if m.file_name() == Some(base) {
            if hit.is_some() {
                return None;
            }
            hit = Some(m);
        }
    }
    hit
}

/// Cut every marked range in `trip` into `<trip>/clips/`, streaming per-segment
/// progress through `on`. Lossless stream-copy; an output that already exists is
/// left as-is (additive, re-runnable). Errors only on a bad trip or an empty mark
/// list — a single segment whose master is missing or whose ffmpeg fails is
/// counted, not fatal.
pub fn cut_trip(
    cfg: &Config,
    trip: &str,
    mut on: impl FnMut(CutProgress),
) -> Result<CutResult, String> {
    if !valid_trip(trip) {
        return Err(format!("invalid trip name: {trip:?}"));
    }
    let dir = cfg.lib.join(trip);
    if !dir.join(".reel").is_file() {
        return Err(format!("no such trip: {trip}"));
    }
    let marks = read_marks(&dir);
    if marks.is_empty() {
        return Err(format!("no marks to cut in '{trip}' — review it first"));
    }
    let clips = dir.join("clips");
    std::fs::create_dir_all(&clips).map_err(|e| format!("couldn't make clips/: {e}"))?;

    let count = marks.len();
    let (mut made, mut skipped, mut failed) = (0, 0, 0);
    // ffmpeg's own words for each failure, logged once at the end.
    let mut reasons: Vec<String> = Vec::new();
    for (i, m) in marks.iter().enumerate() {
        let n = i + 1;
        let Some(master) = locate(Path::new(&m.master), &dir) else {
            failed += 1;
            on(CutProgress {
                file: String::new(),
                index: n,
                count,
            });
            continue;
        };
        let stem = rel_stem(&master, &dir);
        let slug = slugify(&m.label);
        let name = if slug.is_empty() {
            format!("{stem}_c{n:02}.mp4")
        } else {
            format!("{stem}_c{n:02}_{slug}.mp4")
        };
        let out = clips.join(&name);
        let name_for_err = name.clone();
        on(CutProgress {
            file: name,
            index: n,
            count,
        });
        if out.exists() {
            skipped += 1;
            continue;
        }
        // -ss/-to before -i: fast keyframe seek, no re-encode. Map only the
        // primary video + optional audio, dropping telemetry/timecode/thumbnail
        // tracks an mp4 can't hold. Temp sibling → an interrupted cut never
        // leaves a half clip that looks finished.
        // Unique per process and call. With a fixed name two concurrent cuts of one
        // trip (reachable whenever a re-render restores the Cut button mid-run)
        // shared a scratch file, each deleting the other's in-progress output, and
        // the first rename published a garbled clip that then looked finished to
        // the `out.exists()` skip above.
        let tmp = crate::store::temp_sibling_keep_ext(&out);
        let ok = Command::new("ffmpeg")
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-ss",
            ])
            .arg(format!("{:.3}", m.start))
            .arg("-to")
            .arg(format!("{:.3}", m.end))
            .arg("-i")
            .arg(&master)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "0:a?",
                "-c",
                "copy",
                "-movflags",
                "+faststart",
            ])
            .arg(&tmp)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output();
        // Keep ffmpeg's last line. `proxy` and `still` both surface it; cut threw it
        // away and reported a bare `failed` count, so a whole class of legible
        // failures was undiagnosable — a zero-width mark, for one, which the
        // timeline export deliberately accepts but `-ss`/`-to` rejects outright
        // ("-to value smaller than -ss").
        let ok = match &ok {
            Ok(o) if o.status.success() => true,
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr);
                if let Some(line) = err.lines().last().map(str::trim).filter(|l| !l.is_empty()) {
                    reasons.push(format!("{}: {line}", &name_for_err));
                }
                false
            }
            Err(_) => false,
        };
        if ok && tmp.is_file() && std::fs::rename(&tmp, &out).is_ok() {
            made += 1;
        } else {
            let _ = std::fs::remove_file(&tmp);
            failed += 1;
        }
    }
    if failed > 0 && !reasons.is_empty() {
        crate::log::event(
            crate::log::Level::Warn,
            "engine",
            "cut: some segments failed",
            Some(serde_json::json!({ "trip": trip, "failed": failed, "why": reasons })),
        );
    }
    Ok(CutResult {
        trip: trip.to_string(),
        made,
        skipped,
        failed,
        dir: clips.display().to_string(),
    })
}
