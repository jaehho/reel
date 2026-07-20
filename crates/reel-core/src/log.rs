//! A durable event log, so a misbehaving app leaves evidence behind.
//!
//! Everything reel does runs on a `spawn_blocking` thread behind a GUI. When
//! something goes wrong the user sees a toast at best and a silently empty
//! dashboard at worst, and the reason is gone the moment the process exits.
//! This appends JSONL to `<state_dir>/log/reel.jsonl` — one object per line,
//! newest at the bottom — so a bug report can carry a trace instead of a
//! reproduction.
//!
//! Three properties are deliberate:
//!
//! - **It never fails a caller.** Every error in here is swallowed. That is the
//!   one place in reel where swallowing is right: a logger that propagates turns
//!   a cosmetic problem into a broken feature, and a logger that panics takes the
//!   app down over a full disk.
//! - **It holds its own lock**, not `store::state_guard()`. Engine code logs from
//!   inside state writes that already hold that guard, and `std::sync::Mutex`
//!   isn't reentrant — sharing it would deadlock on the first such line.
//! - **It is bounded.** Rotates past `MAX_BYTES` keeping one previous
//!   generation, so it can't grow without limit on a machine nobody prunes.
//!
//! Uninitialised, every call is a no-op. Tests and the `dump` example therefore
//! write nothing unless they opt in via [`init_at`].

use crate::config::Config;
use crate::store::now_epoch;
use serde_json::{json, Map, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Rotate once the live file passes this. Two generations, so the log costs at
/// most ~4 MiB — big enough to hold a long session, small enough to paste.
const MAX_BYTES: u64 = 2 * 1024 * 1024;

static LOG_DIR: OnceLock<PathBuf> = OnceLock::new();
static LOG_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }

    /// Parse a level coming off the IPC wire. Unknown strings read as `info`
    /// rather than erroring — a mislabelled line is worth keeping.
    pub fn parse(s: &str) -> Level {
        match s {
            "debug" => Level::Debug,
            "warn" => Level::Warn,
            "error" => Level::Error,
            _ => Level::Info,
        }
    }
}

/// Point the logger at `<state_dir>/log` and start a session. Call once at
/// startup, before anything that might log.
pub fn init(cfg: &Config) {
    init_at(cfg.state_dir.join("log"));
}

/// [`init`] against an explicit directory — for tests, which must not write into
/// the real state dir.
pub fn init_at(dir: PathBuf) {
    let _ = LOG_DIR.set(dir);
}

/// Where the live log is, once initialised. The UI shows this so a user can find
/// the file without knowing the XDG layout.
pub fn path() -> Option<PathBuf> {
    LOG_DIR.get().map(|d| d.join("reel.jsonl"))
}

/// Append one record. `src` says which layer emitted it (`"ui"`, `"engine"`,
/// `"tauri"`), `ctx` carries structured detail — a path, a trip, an exit code.
pub fn event(level: Level, src: &str, msg: &str, ctx: Option<Value>) {
    let Some(dir) = LOG_DIR.get() else { return };
    append(dir, &record(now_epoch(), level, src, msg, ctx));
}

pub fn info(src: &str, msg: &str) {
    event(Level::Info, src, msg, None);
}
pub fn warn(src: &str, msg: &str) {
    event(Level::Warn, src, msg, None);
}
pub fn error(src: &str, msg: &str) {
    event(Level::Error, src, msg, None);
}

/// One JSONL line. Built through `serde_json` rather than `format!` so a message
/// containing a quote or a newline — a panic payload, a JS stack trace — can't
/// break the one-object-per-line invariant the whole format rests on.
fn record(ts: i64, level: Level, src: &str, msg: &str, ctx: Option<Value>) -> String {
    let mut rec = Map::new();
    rec.insert("at".into(), json!(iso8601(ts)));
    rec.insert("ts".into(), json!(ts));
    rec.insert("lvl".into(), json!(level.as_str()));
    rec.insert("src".into(), json!(src));
    rec.insert("msg".into(), json!(msg));
    if let Some(c) = ctx {
        rec.insert("ctx".into(), c);
    }
    Value::Object(rec).to_string()
}

/// Append a line, rotating first if the file has grown past the cap. Errors are
/// intentionally dropped — see the module docs.
fn append(dir: &Path, line: &str) {
    let _g = LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let live = dir.join("reel.jsonl");
    if fs::metadata(&live).map(|m| m.len()).unwrap_or(0) >= MAX_BYTES {
        // Single previous generation; the rename replaces it.
        let _ = fs::rename(&live, dir.join("reel.1.jsonl"));
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&live) {
        let _ = writeln!(f, "{line}");
    }
}

/// `1970-01-01T00:00:00Z`. Hand-rolled so the engine keeps its four-dependency
/// diet; the civil-calendar conversion is Howard Hinnant's, which is exact for
/// every date the proleptic Gregorian calendar covers.
fn iso8601(epoch: i64) -> String {
    let (y, m, d) = civil_from_days(epoch.div_euclid(86_400));
    let s = epoch.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        (s % 3600) / 60,
        s % 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096], so plain division below is safe
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epochs_render_as_utc_timestamps() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_000_000_000), "2001-09-09T01:46:40Z");
        // a leap day, the case an off-by-one in the calendar maths lands on
        assert_eq!(iso8601(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn a_record_is_one_parseable_line_however_ugly_the_message() {
        let line = record(
            0,
            Level::Error,
            "ui",
            "boom \"quoted\"\nand a second line",
            Some(json!({ "trip": "ha-giang" })),
        );
        assert!(!line.contains('\n'), "a raw newline would split the record");
        let v: Value = serde_json::from_str(&line).expect("valid JSON");
        assert_eq!(v["lvl"], "error");
        assert_eq!(v["src"], "ui");
        assert_eq!(v["msg"], "boom \"quoted\"\nand a second line");
        assert_eq!(v["ctx"]["trip"], "ha-giang");
        assert_eq!(v["at"], "1970-01-01T00:00:00Z");
    }

    #[test]
    fn the_log_rotates_instead_of_growing_forever() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let live = dir.join("reel.jsonl");
        let old = dir.join("reel.1.jsonl");

        // Push the live file past the cap, then write one more line.
        append(dir, &"x".repeat(MAX_BYTES as usize));
        assert!(!old.exists(), "nothing to rotate yet");
        append(dir, "after");

        assert!(
            old.exists(),
            "the oversized file should have been set aside"
        );
        let live_txt = fs::read_to_string(&live).unwrap();
        assert_eq!(live_txt.trim(), "after");
        assert!(fs::metadata(&old).unwrap().len() >= MAX_BYTES);
    }

    /// The one test allowed to touch `LOG_DIR` — it's a process-wide `OnceLock`,
    /// so a second test calling `init_at`/`event` would race this one.
    #[test]
    fn init_then_event_lands_on_disk_as_readable_jsonl() {
        let tmp = tempfile::tempdir().unwrap();
        init_at(tmp.path().to_path_buf());
        assert_eq!(path().unwrap(), tmp.path().join("reel.jsonl"));

        event(Level::Warn, "engine", "first", None);
        event(
            Level::Error,
            "ui",
            "second",
            Some(json!({ "clip": "C0001.MP4" })),
        );

        let txt = fs::read_to_string(tmp.path().join("reel.jsonl")).unwrap();
        let lines: Vec<&str> = txt.lines().collect();
        assert_eq!(lines.len(), 2, "one object per line, in call order");

        let a: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(a["msg"], "first");
        assert_eq!(a["lvl"], "warn");
        assert_eq!(a["src"], "engine");
        assert!(a.get("ctx").is_none(), "no ctx key when none was given");

        let b: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(b["msg"], "second");
        assert_eq!(b["ctx"]["clip"], "C0001.MP4");
    }
}
