//! The scan result file (§10.2): a self-contained SQLite database in the
//! inbox. A scan, often running as a Slurm task, writes it; the next
//! foreground command imports it into the catalog. Finished work units are
//! committed as they complete, so an interrupted scan can resume.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, params, params_from_iter};

use super::walk::{FileStats, Progress, UnitStore, Walked};
use super::{DIR_COLUMNS, DirRow, path_bytes};

const SCHEMA: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE dirs (
    path BLOB PRIMARY KEY, role TEXT, excluded TEXT,
    direct_bytes INTEGER NOT NULL, direct_files INTEGER NOT NULL,
    bytes INTEGER NOT NULL, files INTEGER NOT NULL, unreadable INTEGER NOT NULL,
    newest_mtime INTEGER, oldest_mtime INTEGER, newest_change INTEGER,
    sizes BLOB, months BLOB, reused INTEGER NOT NULL
) WITHOUT ROWID;
-- Finished units, waiting for their parent to fold or keep them.
CREATE TABLE unit_roots (
    path BLOB PRIMARY KEY, role TEXT, excluded TEXT,
    direct_bytes INTEGER NOT NULL, direct_files INTEGER NOT NULL,
    bytes INTEGER NOT NULL, files INTEGER NOT NULL, unreadable INTEGER NOT NULL,
    newest_mtime INTEGER, oldest_mtime INTEGER, newest_change INTEGER,
    sizes BLOB, months BLOB, reused INTEGER NOT NULL,
    has_rows_below INTEGER NOT NULL
) WITHOUT ROWID;
CREATE TABLE ext (scope TEXT NOT NULL, name TEXT NOT NULL, ext TEXT NOT NULL,
                  bytes INTEGER NOT NULL, files INTEGER NOT NULL);
CREATE TABLE largest (scope TEXT NOT NULL, name TEXT NOT NULL, path BLOB NOT NULL,
                      bytes INTEGER NOT NULL, mtime INTEGER);
";

/// What a result file says about its scan.
#[derive(Debug, Clone)]
pub struct Meta {
    pub uuid: String,
    /// `running`, `incomplete` or `complete`.
    pub status: String,
    pub started_at: i64,
    pub full: bool,
    /// Whether file types and largest files were collected for the whole tree.
    pub file_stats: bool,
    /// The scan whose rows unchanged subtrees reuse.
    pub prev_uuid: Option<String>,
    pub prev_started_at: Option<i64>,
    pub task_id: Option<String>,
    pub host: String,
}

fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(30))?;
    // This file is a checkpoint, committed once per work unit. Waiting for the
    // disk on every commit made scans an order of magnitude slower, and each
    // journal file created and deleted costs metadata-server round trips on
    // CephFS. A process that is killed loses nothing without syncing; only an
    // OS crash could lose recent units, which a resumed scan simply redoes.
    conn.pragma_update_and_check(None, "journal_mode", "PERSIST", |row| {
        row.get::<_, String>(0)
    })?;
    conn.pragma_update(None, "synchronous", "OFF")?;
    Ok(())
}

fn read_meta(conn: &Connection) -> Result<Meta> {
    let mut values: HashMap<String, String> = HashMap::new();
    let mut stmt = conn.prepare("SELECT key, value FROM meta")?;
    for row in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
        let (k, v) = row?;
        values.insert(k, v);
    }
    let get = |k: &str| values.get(k).cloned();
    let int = |k: &str| get(k).and_then(|v| v.parse::<i64>().ok());
    Ok(Meta {
        uuid: get("uuid").context("result file has no uuid")?,
        status: get("status").unwrap_or_default(),
        started_at: int("started_at").context("result file has no start time")?,
        full: int("full") == Some(1),
        file_stats: int("file_stats") == Some(1),
        prev_uuid: get("prev_uuid").filter(|v| !v.is_empty()),
        prev_started_at: int("prev_started_at"),
        task_id: get("task_id").filter(|v| !v.is_empty()),
        host: get("host").unwrap_or_default(),
    })
}

pub struct ResultDb {
    conn: Mutex<Connection>,
    done: HashMap<PathBuf, Walked>,
}

impl ResultDb {
    pub fn create(path: &Path, meta: &Meta) -> Result<ResultDb> {
        if path.exists() {
            bail!("{} already exists", path.display());
        }
        let conn = Connection::open(path)?;
        configure(&conn)?;
        conn.execute_batch(SCHEMA)?;
        let db = ResultDb {
            conn: Mutex::new(conn),
            done: HashMap::new(),
        };
        let flag = |b: bool| if b { "1" } else { "0" }.to_string();
        let pairs = [
            ("uuid", meta.uuid.clone()),
            ("status", meta.status.clone()),
            ("started_at", meta.started_at.to_string()),
            ("full", flag(meta.full)),
            ("file_stats", flag(meta.file_stats)),
            ("prev_uuid", meta.prev_uuid.clone().unwrap_or_default()),
            (
                "prev_started_at",
                meta.prev_started_at
                    .map(|t| t.to_string())
                    .unwrap_or_default(),
            ),
            ("task_id", meta.task_id.clone().unwrap_or_default()),
            ("host", meta.host.clone()),
        ];
        for (k, v) in pairs {
            db.set_meta(k, &v)?;
        }
        Ok(db)
    }

    /// Opens an interrupted scan to resume it, loading its finished units.
    pub fn open(path: &Path) -> Result<(ResultDb, Meta)> {
        let conn = Connection::open(path)?;
        configure(&conn)?;
        let meta = read_meta(&conn)?;
        let mut done = HashMap::new();
        {
            let mut stmt = conn.prepare(&format!(
                "SELECT {DIR_COLUMNS}, has_rows_below FROM unit_roots"
            ))?;
            let rows = stmt.query_map([], |r| {
                Ok(Walked {
                    row: DirRow::from_row(r, 0)?,
                    has_rows_below: r.get::<_, i64>(14)? != 0,
                })
            })?;
            for walked in rows {
                let walked = walked?;
                done.insert(walked.row.path.clone(), walked);
            }
        }
        Ok((
            ResultDb {
                conn: Mutex::new(conn),
                done,
            },
            meta,
        ))
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn set_status(&self, status: &str) -> Result<()> {
        self.set_meta("status", status)
    }

    /// Writes the remaining rows and file statistics, and marks the scan complete.
    pub fn finish(
        &self,
        rows: &[DirRow],
        stats: Option<&FileStats>,
        progress: &Progress,
        finished_at: i64,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        {
            let mut insert = tx.prepare(&format!(
                "INSERT OR REPLACE INTO dirs ({DIR_COLUMNS}) VALUES ({})",
                placeholders(14)
            ))?;
            for row in rows {
                insert.execute(params_from_iter(row.values()))?;
            }
            if let Some(stats) = stats {
                let mut ext = tx.prepare(
                    "INSERT INTO ext (scope, name, ext, bytes, files) VALUES (?1, ?2, ?3, ?4, ?5)",
                )?;
                let mut largest = tx.prepare(
                    "INSERT INTO largest (scope, name, path, bytes, mtime) VALUES (?1, ?2, ?3, ?4, ?5)",
                )?;
                for ((scope, name), s) in stats.scopes.lock().unwrap().iter() {
                    for (e, count) in &s.extensions {
                        ext.execute(params![
                            scope,
                            name,
                            e,
                            to_db(count.bytes),
                            to_db(count.files)
                        ])?;
                    }
                    for std::cmp::Reverse((bytes, path, mtime)) in &s.largest {
                        largest.execute(params![
                            scope,
                            name,
                            path_bytes(path),
                            to_db(*bytes),
                            mtime
                        ])?;
                    }
                }
            }
            let counters = [
                ("dirs", &progress.dirs),
                ("files", &progress.files),
                ("bytes", &progress.bytes),
                ("reused", &progress.reused),
            ];
            for (key, counter) in counters {
                tx.execute(
                    "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                    params![key, counter.load(Ordering::Relaxed).to_string()],
                )?;
            }
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('finished_at', ?1)",
                params![finished_at.to_string()],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('status', 'complete')",
                [],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

impl UnitStore for ResultDb {
    fn done(&self, path: &Path) -> Option<Walked> {
        self.done.get(path).cloned()
    }

    fn commit(&self, unit: &Walked, rows: Vec<DirRow>) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        {
            let mut insert = tx.prepare_cached(&format!(
                "INSERT OR REPLACE INTO dirs ({DIR_COLUMNS}) VALUES ({})",
                placeholders(14)
            ))?;
            for row in &rows {
                insert.execute(params_from_iter(row.values()))?;
            }
            let mut values = unit.row.values();
            values.push(Value::Integer(i64::from(unit.has_rows_below)));
            tx.prepare_cached(&format!(
                "INSERT OR REPLACE INTO unit_roots ({DIR_COLUMNS}, has_rows_below) VALUES ({})",
                placeholders(15)
            ))?
            .execute(params_from_iter(values))?;
        }
        tx.commit()?;
        Ok(())
    }
}

pub(super) fn placeholders(n: usize) -> String {
    (1..=n)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn to_db(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// A result file found in the inbox.
#[derive(Debug, Clone)]
pub struct ResultInfo {
    pub path: PathBuf,
    pub meta: Meta,
}

impl ResultInfo {
    /// Whether a scan is writing this result right now. A result left
    /// unfinished by a scan that stopped (or died) isn't.
    pub fn is_running(&self) -> bool {
        let Ok(file) = std::fs::File::open(lock_path(&self.path)) else {
            return false; // never locked, or already imported
        };
        matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock))
    }

    /// Unfinished, and no scan is working on it: `stomp scan --resume` can continue it.
    pub fn is_interrupted(&self) -> bool {
        self.meta.status != "complete" && !self.is_running()
    }
}

/// The file a running scan keeps locked beside its result file. flock works
/// across CephFS clients, so this holds wherever the scan runs, and the lock
/// goes when the process does, however it ends.
pub fn lock_path(result: &Path) -> PathBuf {
    result.with_extension("lock")
}

/// Held for as long as a scan writes a result file.
pub struct RunLock(#[allow(dead_code)] std::fs::File);

/// Takes the run lock on a result file; `None` if another scan holds it.
pub fn lock_result(result: &Path) -> Result<Option<RunLock>> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(lock_path(result))
        .context("cannot create the scan's lock file")?;
    match file.try_lock() {
        Ok(()) => Ok(Some(RunLock(file))),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// Result files in the inbox, oldest first. Unreadable files are skipped.
pub fn list_results(inbox: &Path) -> Result<Vec<ResultInfo>> {
    let entries = match std::fs::read_dir(inbox) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut found = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "db") {
            continue;
        }
        let Ok(conn) = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
            continue;
        };
        let _ = conn.busy_timeout(Duration::from_secs(10));
        if let Ok(meta) = read_meta(&conn) {
            found.push(ResultInfo { path, meta });
        }
    }
    found.sort_by_key(|r| r.meta.started_at);
    Ok(found)
}
