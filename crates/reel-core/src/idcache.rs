//! A persistent cache of content ids, so a file that hasn't changed isn't read
//! and hashed again. `fileid_of` reads ~8 MiB per file; a card rescan did that
//! for every clip, off an SD card, each time.
//!
//! An entry is trusted only while the file's path, byte size, and mtime (to the
//! nanosecond the filesystem reports) all match what was recorded. Any change
//! is a miss and the file is hashed again. Ids are the same ones `fileid_of`
//! returns, so the ledger and poster cache are unaffected.
//!
//! The cache is advisory: a missing, unreadable, or lost-update file only
//! costs a re-hash. Each save keeps just the paths seen in that run, so the
//! file never outgrows the card or library last scanned.

use crate::config::Config;
use crate::media::fileid_of;
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

struct Entry {
    size: u64,
    mtime_ns: u128,
    id: String,
}

pub struct IdCache {
    file: PathBuf,
    rows: HashMap<String, Entry>,
    seen: HashSet<String>,
    misses: usize,
}

/// `(key, size, mtime_ns)` for a file, or `None` when it can't be cached: a
/// non-UTF-8 path, one the TSV can't hold, or a filesystem with no mtime.
fn stamp(p: &Path) -> Option<(String, u64, u128)> {
    let key = p.to_str()?;
    if key.contains(['\t', '\n']) {
        return None;
    }
    let m = std::fs::metadata(p).ok()?;
    let mtime = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some((key.to_string(), m.len(), mtime.as_nanos()))
}

impl IdCache {
    pub fn load(cfg: &Config) -> Self {
        Self::load_from(cfg.cache_dir.join("fileids.tsv"))
    }

    fn load_from(file: PathBuf) -> Self {
        let mut rows = HashMap::new();
        if let Ok(txt) = std::fs::read_to_string(&file) {
            for line in txt.lines() {
                let mut f = line.splitn(4, '\t');
                let (Some(id), Some(size), Some(mtime), Some(path)) =
                    (f.next(), f.next(), f.next(), f.next())
                else {
                    continue;
                };
                let (Ok(size), Ok(mtime_ns)) = (size.parse(), mtime.parse()) else {
                    continue;
                };
                rows.insert(
                    path.to_string(),
                    Entry {
                        size,
                        mtime_ns,
                        id: id.to_string(),
                    },
                );
            }
        }
        IdCache {
            file,
            rows,
            seen: HashSet::new(),
            misses: 0,
        }
    }

    /// The content id of `p`, from the cache when the file is unchanged.
    pub fn fileid(&mut self, p: &Path) -> io::Result<String> {
        let Some((key, size, mtime_ns)) = stamp(p) else {
            return fileid_of(p);
        };
        if let Some(e) = self.rows.get(&key) {
            // The id leads with the size it was computed from; a mismatch means
            // a damaged row, which is a miss like any other.
            if e.size == size && e.mtime_ns == mtime_ns && e.id.starts_with(&format!("{size}-")) {
                let id = e.id.clone();
                self.seen.insert(key);
                return Ok(id);
            }
        }
        // Stamped before hashing: a file that changes mid-read gets a stamp
        // older than its content, so the next lookup misses rather than lies.
        let id = fileid_of(p)?;
        self.rows.insert(
            key.clone(),
            Entry {
                size,
                mtime_ns,
                id: id.clone(),
            },
        );
        self.seen.insert(key);
        self.misses += 1;
        Ok(id)
    }

    /// Write the cache back, keeping only the paths looked up this run. A no-op
    /// when nothing was hashed and nothing is being dropped. Best-effort.
    pub fn save(mut self) {
        if self.misses == 0 && self.rows.len() == self.seen.len() {
            return;
        }
        self.rows.retain(|k, _| self.seen.contains(k));
        let mut keys: Vec<&String> = self.rows.keys().collect();
        keys.sort();
        let mut s = String::new();
        for k in keys {
            let e = &self.rows[k];
            s.push_str(&format!("{}\t{}\t{}\t{}\n", e.id, e.size, e.mtime_ns, k));
        }
        if let Some(parent) = self.file.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        let _ = crate::store::write_atomic(&self.file, &s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::time::Duration;

    fn put(path: &Path, fill: u8, len: usize, epoch: u64) {
        fs::write(path, vec![fill; len]).unwrap();
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(epoch))
            .unwrap();
    }

    #[test]
    fn hit_skips_the_hash_and_any_change_is_a_miss() {
        let d = tempfile::tempdir().unwrap();
        let clip = d.path().join("DJI_0001.MP4");
        let cache = d.path().join("cache/fileids.tsv");
        put(&clip, 1, 9_000_000, 1_700_000_000);

        let real = fileid_of(&clip).unwrap();
        let mut c = IdCache::load_from(cache.clone());
        assert_eq!(c.fileid(&clip).unwrap(), real);
        c.save();

        // Plant a recognisable id in the saved row: only a hit returns it.
        let planted = "9000000-planted";
        let txt = fs::read_to_string(&cache).unwrap().replace(&real, planted);
        fs::write(&cache, txt).unwrap();
        let mut c = IdCache::load_from(cache.clone());
        assert_eq!(c.fileid(&clip).unwrap(), planted, "unchanged file hits");

        // Same size, new mtime.
        put(&clip, 1, 9_000_000, 1_700_000_001);
        assert_eq!(c.fileid(&clip).unwrap(), real, "mtime change misses");

        // New mtime and new size.
        put(&clip, 2, 9_000_001, 1_700_000_002);
        let id = c.fileid(&clip).unwrap();
        assert_eq!(id, fileid_of(&clip).unwrap());
        assert!(id.starts_with("9000001-"));
    }

    #[test]
    fn a_damaged_row_is_rehashed_and_save_drops_unseen_paths() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("A.MP4");
        let b = d.path().join("B.MP4");
        let cache = d.path().join("fileids.tsv");
        put(&a, 1, 5_000, 1_700_000_000);
        put(&b, 2, 5_000, 1_700_000_000);

        let mut c = IdCache::load_from(cache.clone());
        c.fileid(&a).unwrap();
        c.fileid(&b).unwrap();
        c.save();
        assert_eq!(fs::read_to_string(&cache).unwrap().lines().count(), 2);

        // A row whose id doesn't lead with the file's size is not trusted.
        let (_, size, mtime) = stamp(&a).unwrap();
        let bad = format!("1-wrong\t{size}\t{mtime}\t{}\n", a.display());
        fs::write(&cache, bad).unwrap();
        let mut c = IdCache::load_from(cache.clone());
        assert_eq!(c.fileid(&a).unwrap(), fileid_of(&a).unwrap());
        c.save();
        let saved = fs::read_to_string(&cache).unwrap();
        assert_eq!(saved.lines().count(), 1);
        assert!(!saved.contains("1-wrong"));
    }

    #[test]
    fn unreadable_files_error_like_fileid_of() {
        let d = tempfile::tempdir().unwrap();
        let mut c = IdCache::load_from(d.path().join("fileids.tsv"));
        assert!(c.fileid(&d.path().join("nope.MP4")).is_err());
    }
}
