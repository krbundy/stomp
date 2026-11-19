//! Scanning (§9 of the spec): a detailed walk that records per-directory
//! aggregates, used for age profiles, change tracking and archive candidates.

pub mod hist;
mod result;
pub mod rules;
pub mod walk;

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use rusqlite::types::Value;
use rusqlite::{Connection, params};
use serde::Serialize;

pub use result::{Meta, ResultInfo, list_results, lock_path};

use crate::catalog;
use crate::config::Config;
use crate::fs;
use crate::paths;
use hist::Agg;
use result::{ResultDb, to_db};
use rules::Rules;
use walk::{FileStats, PrevRows, Progress, Stop, Walker};

/// Depth below each root at which directories become resumable work units.
const UNIT_DEPTH: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Root,
    Grant,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Root => "root",
            Role::Grant => "grant",
        }
    }

    fn parse(s: &str) -> Option<Role> {
        match s {
            "root" => Some(Role::Root),
            "grant" => Some(Role::Grant),
            _ => None,
        }
    }
}

/// Why a directory was counted but not scanned (§9.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ExcludeKind {
    Environment,
    Cache,
    User,
    Stomp,
}

impl ExcludeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Environment => "environment",
            Self::Cache => "cache",
            Self::User => "user",
            Self::Stomp => "stomp",
        }
    }

    pub fn parse(s: &str) -> Option<ExcludeKind> {
        match s {
            "environment" => Some(Self::Environment),
            "cache" => Some(Self::Cache),
            "user" => Some(Self::User),
            "stomp" => Some(Self::Stomp),
            _ => None,
        }
    }
}

/// One recorded directory.
#[derive(Debug, Clone, PartialEq)]
pub struct DirRow {
    pub path: PathBuf,
    pub role: Option<Role>,
    pub excluded: Option<ExcludeKind>,
    /// Files directly in the directory, plus small subdirectories folded into it.
    pub direct_bytes: u64,
    pub direct_files: u64,
    /// The whole subtree.
    pub agg: Agg,
    /// Copied from the previous scan because nothing under it changed.
    pub reused: bool,
}

/// Column order shared by the catalog and result files.
pub const DIR_COLUMNS: &str = "path, role, excluded, direct_bytes, direct_files, bytes, files, \
     unreadable, newest_mtime, oldest_mtime, newest_change, sizes, months, reused";

impl DirRow {
    pub fn new(
        path: &Path,
        role: Option<Role>,
        excluded: Option<ExcludeKind>,
        direct_bytes: u64,
        direct_files: u64,
        agg: Agg,
    ) -> DirRow {
        DirRow {
            path: path.to_path_buf(),
            role,
            excluded,
            direct_bytes,
            direct_files,
            agg,
            reused: false,
        }
    }

    /// Values in [`DIR_COLUMNS`] order.
    pub fn values(&self) -> Vec<Value> {
        let text = |s: Option<&str>| s.map_or(Value::Null, |s| Value::Text(s.to_string()));
        let int = |n: Option<i64>| n.map_or(Value::Null, Value::Integer);
        vec![
            Value::Blob(path_bytes(&self.path)),
            text(self.role.map(Role::as_str)),
            text(self.excluded.map(ExcludeKind::as_str)),
            Value::Integer(to_db(self.direct_bytes)),
            Value::Integer(to_db(self.direct_files)),
            Value::Integer(to_db(self.agg.bytes)),
            Value::Integer(to_db(self.agg.files)),
            Value::Integer(to_db(self.agg.unreadable)),
            int(self.agg.newest_mtime),
            int(self.agg.oldest_mtime),
            int(self.agg.newest_change),
            Value::Blob(hist::encode_sizes(&self.agg.sizes)),
            Value::Blob(hist::encode_months(&self.agg.months)),
            Value::Integer(i64::from(self.reused)),
        ]
    }

    /// Reads a row selected with [`DIR_COLUMNS`] starting at column `start`.
    pub fn from_row(row: &rusqlite::Row, start: usize) -> rusqlite::Result<DirRow> {
        let count =
            |i: usize| -> rusqlite::Result<u64> { Ok(row.get::<_, i64>(start + i)?.max(0) as u64) };
        let role: Option<String> = row.get(start + 1)?;
        let excluded: Option<String> = row.get(start + 2)?;
        let sizes: Option<Vec<u8>> = row.get(start + 11)?;
        let months: Option<Vec<u8>> = row.get(start + 12)?;
        Ok(DirRow {
            path: path_from_bytes(row.get(start)?),
            role: role.as_deref().and_then(Role::parse),
            excluded: excluded.as_deref().and_then(ExcludeKind::parse),
            direct_bytes: count(3)?,
            direct_files: count(4)?,
            agg: Agg {
                bytes: count(5)?,
                files: count(6)?,
                unreadable: count(7)?,
                newest_mtime: row.get(start + 8)?,
                oldest_mtime: row.get(start + 9)?,
                newest_change: row.get(start + 10)?,
                sizes: sizes.map(|b| hist::decode_sizes(&b)).unwrap_or_default(),
                months: months.map(|b| hist::decode_months(&b)).unwrap_or_default(),
            },
            reused: row.get::<_, i64>(start + 13)? != 0,
        })
    }
}

pub fn path_bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}

pub fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(&bytes))
}

/// BLOB bounds selecting every path strictly below `path`: `path/` up to `path0`.
pub fn below_bounds(path: &Path) -> (Vec<u8>, Vec<u8>) {
    let mut low = path_bytes(path);
    let mut high = low.clone();
    low.push(b'/');
    high.push(b'0'); // the byte after '/'
    (low, high)
}

/// Where scan results wait to be imported.
pub fn inbox_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("inbox").join("scans")
}

/// The previous scan's rows, read from the catalog without writing to it.
struct PrevScan {
    conn: Mutex<Connection>,
    scan_id: i64,
}

impl PrevRows for PrevScan {
    fn get(&self, path: &Path) -> Option<DirRow> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached(&format!(
                "SELECT {DIR_COLUMNS} FROM dir_stats WHERE scan_id = ?1 AND path = ?2"
            ))
            .ok()?;
        stmt.query_row(params![self.scan_id, path_bytes(path)], |r| {
            DirRow::from_row(r, 0)
        })
        .ok()
    }
}

pub struct Request {
    pub full: bool,
    pub resume: bool,
    pub task_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub uuid: String,
    pub full: bool,
    pub resumed: bool,
    pub dirs: u64,
    pub files: u64,
    pub bytes: u64,
    pub reused: u64,
    pub seconds: u64,
}

pub enum Outcome {
    Complete(Summary),
    /// Stopped early; finished units are saved and `stomp scan --resume` continues.
    Incomplete(Summary),
}

/// Runs a scan, writing a result file to the inbox.
pub fn run(
    cfg: &Config,
    data_dir: &Path,
    req: &Request,
    stop: &AtomicBool,
    progress: &Progress,
) -> Result<Outcome> {
    let inbox = inbox_dir(data_dir);
    paths::create_private_dir(&inbox)?;
    // `--resume` continues the newest interrupted scan, whichever task ran it; a
    // task resubmitted after its time limit continues its own.
    let resumable = if req.resume || req.task_id.is_some() {
        list_results(&inbox)?
            .into_iter()
            .filter(|r| r.meta.status != "complete")
            .rfind(|r| req.resume || r.meta.task_id == req.task_id)
    } else {
        None
    };
    // Held while this scan writes its result, so nothing resumes it twice
    // and `stomp tasks` can tell it's running, not interrupted.
    let _lock;
    let resumed = resumable.is_some();
    let (db, meta) = match resumable {
        Some(found) => {
            _lock = result::lock_result(&found.path)?.with_context(|| {
                format!(
                    "the scan started {} is still running{}; let it finish, or cancel it first",
                    crate::cmd::local_time(
                        jiff::Timestamp::from_second(found.meta.started_at).unwrap_or_default()
                    ),
                    found
                        .meta
                        .task_id
                        .as_deref()
                        .map(|t| format!(" (task {t}; `stomp tasks cancel {t}`)"))
                        .unwrap_or_default()
                )
            })?;
            let (db, mut meta) = ResultDb::open(&found.path)?;
            // File statistics from before the interruption were held in memory and lost.
            meta.file_stats = false;
            db.set_meta("file_stats", "0")?;
            db.set_status("running")?;
            (db, meta)
        }
        None => {
            let meta = new_meta(cfg, data_dir, req)?;
            let path = inbox.join(format!("{}.db", meta.uuid));
            _lock = result::lock_result(&path)?.context("a new scan's result file is locked")?;
            let db = ResultDb::create(&path, &meta)?;
            (db, meta)
        }
    };

    let prev = match (&meta.prev_uuid, meta.full) {
        (Some(uuid), false) => open_prev(data_dir, uuid)?,
        _ => None,
    };
    let threshold = meta
        .prev_started_at
        .map(|t| t - cfg.scan.rctime_margin.as_secs() as i64);
    let rules = Rules::new(cfg, data_dir)?;
    let grant_dirs: HashMap<PathBuf, String> = cfg
        .grants
        .iter()
        .flat_map(|g| {
            g.dirs
                .iter()
                .filter(|d| d.exists)
                .map(move |d| (d.path.clone(), g.id.clone()))
        })
        .collect();
    let file_stats = meta.file_stats.then(FileStats::default);
    let started = Instant::now();
    let summary = || Summary {
        uuid: meta.uuid.clone(),
        full: meta.full,
        resumed,
        dirs: progress.dirs.load(Ordering::Relaxed),
        files: progress.files.load(Ordering::Relaxed),
        bytes: progress.bytes.load(Ordering::Relaxed),
        reused: progress.reused.load(Ordering::Relaxed),
        seconds: started.elapsed().as_secs(),
    };

    let mut top_rows = Vec::new();
    for root in &cfg.roots {
        let backend = fs::detect(root)?;
        let reuse = match (&prev, threshold) {
            (Some(p), Some(t)) if backend.is_instant() => Some((p as &dyn PrevRows, t)),
            _ => None,
        };
        let walker = Walker {
            backend: backend.as_ref(),
            rules: &rules,
            grant_dirs: &grant_dirs,
            prev: reuse,
            fold_bytes: cfg.scan.fold_bytes,
            fold_files: cfg.scan.fold_files,
            unit_depth: UNIT_DEPTH,
            units: &db,
            stop,
            progress,
            file_stats: file_stats.as_ref(),
            root_name: root.to_string_lossy().into_owned(),
        };
        match walker.walk_root(root) {
            Ok((root_row, rows)) => {
                top_rows.extend(rows);
                top_rows.push(root_row);
            }
            Err(Stop::Interrupted) => {
                db.set_status("incomplete")?;
                return Ok(Outcome::Incomplete(summary()));
            }
            Err(Stop::Failed(e)) => {
                db.set_status("incomplete")?;
                return Err(e);
            }
        }
    }
    db.finish(
        &top_rows,
        file_stats.as_ref(),
        progress,
        jiff::Timestamp::now().as_second(),
    )?;
    Ok(Outcome::Complete(summary()))
}

fn new_uuid() -> String {
    let now = jiff::Timestamp::now();
    let noise = (now.subsec_nanosecond() as u64) ^ (u64::from(std::process::id()) << 32);
    format!(
        "{}-{:08x}",
        now.strftime("%Y%m%dT%H%M%S"),
        noise as u32 ^ (noise >> 32) as u32
    )
}

/// Decides whether a new scan is full or incremental, and which scan it builds on.
fn new_meta(cfg: &Config, data_dir: &Path, req: &Request) -> Result<Meta> {
    let now = jiff::Timestamp::now().as_second();
    let (prev, last_full) = match catalog::open_read_only(data_dir)? {
        Some(conn) => (
            catalog::latest_scan(&conn, true)?,
            catalog::last_full_scan(&conn)?,
        ),
        None => (None, None),
    };
    let full_due = last_full.is_none_or(|t| now - t >= cfg.scan.full_every.as_secs() as i64);
    // Only CephFS can skip unchanged subtrees; elsewhere every scan walks everything.
    let any_ceph = cfg
        .roots
        .iter()
        .any(|r| fs::detect(r).is_ok_and(|b| b.is_instant()));
    let full = req.full || prev.is_none() || full_due || !any_ceph;
    Ok(Meta {
        uuid: new_uuid(),
        status: "running".to_string(),
        started_at: now,
        full,
        file_stats: full,
        prev_uuid: if full {
            None
        } else {
            prev.as_ref().map(|p| p.uuid.clone())
        },
        prev_started_at: if full {
            None
        } else {
            prev.as_ref().map(|p| p.started_at)
        },
        task_id: req.task_id.clone(),
        host: crate::sys::hostname(),
    })
}

fn open_prev(data_dir: &Path, uuid: &str) -> Result<Option<PrevScan>> {
    let Some(conn) = catalog::open_read_only(data_dir)? else {
        return Ok(None);
    };
    let Some(scan_id) = catalog::scan_id(&conn, uuid)? else {
        return Ok(None);
    };
    Ok(Some(PrevScan {
        conn: Mutex::new(conn),
        scan_id,
    }))
}

#[cfg(test)]
mod tests;
