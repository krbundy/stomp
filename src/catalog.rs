//! The catalog: stomp's SQLite database (§10 of the spec).
//!
//! Only the foreground CLI writes the catalog, while holding an exclusive lock
//! on `catalog.lock`. Slurm tasks and `stomp jobs log` write files to the inbox
//! instead (§10.2).

use std::fs::{File, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};

use crate::attribution::Measurement;
use crate::paths;

const SCHEMA_VERSION: i64 = 1;

const SCHEMA_V1: &str = "
BEGIN;
CREATE TABLE measurements (
    id        INTEGER PRIMARY KEY,
    taken_at  INTEGER NOT NULL,   -- Unix seconds
    host      TEXT
);
CREATE TABLE usage_samples (
    measurement_id  INTEGER NOT NULL REFERENCES measurements(id) ON DELETE CASCADE,
    kind            TEXT NOT NULL CHECK (kind IN ('root', 'unassigned', 'grant_dir')),
    path            TEXT NOT NULL,
    grant_id        TEXT,         -- the grant at the time; reports use the current config
    backend         TEXT NOT NULL,
    bytes           INTEGER NOT NULL,
    files           INTEGER NOT NULL,
    newest_change   INTEGER       -- Unix seconds
);
CREATE INDEX usage_samples_by_measurement ON usage_samples(measurement_id);
PRAGMA user_version = 1;
COMMIT;
";

pub struct Catalog {
    conn: Connection,
    path: PathBuf,
    _lock: File,
}

/// One recorded usage value.
#[derive(Debug, Clone)]
pub struct Sample {
    pub measurement_id: i64,
    pub taken_at: i64,
    pub kind: String,
    pub path: String,
    pub grant_id: Option<String>,
    pub bytes: u64,
    pub files: u64,
}

fn to_db(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

impl Catalog {
    /// Opens the catalog in `data_dir`, creating it if needed. The catalog lock is
    /// held until the returned value is dropped.
    pub fn open(data_dir: &Path) -> Result<Catalog> {
        paths::create_private_dir(data_dir)
            .with_context(|| format!("cannot create {}", data_dir.display()))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(data_dir.join("catalog.lock"))
            .context("cannot open the catalog lock file")?;
        lock.lock().context("cannot lock the catalog")?;

        let path = data_dir.join("catalog.db");
        let created = !path.exists();
        let conn =
            Connection::open(&path).with_context(|| format!("cannot open {}", path.display()))?;
        if created {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        conn.busy_timeout(Duration::from_secs(30))?;
        // WAL needs shared memory, which isn't reliable on network filesystems (§10.2).
        conn.pragma_update_and_check(None, "journal_mode", "DELETE", |row| {
            row.get::<_, String>(0)
        })?;
        conn.pragma_update(None, "foreign_keys", true)?;

        let catalog = Catalog {
            conn,
            path,
            _lock: lock,
        };
        catalog.migrate()?;
        Ok(catalog)
    }

    fn migrate(&self) -> Result<()> {
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            bail!(
                "{} was written by a newer version of stomp (schema {version}); upgrade stomp",
                self.path.display()
            );
        }
        if version < 1 {
            self.conn.execute_batch(SCHEMA_V1)?;
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Records a usage measurement and returns its ID.
    pub fn record(&mut self, m: &Measurement, host: &str) -> Result<i64> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO measurements (taken_at, host) VALUES (?1, ?2)",
            params![m.taken_at.as_second(), host],
        )?;
        let id = tx.last_insert_rowid();
        {
            let mut insert = tx.prepare(
                "INSERT INTO usage_samples
                    (measurement_id, kind, path, grant_id, backend, bytes, files, newest_change)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for root in &m.roots {
                let path = root.path.to_string_lossy();
                let backend = root.backend.as_str();
                for (kind, u) in [("root", &root.total), ("unassigned", &root.unassigned)] {
                    insert.execute(params![
                        id,
                        kind,
                        path,
                        None::<String>,
                        backend,
                        to_db(u.bytes),
                        to_db(u.files),
                        u.newest_change
                    ])?;
                }
            }
            for grant in &m.grants {
                for dir in &grant.dirs {
                    let Some(u) = &dir.usage else { continue };
                    insert.execute(params![
                        id,
                        "grant_dir",
                        dir.path.to_string_lossy(),
                        grant.id,
                        m.roots[dir.root].backend.as_str(),
                        to_db(u.bytes),
                        to_db(u.files),
                        u.newest_change
                    ])?;
                }
            }
        }
        tx.commit()?;
        Ok(id)
    }

    /// Samples from measurements taken in `[from, to)` (Unix seconds), oldest first.
    pub fn samples(&self, from: Option<i64>, to: Option<i64>) -> Result<Vec<Sample>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.taken_at, s.kind, s.path, s.grant_id, s.bytes, s.files
             FROM usage_samples s JOIN measurements m ON m.id = s.measurement_id
             WHERE m.taken_at >= ?1 AND m.taken_at < ?2
             ORDER BY m.taken_at, m.id",
        )?;
        let rows = stmt.query_map(
            params![from.unwrap_or(i64::MIN), to.unwrap_or(i64::MAX)],
            |row| {
                Ok(Sample {
                    measurement_id: row.get(0)?,
                    taken_at: row.get(1)?,
                    kind: row.get(2)?,
                    path: row.get(3)?,
                    grant_id: row.get(4)?,
                    bytes: row.get::<_, i64>(5)?.max(0) as u64,
                    files: row.get::<_, i64>(6)?.max(0) as u64,
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn measurement_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM measurements", [], |row| row.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribution::{DirUsage, GrantUsage, RootUsage};
    use crate::fs::{BackendKind, Usage};
    use jiff::Timestamp;

    fn usage(bytes: u64, files: u64) -> Usage {
        Usage {
            bytes,
            files,
            newest_change: Some(1_700_000_000),
            unreadable: 0,
        }
    }

    #[test]
    fn records_and_reads_back_measurements() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let mut catalog = Catalog::open(&data).unwrap();
        let m = Measurement {
            taken_at: Timestamp::from_second(1_700_000_100).unwrap(),
            roots: vec![RootUsage {
                path: "/home/u".into(),
                backend: BackendKind::Ceph,
                total: usage(300, 3),
                unassigned: usage(100, 1),
                quota: None,
            }],
            grants: vec![GrantUsage {
                id: "A".into(),
                total: usage(200, 2),
                dirs: vec![
                    DirUsage {
                        path: "/home/u/a".into(),
                        root: 0,
                        usage: Some(usage(200, 2)),
                    },
                    DirUsage {
                        path: "/home/u/missing".into(),
                        root: 0,
                        usage: None,
                    },
                ],
            }],
            warnings: vec![],
        };
        catalog.record(&m, "login1").unwrap();
        assert_eq!(catalog.measurement_count().unwrap(), 1);

        let samples = catalog.samples(None, None).unwrap();
        let kinds: Vec<&str> = samples.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(kinds, ["root", "unassigned", "grant_dir"]);
        assert_eq!(samples[2].grant_id.as_deref(), Some("A"));
        assert_eq!(samples[2].bytes, 200);

        assert!(
            catalog
                .samples(Some(1_700_000_101), None)
                .unwrap()
                .is_empty()
        );
        drop(catalog);

        // Reopening finds the existing schema and data.
        let catalog = Catalog::open(&data).unwrap();
        assert_eq!(catalog.measurement_count().unwrap(), 1);
        let mode = std::fs::metadata(catalog.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
