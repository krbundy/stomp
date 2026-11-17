//! `stomp doctor`: checks stomp's configuration and environment.

use std::fmt::Display;
use std::fs;
use std::path::Path;

use anyhow::Result;

use crate::catalog::Catalog;
use crate::config::{Config, Destination};
use crate::fs as sfs;
use crate::units::{format_bytes, format_count};
use crate::{cmd, compute, jobs, paths, scan, sys, tasks};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Info,
    Warn,
    Fail,
}

#[derive(Default)]
struct Report {
    failures: usize,
    warnings: usize,
}

impl Report {
    fn line(&mut self, status: Status, topic: &str, message: impl Display) {
        let tag = match status {
            Status::Pass => "ok  ",
            Status::Info => "info",
            Status::Warn => "warn",
            Status::Fail => "FAIL",
        };
        match status {
            Status::Fail => self.failures += 1,
            Status::Warn => self.warnings += 1,
            _ => {}
        }
        println!("{tag}  {topic:<12} {message}");
    }
}

/// Runs the checks and prints a line for each. Returns false if anything failed.
pub fn run(config_path: &Path, data_dir: &Path) -> Result<bool> {
    let mut r = Report::default();
    let config = check_config(&mut r, config_path);
    check_state(&mut r, data_dir);
    if let Some(cfg) = &config {
        check_roots(&mut r, cfg);
    }
    check_slurm(&mut r, config.as_ref());
    if let Some(cfg) = &config {
        check_destinations(&mut r, cfg);
    }

    println!();
    match (r.failures, r.warnings) {
        (0, 0) => println!("No problems found."),
        (0, w) => println!("No failures; {w} warning(s) to look at."),
        (f, w) => println!("{f} failure(s) and {w} warning(s)."),
    }
    Ok(r.failures == 0)
}

fn check_config(r: &mut Report, config_path: &Path) -> Option<Config> {
    let shown = paths::display(config_path);
    match Config::load(config_path) {
        Ok(cfg) => {
            r.line(
                Status::Pass,
                "config",
                format!(
                    "{shown}: {} grant(s), {} root(s), {} destination(s)",
                    cfg.grants.len(),
                    cfg.roots.len(),
                    cfg.destinations.len()
                ),
            );
            for w in &cfg.warnings {
                r.line(Status::Warn, "config", w);
            }
            Some(cfg)
        }
        Err(e) => {
            for problem in &e.problems {
                r.line(Status::Fail, "config", format!("{shown}: {problem}"));
            }
            None
        }
    }
}

fn check_state(r: &mut Report, data_dir: &Path) {
    let shown = paths::display(data_dir);
    match check_writable(data_dir) {
        Ok(()) => r.line(Status::Pass, "data dir", format!("{shown} is writable")),
        Err(e) => r.line(Status::Fail, "data dir", format!("{shown}: {e}")),
    }
    // Doctor looks without importing, so it can report what's waiting.
    match Catalog::open(data_dir) {
        Ok(catalog) => {
            let size = fs::metadata(catalog.path()).map_or(0, |m| m.len());
            let count = |n: Result<i64>| format_count(n.unwrap_or(0).max(0) as u64);
            r.line(
                Status::Pass,
                "catalog",
                format!(
                    "{}: {} measurement(s), {} scan(s), {} job record(s)",
                    format_bytes(size),
                    count(catalog.measurement_count()),
                    count(catalog.scan_count()),
                    count(catalog.job_count())
                ),
            );
            match compute::last_sync(&catalog) {
                Ok(Some(t)) => r.line(
                    Status::Info,
                    "accounting",
                    format!("last imported from sacct {}", cmd::age(Some(t))),
                ),
                Ok(None) => r.line(
                    Status::Info,
                    "accounting",
                    "not imported yet; `stomp jobs sync` does it",
                ),
                Err(_) => {}
            }
        }
        Err(e) => r.line(Status::Fail, "catalog", format!("{e:#}")),
    }
    let waiting = fs::read_dir(jobs::inbox_dir(data_dir)).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .count()
    });
    if waiting > 0 {
        r.line(
            Status::Info,
            "job logs",
            format!("{waiting} job record(s) waiting to be imported"),
        );
    }
    if let Ok(results) = scan::list_results(&scan::inbox_dir(data_dir)) {
        let (done, interrupted): (Vec<_>, Vec<_>) =
            results.iter().partition(|r| r.meta.status == "complete");
        if !done.is_empty() {
            r.line(
                Status::Info,
                "scans",
                format!("{} finished scan(s) waiting to be imported", done.len()),
            );
        }
        if !interrupted.is_empty() {
            r.line(
                Status::Warn,
                "scans",
                format!(
                    "{} interrupted scan(s); `stomp scan --resume` continues the latest",
                    interrupted.len()
                ),
            );
        }
    }
    if let Ok(all) = tasks::list(data_dir) {
        let active = all.iter().filter(|t| !t.status.is_final()).count();
        if active > 0 {
            r.line(
                Status::Info,
                "tasks",
                format!("{active} task(s) submitted or running; see `stomp tasks`"),
            );
        }
    }
}

fn check_writable(dir: &Path) -> std::io::Result<()> {
    paths::create_private_dir(dir)?;
    let probe = dir.join(format!(".doctor-{}", std::process::id()));
    fs::write(&probe, b"ok")?;
    fs::remove_file(&probe)
}

fn check_roots(r: &mut Report, cfg: &Config) {
    for root in &cfg.roots {
        let shown = paths::display(root);
        let backend = match sfs::detect(root) {
            Ok(b) => b,
            Err(e) => {
                r.line(Status::Fail, "filesystem", format!("{shown}: {e}"));
                continue;
            }
        };
        if !backend.is_instant() {
            r.line(
                Status::Info,
                "filesystem",
                format!("{shown} is not CephFS, so measuring it means walking every file"),
            );
            continue;
        }
        r.line(
            Status::Pass,
            "filesystem",
            format!("{shown} is CephFS, so usage is instant"),
        );
        match backend.quota(root) {
            Ok(Some(q)) => {
                let mut limits = Vec::new();
                if let Some(max) = q.max_bytes {
                    limits.push(format!(
                        "{} of {}",
                        format_bytes(q.used.bytes),
                        format_bytes(max)
                    ));
                }
                if let Some(max) = q.max_files {
                    limits.push(format!(
                        "{} of {} files",
                        format_count(q.used.files),
                        format_count(max)
                    ));
                }
                r.line(
                    Status::Pass,
                    "quota",
                    format!(
                        "{} used, set on {}",
                        limits.join(", "),
                        paths::display(&q.dir)
                    ),
                );
            }
            Ok(None) => r.line(
                Status::Info,
                "quota",
                format!("no CephFS quota on {shown} or the directories above it"),
            ),
            Err(e) => r.line(Status::Warn, "quota", format!("{shown}: {e}")),
        }
    }
}

fn check_slurm(r: &mut Report, cfg: Option<&Config>) {
    let missing: Vec<&str> = ["sbatch", "squeue", "sacct"]
        .into_iter()
        .filter(|tool| sys::which(tool).is_none())
        .collect();
    if missing.is_empty() {
        r.line(Status::Pass, "slurm", "sbatch, squeue and sacct found");
    } else {
        r.line(
            Status::Warn,
            "slurm",
            format!(
                "not found: {} (needed for tasks and compute reports)",
                missing.join(", ")
            ),
        );
    }
    if sys::which("scrontab").is_some() {
        r.line(
            Status::Pass,
            "slurm",
            "scrontab found; recurring work can use it if the site allows",
        );
    } else {
        r.line(
            Status::Info,
            "slurm",
            "scrontab not found; recurring work will use self-resubmitting tasks",
        );
    }
    if let Ok(job) = std::env::var("SLURM_JOB_ID") {
        r.line(Status::Info, "slurm", format!("running inside job {job}"));
    }
    let configured = cfg.map_or(8, |c| c.scan.max_threads);
    r.line(
        Status::Info,
        "threads",
        format!("{} worker thread(s)", sys::effective_threads(configured)),
    );
}

fn check_destinations(r: &mut Report, cfg: &Config) {
    if cfg
        .destinations
        .iter()
        .any(|d| matches!(d, Destination::Sftp(_)))
    {
        for tool in ["ssh", "sftp"] {
            if sys::which(tool).is_none() {
                r.line(
                    Status::Fail,
                    "openssh",
                    format!("{tool} not found; SFTP destinations need OpenSSH"),
                );
            }
        }
    }
    for d in &cfg.destinations {
        r.line(
            Status::Info,
            "destination",
            format!(
                "{} ({}): connection checks arrive with milestone M4",
                d.name(),
                d.kind()
            ),
        );
    }
}
