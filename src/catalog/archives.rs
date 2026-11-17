//! Archives known to the catalog (§12), learned from records that archive
//! and restore runs leave in the inbox.

use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use super::{Catalog, to_db};
use crate::archive::ArchiveRecord;

#[derive(Debug, Clone, Serialize)]
pub struct CatalogArchive {
    pub id: String,
    pub source: String,
    pub grant: Option<String>,
    pub destination: String,
    pub location: String,
    pub state: String,
    pub created_at: Option<String>,
    pub finished_at: Option<String>,
    pub files: u64,
    pub bytes: u64,
    pub compressed: u64,
    pub chunks: u64,
    pub kept: u64,
    pub skipped: u64,
}

const COLUMNS: &str = "id, source, grant_id, destination, location, state, created_at, finished_at, \
     files, bytes, compressed, chunks, kept, skipped";

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<CatalogArchive> {
    let n = |i: usize| -> rusqlite::Result<u64> { Ok(r.get::<_, i64>(i)?.max(0) as u64) };
    Ok(CatalogArchive {
        id: r.get(0)?,
        source: r.get(1)?,
        grant: r.get(2)?,
        destination: r.get(3)?,
        location: r.get(4)?,
        state: r.get(5)?,
        created_at: r.get(6)?,
        finished_at: r.get(7)?,
        files: n(8)?,
        bytes: n(9)?,
        compressed: n(10)?,
        chunks: n(11)?,
        kept: n(12)?,
        skipped: n(13)?,
    })
}

impl Catalog {
    /// Imports archive records, oldest first, so the newest state wins.
    pub(super) fn ingest_archives(&mut self, inbox: &Path) -> Result<Vec<String>> {
        let entries = match std::fs::read_dir(inbox) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        // Names are `<id>-<nanosecond stamp>.json`, so sorting orders each archive's records in time.
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        files.sort();
        let mut messages = Vec::new();
        let now = jiff::Timestamp::now().as_second();
        for path in files {
            let parsed = std::fs::read_to_string(&path)
                .map_err(anyhow::Error::from)
                .and_then(|t| Ok(serde_json::from_str::<ArchiveRecord>(&t)?));
            match parsed {
                Ok(r) if r.state == "restored" => {
                    // A restore only changes the state; the rest of the record stays for history.
                    let updated = self.conn.execute(
                        "UPDATE archives SET state = 'restored', updated_at = ?2 WHERE id = ?1",
                        params![r.id, now],
                    )?;
                    if updated == 0 {
                        self.put_archive(&r, now)?;
                    }
                    std::fs::remove_file(&path)?;
                }
                Ok(r) => {
                    self.put_archive(&r, now)?;
                    std::fs::remove_file(&path)?;
                }
                Err(e) => {
                    let aside = path.with_extension("rejected");
                    let _ = std::fs::rename(&path, &aside);
                    messages.push(format!(
                        "could not import archive record {}: {e:#}",
                        path.display()
                    ));
                }
            }
        }
        Ok(messages)
    }

    fn put_archive(&mut self, r: &ArchiveRecord, now: i64) -> Result<()> {
        self.conn.execute(
            &format!(
                "INSERT OR REPLACE INTO archives ({COLUMNS}, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)"
            ),
            params![
                r.id,
                r.source,
                r.grant,
                r.destination,
                r.location,
                r.state,
                r.created_at,
                r.finished_at,
                to_db(r.files),
                to_db(r.bytes),
                to_db(r.compressed),
                r.chunks,
                to_db(r.kept),
                to_db(r.skipped),
                now
            ],
        )?;
        Ok(())
    }

    /// All archives, newest first.
    pub fn archives(&self) -> Result<Vec<CatalogArchive>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM archives ORDER BY coalesce(created_at, '') DESC, id"
        ))?;
        let rows = stmt.query_map([], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn archive(&self, id: &str) -> Result<Option<CatalogArchive>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM archives WHERE id = ?1"),
                [id],
                from_row,
            )
            .optional()?)
    }
}
