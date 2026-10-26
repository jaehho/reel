//! Reveal a path in the file manager. "Open trip folder" and "Open cut clips"
//! are how you get from reel to the footage itself, and after a Cut the toast's
//! "Open folder" lands here too (#57). Before this the only way out of the app
//! was to remember where the library lives.

use crate::edit::on_path;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What the file manager should open so `path` is visible: a directory opens in
/// place; a file (or a name that no longer exists) opens its parent folder. A
/// bare filename has no parent worth opening, so that's nothing to show.
pub fn reveal_target(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        return Some(path.to_path_buf());
    }
    let parent = path.parent()?;
    if parent.as_os_str().is_empty() {
        return None;
    }
    Some(parent.to_path_buf())
}

/// Hand `path` to the desktop's file manager, detached (the same `setsid --fork`
/// shape as the editor hand-off) so closing reel never closes the window.
///
/// `Err` when there's nothing to show or `xdg-open` isn't installed.
pub fn reveal_path(path: &Path) -> Result<(), String> {
    let target =
        reveal_target(path).ok_or_else(|| format!("nothing to open for {}", path.display()))?;
    if !on_path("xdg-open") {
        return Err("xdg-open isn't installed — install it with pacman".into());
    }
    let mut child = Command::new("setsid")
        .arg("--fork")
        .arg("xdg-open")
        .arg(&target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't launch xdg-open: {e}"))?;
    // setsid --fork returns at once; reap it so it doesn't linger as a zombie.
    let _ = child.wait();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_opens_itself_and_a_file_opens_its_folder() {
        let root = std::env::temp_dir().join(format!("reel-reveal-test-{}", std::process::id()));
        let clips = root.join("clips");
        std::fs::create_dir_all(&clips).unwrap();
        assert_eq!(reveal_target(&clips), Some(clips));
        let file = root.join("clip.mp4");
        std::fs::write(&file, b"").unwrap();
        assert_eq!(reveal_target(&file), Some(root.clone()));
        // one that isn't there still shows where it belongs — the trip folder
        // when a cut's clips/ has been cleaned out
        assert_eq!(reveal_target(&root.join("gone")), Some(root.clone()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_bare_filename_has_nothing_to_reveal() {
        assert_eq!(reveal_target(Path::new("clip.mp4")), None);
    }
}
