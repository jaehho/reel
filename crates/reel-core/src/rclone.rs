//! Shared rclone plumbing for the shared pool: run a subcommand with JSON-log
//! stats and stream each stats object to a callback, plus a reachability check
//! for the configured remote. Used by `push` (copy + verify) and `wipe` (the
//! pre-delete pool check).

use serde_json::Value;
use std::ffi::OsStr;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

/// rclone reads `name:path` as a configured remote and a plain path as local.
/// Returns the remote name when there is one; `None` for a local-path pool (what
/// the tests use), which is always reachable.
pub fn remote_name(remote: &str) -> Option<&str> {
    let (head, _) = remote.split_once(':')?;
    if head.is_empty() || head.contains('/') {
        None // a path like `/pool` or `./pool`, not a remote
    } else {
        Some(head)
    }
}

/// Fail early, with a clear message, if the configured remote isn't set up —
/// better than letting rclone reach a typo'd destination.
pub fn remote_ok(remote: &str) -> Result<(), String> {
    let Some(name) = remote_name(remote) else {
        return Ok(()); // local-path pool
    };
    let out = Command::new("rclone")
        .arg("listremotes")
        .output()
        .map_err(|e| format!("rclone isn't available: {e}"))?;
    let want = format!("{name}:");
    if String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l.trim() == want)
    {
        Ok(())
    } else {
        Err(format!(
            "rclone remote '{name}:' isn't configured — set REEL_REMOTE or run `rclone config`"
        ))
    }
}

/// Run `rclone <args…>` with JSON-log stats appended, calling `on_stats` with each
/// stats object as it streams on stderr. Returns whether rclone exited zero. The
/// caller passes the subcommand and its operands; the stats flags are added here.
pub fn stream<I, S>(args: I, mut on_stats: impl FnMut(&Value)) -> Result<bool, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut child = Command::new("rclone")
        .args(args)
        .args([
            "--use-json-log",
            "--stats",
            "500ms",
            "--stats-log-level",
            "NOTICE",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't run rclone: {e}"))?;

    // Each stderr line is a JSON log record; the ones carrying a `stats` object
    // drive progress. Non-stats lines (per-file notices) are ignored.
    if let Some(err) = child.stderr.take() {
        for line in BufReader::new(err).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if v.get("stats").is_some() {
                    on_stats(&v);
                }
            }
        }
    }
    child
        .wait()
        .map(|s| s.success())
        .map_err(|e| format!("rclone wait failed: {e}"))
}
