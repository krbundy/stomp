//! `stomp jobs`: attributing Slurm compute jobs to grants (§15).

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, bail};
use jiff::Timestamp;
use serde::Serialize;

use super::{Format, emit, local_time, open_catalog, parse_since, rfc3339, short_local, warn};
use crate::catalog::{Catalog, JobTag};
use crate::compute::{self, Attributor, UNASSIGNED};
use crate::config::Config;
use crate::table::{Align, Table};
use crate::units::format_hours;
use crate::{jobs, paths};

/// Accounting is re-imported automatically when older than this.
const AUTO_SYNC_AFTER: i64 = 15 * 60;

/// Opens the catalog, importing recent accounting first if it's stale and
/// `sync` is set. A failed import is a warning: older records still work.
/// Returns the catalog and when accounting was last imported.
pub(crate) fn synced_catalog(
    cfg: &Config,
    data_dir: &Path,
    sync: bool,
) -> Result<(Catalog, Option<i64>)> {
    let mut catalog = open_catalog(data_dir)?;
    let now = Timestamp::now().as_second();
    let last = compute::last_sync(&catalog)?;
    if sync && last.is_none_or(|t| now - t > AUTO_SYNC_AFTER) {
        match compute::sync(&mut catalog, cfg, now) {
            Ok(summary) => {
                for w in summary.warnings {
                    warn(w);
                }
                return Ok((catalog, Some(now)));
            }
            Err(e) => warn(format_args!(
                "could not import job accounting, so recent jobs may be missing: {e:#}"
            )),
        }
    }
    Ok((catalog, last))
}

/// `stomp jobs sync`: imports accounting now.
pub fn sync(cfg: &Config, data_dir: &Path) -> Result<()> {
    let mut catalog = open_catalog(data_dir)?;
    let now = Timestamp::now().as_second();
    let summary = compute::sync(&mut catalog, cfg, now)?;
    for w in &summary.warnings {
        warn(w);
    }
    println!(
        "Imported {} job record(s) from sacct, covering {} to now. {} job(s) in the catalog.",
        summary.jobs,
        local_time(Timestamp::from_second(summary.from)?),
        catalog.job_count()?
    );
    Ok(())
}

pub struct ListArgs {
    pub grant: Option<String>,
    pub since: Option<String>,
    pub unassigned: bool,
    pub no_sync: bool,
    pub format: Format,
}

#[derive(Serialize)]
struct JobRow {
    job_id: String,
    name: String,
    state: String,
    start: String,
    end: String,
    #[serde(serialize_with = "crate::units::round2")]
    cpu_hours: f64,
    #[serde(serialize_with = "crate::units::round2")]
    gpu_hours: f64,
    grant: String,
    attributed_by: compute::By,
    /// The `--note` given to `stomp jobs log`, if any.
    note: Option<String>,
}

/// `stomp jobs list`: recent jobs with their grant.
pub fn list(cfg: &Config, data_dir: &Path, args: &ListArgs) -> Result<()> {
    let since = parse_since(args.since.as_deref().unwrap_or("30d"))?;
    let (catalog, _) = synced_catalog(cfg, data_dir, !args.no_sync)?;
    let tags = catalog.tags()?;
    // Notes are stored with tags: on the job itself, or on its array.
    let note_of = |job: &compute::Job| -> Option<String> {
        tags.get(&job.job_id)
            .or_else(|| tags.get(&job.parent_id))
            .and_then(|t| t.note.clone())
            .filter(|n| !n.is_empty())
    };
    let attributor = Attributor::new(cfg, tags.clone());
    let rows: Vec<JobRow> = catalog
        .jobs_in_range(Some(since), None)?
        .into_iter()
        .filter_map(|job| {
            let (grant, by) = attributor.attribute(&job);
            let keep = match (&args.grant, args.unassigned) {
                (_, true) => grant == UNASSIGNED,
                (Some(g), _) => &grant == g,
                (None, false) => true,
            };
            keep.then(|| JobRow {
                note: note_of(&job),
                cpu_hours: job.cpu_hours(),
                gpu_hours: job.gpu_hours(),
                job_id: job.job_id,
                name: job.name,
                state: job.state,
                start: rfc3339(job.start_time),
                end: rfc3339(job.end_time),
                grant,
                attributed_by: by,
            })
        })
        .collect();
    if rows.is_empty() && matches!(args.format, Format::Table | Format::Md) {
        println!("No jobs match.");
        return Ok(());
    }
    let any_notes = rows.iter().any(|r| r.note.is_some());
    emit(args.format, &rows, || {
        let mut columns = vec![
            ("JOB", Align::Left),
            ("NAME", Align::Left),
            ("STATE", Align::Left),
            ("ENDED", Align::Left),
            ("CPU-H", Align::Right),
            ("GPU-H", Align::Right),
            ("GRANT", Align::Left),
            ("BY", Align::Left),
        ];
        if any_notes {
            columns.push(("NOTE", Align::Left));
        }
        let mut t = Table::new(&columns);
        for r in &rows {
            let ended = if r.end.is_empty() {
                "running".to_string()
            } else {
                short_local(&r.end)
            };
            let mut cells = vec![
                r.job_id.clone(),
                r.name.chars().take(24).collect(),
                r.state.clone(),
                ended,
                format_hours(r.cpu_hours),
                format_hours(r.gpu_hours),
                r.grant.clone(),
                r.attributed_by.label().to_string(),
            ];
            if any_notes {
                cells.push(r.note.as_deref().unwrap_or("").chars().take(40).collect());
            }
            t.row(cells);
        }
        t
    })
}

/// `stomp jobs retag`: assigns jobs to a grant by hand, overriding other sources.
pub fn retag(cfg: &Config, data_dir: &Path, grant: &str, job_ids: &[String]) -> Result<()> {
    if cfg.grant(grant).is_none() {
        bail!("no grant {grant:?} in {}", paths::display(&cfg.source));
    }
    for id in job_ids {
        if !jobs::valid_job_id(id) {
            bail!("{id:?} is not a Slurm job ID");
        }
    }
    let mut catalog = open_catalog(data_dir)?;
    for id in job_ids {
        catalog.put_tag(&JobTag {
            job_id: id.clone(),
            is_array: false,
            grant: grant.to_string(),
            source: "manual".to_string(),
            note: None,
            logged_at: Timestamp::now().to_string(),
            host: Some(crate::sys::hostname()),
        })?;
    }
    println!("Tagged {} job(s) as {grant}.", job_ids.len());
    Ok(())
}

/// `stomp jobs untag`: removes explicit tags, so jobs fall back to working-directory inference.
pub fn untag(data_dir: &Path, job_ids: &[String]) -> Result<()> {
    let mut catalog = open_catalog(data_dir)?;
    let removed = catalog.remove_tags(job_ids)?;
    println!(
        "Removed {removed} tag(s). Those jobs are now attributed by their working directory, if it's in a grant."
    );
    Ok(())
}

/// Runs `stomp jobs log`. Never fails the calling job: problems are printed as
/// warnings and the exit code is 0, unless `strict` is set.
pub fn log(
    config_path: &Path,
    data_dir: &Path,
    grant: &str,
    explicit: &[String],
    note: Option<&str>,
    strict: bool,
) -> ExitCode {
    // With --strict, problems are errors: said as such, and an unknown grant
    // isn't recorded. Without it, the job is always logged if it can be.
    let label = if strict { "error" } else { "warning" };
    let mut problems = Vec::new();
    let known_grant = match Config::load(config_path) {
        Ok(cfg) => {
            let known = cfg.grant(grant).is_some();
            if !known {
                problems.push(if strict {
                    format!(
                        "grant {grant:?} is not in {}; the job was not logged",
                        paths::display(config_path)
                    )
                } else {
                    format!(
                        "grant {grant:?} is not in {}; the job is logged anyway and will show as an unknown grant until you fix it",
                        paths::display(config_path)
                    )
                });
            }
            Some(known)
        }
        Err(e) => {
            problems.push(format!(
                "could not read the config, so the grant ID wasn't checked: {e}"
            ));
            None
        }
    };

    if !(strict && known_grant == Some(false)) {
        match jobs::log(data_dir, grant, explicit, note, known_grant, |k| {
            std::env::var(k).ok()
        }) {
            Ok(messages) => {
                for m in messages {
                    eprintln!("stomp: {m}");
                }
            }
            Err(e) => problems.push(format!("job not logged: {e:#}")),
        }
    }

    for p in &problems {
        eprintln!("stomp: {label}: {p}");
    }
    if strict && !problems.is_empty() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
