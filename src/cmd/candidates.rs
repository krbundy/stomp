//! `stomp candidates` and `stomp apply` (§16): directories the policies
//! propose for archiving, and archiving them.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, bail};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::Serialize;

use super::{Format, confirm, emit, local_time, open_catalog, rfc3339, warn};
use crate::archive::NOTE_FILE;
use crate::catalog::{Catalog, Proposal, ProposalSync, ScanInfo};
use crate::config::{Config, LOCAL_DESTINATION};
use crate::policy::{self, Inputs};
use crate::scan::DirRow;
use crate::table::{Align, Table};
use crate::tasks::Overrides;
use crate::units::{format_bytes, format_count};
use crate::{dest, paths};

/// Proposals estimated per foreground run; the rest wait for a later run.
const ESTIMATES_PER_RUN: usize = 8;

/// The result of evaluating the policies against the newest detailed scan.
pub(crate) struct Evaluation {
    pub scan: ScanInfo,
    pub sync: ProposalSync,
    pub rows: Vec<DirRow>,
}

fn now_and_today() -> (i64, jiff::civil::Date) {
    let now = Timestamp::now();
    (
        now.as_second(),
        now.to_zoned_datetime(TimeZone::system()).date(),
    )
}

/// Evaluates the policies against the newest detailed scan and updates the
/// open proposals. `None` if there's no such scan yet.
pub(crate) fn refresh(
    cfg: &Config,
    catalog: &mut Catalog,
    data_dir: &Path,
) -> Result<Option<Evaluation>> {
    let Some(scan) = catalog.latest_scan(true)? else {
        return Ok(None);
    };
    let rows = catalog.dir_rows(scan.id, None)?;
    let archived: HashSet<PathBuf> = catalog
        .archives()?
        .into_iter()
        .filter(|a| matches!(a.state.as_str(), "in-progress" | "complete"))
        .map(|a| PathBuf::from(a.source))
        .collect();
    let mut protected = vec![data_dir.to_path_buf()];
    protected.extend(cfg.staging_dir.clone());
    let (now, today) = now_and_today();
    let inputs = Inputs {
        cfg,
        now,
        today,
        archived: &archived,
        protected: &protected,
    };
    // The scan may be days old: skip what has gone or been archived since.
    let candidates: Vec<_> = policy::evaluate(&inputs, &rows)
        .into_iter()
        .filter(|c| c.path.is_dir() && !c.path.join(NOTE_FILE).exists())
        .collect();
    let sync = catalog.sync_proposals(&candidates, scan.id, now)?;
    Ok(Some(Evaluation { scan, sync, rows }))
}

/// Estimates compression for up to `limit` open proposals that lack an
/// estimate. Returns how many still lack one.
pub(crate) fn estimate_missing(
    cfg: &Config,
    catalog: &Catalog,
    rows: &[DirRow],
    limit: usize,
) -> Result<usize> {
    let missing: Vec<Proposal> = catalog
        .proposals(false)?
        .into_iter()
        .filter(|p| p.ratio.is_none())
        .collect();
    let mut left = missing.len();
    for p in missing.iter().take(limit) {
        if let Some(ratio) = policy::estimate_ratio(&p.path, rows, cfg.archive.zstd_level) {
            catalog.set_proposal_ratio(p.id, ratio, p.bytes)?;
            left -= 1;
        }
    }
    Ok(left)
}

/// Bytes an archive would free on the HPC, if known: everything for a remote
/// destination, the compression savings for a local one.
fn frees(p: &Proposal) -> Option<u64> {
    if p.destination != LOCAL_DESTINATION {
        Some(p.bytes)
    } else {
        p.ratio
            .map(|r| (p.bytes as f64 * (1.0 - r.clamp(0.0, 1.0))) as u64)
    }
}

/// Files left on the HPC after archiving: the chunks (for a local archive)
/// plus the archive's records and note. Excluded directories inside aren't counted.
fn files_after(cfg: &Config, p: &Proposal) -> u64 {
    let records = crate::archive::RECORD_FILES;
    if p.destination == LOCAL_DESTINATION {
        p.bytes.div_ceil(cfg.archive.chunk_size.max(1)).max(1) + records
    } else {
        records
    }
}

/// When an `apply` proposal is archived automatically, in Unix seconds.
fn auto_at(cfg: &Config, p: &Proposal) -> Option<i64> {
    if p.action != "apply" || p.state != "open" {
        return None;
    }
    let policy = cfg.policies.iter().find(|x| x.name == p.policy)?;
    Some(p.first_seen + policy.wait.as_secs() as i64)
}

#[derive(Serialize)]
struct Row {
    id: i64,
    state: String,
    path: String,
    grant: Option<String>,
    policy: String,
    action: String,
    destination: String,
    bytes: u64,
    files: u64,
    files_after: u64,
    kept_bytes: u64,
    estimated_ratio: Option<f64>,
    estimated_freed_bytes: Option<u64>,
    newest_change: String,
    first_proposed: String,
    automatic_at: String,
    reason: String,
    task_id: Option<String>,
    archive_id: Option<String>,
}

fn example_policies() -> &'static str {
    r#"  [[policy]]
  name  = "ended-grants"
  match = { grant_ended_for = "90d" }

  [[policy]]
  name  = "stale-small-files"
  match = { unchanged_for = "1y", min_files = 10000, max_avg_file_size = "1MiB" }
"#
}

pub struct ListArgs {
    pub all: bool,
    pub format: Format,
}

/// `stomp candidates`.
pub fn list(cfg: &Config, data_dir: &Path, args: &ListArgs) -> Result<()> {
    let human = matches!(args.format, Format::Table | Format::Md);
    if cfg.policies.is_empty() && human {
        println!(
            "No policies are configured, so nothing is proposed. Add [[policy]] blocks to {}, for example:\n\n{}",
            paths::display(&cfg.source),
            example_policies()
        );
        println!(
            "Conditions: grant, path, grant_ended_for, unchanged_for, min_bytes, min_files, max_avg_file_size."
        );
    }
    let mut catalog = open_catalog(data_dir)?;
    let evaluation = refresh(cfg, &mut catalog, data_dir)?;
    let mut pending = 0;
    if let Some(e) = &evaluation {
        let missing = catalog
            .proposals(false)?
            .iter()
            .filter(|p| p.ratio.is_none())
            .count();
        if missing > 0 && human {
            eprintln!(
                "stomp: estimating compression for {} proposal(s)…",
                missing.min(ESTIMATES_PER_RUN)
            );
        }
        pending = estimate_missing(cfg, &catalog, &e.rows, ESTIMATES_PER_RUN)?;
    }
    let proposals = catalog.proposals(args.all)?;
    let rows: Vec<Row> = proposals
        .iter()
        .map(|p| Row {
            id: p.id,
            state: p.state.clone(),
            path: p.path.to_string_lossy().into_owned(),
            grant: p.grant.clone(),
            policy: p.policy.clone(),
            action: p.action.clone(),
            destination: p.destination.clone(),
            bytes: p.bytes,
            files: p.files,
            files_after: files_after(cfg, p),
            kept_bytes: p.kept_bytes,
            estimated_ratio: p.ratio,
            estimated_freed_bytes: frees(p),
            newest_change: rfc3339(p.newest),
            first_proposed: rfc3339(Some(p.first_seen)),
            automatic_at: rfc3339(auto_at(cfg, p)),
            reason: p.reason.clone(),
            task_id: p.task_id.clone(),
            archive_id: p.archive_id.clone(),
        })
        .collect();

    if human {
        match &evaluation {
            None if !cfg.policies.is_empty() => {
                println!(
                    "Policies are checked against a scan; run `stomp scan` (it runs as a Slurm task), then try again."
                );
                if rows.is_empty() {
                    return Ok(());
                }
            }
            None => {}
            Some(e) => println!(
                "Archive proposals from the scan of {}, by {} polic{}.\n",
                local_time(Timestamp::from_second(e.scan.started_at)?),
                cfg.policies.len(),
                if cfg.policies.len() == 1 { "y" } else { "ies" }
            ),
        }
        if rows.is_empty() {
            if !cfg.policies.is_empty() {
                println!("No directories match your policies.");
            }
            return Ok(());
        }
    }
    let now = Timestamp::now().as_second();
    let schedule_on = super::schedule::enabled(data_dir);
    emit(args.format, &rows, || {
        let mut columns = vec![
            ("ID", Align::Right),
            ("SIZE", Align::Right),
            ("FILES", Align::Right),
            ("FREES", Align::Right),
            ("TO", Align::Left),
            ("GRANT", Align::Left),
            ("PATH", Align::Left),
            ("WHY", Align::Left),
        ];
        if args.all {
            columns.insert(1, ("STATE", Align::Left));
        }
        let mut t = Table::new(&columns);
        for p in &proposals {
            let mut why = format!("{} ({})", p.reason, p.policy);
            if let Some(at) = auto_at(cfg, p) {
                why += &if !schedule_on {
                    " — archived automatically once `stomp schedule enable` is on".to_string()
                } else if at <= now {
                    " — archived automatically on the next scheduled run".to_string()
                } else {
                    format!(" — archived automatically after {}", super::local_date(at))
                };
            }
            let mut cells = vec![
                p.id.to_string(),
                format_bytes(p.bytes),
                format!(
                    "{} → {}",
                    format_count(p.files),
                    format_count(files_after(cfg, p))
                ),
                frees(p).map_or("?".into(), |b| format!("≈{}", format_bytes(b))),
                p.destination.clone(),
                p.grant.clone().unwrap_or_else(|| "—".into()),
                paths::display(&p.path),
                why,
            ];
            if args.all {
                let state = match (&p.state[..], &p.archive_id) {
                    ("applied", Some(id)) => format!("applied ({id})"),
                    (s, _) => s.to_string(),
                };
                cells.insert(1, state);
            }
            t.row(cells);
        }
        t
    })?;
    if human {
        let open: Vec<&Proposal> = proposals.iter().filter(|p| p.state == "open").collect();
        if !open.is_empty() {
            let bytes: u64 = open.iter().map(|p| p.bytes).sum();
            let known: Vec<u64> = open.iter().filter_map(|p| frees(p)).collect();
            let freed: u64 = known.iter().sum();
            println!(
                "\n{} proposal(s), {} in {} files ({} after archiving){}.",
                open.len(),
                format_bytes(bytes),
                format_count(open.iter().map(|p| p.files).sum()),
                format_count(open.iter().map(|p| files_after(cfg, p)).sum()),
                if known.len() == open.len() {
                    format!("; archiving them frees about {}", format_bytes(freed))
                } else {
                    String::new()
                }
            );
            if pending > 0 {
                println!(
                    "{pending} estimate(s) are still to come (\"?\"); they're filled in on later runs."
                );
            }
            println!(
                "Archive with `stomp apply ID...` or `stomp apply --all`; skip one for good with `stomp candidates dismiss ID`."
            );
            println!(
                "Directories read but never modified (software, reference data) look unchanged too; check before archiving."
            );
        }
    }
    Ok(())
}

/// `stomp candidates dismiss`.
pub fn dismiss(cfg: &Config, data_dir: &Path, ids: &[i64]) -> Result<()> {
    let mut catalog = open_catalog(data_dir)?;
    refresh(cfg, &mut catalog, data_dir)?;
    for id in ids {
        match catalog.proposal(*id)? {
            None => bail!("there's no proposal {id}; see `stomp candidates`"),
            Some(p) if p.state != "open" => {
                warn(format_args!("proposal {id} is already {}", p.state))
            }
            Some(_) => {}
        }
    }
    let n = catalog.dismiss_proposals(ids, Timestamp::now().as_second())?;
    println!(
        "Dismissed {n} proposal(s); they won't be proposed again. `stomp candidates --all` lists them, and `stomp apply ID` still archives one."
    );
    Ok(())
}

pub struct ApplyArgs {
    pub ids: Vec<i64>,
    pub all: bool,
    pub yes: bool,
    pub here: bool,
    pub overrides: Overrides,
}

/// Starts archiving one proposal: a Slurm task, or here. Returns what happened.
fn start(
    cfg: &Config,
    config_path: &Path,
    data_dir: &Path,
    catalog: &Catalog,
    p: &Proposal,
    here: bool,
    overrides: &Overrides,
) -> Result<String> {
    if p.destination != LOCAL_DESTINATION {
        dest::open(cfg, &p.destination)?;
    }
    let settings = super::archive::new_settings(cfg, &p.path, &p.destination, None, None, None)?;
    let now = Timestamp::now().as_second();
    if here {
        // Marked first: the archive's own checks refuse a second start anyway.
        catalog.mark_proposal_applied(p.id, None, &settings.id, now)?;
        let code = super::archive::archive_here(cfg, data_dir, &settings)?;
        if code != ExitCode::SUCCESS {
            bail!(
                "archive {} stopped early; continue it with `stomp archive --resume {}`",
                settings.id,
                paths::display(&p.path)
            );
        }
        return Ok(format!("archived as {}", settings.id));
    }
    super::archive::check_headroom(&settings)?;
    let task = super::archive::submit_archive(cfg, config_path, data_dir, &settings, overrides)?;
    catalog.mark_proposal_applied(p.id, Some(&task.id), &settings.id, now)?;
    Ok(format!(
        "archive {} submitted as task {} (Slurm job {})",
        settings.id,
        task.id,
        task.slurm_jobs.first().map_or("?", String::as_str)
    ))
}

/// `stomp apply`.
pub fn apply(
    cfg: &Config,
    config_path: &Path,
    data_dir: &Path,
    args: ApplyArgs,
) -> Result<ExitCode> {
    if args.ids.is_empty() && !args.all {
        bail!("name proposals to archive (`stomp apply 3 7`), or use --all");
    }
    let mut catalog = open_catalog(data_dir)?;
    refresh(cfg, &mut catalog, data_dir)?;
    let mut chosen = Vec::new();
    if args.all {
        chosen = catalog.proposals(false)?;
    } else {
        for id in &args.ids {
            match catalog.proposal(*id)? {
                None => bail!("there's no proposal {id}; see `stomp candidates`"),
                Some(p) if p.state == "applied" => warn(format_args!(
                    "proposal {id} was already applied as archive {}",
                    p.archive_id.as_deref().unwrap_or("?")
                )),
                Some(p) => chosen.push(p),
            }
        }
    }
    let (now, today) = now_and_today();
    let mut ready = Vec::new();
    for p in chosen {
        match policy::recheck(cfg, &p.policy, &p.path, data_dir, now, today) {
            Ok(()) => ready.push(p),
            Err(e) => warn(format_args!(
                "skipping proposal {} ({}): {e:#}",
                p.id,
                paths::display(&p.path)
            )),
        }
    }
    if ready.is_empty() {
        println!("Nothing to archive.");
        return Ok(ExitCode::SUCCESS);
    }
    let here = args.here || std::env::var_os("SLURM_JOB_ID").is_some();
    if !args.yes {
        println!(
            "These directories will be archived, each verified before its originals are removed:\n"
        );
        let mut t = Table::new(&[
            ("ID", Align::Right),
            ("SIZE", Align::Right),
            ("FILES", Align::Right),
            ("TO", Align::Left),
            ("PATH", Align::Left),
        ]);
        for p in &ready {
            t.row(vec![
                p.id.to_string(),
                format_bytes(p.bytes),
                format_count(p.files),
                p.destination.clone(),
                paths::display(&p.path),
            ]);
        }
        print!("{}", t.render());
        println!(
            "\n{}",
            if here {
                "They run here, one after another."
            } else {
                "Each runs as its own Slurm task."
            }
        );
        if !confirm(&format!(
            "Archive {} director{}?",
            ready.len(),
            if ready.len() == 1 { "y" } else { "ies" }
        ))? {
            println!("Nothing was changed.");
            return Ok(ExitCode::SUCCESS);
        }
    }
    let mut failed = 0;
    for p in &ready {
        match start(
            cfg,
            config_path,
            data_dir,
            &catalog,
            p,
            here,
            &args.overrides,
        ) {
            Ok(text) => println!("{} {}: {text}", p.id, paths::display(&p.path)),
            Err(e) => {
                failed += 1;
                eprintln!("stomp: error: {} {}: {e:#}", p.id, paths::display(&p.path));
            }
        }
    }
    if !here {
        println!("Follow them with `stomp tasks`.");
    }
    Ok(if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Archives proposals from `apply` policies whose wait has passed, as Slurm
/// tasks, at most `limit` per call. Returns lines for the schedule's log.
pub(crate) fn auto_apply(
    cfg: &Config,
    config_path: &Path,
    data_dir: &Path,
    catalog: &Catalog,
    limit: usize,
) -> Result<Vec<String>> {
    let (now, today) = now_and_today();
    let mut due: Vec<Proposal> = catalog
        .proposals(false)?
        .into_iter()
        .filter(|p| auto_at(cfg, p).is_some_and(|at| at <= now))
        .collect();
    due.sort_by_key(|p| p.first_seen);
    let mut lines = Vec::new();
    let waiting = due.len().saturating_sub(limit);
    for p in due.into_iter().take(limit) {
        let shown = paths::display(&p.path);
        if let Err(e) = policy::recheck(cfg, &p.policy, &p.path, data_dir, now, today) {
            lines.push(format!("not archiving proposal {} ({shown}): {e:#}", p.id));
            continue;
        }
        match start(
            cfg,
            config_path,
            data_dir,
            catalog,
            &p,
            false,
            &Overrides::default(),
        ) {
            Ok(text) => lines.push(format!(
                "proposal {} ({shown}, policy {}): {text}",
                p.id, p.policy
            )),
            Err(e) => lines.push(format!(
                "couldn't archive proposal {} ({shown}): {e:#}",
                p.id
            )),
        }
    }
    if waiting > 0 {
        lines.push(format!(
            "{waiting} more automatic archive(s) are due; they start on the next runs"
        ));
    }
    Ok(lines)
}
