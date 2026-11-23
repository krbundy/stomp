//! Importing scan results and reading scans back (§9, §10.3).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use super::Catalog;
use crate::scan::hist::{Count, month_index};
use crate::scan::{DIR_COLUMNS, DirRow, ResultInfo, below_bounds, path_bytes, path_from_bytes};

/// Scans keep per-directory detail for the newest few, plus one a month for a year.
const KEEP_RECENT: usize = 4;
const KEEP_MONTHLY: usize = 12;
/// Interrupted scans that were never resumed are discarded after this long.
const ABANDONED_AFTER: i64 = 14 * 86_400;

#[derive(Debug, Clone, Serialize)]
pub struct ScanInfo {
    pub id: i64,
    pub uuid: String,
    pub started_at: i64,
    pub finished_at: i64,
    pub full: bool,
    pub file_stats: bool,
    /// `full`, `partial` (some reused rows missing) or `pruned` (only root and grant rows kept).
    pub detail: String,
    pub dirs_walked: u64,
    pub files_walked: u64,
    pub dirs_reused: u64,
}

const SCAN_COLUMNS: &str = "id, uuid, started_at, finished_at, full, file_stats, detail, \
     dirs_walked, files_walked, dirs_reused";

fn scan_from_row(r: &rusqlite::Row) -> rusqlite::Result<ScanInfo> {
    let count = |i: usize| -> rusqlite::Result<u64> { Ok(r.get::<_, i64>(i)?.max(0) as u64) };
    Ok(ScanInfo {
        id: r.get(0)?,
        uuid: r.get(1)?,
        started_at: r.get(2)?,
        finished_at: r.get(3)?,
        full: r.get(4)?,
        file_stats: r.get(5)?,
        detail: r.get(6)?,
        dirs_walked: count(7)?,
        files_walked: count(8)?,
        dirs_reused: count(9)?,
    })
}

fn one_scan(
    conn: &Connection,
    condition: &str,
    args: impl rusqlite::Params,
) -> Result<Option<ScanInfo>> {
    Ok(conn
        .query_row(
            &format!("SELECT {SCAN_COLUMNS} FROM scans WHERE {condition} ORDER BY started_at DESC, id DESC LIMIT 1"),
            args,
            scan_from_row,
        )
        .optional()?)
}

/// Deletes a result file and the journal SQLite keeps beside it.
fn remove_result(path: &Path) -> std::io::Result<()> {
    std::fs::remove_file(path)?;
    let _ = std::fs::remove_file(crate::scan::lock_path(path));
    let mut journal = path.as_os_str().to_owned();
    journal.push("-journal");
    match std::fs::remove_file(PathBuf::from(journal)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// The newest scan; with `require_detail`, the newest with complete per-directory detail.
pub fn latest_scan(conn: &Connection, require_detail: bool) -> Result<Option<ScanInfo>> {
    one_scan(
        conn,
        if require_detail {
            "detail = 'full'"
        } else {
            "1"
        },
        [],
    )
}

/// Start time of the newest full (non-incremental) scan.
pub fn last_full_scan(conn: &Connection) -> Result<Option<i64>> {
    Ok(conn.query_row(
        "SELECT max(started_at) FROM scans WHERE full = 1",
        [],
        |r| r.get(0),
    )?)
}

/// The catalog ID of a scan with complete detail, by its UUID.
pub fn scan_id(conn: &Connection, uuid: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM scans WHERE uuid = ?1 AND detail = 'full'",
            [uuid],
            |r| r.get(0),
        )
        .optional()?)
}

impl Catalog {
    /// Imports finished scans from the inbox, oldest first, then prunes old detail.
    pub(super) fn ingest_scans(&mut self, inbox: &Path) -> Result<Vec<String>> {
        let now = jiff::Timestamp::now().as_second();
        let mut messages = Vec::new();
        let mut imported = false;
        for result in crate::scan::list_results(inbox)? {
            if result.meta.status != "complete" {
                if now - result.meta.started_at > ABANDONED_AFTER {
                    remove_result(&result.path)?;
                    messages.push(format!(
                        "discarded an interrupted scan from {} that was never resumed",
                        crate::cmd::local_time(jiff::Timestamp::from_second(
                            result.meta.started_at
                        )?)
                    ));
                }
                continue;
            }
            messages.extend(self.ingest_scan(&result)?);
            remove_result(&result.path)?;
            imported = true;
        }
        if imported {
            self.prune_scan_detail()?;
        }
        Ok(messages)
    }

    fn ingest_scan(&mut self, result: &ResultInfo) -> Result<Vec<String>> {
        let already: bool = self.conn.query_row(
            "SELECT count(*) > 0 FROM scans WHERE uuid = ?1",
            [&result.meta.uuid],
            |r| r.get(0),
        )?;
        if already {
            return Ok(Vec::new());
        }
        self.conn
            .execute("ATTACH DATABASE ?1 AS r", [result.path.to_string_lossy()])?;
        let imported = self.copy_scan(result);
        self.conn.execute("DETACH DATABASE r", [])?;
        imported
    }

    fn copy_scan(&mut self, result: &ResultInfo) -> Result<Vec<String>> {
        let mut messages = Vec::new();
        let tx = self.conn.transaction()?;
        let meta = |key: &str| -> Result<i64> {
            Ok(tx
                .query_row("SELECT value FROM r.meta WHERE key = ?1", [key], |r| {
                    r.get::<_, String>(0)
                })
                .optional()?
                .and_then(|v| v.parse().ok())
                .unwrap_or(0))
        };
        let (finished_at, dirs, files, reused) = (
            meta("finished_at")?,
            meta("dirs")?,
            meta("files")?,
            meta("reused")?,
        );
        tx.execute(
            "INSERT INTO scans (uuid, started_at, finished_at, full, file_stats, task_id, host,
                                dirs_walked, files_walked, dirs_reused, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'full')",
            params![
                result.meta.uuid,
                result.meta.started_at,
                finished_at,
                result.meta.full,
                result.meta.file_stats,
                result.meta.task_id,
                result.meta.host,
                dirs,
                files,
                reused
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            &format!("INSERT INTO dir_stats (scan_id, {DIR_COLUMNS}) SELECT ?1, {DIR_COLUMNS} FROM r.dirs"),
            [id],
        )?;

        // Unchanged subtrees were recorded by their top directory only; copy
        // the rows below each from the scan they were reused from.
        let reused_paths: Vec<Vec<u8>> = {
            let mut stmt = tx.prepare("SELECT path FROM r.dirs WHERE reused = 1")?;
            stmt.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        if !reused_paths.is_empty() {
            let prev = match &result.meta.prev_uuid {
                Some(uuid) => scan_id(&tx, uuid)?,
                None => None,
            };
            match prev {
                Some(prev_id) => {
                    let mut copy = tx.prepare(
                        "INSERT OR IGNORE INTO dir_stats (scan_id, path, role, excluded, direct_bytes,
                             direct_files, bytes, files, unreadable, newest_mtime, oldest_mtime,
                             newest_change, sizes, months, reused)
                         SELECT ?1, path, role, excluded, direct_bytes, direct_files, bytes, files,
                             unreadable, newest_mtime, oldest_mtime, newest_change, sizes, months, 1
                         FROM dir_stats WHERE scan_id = ?2 AND path > ?3 AND path < ?4",
                    )?;
                    for path in reused_paths {
                        let (low, high) = below_bounds(&path_from_bytes(path));
                        copy.execute(params![id, prev_id, low, high])?;
                    }
                }
                None => {
                    tx.execute("UPDATE scans SET detail = 'partial' WHERE id = ?1", [id])?;
                    messages.push(
                        "a scan built on an earlier scan that is no longer in the catalog, so some \
                         directory detail is missing; run `stomp scan --full` to rebuild it"
                            .to_string(),
                    );
                }
            }
        }
        tx.execute(
            "INSERT INTO scan_ext (scan_id, scope, name, ext, bytes, files)
             SELECT ?1, scope, name, ext, bytes, files FROM r.ext",
            [id],
        )?;
        tx.execute(
            "INSERT INTO scan_largest (scan_id, scope, name, path, bytes, mtime)
             SELECT ?1, scope, name, path, bytes, mtime FROM r.largest",
            [id],
        )?;
        tx.commit()?;
        Ok(messages)
    }

    /// Drops per-directory detail from old scans, keeping root and grant rows (§10.3).
    fn prune_scan_detail(&mut self) -> Result<usize> {
        let scans: Vec<(i64, i64)> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, started_at FROM scans WHERE detail != 'pruned' ORDER BY started_at DESC, id DESC",
            )?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut keep: HashSet<i64> = scans.iter().take(KEEP_RECENT).map(|(id, _)| *id).collect();
        let mut months_seen = Vec::new();
        for (id, started) in &scans {
            let month = month_index(*started);
            if !months_seen.contains(&month) && months_seen.len() < KEEP_MONTHLY {
                months_seen.push(month);
                keep.insert(*id);
            }
        }
        let mut pruned = 0;
        let tx = self.conn.transaction()?;
        for (id, _) in scans.iter().filter(|(id, _)| !keep.contains(id)) {
            tx.execute(
                "DELETE FROM dir_stats WHERE scan_id = ?1 AND role IS NULL",
                [id],
            )?;
            tx.execute("UPDATE scans SET detail = 'pruned' WHERE id = ?1", [id])?;
            pruned += 1;
        }
        tx.commit()?;
        Ok(pruned)
    }

    pub fn latest_scan(&self, require_detail: bool) -> Result<Option<ScanInfo>> {
        latest_scan(&self.conn, require_detail)
    }

    /// The newest scan started at or before `t`.
    pub fn scan_at_or_before(&self, t: i64, require_detail: bool) -> Result<Option<ScanInfo>> {
        let detail = if require_detail {
            " AND detail = 'full'"
        } else {
            ""
        };
        one_scan(&self.conn, &format!("started_at <= ?1{detail}"), [t])
    }

    /// The newest scan before `scan`.
    pub fn scan_before(&self, scan: &ScanInfo, require_detail: bool) -> Result<Option<ScanInfo>> {
        let detail = if require_detail {
            " AND detail = 'full'"
        } else {
            ""
        };
        one_scan(
            &self.conn,
            &format!("(started_at < ?1 OR (started_at = ?1 AND id < ?2)){detail}"),
            params![scan.started_at, scan.id],
        )
    }

    /// The newest scan that recorded file types and largest files.
    pub fn latest_file_stats_scan(&self) -> Result<Option<ScanInfo>> {
        one_scan(&self.conn, "file_stats = 1", [])
    }

    pub fn scan_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM scans", [], |r| r.get(0))?)
    }

    /// A scan's rows for `under` and everything below it, or all rows.
    pub fn dir_rows(&self, scan_id: i64, under: Option<&Path>) -> Result<Vec<DirRow>> {
        let mut rows = Vec::new();
        match under {
            None => {
                let mut stmt = self.conn.prepare(&format!(
                    "SELECT {DIR_COLUMNS} FROM dir_stats WHERE scan_id = ?1 ORDER BY path"
                ))?;
                for row in stmt.query_map([scan_id], |r| DirRow::from_row(r, 0))? {
                    rows.push(row?);
                }
            }
            Some(dir) => {
                let (low, high) = below_bounds(dir);
                let mut stmt = self.conn.prepare(&format!(
                    "SELECT {DIR_COLUMNS} FROM dir_stats
                     WHERE scan_id = ?1 AND (path = ?2 OR (path > ?3 AND path < ?4)) ORDER BY path"
                ))?;
                for row in stmt.query_map(params![scan_id, path_bytes(dir), low, high], |r| {
                    DirRow::from_row(r, 0)
                })? {
                    rows.push(row?);
                }
            }
        }
        Ok(rows)
    }

    pub fn dir_row(&self, scan_id: i64, path: &Path) -> Result<Option<DirRow>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {DIR_COLUMNS} FROM dir_stats WHERE scan_id = ?1 AND path = ?2"),
                params![scan_id, path_bytes(path)],
                |r| DirRow::from_row(r, 0),
            )
            .optional()?)
    }

    /// File types for a scope (`grant` or `root`), largest first.
    pub fn scan_extensions(
        &self,
        scan_id: i64,
        scope: &str,
        name: &str,
    ) -> Result<Vec<(String, Count)>> {
        let mut stmt = self.conn.prepare(
            "SELECT ext, bytes, files FROM scan_ext WHERE scan_id = ?1 AND scope = ?2 AND name = ?3
             ORDER BY bytes DESC",
        )?;
        let rows = stmt.query_map(params![scan_id, scope, name], |r| {
            Ok((
                r.get(0)?,
                Count {
                    bytes: r.get::<_, i64>(1)?.max(0) as u64,
                    files: r.get::<_, i64>(2)?.max(0) as u64,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Largest files for a scope, largest first: (path, bytes, mtime).
    pub fn scan_largest(
        &self,
        scan_id: i64,
        scope: &str,
        name: &str,
    ) -> Result<Vec<(PathBuf, u64, Option<i64>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, bytes, mtime FROM scan_largest WHERE scan_id = ?1 AND scope = ?2 AND name = ?3
             ORDER BY bytes DESC",
        )?;
        let rows = stmt.query_map(params![scan_id, scope, name], |r| {
            Ok((
                path_from_bytes(r.get(0)?),
                r.get::<_, i64>(1)?.max(0) as u64,
                r.get(2)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}
