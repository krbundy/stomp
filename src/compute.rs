//! Compute attribution (§15): importing Slurm accounting with `sacct` and
//! attributing jobs to grants.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use serde::Serialize;

use crate::catalog::{Catalog, JobTag};
use crate::config::Config;
use crate::sys;

pub const SACCT_FIELDS: &str = "JobID,JobName,State,Submit,Start,End,ElapsedRaw,CPUTimeRAW,AllocTRES,Partition,Account,WorkDir";
const FIELD_COUNT: usize = 12;
/// The ASCII unit separator: it doesn't occur in job names or paths in practice.
const DELIMITER: &str = "\u{1f}";
const DAY: i64 = 86_400;
/// sacct is queried a month at a time to keep load off the accounting database (§15.2).
const WINDOW: i64 = 30 * DAY;
/// Each sync re-reads a few days, catching late accounting updates.
const OVERLAP: i64 = 3 * DAY;
const DEFAULT_HISTORY: i64 = 90 * DAY;
const SYNC_KEY: &str = "sacct_synced_until";

/// Report label for compute that couldn't be attributed.
pub const UNASSIGNED: &str = "(unassigned)";
/// Label for stomp's own tasks.
pub const STOMP: &str = "stomp";

/// One job allocation as recorded by Slurm accounting.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Job {
    /// As sacct prints it: `123`, `123_4` (array task) or `123+0` (het job component).
    pub job_id: String,
    /// `123` for all of the above.
    pub parent_id: String,
    pub name: String,
    /// The first word of sacct's state, e.g. `COMPLETED` or `CANCELLED`.
    pub state: String,
    pub submit_time: Option<i64>,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub elapsed_s: u64,
    pub cpu_s: u64,
    pub gpus: u64,
    pub billing: Option<u64>,
    pub partition: String,
    pub account: String,
    pub workdir: String,
}

impl Job {
    pub fn cpu_hours(&self) -> f64 {
        self.cpu_s as f64 / 3600.0
    }

    pub fn gpu_hours(&self) -> f64 {
        self.gpus as f64 * self.elapsed_s as f64 / 3600.0
    }

    pub fn billing_hours(&self) -> Option<f64> {
        self.billing
            .map(|b| b as f64 * self.elapsed_s as f64 / 3600.0)
    }

    pub fn finished(&self) -> bool {
        self.end_time.is_some()
    }
}

/// The ID that an array's tasks and a heterogeneous job's components share.
pub fn parent_id(job_id: &str) -> &str {
    job_id.split(['_', '+']).next().unwrap_or(job_id)
}

fn parse_time(text: &str, tz: &TimeZone) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() || matches!(text, "Unknown" | "None") {
        return None;
    }
    let civil: DateTime = text.parse().ok()?;
    Some(
        civil
            .to_zoned_datetime(tz.clone())
            .ok()?
            .timestamp()
            .as_second(),
    )
}

/// GPUs and billing units from an `AllocTRES` value such as
/// `billing=4,cpu=4,gres/gpu=1,gres/gpu:a100=1,mem=16G,node=1`.
fn parse_tres(text: &str) -> (u64, Option<u64>) {
    let (mut generic, mut typed, mut billing) = (None, 0u64, None);
    for item in text.split(',') {
        let Some((key, value)) = item.split_once('=') else {
            continue;
        };
        let Ok(n) = value.parse::<u64>() else {
            continue;
        };
        match key {
            "gres/gpu" => generic = Some(n),
            "billing" => billing = Some(n),
            k if k.starts_with("gres/gpu:") => typed += n,
            _ => {}
        }
    }
    // Slurm may list the generic count and per-type counts; don't count GPUs twice.
    (generic.unwrap_or(typed), billing)
}

/// Parses `sacct -P` output with [`SACCT_FIELDS`]. Pending jobs are skipped.
pub fn parse_sacct(output: &str, tz: &TimeZone) -> (Vec<Job>, Vec<String>) {
    let mut jobs = Vec::new();
    let mut warnings = Vec::new();
    for line in output.lines().filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = line.splitn(FIELD_COUNT, DELIMITER).collect();
        if f.len() != FIELD_COUNT {
            warnings.push(format!("skipped an unexpected sacct line: {line:?}"));
            continue;
        }
        let state = f[2]
            .split_whitespace()
            .next()
            .unwrap_or("UNKNOWN")
            .to_string();
        if state == "PENDING" {
            continue;
        }
        let (gpus, billing) = parse_tres(f[8]);
        jobs.push(Job {
            job_id: f[0].to_string(),
            parent_id: parent_id(f[0]).to_string(),
            name: f[1].to_string(),
            state,
            submit_time: parse_time(f[3], tz),
            start_time: parse_time(f[4], tz),
            end_time: parse_time(f[5], tz),
            elapsed_s: f[6].trim().parse().unwrap_or(0),
            cpu_s: f[7].trim().parse().unwrap_or(0),
            gpus,
            billing,
            partition: f[9].to_string(),
            account: f[10].to_string(),
            workdir: f[11].to_string(),
        });
    }
    (jobs, warnings)
}

#[derive(Debug, Serialize)]
pub struct SyncSummary {
    pub jobs: usize,
    pub from: i64,
    pub to: i64,
    pub warnings: Vec<String>,
}

/// Imports the user's jobs from `sacct` since the last sync (§15.2).
pub fn sync(catalog: &mut Catalog, cfg: &Config, now: i64) -> Result<SyncSummary> {
    if sys::which("sacct").is_none() {
        bail!("sacct was not found, so compute usage can't be imported here");
    }
    let tz = TimeZone::system();
    let mut start = match catalog
        .kv_get(SYNC_KEY)?
        .and_then(|v| v.parse::<i64>().ok())
    {
        Some(last) => last - OVERLAP,
        None => cfg
            .compute
            .history_start
            .and_then(|d| d.to_zoned_datetime(tz.clone()).ok())
            .map_or(now - DEFAULT_HISTORY, |z| z.timestamp().as_second()),
    };
    if let Some(oldest) = catalog.oldest_unfinished_start()? {
        start = start.min(oldest);
    }
    let user = sys::user_name();
    let mut summary = SyncSummary {
        jobs: 0,
        from: start,
        to: now,
        warnings: Vec::new(),
    };
    let mut from = start;
    while from < now {
        let to = (from + WINDOW).min(now);
        let output = run_sacct(user.as_deref(), from, to, &tz)?;
        let (jobs, warnings) = parse_sacct(&output, &tz);
        summary.warnings.extend(warnings);
        summary.jobs += catalog.upsert_jobs(&jobs, now)?;
        from = to;
    }
    catalog.kv_set(SYNC_KEY, &now.to_string())?;
    Ok(summary)
}

/// When accounting was last imported.
pub fn last_sync(catalog: &Catalog) -> Result<Option<i64>> {
    Ok(catalog.kv_get(SYNC_KEY)?.and_then(|v| v.parse().ok()))
}

fn run_sacct(user: Option<&str>, from: i64, to: i64, tz: &TimeZone) -> Result<String> {
    let format = |t: i64| -> Result<String> {
        Ok(Timestamp::from_second(t)?
            .to_zoned_datetime(tz.clone())
            .strftime("%Y-%m-%dT%H:%M:%S")
            .to_string())
    };
    let mut cmd = Command::new("sacct");
    cmd.args([
        "-X",
        "-P",
        "-n",
        "--delimiter",
        DELIMITER,
        "--format",
        SACCT_FIELDS,
    ])
    .args(["-S", &format(from)?, "-E", &format(to)?]);
    if let Some(user) = user {
        cmd.args(["-u", user]);
    }
    let output = cmd.output().context("cannot run sacct")?;
    if !output.status.success() {
        bail!(
            "sacct failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// How a job got its grant (§15.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum By {
    Log,
    Task,
    Workdir,
    None,
}

impl By {
    pub fn label(self) -> &'static str {
        match self {
            By::Log => "log",
            By::Task => "task",
            By::Workdir => "workdir",
            By::None => "—",
        }
    }
}

/// Attributes jobs to grants: explicit tags first, then the working directory.
pub struct Attributor<'a> {
    cfg: &'a Config,
    tags: HashMap<String, JobTag>,
    canonical: Mutex<HashMap<String, Option<PathBuf>>>,
}

impl<'a> Attributor<'a> {
    pub fn new(cfg: &'a Config, tags: HashMap<String, JobTag>) -> Self {
        Attributor {
            cfg,
            tags,
            canonical: Mutex::new(HashMap::new()),
        }
    }

    /// The grant label for a job and how it was decided. Tags naming a grant
    /// that isn't in the config are labelled `<id> (unknown)`.
    pub fn attribute(&self, job: &Job) -> (String, By) {
        if let Some(tag) = self
            .tags
            .get(&job.job_id)
            .or_else(|| self.tags.get(&job.parent_id))
        {
            let by = if tag.source == "task" {
                By::Task
            } else {
                By::Log
            };
            let known = tag.grant == STOMP || self.cfg.grant(&tag.grant).is_some();
            let label = if known {
                tag.grant.clone()
            } else {
                format!("{} (unknown)", tag.grant)
            };
            return (label, by);
        }
        if self.cfg.compute.infer_from_workdir
            && !job.workdir.is_empty()
            && let Some(grant) = self.grant_for_workdir(&job.workdir)
        {
            return (grant, By::Workdir);
        }
        (UNASSIGNED.to_string(), By::None)
    }

    fn grant_for_workdir(&self, workdir: &str) -> Option<String> {
        let path = Path::new(workdir);
        if let Some(g) = self.cfg.grant_for(path) {
            return Some(g.id.clone());
        }
        // Grant directories are canonical; the job may have run under a symlinked path.
        let canonical = self
            .canonical
            .lock()
            .unwrap()
            .entry(workdir.to_string())
            .or_insert_with(|| std::fs::canonicalize(path).ok())
            .clone()?;
        self.cfg.grant_for(&canonical).map(|g| g.id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(fields: [&str; 12]) -> String {
        fields.join(DELIMITER)
    }

    #[test]
    fn parses_sacct_output() {
        let tz = TimeZone::UTC;
        let output = [
            line([
                "101",
                "train",
                "COMPLETED",
                "2026-11-01T10:00:00",
                "2026-11-01T10:05:00",
                "2026-11-01T12:05:00",
                "7200",
                "57600",
                "billing=8,cpu=8,gres/gpu=2,gres/gpu:a100=2,mem=64G,node=1",
                "gpu",
                "lab",
                "/home/u/alpha/run",
            ]),
            line([
                "102_3",
                "sweep|with|pipes",
                "CANCELLED by 1000",
                "2026-11-02T00:00:00",
                "2026-11-02T00:00:10",
                "2026-11-02T00:10:10",
                "600",
                "600",
                "cpu=1,mem=4G",
                "std",
                "lab",
                "/home/u/beta",
            ]),
            line([
                "103",
                "queued",
                "PENDING",
                "2026-11-03T00:00:00",
                "Unknown",
                "Unknown",
                "0",
                "0",
                "",
                "std",
                "lab",
                "/x",
            ]),
            line([
                "104",
                "running",
                "RUNNING",
                "2026-11-03T00:00:00",
                "2026-11-03T00:01:00",
                "Unknown",
                "120",
                "480",
                "cpu=4,gres/gpu:v100=1",
                "gpu",
                "lab",
                "/x",
            ]),
            "garbage line".to_string(),
        ]
        .join("\n");
        let (jobs, warnings) = parse_sacct(&output, &tz);
        assert_eq!(warnings.len(), 1);
        assert_eq!(jobs.len(), 3);

        let a = &jobs[0];
        assert_eq!((a.gpus, a.billing), (2, Some(8)));
        assert_eq!(a.cpu_hours(), 16.0);
        assert_eq!(a.gpu_hours(), 4.0);
        assert_eq!(a.end_time.unwrap() - a.start_time.unwrap(), 7200);

        let b = &jobs[1];
        assert_eq!(b.parent_id, "102");
        assert_eq!(b.state, "CANCELLED");
        assert_eq!(b.name, "sweep|with|pipes");

        let c = &jobs[2];
        assert_eq!(c.gpus, 1);
        assert!(!c.finished());
    }

    #[test]
    fn attributes_by_tag_then_workdir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir_all(root.join("alpha/run")).unwrap();
        let text = format!(
            "general.roots = [\"{r}\"]\n[[grant]]\nid = \"A\"\ndirs = [\"{r}/alpha\"]\n[[grant]]\nid = \"B\"\n",
            r = root.display()
        );
        let cfg = Config::parse(&text, Path::new("t.toml")).unwrap();
        let tag = |job: &str, grant: &str, source: &str| {
            (
                job.to_string(),
                JobTag {
                    job_id: job.into(),
                    is_array: false,
                    grant: grant.into(),
                    source: source.into(),
                    note: None,
                    logged_at: String::new(),
                    host: None,
                },
            )
        };
        let tags = HashMap::from([
            tag("1", "B", "manual"),
            tag("2", "TYPO", "job-script"),
            tag("3", "stomp", "task"),
            tag("50", "B", "job-script"), // an array, matched through parent_id
        ]);
        let attributor = Attributor::new(&cfg, tags);
        let job = |id: &str, workdir: &str| Job {
            job_id: id.into(),
            parent_id: parent_id(id).into(),
            name: String::new(),
            state: "COMPLETED".into(),
            submit_time: None,
            start_time: None,
            end_time: None,
            elapsed_s: 0,
            cpu_s: 0,
            gpus: 0,
            billing: None,
            partition: String::new(),
            account: String::new(),
            workdir: workdir.into(),
        };
        let alpha = format!("{}/alpha/run", root.display());
        let check = |id: &str, workdir: &str, grant: &str, by: By| {
            assert_eq!(
                attributor.attribute(&job(id, workdir)),
                (grant.to_string(), by),
                "{id}"
            );
        };
        check("1", &alpha, "B", By::Log); // the tag wins over the directory
        check("2", "/", "TYPO (unknown)", By::Log);
        check("3", "/", "stomp", By::Task);
        check("50_7", "/", "B", By::Log);
        check("4", &alpha, "A", By::Workdir);
        check("5", "/elsewhere", UNASSIGNED, By::None);
    }
}
