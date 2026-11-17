//! `stomp schedule` (§14.4): one small daily Slurm job keeps everything current.
//!
//! Each run records a usage sample, imports job accounting, submits a scan
//! task once a week, re-evaluates the policies, and archives proposals from
//! `apply` policies whose wait has passed. The job is installed with
//! `scrontab` where the site allows it, otherwise it is a job that queues
//! its successor with `sbatch --begin` before doing anything else.

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan, ZonedDateTime as Zoned};
use serde::{Deserialize, Serialize};

use super::{local_time, open_catalog};
use crate::config::Config;
use crate::tasks::{self, Overrides, Status};
use crate::{compute, paths, sys};

/// Scheduled-run logs kept.
const LOGS_KEPT: usize = 30;
/// A weekly scan is due this long after the last one started (a little under
/// a week, so a run that starts late doesn't push it back a day).
const SCAN_EVERY: i64 = 7 * 86_400 - 12 * 3600;
/// Automatic archives started per run, so a new policy doesn't flood the queue.
const AUTO_APPLY_PER_RUN: usize = 3;
/// A run is overdue after this long without one.
const OVERDUE: i64 = 2 * 86_400 + 6 * 3600;

const BEGIN_MARK: &str =
    "# BEGIN stomp schedule (managed by `stomp schedule`; edits here are replaced)";
const END_MARK: &str = "# END stomp schedule";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Slurm's cron (`scrontab`), if the site enables it
    Scrontab,
    /// A job that queues the next one before it starts working
    Task,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// `None` while disabled.
    mode: Option<Mode>,
    /// Local time of day, `HH:MM`.
    at: Option<String>,
    enabled_at: Option<String>,
    /// Task mode: the queued Slurm job for the next run.
    next_job: Option<String>,
    last_run: Option<LastRun>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LastRun {
    started_at: String,
    finished_at: String,
    ok: bool,
    lines: Vec<String>,
}

fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("schedule")
}

fn logs_dir(data_dir: &Path) -> PathBuf {
    dir(data_dir).join("logs")
}

fn load(data_dir: &Path) -> State {
    std::fs::read_to_string(dir(data_dir).join("state.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save(data_dir: &Path, state: &State) -> Result<()> {
    paths::create_private_dir(&dir(data_dir))?;
    let path = dir(data_dir).join("state.json");
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(state)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Whether recurring runs are enabled.
pub(crate) fn enabled(data_dir: &Path) -> bool {
    load(data_dir).mode.is_some()
}

/// A problem with the schedule worth reporting, if any.
pub(crate) fn health(data_dir: &Path) -> Option<String> {
    let state = load(data_dir);
    state.mode?;
    let since = state
        .last_run
        .as_ref()
        .and_then(|r| r.finished_at.parse::<Timestamp>().ok())
        .or_else(|| state.enabled_at.as_ref()?.parse().ok())?;
    let now = Timestamp::now();
    if now.as_second() - since.as_second() > OVERDUE {
        return Some(format!(
            "no scheduled run since {}; see `stomp schedule status`",
            local_time(since)
        ));
    }
    match &state.last_run {
        Some(r) if !r.ok => {
            Some("the last scheduled run had problems; see `stomp schedule status`".into())
        }
        _ => None,
    }
}

/// Parses `HH:MM`.
fn parse_at(text: &str) -> Result<(i8, i8)> {
    let (h, m) = text
        .split_once(':')
        .with_context(|| format!("{text:?} isn't a time of day such as 03:15"))?;
    let (h, m): (i8, i8) = (h.parse()?, m.parse()?);
    if !(0..24).contains(&h) || !(0..60).contains(&m) {
        bail!("{text:?} isn't a time of day such as 03:15");
    }
    Ok((h, m))
}

/// A quiet time between 02:00 and 04:59, different for each user so that
/// colleagues' runs don't all start together.
fn default_at() -> String {
    let user = sys::user_name().unwrap_or_default();
    let h = blake3::hash(user.as_bytes());
    let minutes = 120 + u32::from(u16::from_le_bytes([h.as_bytes()[0], h.as_bytes()[1]])) % 180;
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// The next time of day `at` strictly after `now` plus a few minutes.
fn next_run(now: &Zoned, (h, m): (i8, i8)) -> Result<Zoned> {
    let today = now
        .date()
        .at(h, m, 0, 0)
        .to_zoned_datetime(now.time_zone().clone())?;
    let soonest = now.checked_add(5.minutes())?;
    Ok(if today > soonest {
        today
    } else {
        today.checked_add(1.day())?
    })
}

/// sbatch options for a scheduled run: small and short.
fn job_options(cfg: &Config, data_dir: &Path) -> Vec<String> {
    let mut out = vec![
        "--job-name=stomp-schedule".to_string(),
        format!("--output={}/run-%j.out", logs_dir(data_dir).display()),
        "--time=00:30:00".into(),
        "--cpus-per-task=1".into(),
        "--mem=2G".into(),
    ];
    if let Some(p) = &cfg.slurm.partition {
        out.push(format!("--partition={p}"));
    }
    if let Some(a) = &cfg.slurm.account {
        out.push(format!("--account={a}"));
    }
    out
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The command a scheduled run executes.
fn run_command(config_path: &Path, chain: bool) -> Result<String> {
    let exe = std::env::current_exe().context("cannot find the stomp executable")?;
    let config = std::fs::canonicalize(config_path).unwrap_or(config_path.to_path_buf());
    Ok(format!(
        "{} --config {} schedule run{}",
        sh_quote(&exe.to_string_lossy()),
        sh_quote(&config.to_string_lossy()),
        if chain { " --chain" } else { "" }
    ))
}

// ---- scrontab

/// The user's scrontab, or why it can't be used.
fn scrontab_read() -> std::result::Result<String, String> {
    if sys::which("scrontab").is_none() {
        return Err("scrontab isn't installed here".into());
    }
    let out = Command::new("scrontab")
        .arg("-l")
        .output()
        .map_err(|e| format!("cannot run scrontab: {e}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else if stderr.to_ascii_lowercase().contains("no crontab") {
        Ok(String::new())
    } else {
        Err(format!("scrontab isn't available: {}", stderr.trim()))
    }
}

fn scrontab_write(data_dir: &Path, text: &str) -> Result<()> {
    if text.trim().is_empty() {
        let out = Command::new("scrontab").arg("-r").output()?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() && !stderr.to_ascii_lowercase().contains("no crontab") {
            bail!("scrontab -r failed: {}", stderr.trim());
        }
        return Ok(());
    }
    paths::create_private_dir(&dir(data_dir))?;
    let file = dir(data_dir).join("scrontab.new");
    std::fs::write(&file, text)?;
    let out = Command::new("scrontab").arg(&file).output()?;
    let _ = std::fs::remove_file(&file);
    if !out.status.success() {
        bail!(
            "scrontab didn't accept the entry: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// The crontab text without stomp's block.
fn without_block(text: &str) -> String {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.trim() == BEGIN_MARK {
            inside = true;
        } else if line.trim() == END_MARK {
            inside = false;
        } else if !inside {
            out.push(line);
        }
    }
    let mut text = out.join("\n");
    while text.ends_with("\n\n") {
        text.pop();
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

fn has_block(text: &str) -> bool {
    text.lines().any(|l| l.trim() == BEGIN_MARK)
}

/// stomp's scrontab block: `#SCRON` option lines, then the cron line.
fn block(options: &[String], (h, m): (i8, i8), command: &str) -> String {
    let mut lines = vec![BEGIN_MARK.to_string()];
    lines.extend(options.iter().map(|o| format!("#SCRON {o}")));
    lines.push(format!("{m} {h} * * * {command}"));
    lines.push(END_MARK.to_string());
    lines.join("\n") + "\n"
}

// ---- task mode

fn sbatch_next(
    cfg: &Config,
    config_path: &Path,
    data_dir: &Path,
    at: (i8, i8),
) -> Result<(String, Zoned)> {
    paths::create_private_dir(&logs_dir(data_dir))?;
    let script = dir(data_dir).join("run.sh");
    let mut text = String::from("#!/bin/bash\n");
    for o in job_options(cfg, data_dir) {
        text += &format!("#SBATCH {o}\n");
    }
    text += "\n# Generated by `stomp schedule enable`; each run queues the next.\n";
    text += &format!("exec {}\n", run_command(config_path, true)?);
    let _ = std::fs::remove_file(&script);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&script)?
        .write_all(text.as_bytes())?;
    let when = next_run(&Zoned::now(), at)?;
    let out = Command::new("sbatch")
        .arg("--parsable")
        .arg(format!("--begin={}", when.strftime("%Y-%m-%dT%H:%M:%S")))
        .arg(&script)
        .output()
        .context("cannot run sbatch; is Slurm available here?")?;
    if !out.status.success() {
        bail!(
            "sbatch failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let job = String::from_utf8_lossy(&out.stdout)
        .trim()
        .split(';')
        .next()
        .unwrap_or_default()
        .to_string();
    if job.is_empty() {
        bail!("sbatch did not print a job ID");
    }
    Ok((job, when))
}

fn scancel(job: &str) {
    let _ = Command::new("scancel").arg(job).output();
}

// ---- commands

/// Stops recurring runs, however they were set up. Returns what was removed.
fn stop(data_dir: &Path, state: &mut State) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    if let Ok(text) = scrontab_read()
        && has_block(&text)
    {
        scrontab_write(data_dir, &without_block(&text))?;
        removed.push("the scrontab entry".to_string());
    }
    if let Some(job) = state.next_job.take() {
        scancel(&job);
        removed.push(format!("queued Slurm job {job}"));
    }
    state.mode = None;
    Ok(removed)
}

/// `stomp schedule enable`.
pub fn enable(
    cfg: &Config,
    config_path: &Path,
    data_dir: &Path,
    at: Option<&str>,
    mode: Option<Mode>,
) -> Result<()> {
    let at_text = match at {
        Some(t) => t.to_string(),
        None => default_at(),
    };
    let time = parse_at(&at_text)?;
    let at_text = format!("{:02}:{:02}", time.0, time.1);
    let mut state = load(data_dir);
    stop(data_dir, &mut state)?;
    let mode = match mode {
        Some(m) => m,
        None => match scrontab_read() {
            Ok(_) => Mode::Scrontab,
            Err(why) => {
                eprintln!("stomp: {why}; using a job that queues its successor instead.");
                Mode::Task
            }
        },
    };
    paths::create_private_dir(&logs_dir(data_dir))?;
    let how = match mode {
        Mode::Scrontab => {
            let current = scrontab_read().map_err(anyhow::Error::msg)?;
            let mut text = without_block(&current);
            if !text.is_empty() {
                text.push('\n');
            }
            text += &block(
                &job_options(cfg, data_dir),
                time,
                &run_command(config_path, false)?,
            );
            scrontab_write(data_dir, &text)?;
            "through scrontab".to_string()
        }
        Mode::Task => {
            let (job, when) = sbatch_next(cfg, config_path, data_dir, time)?;
            state.next_job = Some(job.clone());
            format!(
                "as a Slurm job that queues the next one; the first is job {job}, starting {}",
                when.strftime("%Y-%m-%d %H:%M")
            )
        }
    };
    state.mode = Some(mode);
    state.at = Some(at_text.clone());
    state.enabled_at = Some(Timestamp::now().to_string());
    save(data_dir, &state)?;
    println!("stomp will run daily at {at_text}, {how}.");
    println!(
        "Each run records usage, imports job accounting, starts a scan once a week, and updates archive proposals{}.",
        if cfg
            .policies
            .iter()
            .any(|p| p.action == crate::config::PolicyAction::Apply)
        {
            ", archiving those from \"apply\" policies once their wait has passed"
        } else {
            ""
        }
    );
    println!(
        "`stomp schedule status` shows how the runs went; logs are in {}.",
        paths::display(&logs_dir(data_dir))
    );
    Ok(())
}

/// `stomp schedule disable`.
pub fn disable(data_dir: &Path) -> Result<()> {
    let mut state = load(data_dir);
    let was = state.mode.is_some();
    let removed = stop(data_dir, &mut state)?;
    save(data_dir, &state)?;
    if !was && removed.is_empty() {
        println!("Recurring runs weren't enabled.");
    } else if removed.is_empty() {
        println!("Recurring runs are off.");
    } else {
        println!(
            "Recurring runs are off (removed {}).",
            removed.join(" and ")
        );
    }
    Ok(())
}

/// `stomp schedule status`.
pub fn status(data_dir: &Path) -> Result<ExitCode> {
    let state = load(data_dir);
    let mut ok = true;
    match (state.mode, &state.at) {
        (None, _) => println!("Recurring runs are off; `stomp schedule enable` turns them on."),
        (Some(Mode::Scrontab), at) => {
            println!(
                "Runs daily at {} through scrontab.",
                at.as_deref().unwrap_or("?")
            );
            match scrontab_read() {
                Ok(text) if has_block(&text) => {}
                Ok(_) => {
                    ok = false;
                    println!(
                        "  Problem: the scrontab entry is gone; run `stomp schedule enable` again."
                    );
                }
                Err(why) => {
                    ok = false;
                    println!("  Problem: {why}.");
                }
            }
        }
        (Some(Mode::Task), at) => {
            println!(
                "Runs daily at {} as a Slurm job that queues the next one.",
                at.as_deref().unwrap_or("?")
            );
            match &state.next_job {
                Some(job) => {
                    let states = tasks::queue_states(std::slice::from_ref(job));
                    match states.get(job) {
                        Some(s) => println!("  Next run: Slurm job {job} ({})", s.to_lowercase()),
                        None if sys::which("squeue").is_some() => {
                            ok = false;
                            println!(
                                "  Problem: the next run (job {job}) isn't queued; run `stomp schedule enable` again."
                            );
                        }
                        None => println!("  Next run: Slurm job {job}"),
                    }
                }
                None => {
                    ok = false;
                    println!(
                        "  Problem: no next run is queued; run `stomp schedule enable` again."
                    );
                }
            }
        }
    }
    match &state.last_run {
        None if state.mode.is_some() => println!("No run has finished yet."),
        None => {}
        Some(r) => {
            let when = r
                .started_at
                .parse::<Timestamp>()
                .map(local_time)
                .unwrap_or_else(|_| r.started_at.clone());
            println!(
                "\nLast run: {when}, {}",
                if r.ok { "ok" } else { "with problems" }
            );
            for line in &r.lines {
                println!("  {line}");
            }
        }
    }
    if let Some(problem) = health(data_dir)
        && problem.starts_with("no scheduled run")
    {
        ok = false;
        println!("\nProblem: {problem}");
    }
    if state.mode.is_some() {
        println!("\nLogs: {}", paths::display(&logs_dir(data_dir)));
    }
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Collects the outcome of each step of a run.
#[derive(Default)]
struct Log {
    lines: Vec<String>,
    ok: bool,
}

impl Log {
    fn line(&mut self, text: impl Into<String>) {
        let text = text.into();
        println!("{text}");
        self.lines.push(text);
    }

    fn problem(&mut self, text: impl Into<String>) {
        self.ok = false;
        self.line(format!("problem: {}", text.into()));
    }
}

/// Whether a scan task is queued or running, or stopped early and resumable.
fn scan_in_progress(data_dir: &Path) -> (bool, bool) {
    let now = Timestamp::now().as_second();
    let mut active = false;
    let mut incomplete = false;
    for t in tasks::list(data_dir).unwrap_or_default() {
        if t.kind != "scan" {
            continue;
        }
        let recent = t
            .updated_at
            .parse::<Timestamp>()
            .is_ok_and(|u| now - u.as_second() < 2 * 86_400);
        match t.status {
            Status::Submitted | Status::Running if recent => active = true,
            Status::Incomplete => incomplete = true,
            _ => {}
        }
    }
    (active, incomplete)
}

/// `stomp schedule run`: one scheduled run. With `chain`, queues the next first.
pub fn run(cfg: &Config, config_path: &Path, data_dir: &Path, chain: bool) -> Result<ExitCode> {
    let started = Timestamp::now();
    let mut log = Log {
        ok: true,
        ..Log::default()
    };
    println!(
        "stomp scheduled run, {}",
        started
            .to_zoned_datetime(TimeZone::system())
            .strftime("%Y-%m-%d %H:%M %Z")
    );
    let mut state = load(data_dir);
    if chain {
        let at = state
            .at
            .as_deref()
            .map(parse_at)
            .transpose()?
            .unwrap_or((3, 0));
        match sbatch_next(cfg, config_path, data_dir, at) {
            Ok((job, when)) => {
                state.next_job = Some(job.clone());
                save(data_dir, &state)?;
                log.line(format!(
                    "queued the next run as Slurm job {job} for {}",
                    when.strftime("%Y-%m-%d %H:%M")
                ));
            }
            Err(e) => log.problem(format!("couldn't queue the next run: {e:#}")),
        }
    }

    match open_catalog(data_dir) {
        Err(e) => log.problem(format!("couldn't open the catalog: {e:#}")),
        Ok(mut catalog) => {
            // Usage, if it can be measured without walking every file.
            match super::usage::detect_backends(cfg) {
                Ok(backends) if backends.iter().all(|b| b.is_instant()) => {
                    let m = crate::attribution::measure(cfg, &backends);
                    match catalog.record(&m, &sys::hostname()) {
                        Ok(_) => log.line("recorded a usage sample"),
                        Err(e) => log.problem(format!("couldn't record usage: {e:#}")),
                    }
                }
                Ok(_) => log.line(
                    "no usage sample: a root isn't on CephFS, so measuring means walking every file; `stomp usage` records one when you run it",
                ),
                Err(e) => log.problem(format!("couldn't measure usage: {e:#}")),
            }

            if sys::which("sacct").is_some() {
                match compute::sync(&mut catalog, cfg, Timestamp::now().as_second()) {
                    Ok(s) => log.line(format!("imported {} job record(s) from sacct", s.jobs)),
                    Err(e) => log.problem(format!("couldn't import job accounting: {e:#}")),
                }
            }

            let last_scan = catalog.latest_scan(false).ok().flatten();
            let (active, incomplete) = scan_in_progress(data_dir);
            let due = last_scan
                .as_ref()
                .is_none_or(|s| started.as_second() - s.started_at >= SCAN_EVERY);
            if active {
                log.line("a scan is already queued or running");
            } else if due || incomplete {
                let mut args = vec!["scan".to_string()];
                if incomplete {
                    args.push("--resume".into());
                }
                match tasks::submit(
                    cfg,
                    config_path,
                    data_dir,
                    "scan",
                    &args,
                    &Overrides::default(),
                ) {
                    Ok(t) => log.line(format!(
                        "submitted {} as task {} (Slurm job {})",
                        if incomplete {
                            "the unfinished scan"
                        } else {
                            "the weekly scan"
                        },
                        t.id,
                        t.slurm_jobs.first().map_or("?", String::as_str)
                    )),
                    Err(e) => log.problem(format!("couldn't submit the scan: {e:#}")),
                }
            }

            if !cfg.policies.is_empty() {
                match super::candidates::refresh(cfg, &mut catalog, data_dir) {
                    Ok(None) => log.line("archive proposals: waiting for the first scan"),
                    Ok(Some(e)) => {
                        log.line(format!(
                            "archive proposals: {} open ({} new, {} gone)",
                            e.sync.open, e.sync.new, e.sync.removed
                        ));
                        if let Err(err) =
                            super::candidates::estimate_missing(cfg, &catalog, &e.rows, usize::MAX)
                        {
                            log.problem(format!("couldn't estimate compression: {err:#}"));
                        }
                        match super::candidates::auto_apply(
                            cfg,
                            config_path,
                            data_dir,
                            &catalog,
                            AUTO_APPLY_PER_RUN,
                        ) {
                            Ok(lines) => lines.into_iter().for_each(|l| log.line(l)),
                            Err(err) => log.problem(format!("couldn't apply proposals: {err:#}")),
                        }
                    }
                    Err(e) => log.problem(format!("couldn't evaluate the policies: {e:#}")),
                }
            }
        }
    }

    prune_logs(data_dir);
    let mut state = load(data_dir);
    state.last_run = Some(LastRun {
        started_at: started.to_string(),
        finished_at: Timestamp::now().to_string(),
        ok: log.ok,
        lines: log.lines,
    });
    save(data_dir, &state)?;
    Ok(if log.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Keeps the newest few run logs.
fn prune_logs(data_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(logs_dir(data_dir)) else {
        return;
    };
    let mut logs: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    logs.sort();
    let excess = logs.len().saturating_sub(LOGS_KEPT);
    for (_, path) in logs.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_only_its_own_block() {
        let mine = block(&["--time=00:30:00".into()], (3, 7), "stomp schedule run");
        assert_eq!(
            mine,
            format!(
                "{BEGIN_MARK}\n#SCRON --time=00:30:00\n7 3 * * * stomp schedule run\n{END_MARK}\n"
            )
        );
        let theirs = "#SCRON -t 5\n0 * * * * other\n";
        let combined = format!("{theirs}\n{mine}");
        assert!(has_block(&combined));
        assert_eq!(without_block(&combined), theirs);
        assert_eq!(without_block(&mine), "");
        assert!(!has_block(theirs));
    }

    #[test]
    fn times_of_day() {
        assert_eq!(parse_at("03:15").unwrap(), (3, 15));
        assert!(parse_at("24:00").is_err());
        assert!(parse_at("3pm").is_err());
        let (h, m) = parse_at(&default_at()).unwrap();
        assert!((2..5).contains(&h) && (0..60).contains(&m));

        let tz = TimeZone::get("America/New_York").unwrap();
        let now = jiff::civil::date(2026, 11, 17)
            .at(2, 0, 0, 0)
            .to_zoned_datetime(tz)
            .unwrap();
        assert_eq!(
            next_run(&now, (3, 15))
                .unwrap()
                .strftime("%F %R")
                .to_string(),
            "2026-11-17 03:15"
        );
        assert_eq!(
            next_run(&now, (2, 3))
                .unwrap()
                .strftime("%F %R")
                .to_string(),
            "2026-11-18 02:03"
        );
        assert_eq!(
            next_run(&now, (1, 0))
                .unwrap()
                .strftime("%F %R")
                .to_string(),
            "2026-11-18 01:00"
        );
    }
}
