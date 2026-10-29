//! Cut a trip's marked ranges into `clips/` — lossless, one ffmpeg stream-copy
//! per mark. Pure engine: progress streams through a callback so the GUI reports
//! it without reel-core taking a UI dep.
//!
//! Ports the script's `reel cut`. Marks live in `marks.tsv` keyed on the MASTER
//! (a proxy shares its timeline), so GUI marks and the script's cut read the same
//! file. Each output is `clips/<stem>_c<NN>[_<slug>].mp4`; the seek is `-ss/-to`
//! *before* `-i` (starts on the keyframe at or just before the mark — never late,
//! no re-encode), and only the primary video + optional audio are mapped, since
//! DJI/GoPro masters carry telemetry/timecode/thumbnail tracks mp4 can't hold.
//!
//! A cut leaves `clips/` matching the marks as they are now, and is safe to
//! re-run. The name carries a mark's position and label but not its range, so
//! `clips/.manifest` remembers which range each clip was cut from: a clip whose
//! mark is unchanged is left alone, a trimmed mark is cut again, a relabelled or
//! renumbered one has its clip renamed (no second cut), and a clip no mark asks
//! for is removed. Cut only removes or replaces a file it can prove is still its
//! own output: the manifest also keeps the size and mtime each clip had when Cut
//! finished it, so a clip re-encoded or graded in place, or a file the manifest
//! never listed, is left exactly as it is.

use crate::config::Config;
use crate::media::{masters_in, rel_stem};
use crate::model::{CutProgress, CutResult, Mark};
use crate::review::read_marks;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::UNIX_EPOCH;

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

/// Sidecar in `clips/` listing the clips this trip's Cut made and the mark each
/// was cut from. Removal and replacement are limited to what it lists and still
/// vouches for (`Made::intact`), so a cut carried in from a pruned duplicate, a
/// file placed by hand, the finished cut of a deleted master, or a clip edited
/// in place since is never mistaken for a stale one. A row it lacks is the safe
/// direction: that clip is just not tracked.
pub(crate) const MANIFEST: &str = ".manifest";

/// A clip file as Cut left it: size and mtime (ns). A name alone cannot say the
/// file under it is still Cut's output; this can, since an edit changes one or
/// both and a rename leaves both alone.
#[derive(Clone, Copy, PartialEq)]
struct Stamp {
    size: u64,
    mtime_ns: u128,
}

/// The stamp of the regular file at `path`; `None` if it is absent, not a
/// regular file (a symlink is not Cut's output), or the filesystem keeps no
/// mtime.
fn stamp_of(path: &Path) -> Option<Stamp> {
    let m = std::fs::symlink_metadata(path)
        .ok()
        .filter(|m| m.is_file())?;
    let since = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(Stamp {
        size: m.len(),
        mtime_ns: since.as_nanos(),
    })
}

/// One manifest row: the clip's file name, the master it was cut from (relative
/// to the trip, so a renamed trip keeps its rows), the range as
/// `<start>-<end>` in the same `{:.3}` seconds the ffmpeg call is given, and the
/// stamp the file had when Cut finished it (`None`: a row with no stamp, or one
/// that could not be taken, vouches for nothing).
#[derive(Clone, PartialEq)]
struct Made {
    name: String,
    master: String,
    range: String,
    stamp: Option<Stamp>,
}

impl Made {
    fn same_cut(&self, other: &Made) -> bool {
        self.master == other.master && self.range == other.range
    }

    /// The same clip from the same range, whatever became of the file since.
    fn same_clip(&self, other: &Made) -> bool {
        self.name == other.name && self.same_cut(other)
    }

    /// The file is still what Cut wrote: only then may Cut delete or overwrite it.
    fn intact(&self, clips: &Path) -> bool {
        self.stamp.is_some() && stamp_of(&clips.join(&self.name)) == self.stamp
    }

    /// Fits on a tab-separated line. A name or path with a tab or newline in it
    /// would read back as a different row, so such a clip goes untracked.
    fn fits_a_row(&self) -> bool {
        ![&self.name, &self.master]
            .iter()
            .any(|f| f.contains(['\t', '\n', '\r']))
    }
}

fn read_manifest(clips: &Path) -> Vec<Made> {
    let text = std::fs::read_to_string(clips.join(MANIFEST)).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (name, master, range) = (f.next()?, f.next()?, f.next()?);
            let stamp = f.next().zip(f.next()).and_then(|(size, ns)| {
                Some(Stamp {
                    size: size.parse().ok()?,
                    mtime_ns: ns.parse().ok()?,
                })
            });
            Some(Made {
                name: name.to_string(),
                master: master.to_string(),
                range: range.to_string(),
                stamp,
            })
        })
        .collect()
}

/// How many cut clips each master has produced (#58), read from the manifest —
/// what the dedup rows and organize cells show before asking you to prune or
/// move a copy.
pub(crate) fn cuts_by_master(clips: &Path) -> HashMap<String, usize> {
    let mut out: HashMap<String, usize> = HashMap::new();
    for r in read_manifest(clips) {
        *out.entry(r.master).or_default() += 1;
    }
    out
}

/// Read-modify-write the manifest under the state lock, so the rows a concurrent
/// cut of the same trip just added aren't written back out from a stale read. A
/// failed write only costs a later re-cut of that clip, so it isn't an error.
fn update_manifest(clips: &Path, f: impl FnOnce(&mut Vec<Made>)) {
    let _g = crate::store::state_guard();
    let before = read_manifest(clips);
    let mut rows = before.clone();
    f(&mut rows);
    if rows == before {
        return;
    }
    let path = clips.join(MANIFEST);
    if rows.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let body: String = rows
        .iter()
        .map(|r| match r.stamp {
            Some(s) => format!(
                "{}\t{}\t{}\t{}\t{}\n",
                r.name, r.master, r.range, s.size, s.mtime_ns
            ),
            None => format!("{}\t{}\t{}\n", r.name, r.master, r.range),
        })
        .collect();
    let _ = crate::store::write_atomic(&path, &body);
}

/// Note that `row`'s clip now holds its range, in place of the rows for the file
/// it overwrote and for `renamed`, the clip that was moved into its name. The
/// stamp is taken from the file as it lies now.
fn record(clips: &Path, row: &Made, renamed: Option<&str>) {
    let row = Made {
        stamp: stamp_of(&clips.join(&row.name)),
        ..row.clone()
    };
    update_manifest(clips, |rows| {
        rows.retain(|r| r.name != row.name && Some(r.name.as_str()) != renamed);
        if row.fits_a_row() {
            rows.push(row);
        }
    });
}

/// A mark whose master was found: where its clip goes and what it is cut from.
struct Planned {
    master: PathBuf,
    row: Made,
}

fn plan_mark(m: &Mark, n: usize, dir: &Path) -> Option<Planned> {
    let master = locate(Path::new(&m.master), dir)?;
    let stem = rel_stem(&master, dir);
    let slug = slugify(&m.label);
    let name = if slug.is_empty() {
        format!("{stem}_c{n:02}.mp4")
    } else {
        format!("{stem}_c{n:02}_{slug}.mp4")
    };
    let row = Made {
        name,
        master: master
            .strip_prefix(dir)
            .unwrap_or(&master)
            .to_string_lossy()
            .into_owned(),
        range: format!("{:.3}-{:.3}", m.start, m.end),
        stamp: None,
    };
    Some(Planned { master, row })
}

/// Cut every marked range in `trip` into `<trip>/clips/`, streaming per-segment
/// progress through `on`. Lossless stream-copy, and re-runnable: a clip already
/// cut from the mark's exact range is left as-is, one cut from another range is
/// cut again, and one that only changed name (relabelled, renumbered) is renamed.
/// A clip this trip's Cut made that no mark asks for any more is removed, but
/// only while its master is still on disk to cut it from again, and only after a
/// run where nothing failed — so a failing cut never costs the clip it replaces.
/// Neither removal nor replacement touches a file that is no longer what Cut
/// wrote (edited since, or never listed in the manifest): it is counted as
/// `kept` and left for the user to delete if they want it cut afresh.
/// Errors only on a bad trip or an empty mark list — a single segment whose master
/// is missing or whose ffmpeg fails is counted, not fatal.
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
    crate::cancel::reset_stop(); // a cut is its own user action (#56)
    let clips = dir.join("clips");
    std::fs::create_dir_all(&clips).map_err(|e| format!("couldn't make clips/: {e}"))?;

    let count = marks.len();
    let (mut made, mut skipped, mut kept, mut failed) = (0, 0, 0, 0);
    // ffmpeg's own words for each failure, logged once at the end.
    let mut reasons: Vec<String> = Vec::new();
    // Every mark's clip is settled before the first cut: the clips an earlier
    // run left that no mark names are the ones to reuse or remove, and that
    // needs the full set of wanted names.
    let plan: Vec<Option<Planned>> = marks
        .iter()
        .enumerate()
        .map(|(i, m)| plan_mark(m, i + 1, &dir))
        .collect();
    let wanted: HashSet<&str> = plan.iter().flatten().map(|p| p.row.name.as_str()).collect();
    let recorded = read_manifest(&clips);
    // Clips a mark once asked for and none does by that name now. One whose
    // range some mark still wants is renamed into place below; the rest are
    // removed at the end. Only a clip that is still as Cut left it: a file the
    // user has since replaced is theirs.
    let mut spare: Vec<Made> = recorded
        .iter()
        .filter(|r| !wanted.contains(r.name.as_str()) && r.intact(&clips))
        .cloned()
        .collect();
    for (i, (m, p)) in marks.iter().zip(&plan).enumerate() {
        // Stop lands between files (#56): each clip already written stays, and
        // the result says how far the run got.
        if crate::cancel::stopped() {
            return Ok(CutResult {
                trip: trip.to_string(),
                made,
                skipped,
                kept,
                removed: 0,
                failed,
                count,
                stopped: true,
                dir: clips.display().to_string(),
            });
        }
        let n = i + 1;
        let Some(Planned { master, row }) = p else {
            failed += 1;
            on(CutProgress {
                file: String::new(),
                index: n,
                count,
            });
            continue;
        };
        let name = row.name.clone();
        let out = clips.join(&name);
        let name_for_err = name.clone();
        on(CutProgress {
            file: name,
            index: n,
            count,
        });
        // Cut from this very range already.
        if out.is_file() && recorded.iter().any(|r| r.same_clip(row)) {
            skipped += 1;
            continue;
        }
        // Whatever sits at this name now must be Cut's own, untouched: a file the
        // manifest doesn't vouch for (cut before it existed, carried in by a
        // dedup prune, placed by hand) or one edited since can't be told from a
        // deliverable, and is not Cut's to overwrite.
        let free = out.symlink_metadata().is_err()
            || recorded
                .iter()
                .any(|r| r.name == row.name && r.intact(&clips));
        if !free {
            kept += 1;
            // A clip with this mark's range under another name is the one to rename
            // in once the name is free, not one to remove meanwhile.
            spare.retain(|r| !r.same_cut(row));
            continue;
        }
        // The same range under another name — the mark was relabelled, or an
        // earlier one was deleted and it renumbered. The clip is already right.
        if let Some(at) = spare.iter().position(|r| r.same_cut(row)) {
            if std::fs::rename(clips.join(&spare[at].name), &out).is_ok() {
                record(&clips, row, Some(&spare.remove(at).name));
                skipped += 1;
                continue;
            }
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
            .arg(master)
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
                    reasons.push(format!("{}: {line}", name_for_err));
                }
                false
            }
            Err(_) => false,
        };
        if ok && tmp.is_file() && std::fs::rename(&tmp, &out).is_ok() {
            record(&clips, row, None);
            made += 1;
        } else {
            let _ = std::fs::remove_file(&tmp);
            failed += 1;
        }
    }
    // Only after a clean run, and only for a master still on disk: the clip of a
    // deleted or archived master can't be made again, and is a deliverable now.
    // Checked again here, since the run above may have been long. The manifest
    // also forgets rows whose file is gone.
    let mut gone: Vec<&str> = Vec::new();
    if failed == 0 {
        for r in &spare {
            if dir.join(&r.master).is_file()
                && r.intact(&clips)
                && std::fs::remove_file(clips.join(&r.name)).is_ok()
            {
                gone.push(&r.name);
            }
        }
        update_manifest(&clips, |rows| {
            rows.retain(|r| clips.join(&r.name).is_file())
        });
    }
    if !gone.is_empty() {
        crate::log::event(
            crate::log::Level::Info,
            "engine",
            "cut: removed clips no mark asks for",
            Some(serde_json::json!({ "trip": trip, "clips": gone })),
        );
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
        kept,
        removed: gone.len(),
        failed,
        count,
        stopped: false,
        dir: clips.display().to_string(),
    })
}
