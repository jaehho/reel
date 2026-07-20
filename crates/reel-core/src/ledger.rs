//! The import ledger: content-id → trip that already owns each clip. Kept in the
//! same TSV the original script used, so the two stay interoperable.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Clone, Debug)]
pub struct LedgerRow {
    pub id: String,
    pub trip: String,
    pub person: String,
    pub camera: String,
    pub base: String,
    pub bytes: String,
    pub captured: String,
    pub imported_at: String,
}

#[derive(Default)]
pub struct Ledger {
    pub rows: Vec<LedgerRow>,
}

fn field(f: &[&str], i: usize) -> String {
    f.get(i).copied().unwrap_or("").to_string()
}

impl Ledger {
    pub fn load(path: &Path) -> Self {
        let mut rows = Vec::new();
        if let Ok(txt) = std::fs::read_to_string(path) {
            for line in txt.lines() {
                if line.is_empty() {
                    continue;
                }
                let f: Vec<&str> = line.split('\t').collect();
                if f.len() >= 2 {
                    rows.push(LedgerRow {
                        id: field(&f, 0),
                        trip: field(&f, 1),
                        person: field(&f, 2),
                        camera: field(&f, 3),
                        base: field(&f, 4),
                        bytes: field(&f, 5),
                        captured: field(&f, 6),
                        imported_at: field(&f, 7),
                    });
                }
            }
        }
        Ledger { rows }
    }

    /// The trip that owns this content id, if any.
    pub fn trip_of(&self, id: &str) -> Option<String> {
        self.rows
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.trip.clone())
    }

    /// The full row for a content id — trip plus the person/camera/base needed to
    /// find that clip's local copy.
    pub fn row_of(&self, id: &str) -> Option<&LedgerRow> {
        self.rows.iter().find(|r| r.id == id)
    }

    /// Insert or replace the row for a content id (upsert by id), so re-importing
    /// the same clip updates its owner rather than duplicating it.
    pub fn upsert(&mut self, row: LedgerRow) {
        self.rows.retain(|r| r.id != row.id);
        self.rows.push(row);
    }

    /// Write the ledger back as the same TSV the script reads. Atomic: a temp
    /// sibling is written and renamed, so a crash mid-write can't truncate it.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut s = String::new();
        for r in &self.rows {
            s.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                r.id, r.trip, r.person, r.camera, r.base, r.bytes, r.captured, r.imported_at
            ));
        }
        crate::store::write_atomic(path, &s)
    }

    /// Load → mutate → save under the state lock. An import's copy and a delete's
    /// cloud call must happen *before* this, with only the resulting row edits made
    /// inside: two commands that each held the whole ledger across their slow work
    /// would silently drop one side's rows.
    pub fn update(path: &Path, f: impl FnOnce(&mut Ledger)) -> io::Result<()> {
        let _g = crate::store::state_guard();
        let mut l = Ledger::load(path);
        f(&mut l);
        l.save(path)
    }
}

/// Content ids the user permanently deleted. A tombstoned clip is dead to the
/// pipeline: import skips it, and a copy still on a card reads as "discarded"
/// rather than "new", so a clip you threw away never claws its way back in.
/// Its own tiny TSV (`deleted.tsv`), separate from the script-shared ledger.
#[derive(Default)]
pub struct Tombstones {
    pub ids: HashSet<String>,
}

impl Tombstones {
    pub fn load(path: &Path) -> Self {
        let mut ids = HashSet::new();
        if let Ok(txt) = std::fs::read_to_string(path) {
            for line in txt.lines() {
                // first field is the id; extra columns (base, when) are for humans
                if let Some(id) = line.split('\t').next() {
                    if !id.is_empty() {
                        ids.insert(id.to_string());
                    }
                }
            }
        }
        Tombstones { ids }
    }

    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// Record a content id as deleted (idempotent).
    pub fn insert(&mut self, id: &str) {
        self.ids.insert(id.to_string());
    }

    /// Write the set back atomically (sorted, so the file is stable across runs).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut ids: Vec<&String> = self.ids.iter().collect();
        ids.sort();
        let mut s = String::new();
        for id in ids {
            s.push_str(id);
            s.push('\n');
        }
        crate::store::write_atomic(path, &s)
    }

    /// Load → mutate → save under the state lock. A lost tombstone is the worst of
    /// the state races: the clip stops reading as "discarded", so footage you
    /// permanently deleted is offered for import again.
    pub fn update(path: &Path, f: impl FnOnce(&mut Tombstones)) -> io::Result<()> {
        let _g = crate::store::state_guard();
        let mut t = Tombstones::load(path);
        f(&mut t);
        t.save(path)
    }
}
