//! The import ledger: content-id → trip that already owns each clip. Kept in the
//! same TSV the original script used, so the two stay interoperable.

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
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, s)?;
        fs::rename(&tmp, path)
    }
}
