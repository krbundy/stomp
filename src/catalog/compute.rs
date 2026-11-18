//! Job tags and imported Slurm accounting records (§15).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::params;

use super::{Catalog, to_db};
use crate::compute::Job;
use crate::jobs::JobLogRecord;

/// Which grant a job belongs to, as logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobTag {
    pub job_id: String,
    pub is_array: bool,
    pub grant: String,
    /// `job-script`, `manual` or `task`.
    pub source: String,
    pub note: Option<String>,
    pub logged_at: String,
    pub host: Option<String>,
}

impl From<JobLogRecord> for JobTag {
    fn from(r: JobLogRecord) -> Self {
        JobTag {
            job_id: r.job_id,
            is_array: r.array,
            grant: r.grant,
            source: r.source,
            note: r.note,
            logged_at: r.logged_at,
            host: Some(r.host),
        }
    }
}

const JOB_COLUMNS: &str = "job_id, parent_id, name, state, submit_time, start_time, end_time, \
     elapsed_s, cpu_s, gpus, billing, partition, account, workdir";

fn job_from_row(r: &rusqlite::Row) -> rusqlite::Result<Job> {
    let count = |i: usize| -> rusqlite::Result<u64> { Ok(r.get::<_, i64>(i)?.max(0) as u64) };
    Ok(Job {
        job_id: r.get(0)?,
        parent_id: r.get(1)?,
        name: r.get(2)?,
        state: r.get(3)?,
        submit_time: r.get(4)?,
        start_time: r.get(5)?,
        end_time: r.get(6)?,
        elapsed_s: count(7)?,
        cpu_s: count(8)?,
        gpus: count(9)?,
        billing: r.get::<_, Option<i64>>(10)?.map(|b| b.max(0) as u64),
        partition: r.get(11)?,
        account: r.get(12)?,
        workdir: r.get(13)?,
    })
}

impl Catalog {
    /// Imports `stomp jobs log` records. Records that can't be parsed are
    /// renamed to `*.rejected` and reported.
    pub(super) fn ingest_job_logs(&mut self, inbox: &Path) -> Result<Vec<String>> {
        let entries = match std::fs::read_dir(inbox) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|x| x == "json")
                    && !p
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .starts_with('.')
            })
            .collect();
        files.sort();
        let mut messages = Vec::new();
        for path in files {
            let parsed = std::fs::read_to_string(&path)
                .map_err(anyhow::Error::from)
                .and_then(|text| Ok(serde_json::from_str::<JobLogRecord>(&text)?));
            match parsed {
                Ok(record) => {
                    self.put_tag(&JobTag::from(record))?;
                    std::fs::remove_file(&path)?;
                }
                Err(e) => {
                    let aside = path.with_extension("rejected");
                    let _ = std::fs::rename(&path, &aside);
                    messages.push(format!(
                        "could not import job record {}: {e:#}; moved it to {}",
                        path.display(),
                        aside.display()
                    ));
                }
            }
        }
        Ok(messages)
    }

    /// Stores a tag. A tag logged from inside a job never replaces one set by
    /// hand or by a stomp task. Returns whether anything changed.
    pub fn put_tag(&mut self, tag: &JobTag) -> Result<bool> {
        let changed = self.conn.execute(
            "INSERT INTO job_tags (job_id, is_array, grant_id, source, note, logged_at, host)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (job_id) DO UPDATE SET
                 is_array = excluded.is_array, grant_id = excluded.grant_id,
                 source = excluded.source, note = excluded.note,
                 logged_at = excluded.logged_at, host = excluded.host
             WHERE excluded.source != 'job-script' OR job_tags.source = 'job-script'",
            params![
                tag.job_id,
                tag.is_array,
                tag.grant,
                tag.source,
                tag.note,
                tag.logged_at,
                tag.host
            ],
        )?;
        Ok(changed > 0)
    }

    /// Removes tags; returns how many existed.
    pub fn remove_tags(&mut self, job_ids: &[String]) -> Result<usize> {
        let mut removed = 0;
        for id in job_ids {
            removed += self
                .conn
                .execute("DELETE FROM job_tags WHERE job_id = ?1", [id])?;
        }
        Ok(removed)
    }

    /// All tags, keyed by job ID.
    pub fn tags(&self) -> Result<HashMap<String, JobTag>> {
        let mut stmt = self.conn.prepare(
            "SELECT job_id, is_array, grant_id, source, note, logged_at, host FROM job_tags",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(JobTag {
                job_id: r.get(0)?,
                is_array: r.get(1)?,
                grant: r.get(2)?,
                source: r.get(3)?,
                note: r.get(4)?,
                logged_at: r.get(5)?,
                host: r.get(6)?,
            })
        })?;
        let mut tags = HashMap::new();
        for tag in rows {
            let tag = tag?;
            tags.insert(tag.job_id.clone(), tag);
        }
        Ok(tags)
    }

    /// Inserts or refreshes accounting records; returns how many were written.
    pub fn upsert_jobs(&mut self, jobs: &[Job], synced_at: i64) -> Result<usize> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(&format!(
                "INSERT OR REPLACE INTO compute_jobs ({JOB_COLUMNS}, synced_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)"
            ))?;
            for j in jobs {
                stmt.execute(params![
                    j.job_id,
                    j.parent_id,
                    j.name,
                    j.state,
                    j.submit_time,
                    j.start_time,
                    j.end_time,
                    to_db(j.elapsed_s),
                    to_db(j.cpu_s),
                    to_db(j.gpus),
                    j.billing.map(to_db),
                    j.partition,
                    j.account,
                    j.workdir,
                    synced_at
                ])?;
            }
        }
        tx.commit()?;
        Ok(jobs.len())
    }

    /// Jobs that ended in `[from, to)`, plus jobs still running that started before `to`.
    pub fn jobs_in_range(&self, from: Option<i64>, to: Option<i64>) -> Result<Vec<Job>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {JOB_COLUMNS} FROM compute_jobs
             WHERE (end_time >= ?1 AND end_time < ?2)
                OR (end_time IS NULL AND coalesce(start_time, 0) < ?2)
             ORDER BY coalesce(end_time, start_time, submit_time), job_id"
        ))?;
        let rows = stmt.query_map(
            params![from.unwrap_or(i64::MIN), to.unwrap_or(i64::MAX)],
            job_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Start time of the oldest job not yet finished, so syncing refreshes it.
    pub fn oldest_unfinished_start(&self) -> Result<Option<i64>> {
        Ok(self.conn.query_row(
            "SELECT min(coalesce(start_time, submit_time)) FROM compute_jobs WHERE end_time IS NULL",
            [],
            |r| r.get(0),
        )?)
    }

    /// When the oldest imported job started (or was submitted).
    pub fn oldest_job_start(&self) -> Result<Option<i64>> {
        Ok(self.conn.query_row(
            "SELECT min(coalesce(start_time, submit_time)) FROM compute_jobs",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn job_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM compute_jobs", [], |r| r.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(job: &str, grant: &str, source: &str) -> JobTag {
        JobTag {
            job_id: job.into(),
            is_array: false,
            grant: grant.into(),
            source: source.into(),
            note: None,
            logged_at: "2026-11-17T00:00:00Z".into(),
            host: None,
        }
    }

    #[test]
    fn manual_tags_beat_job_script_tags() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = Catalog::open(tmp.path()).unwrap();
        assert!(c.put_tag(&tag("1", "A", "job-script")).unwrap());
        assert!(c.put_tag(&tag("1", "B", "manual")).unwrap());
        // A requeued job logging itself again must not undo the manual fix.
        assert!(!c.put_tag(&tag("1", "A", "job-script")).unwrap());
        assert_eq!(c.tags().unwrap()["1"].grant, "B");
        assert!(c.put_tag(&tag("1", "C", "manual")).unwrap());
        assert_eq!(c.tags().unwrap()["1"].grant, "C");
        assert_eq!(c.remove_tags(&["1".into(), "2".into()]).unwrap(), 1);
    }

    #[test]
    fn ingests_and_rejects_job_logs() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path();
        let env = |k: &str| (k == "SLURM_JOB_ID").then(|| "77".to_string());
        crate::jobs::log(data, "A", &[], None, Some(true), env).unwrap();
        let inbox = crate::jobs::inbox_dir(data);
        std::fs::write(inbox.join("88.json"), "not json").unwrap();

        let mut c = Catalog::open(data).unwrap();
        let messages = c.ingest(data).unwrap();
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(messages[0].contains("88.json"));
        assert_eq!(c.tags().unwrap()["77"].grant, "A");
        assert!(!inbox.join("77.json").exists());
        assert!(inbox.join("88.rejected").exists());
    }

    #[test]
    fn stores_and_selects_jobs() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = Catalog::open(tmp.path()).unwrap();
        let job = |id: &str, end: Option<i64>| Job {
            job_id: id.into(),
            parent_id: crate::compute::parent_id(id).into(),
            name: "x".into(),
            state: if end.is_some() {
                "COMPLETED"
            } else {
                "RUNNING"
            }
            .into(),
            submit_time: Some(50),
            start_time: Some(100),
            end_time: end,
            elapsed_s: 3600,
            cpu_s: 4 * 3600,
            gpus: 1,
            billing: None,
            partition: "p".into(),
            account: "a".into(),
            workdir: "/w".into(),
        };
        c.upsert_jobs(
            &[job("1", Some(200)), job("2_1", Some(900)), job("3", None)],
            1000,
        )
        .unwrap();
        let ids = |v: Vec<Job>| v.into_iter().map(|j| j.job_id).collect::<Vec<_>>();
        assert_eq!(
            ids(c.jobs_in_range(Some(0), Some(500)).unwrap()),
            ["3", "1"]
        );
        assert_eq!(ids(c.jobs_in_range(Some(500), None).unwrap()), ["3", "2_1"]);
        assert_eq!(c.oldest_unfinished_start().unwrap(), Some(100));
        // Re-importing replaces rather than duplicates.
        c.upsert_jobs(&[job("3", Some(950))], 2000).unwrap();
        assert_eq!(c.job_count().unwrap(), 3);
        assert_eq!(c.oldest_unfinished_start().unwrap(), None);
    }
}
