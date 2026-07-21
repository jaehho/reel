//! Hand a trip's cut off to an external editor — the pipeline's final step, and
//! the last piece that lived only in the `reel` script. Ports its `cmd_edit`:
//! open the trip's `clips/*.mp4` (or, for a trip that hasn't been cut, its
//! masters) in Kdenlive. reel is a cutting pipeline, not an NLE — the actual
//! editing hands off to Kdenlive (see PRODUCT.md), so this is where reel's job
//! ends. The editor is launched fully detached, so closing reel never takes it
//! down and reel never waits on the editing session.

use crate::config::Config;
use crate::media::masters_in;
use crate::model::EditResult;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The editor reel hands a finished cut to. Matches the script's hardcoded
/// `kdenlive` — reel doesn't edit, it hands off.
const EDITOR: &str = "kdenlive";

fn valid_trip(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != ".."
}

/// Is `bin` present on `PATH`? A cheap lookup so a missing editor gets a friendly
/// message up front instead of a raw spawn error.
fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file()))
}

/// A trip's cut clips (`clips/*.mp4`), name-sorted so they open in cut order.
fn clips_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir.join("clips"))
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|x| x.to_str())
                .map(|x| x.eq_ignore_ascii_case("mp4"))
                .unwrap_or(false)
        })
        .collect();
    v.sort();
    v
}

/// What `edit` would open for a trip: the finished cut if there is one, else the
/// raw masters (so a not-yet-cut trip is still openable, matching the script).
/// Pure resolution, no launch — the tested core of `open_in_editor`.
pub fn media_for(dir: &Path) -> Vec<PathBuf> {
    let clips = clips_in(dir);
    if clips.is_empty() {
        masters_in(dir)
    } else {
        clips
    }
}

/// Open `trip` in the editor.
///
/// Prefers a **timeline**: when the trip has marks whose raw is still on disk,
/// `timeline::build_timeline` writes a `.kdenlive` laying every mark end to end
/// against its master, and that project is what opens. It's the better hand-off in
/// every way that matters — the segments are already in order, each one is named,
/// and because they point at the master rather than a cut file, an edge is still
/// draggable once you're in there.
///
/// Falls back to handing over loose files (the cut, else the masters) when there's
/// no timeline to build: a trip with no marks, or an archived one whose raw is gone
/// and whose `clips/` are all that's left. That's the old behaviour, kept because
/// those are exactly the cases where it's still the only thing that works.
///
/// `Err` on a bad/empty trip or a missing editor.
pub fn open_in_editor(cfg: &Config, trip: &str) -> Result<EditResult, String> {
    if !valid_trip(trip) {
        return Err(format!("invalid trip name: {trip:?}"));
    }
    let dir = cfg.lib.join(trip);
    if !dir.join(".reel").is_file() {
        return Err(format!("no such trip: {trip}"));
    }
    if !on_path(EDITOR) {
        return Err(format!("{EDITOR} isn't installed — run 'make sync'"));
    }
    // A failed build is never fatal here: the loose-file hand-off below still works,
    // and refusing to open the editor at all because the project couldn't be written
    // would be a worse outcome than the one the user had before this existed.
    if let Ok(t) = crate::timeline::build_timeline(cfg, trip) {
        let project = PathBuf::from(&t.path);
        launch(&[project])?;
        return Ok(EditResult {
            files: 1,
            timeline: Some(t),
        });
    }
    let media = media_for(&dir);
    if media.is_empty() {
        return Err(format!(
            "nothing to edit in '{trip}' — import or cut it first"
        ));
    }
    launch(&media)?;
    Ok(EditResult {
        files: media.len(),
        timeline: None,
    })
}

/// Hand `media` to the editor, fully detached.
fn launch(media: &[PathBuf]) -> Result<(), String> {
    // `setsid --fork` always forks the editor into a fresh session and exits, so
    // the child we spawn is the short-lived setsid (reaped just below) while the
    // editor reparents to init — fully detached from reel. stdio is nulled so it
    // never writes into reel's streams.
    let mut child = Command::new("setsid")
        .arg("--fork")
        .arg(EDITOR)
        .args(media)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't launch {EDITOR}: {e}"))?;
    // setsid --fork returns at once; reap it so it doesn't linger as a zombie.
    let _ = child.wait();
    Ok(())
}
