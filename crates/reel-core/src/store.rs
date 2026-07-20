//! Durable state behind cloud sync: the comparison unit (`FileSet`), the owed-op
//! queue (`Pending`), and the cloud-listing cache. All three are tiny TSVs written
//! atomically (temp sibling + rename, like `ledger.rs`), so a crash mid-write can
//! never leave a half-file.
//!
//! A clip's identity across local disk, the baseline, and the cloud is its
//! cloud-relative path `person/camera/base` plus byte size — the same key the
//! push/pull/reclaim paths already use. No content hashing here; that stays in
//! the import/delete paths where a stable id actually matters.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// Process-wide guard for the shared state files (ledger, tombstones, the owed-op
/// queue, per-trip baselines).
///
/// Every Tauri command runs on its own blocking thread with no coordination, so a
/// background sweep and a user action can interleave a read-modify-write on the
/// same TSV. The atomic write below stops a *torn* file but not a **lost update**:
/// both sides load, both mutate, the last save wins and the other's change is gone.
/// A dropped baseline row self-heals on the next refresh, but a dropped tombstone
/// lets footage you deleted re-import, and a dropped queue row strands an owed cloud
/// op forever — neither comes back.
///
/// Held only across load → mutate → save (microseconds). Never hold it across a
/// copy, an ffmpeg run, or a network call: the `update_*` helpers exist so callers
/// do the slow work first and merge their result into fresh state at the end.
static STATE_LOCK: Mutex<()> = Mutex::new(());

/// Take the state lock, ignoring poisoning — a panicking writer leaves the files
/// themselves consistent (every write is atomic), so refusing later writes would
/// turn one failed op into a dead app.
pub fn state_guard() -> MutexGuard<'static, ()> {
    STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A temp sibling name unique to this process and call. A fixed `.tmp` name is a
/// second race: two writers both create it, and one's `rename` can publish the
/// other's half-written body (or fail outright once the first rename moved it).
pub(crate) fn temp_sibling(path: &Path) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.{}.tmp", std::process::id(), n));
    path.with_file_name(name)
}

/// Wall-clock epoch seconds, for stamping queued ops (informational ordering).
pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `person/camera/base` → size. Used for the baseline `B`, a fetched cloud
/// listing `R`, and the local set `L`; sync status is set arithmetic over three
/// of these.
#[derive(Default, Clone, Debug)]
pub struct FileSet {
    pub files: BTreeMap<String, u64>,
}

impl FileSet {
    /// Parse `rel\tsize` rows, tolerating (and skipping) `#`-prefixed header
    /// lines like the cloud cache's timestamp.
    pub fn load(path: &Path) -> Self {
        let mut files = BTreeMap::new();
        if let Ok(txt) = fs::read_to_string(path) {
            for line in txt.lines() {
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let mut f = line.split('\t');
                if let Some(rel) = f.next().filter(|r| !r.is_empty()) {
                    let size = f.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                    files.insert(rel.to_string(), size);
                }
            }
        }
        FileSet { files }
    }

    /// Write `rel\tsize` rows, sorted (BTreeMap iterates sorted), atomically.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut s = String::new();
        for (rel, size) in &self.files {
            s.push_str(&format!("{rel}\t{size}\n"));
        }
        write_atomic(path, &s)
    }

    pub fn insert(&mut self, rel: &str, size: u64) {
        self.files.insert(rel.to_string(), size);
    }
    pub fn remove(&mut self, rel: &str) {
        self.files.remove(rel);
    }
    pub fn contains(&self, rel: &str) -> bool {
        self.files.contains_key(rel)
    }
    pub fn get(&self, rel: &str) -> Option<u64> {
        self.files.get(rel).copied()
    }
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Load → mutate → save under the state lock, so a concurrent writer can't
    /// clobber the change. Do the slow part (upload, cloud listing, delete) *first*
    /// and merge only the result in here; the closure must not touch another state
    /// file, since the lock isn't reentrant.
    pub fn update(path: &Path, f: impl FnOnce(&mut FileSet)) -> io::Result<()> {
        let _g = state_guard();
        let mut set = FileSet::load(path);
        f(&mut set);
        set.save(path)
    }
}

/// One cloud op owed because the remote was down when it ran. These are exactly
/// the ops the local/base/cloud compare can't reconstruct: a move and a rename
/// (which would otherwise look like a delete + a re-add), and a whole-trip purge
/// (whose local trip is already gone, so nothing anchors the compare).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingOp {
    /// Move one clip's cloud copy `<remote>/<from>/<rel>` → `<remote>/<to>/<rel>`.
    Move {
        from: String,
        to: String,
        rel: String,
    },
    /// Rename your subtree `<remote>/<old>/<user>` → `<remote>/<new>/<user>`.
    Rename { old: String, new: String },
    /// Purge your subtree of a trip deleted while offline: `<remote>/<trip>/<user>`.
    Purge { trip: String },
}

impl PendingOp {
    /// The trip this op is "about" for panel grouping (the destination for a
    /// move, since that's the trip the user is looking at).
    pub fn trip(&self) -> &str {
        match self {
            PendingOp::Move { to, .. } => to,
            PendingOp::Rename { new, .. } => new,
            PendingOp::Purge { trip } => trip,
        }
    }
}

/// The owed-op queue. Rows are `when\tkind\ta\tb\tc`; extra columns are unused
/// per kind. Order is preserved so a move then a rename replay in sequence.
#[derive(Default)]
pub struct Pending {
    pub rows: Vec<(i64, PendingOp)>,
}

impl Pending {
    pub fn load(path: &Path) -> Self {
        let mut rows = Vec::new();
        if let Ok(txt) = fs::read_to_string(path) {
            for line in txt.lines() {
                if line.is_empty() {
                    continue;
                }
                let f: Vec<&str> = line.split('\t').collect();
                if f.len() < 2 {
                    continue;
                }
                let when: i64 = f[0].parse().unwrap_or(0);
                let op = match f[1] {
                    "move" if f.len() >= 5 => PendingOp::Move {
                        from: f[2].to_string(),
                        to: f[3].to_string(),
                        rel: f[4].to_string(),
                    },
                    "rename" if f.len() >= 4 => PendingOp::Rename {
                        old: f[2].to_string(),
                        new: f[3].to_string(),
                    },
                    "purge" if f.len() >= 3 => PendingOp::Purge {
                        trip: f[2].to_string(),
                    },
                    _ => continue,
                };
                rows.push((when, op));
            }
        }
        Pending { rows }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut s = String::new();
        for (when, op) in &self.rows {
            let line = match op {
                PendingOp::Move { from, to, rel } => format!("{when}\tmove\t{from}\t{to}\t{rel}"),
                PendingOp::Rename { old, new } => format!("{when}\trename\t{old}\t{new}\t"),
                PendingOp::Purge { trip } => format!("{when}\tpurge\t{trip}\t\t"),
            };
            s.push_str(&line);
            s.push('\n');
        }
        write_atomic(path, &s)
    }

    /// Enqueue an op (idempotent — a duplicate op isn't queued twice).
    pub fn push(&mut self, when: i64, op: PendingOp) {
        if !self.rows.iter().any(|(_, o)| o == &op) {
            self.rows.push((when, op));
        }
    }

    pub fn count_for(&self, trip: &str) -> usize {
        self.rows.iter().filter(|(_, o)| o.trip() == trip).count()
    }

    /// Load → mutate → save under the state lock. Replay is a network op, so a
    /// reconcile must run it *outside* this and then drop the ops that landed —
    /// re-reading here keeps an op queued by another command in the meantime.
    pub fn update(path: &Path, f: impl FnOnce(&mut Pending)) -> io::Result<()> {
        let _g = state_guard();
        let mut p = Pending::load(path);
        f(&mut p);
        p.save(path)
    }
}

/// A cached cloud listing plus when it was fetched (`None` = never / unparseable).
pub struct CloudCache {
    pub checked: Option<i64>,
    pub files: FileSet,
}

/// Read a cloud cache: a `#checked\t<epoch>` header line then `rel\tsize` rows.
pub fn load_cloud_cache(path: &Path) -> CloudCache {
    let checked = fs::read_to_string(path).ok().and_then(|t| {
        t.lines()
            .find_map(|l| l.strip_prefix("#checked\t").and_then(|v| v.parse().ok()))
    });
    CloudCache {
        checked,
        files: FileSet::load(path), // FileSet::load skips the '#' header
    }
}

/// Write a cloud cache (timestamp header + the listing), atomically.
pub fn save_cloud_cache(path: &Path, checked: i64, files: &FileSet) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut s = format!("#checked\t{checked}\n");
    for (rel, size) in &files.files {
        s.push_str(&format!("{rel}\t{size}\n"));
    }
    write_atomic(path, &s)
}

/// Write `body` to `path` through a temp sibling + rename, so a reader never sees
/// a partial file. Mirrors `Ledger::save`.
pub(crate) fn write_atomic(path: &Path, body: &str) -> io::Result<()> {
    let tmp = temp_sibling(path);
    fs::write(&tmp, body)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp); // don't leave a stray temp behind
    })
}
