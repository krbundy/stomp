//! Compute-job attribution records (§15.1 of the spec).
//!
//! `stomp jobs log` must never break the job that runs it, so it only writes
//! one small file per job into the inbox: no catalog, no locks. The next
//! foreground command will ingest these files (milestone M2).

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::paths;

pub const RECORD_VERSION: u32 = 1;

/// One job-to-grant record, as stored in `inbox/jobs/<job-id>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobLogRecord {
    pub version: u32,
    pub job_id: String,
    /// True when `job_id` is an array job ID covering every task in the array.
    pub array: bool,
    pub grant: String,
    /// Whether the grant was in the config when logged; `None` if the config couldn't be read.
    pub known_grant: Option<bool>,
    pub note: Option<String>,
    /// `"job-script"` when logged from inside the job, `"manual"` when tagged with `--job`.
    pub source: String,
    /// RFC 3339 timestamp.
    pub logged_at: String,
    pub host: String,
    pub user: Option<String>,
}

/// A job to log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub job_id: String,
    pub array: bool,
    /// Tagged by hand with `--job`, rather than from inside the job.
    pub manual: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Logged(PathBuf),
    /// Another task of the same job array logged it first.
    AlreadyLogged(PathBuf),
}

/// Where job records wait to be ingested.
pub fn inbox_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("inbox").join("jobs")
}

/// Whether `id` looks like a Slurm job ID: `123`, `123_4` (array task) or `123+1` (het job).
pub fn valid_job_id(id: &str) -> bool {
    let mut parts = id.splitn(2, ['_', '+']);
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    parts.next().is_some_and(all_digits) && parts.next().is_none_or(all_digits)
}

/// The jobs to log: the explicit `--job` IDs, or else the current job from the Slurm environment.
pub fn targets(explicit: &[String], env: impl Fn(&str) -> Option<String>) -> Result<Vec<Target>> {
    if !explicit.is_empty() {
        return explicit
            .iter()
            .map(|id| {
                if valid_job_id(id) {
                    Ok(Target {
                        job_id: id.clone(),
                        array: false,
                        manual: true,
                    })
                } else {
                    bail!("{id:?} is not a Slurm job ID")
                }
            })
            .collect();
    }
    let present = |var: &str| env(var).filter(|v| !v.is_empty());
    // Every task of an array shares SLURM_ARRAY_JOB_ID, so the whole array is logged once.
    let (job_id, array) = if let Some(id) = present("SLURM_ARRAY_JOB_ID") {
        (id, true)
    } else if let Some(id) = present("SLURM_JOB_ID") {
        (id, false)
    } else {
        bail!(
            "not running inside a Slurm job (SLURM_JOB_ID is not set); use --job <ID> to tag a job that already ran"
        );
    };
    if !valid_job_id(&job_id) {
        bail!("the Slurm environment has an unexpected job ID {job_id:?}");
    }
    Ok(vec![Target {
        job_id,
        array,
        manual: false,
    }])
}

/// Writes a record to the inbox. Manual tags replace an existing record; a
/// record from inside a job never does, so only the first array task wins.
pub fn write_record(inbox: &Path, record: &JobLogRecord, replace: bool) -> io::Result<Outcome> {
    paths::create_private_dir(inbox)?;
    let final_path = inbox.join(format!("{}.json", record.job_id));
    if !replace && final_path.exists() {
        return Ok(Outcome::AlreadyLogged(final_path));
    }
    let tmp = inbox.join(format!(".{}.{}.tmp", record.job_id, std::process::id()));
    let written = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        serde_json::to_writer_pretty(&mut file, record)?;
        file.write_all(b"\n")?;
        file.sync_all()
    })();
    let result = written.and_then(|()| {
        if replace {
            fs::rename(&tmp, &final_path).map(|()| Outcome::Logged(final_path.clone()))
        } else {
            // A hard link fails if the name exists, so concurrent array tasks can't both win.
            match fs::hard_link(&tmp, &final_path) {
                Ok(()) => Ok(Outcome::Logged(final_path.clone())),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    Ok(Outcome::AlreadyLogged(final_path.clone()))
                }
                Err(e) => Err(e),
            }
        }
    });
    let _ = fs::remove_file(&tmp); // already gone after a rename
    result
}

/// Records that the current Slurm job is a stomp task working for `grant`
/// (`stomp` for overhead such as scans), so compute reports show it (§14.2).
/// Best effort: a failure here must not stop the task.
pub fn log_task(data_dir: &Path, task_id: &str, grant: &str) {
    let Some(job_id) = std::env::var("SLURM_JOB_ID")
        .ok()
        .filter(|j| valid_job_id(j))
    else {
        return;
    };
    let record = JobLogRecord {
        version: RECORD_VERSION,
        job_id,
        array: false,
        grant: grant.to_string(),
        known_grant: Some(true),
        note: Some(format!("stomp task {task_id}")),
        source: "task".to_string(),
        logged_at: jiff::Timestamp::now().to_string(),
        host: crate::sys::hostname(),
        user: crate::sys::user_name(),
    };
    let _ = write_record(&inbox_dir(data_dir), &record, true);
}

/// Logs the target jobs to `grant` and returns a message per job.
pub fn log(
    data_dir: &Path,
    grant: &str,
    explicit: &[String],
    note: Option<&str>,
    known_grant: Option<bool>,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Vec<String>> {
    if grant.trim().is_empty() {
        bail!("the grant ID must not be empty");
    }
    let inbox = inbox_dir(data_dir);
    let host = crate::sys::hostname();
    let user = crate::sys::user_name();
    let mut messages = Vec::new();
    for target in targets(explicit, &env)? {
        let record = JobLogRecord {
            version: RECORD_VERSION,
            job_id: target.job_id.clone(),
            array: target.array,
            grant: grant.to_string(),
            known_grant,
            note: note.map(str::to_string),
            source: if target.manual {
                "manual"
            } else {
                "job-script"
            }
            .to_string(),
            logged_at: jiff::Timestamp::now().to_string(),
            host: host.clone(),
            user: user.clone(),
        };
        let what = if target.array { "job array" } else { "job" };
        let outcome = write_record(&inbox, &record, target.manual)
            .with_context(|| format!("could not write to {}", inbox.display()))?;
        messages.push(match outcome {
            Outcome::Logged(_) => format!("logged {what} {} to {grant}", target.job_id),
            Outcome::AlreadyLogged(_) => {
                format!("{what} {} is already logged", target.job_id)
            }
        });
    }
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    fn read(path: &Path) -> JobLogRecord {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn job_id_format() {
        for ok in ["1", "123456", "123_4", "123+1"] {
            assert!(valid_job_id(ok), "{ok}");
        }
        for bad in ["", "abc", "12a", "123_", "_4", "1_2_3", "../x"] {
            assert!(!valid_job_id(bad), "{bad}");
        }
    }

    #[test]
    fn chooses_targets() {
        let t = targets(&[], env(&[("SLURM_JOB_ID", "77")])).unwrap();
        assert_eq!(
            t,
            [Target {
                job_id: "77".into(),
                array: false,
                manual: false
            }]
        );

        let t = targets(
            &[],
            env(&[("SLURM_JOB_ID", "78"), ("SLURM_ARRAY_JOB_ID", "70")]),
        )
        .unwrap();
        assert_eq!(
            t,
            [Target {
                job_id: "70".into(),
                array: true,
                manual: false
            }]
        );

        let t = targets(&["5".into(), "6_1".into()], env(&[("SLURM_JOB_ID", "77")])).unwrap();
        assert!(t.iter().all(|t| t.manual));
        assert_eq!(t.len(), 2);

        assert!(targets(&[], env(&[])).is_err());
        assert!(targets(&["nope".into()], env(&[])).is_err());
    }

    #[test]
    fn logs_from_inside_a_job() {
        let tmp = tempfile::tempdir().unwrap();
        let msgs = log(
            tmp.path(),
            "GRANT-A",
            &[],
            Some("sweep"),
            Some(true),
            env(&[("SLURM_JOB_ID", "42")]),
        )
        .unwrap();
        assert_eq!(msgs, ["logged job 42 to GRANT-A"]);
        let rec = read(&inbox_dir(tmp.path()).join("42.json"));
        assert_eq!(rec.grant, "GRANT-A");
        assert_eq!(rec.source, "job-script");
        assert_eq!(rec.note.as_deref(), Some("sweep"));
        assert_eq!(rec.known_grant, Some(true));
    }

    #[test]
    fn array_is_logged_once() {
        let tmp = tempfile::tempdir().unwrap();
        let task = |n: &str| env(&[("SLURM_JOB_ID", n), ("SLURM_ARRAY_JOB_ID", "900")]);
        let first = log(tmp.path(), "A", &[], None, None, task("901")).unwrap();
        let second = log(tmp.path(), "B", &[], None, None, task("902")).unwrap();
        assert_eq!(first, ["logged job array 900 to A"]);
        assert_eq!(second, ["job array 900 is already logged"]);
        assert_eq!(read(&inbox_dir(tmp.path()).join("900.json")).grant, "A");
        // No temporary files are left behind.
        assert_eq!(fs::read_dir(inbox_dir(tmp.path())).unwrap().count(), 1);
    }

    #[test]
    fn manual_tag_replaces_existing_record() {
        let tmp = tempfile::tempdir().unwrap();
        log(
            tmp.path(),
            "A",
            &[],
            None,
            None,
            env(&[("SLURM_JOB_ID", "10")]),
        )
        .unwrap();
        let msgs = log(tmp.path(), "B", &["10".into()], None, Some(false), env(&[])).unwrap();
        assert_eq!(msgs, ["logged job 10 to B"]);
        let rec = read(&inbox_dir(tmp.path()).join("10.json"));
        assert_eq!(rec.grant, "B");
        assert_eq!(rec.source, "manual");
        assert_eq!(rec.known_grant, Some(false));
    }

    #[test]
    fn rejects_empty_grant() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(log(tmp.path(), " ", &["1".into()], None, None, env(&[])).is_err());
    }
}
