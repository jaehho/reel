//! Hand a trip off to Kdenlive as a timeline. reel reviews and marks, it isn't an
//! NLE — the actual editing happens in Kdenlive (see PRODUCT.md), so this is where
//! reel's job ends. The editor is launched fully detached, so closing reel never
//! takes it down and reel never waits on the editing session.

use crate::config::Config;
use crate::model::TimelineResult;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The editor reel hands a trip to. Matches the script's hardcoded `kdenlive` —
/// reel doesn't edit, it hands off.
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

/// Open `trip` in the editor as a timeline.
///
/// `timeline::build_timeline` writes a `.kdenlive` laying every mark end to end
/// against its master, and that project is what opens. Because the segments point
/// at the master rather than a cut file, an edge is still draggable once you're in
/// there.
///
/// There is no second way in. Handing over loose files — the cut, else the masters
/// — used to be the fallback whenever the build failed, but a build only fails for
/// reasons you want to hear about: no marks yet, or the raw is archived. Opening
/// *something* anyway turned both into a surprise, since what came up was neither
/// the timeline nor an error. `build_timeline`'s own message says which it was, and
/// for the archived case it says to restore first, so that message is the answer.
///
/// `Err` on a bad trip, a missing editor, or anything that stopped the build.
pub fn open_in_editor(cfg: &Config, trip: &str) -> Result<TimelineResult, String> {
    if !valid_trip(trip) {
        return Err(format!("invalid trip name: {trip:?}"));
    }
    // Checked here rather than left to `build_timeline`, so a bogus name is rejected
    // before a missing editor is — the cheaper, more specific complaint wins.
    if !cfg.lib.join(trip).join(".reel").is_file() {
        return Err(format!("no such trip: {trip}"));
    }
    if !on_path(EDITOR) {
        return Err(format!("{EDITOR} isn't installed — run 'make sync'"));
    }
    let t = crate::timeline::build_timeline(cfg, trip)?;
    launch(&[PathBuf::from(&t.path)])?;
    Ok(t)
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
