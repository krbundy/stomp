//! `stomp archive`, `stomp archives` and `stomp restore` (§12).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::{Format, confirm, emit, open_catalog, progress, short_local};
use crate::archive::manifest::{self, Record};
use crate::archive::{
    self, Outcome, Progress, RestoreOptions, RestoreOutcome, RestoreSummary, Settings,
};
use crate::config::{Config, LOCAL_DESTINATION};
use crate::dest::{self, Destination};
use crate::scan::rules::Rules;
use crate::table::{Align, Table};
use crate::tasks::{self, Overrides, Status};
use crate::units::{format_bytes, format_count, format_duration, parse_size};
use crate::{compute, fs, jobs, paths, sys};

pub struct ArchiveArgs {
    pub dir: PathBuf,
    pub to: Option<String>,
    pub yes: bool,
    pub dry_run: bool,
    pub estimate: bool,
    pub here: bool,
    pub resume: bool,
    pub level: Option<i32>,
    pub chunk_size: Option<String>,
    pub id: Option<String>,
    pub task: Option<String>,
    pub overrides: Overrides,
    pub format: Format,
}

fn in_job() -> bool {
    std::env::var_os("SLURM_JOB_ID").is_some()
}

pub fn archive(
    cfg: &Config,
    config_path: &Path,
    data_dir: &Path,
    args: ArchiveArgs,
) -> Result<ExitCode> {
    let dir = std::fs::canonicalize(&args.dir)
        .with_context(|| format!("cannot open {}", args.dir.display()))?;
    let destination = args
        .to
        .clone()
        .unwrap_or_else(|| cfg.archive.default_destination.clone());
    if destination != LOCAL_DESTINATION {
        dest::open(cfg, &destination)?; // a clear error now rather than in the task
    }

    // An archive to continue: named by a task's --id, or found by --resume.
    let existing = match &args.id {
        Some(id) => {
            let path = archive::archive_path(&dir, id);
            path.join("settings.json").exists().then_some(path)
        }
        None if args.resume => Some(
            archive::find_in(&dir)
                .into_iter()
                .find(|f| !f.finished && !f.restored)
                .with_context(|| {
                    format!("there's no unfinished archive in {}", paths::display(&dir))
                })?
                .dir,
        ),
        None => None,
    };
    archive::check_target(cfg, &dir, data_dir, existing.is_some() || args.id.is_some())?;
    let threads = sys::effective_threads(cfg.scan.max_threads) as u32;
    let settings = match &existing {
        Some(path) => Settings {
            threads,
            ..archive::load_settings(path)?
        },
        None => {
            let grant = cfg.grant_for(&dir).map(|g| g.id.clone());
            Settings {
                id: args
                    .id
                    .clone()
                    .unwrap_or_else(|| archive::new_id(grant.as_deref())),
                source: dir.clone(),
                grant,
                chunk_size: match &args.chunk_size {
                    Some(s) => parse_size(s)?.max(1 << 20),
                    None => cfg.archive.chunk_size,
                },
                level: args.level.unwrap_or(cfg.archive.zstd_level).clamp(1, 22),
                threads,
                only_owned: cfg.archive.only_owned,
                destination: destination.clone(),
                staging: cfg.staging_dir.clone(),
            }
        }
    };

    if args.dry_run || args.estimate {
        return plan_only(cfg, data_dir, &settings, args.estimate, args.format);
    }

    let running_here = args.task.is_some() || args.here || in_job();
    if args.task.is_none() && !args.yes {
        print_intent(&settings, existing.is_some());
        if !confirm(if existing.is_some() {
            "Continue archiving?"
        } else {
            "Archive it?"
        })? {
            println!("Nothing was changed.");
            return Ok(ExitCode::SUCCESS);
        }
    }
    check_headroom(&settings)?;
    if !running_here {
        let task_args = vec![
            "archive".to_string(),
            dir.to_string_lossy().into_owned(),
            "--yes".into(),
            "--id".into(),
            settings.id.clone(),
            "--level".into(),
            settings.level.to_string(),
            "--chunk-size".into(),
            settings.chunk_size.to_string(),
        ];
        let state = tasks::submit(
            cfg,
            config_path,
            data_dir,
            "archive",
            &task_args,
            &args.overrides,
        )?;
        println!(
            "Submitted archive task {} as Slurm job {}. Follow it with `stomp tasks`.",
            state.id,
            state.slurm_jobs.first().map_or("?", String::as_str)
        );
        println!("Archive ID: {}", settings.id);
        return Ok(ExitCode::SUCCESS);
    }
    run_archive(cfg, data_dir, &settings, args.task.as_deref())
}

fn print_intent(s: &Settings, resuming: bool) {
    let shown = paths::display(&s.source);
    let grant = s
        .grant
        .as_deref()
        .map_or(String::new(), |g| format!(" (grant {g})"));
    if resuming {
        println!("Continue archiving {shown}{grant}, archive {}.", s.id);
        return;
    }
    println!("Archive {shown}{grant}:");
    if let Ok(backend) = fs::detect(&s.source)
        && backend.is_instant()
        && let Ok(u) = backend.usage(&s.source)
    {
        println!(
            "  about {} in {} files",
            format_bytes(u.bytes),
            format_count(u.files)
        );
    }
    println!(
        "  packed into chunks of up to {} with zstd level {}, kept in",
        format_bytes(s.chunk_size),
        s.level
    );
    println!(
        "  {}",
        paths::display(&archive::archive_path(&s.source, &s.id))
    );
    println!("The originals are removed one chunk at a time, after that chunk has been");
    println!("read back and checked. Software environments, caches and files owned by");
    println!("others stay in place. `--dry-run` shows the full plan; `--estimate` adds the");
    println!("expected compressed size.");
}

/// Refuses to start without room under the quota for the chunk being written (§12.4).
fn check_headroom(s: &Settings) -> Result<()> {
    let backend = fs::detect(&s.source)?;
    if let Ok(Some(quota)) = backend.quota(&s.source)
        && let Some(max) = quota.max_bytes
    {
        let free = max.saturating_sub(quota.used.bytes);
        let size = backend.usage(&s.source).map_or(s.chunk_size, |u| u.bytes);
        let need = size.min(s.chunk_size) + (64 << 20);
        if free < need {
            bail!(
                "only {} is free under the quota on {}, and archiving needs about {} for the chunk being written; free some space or use a smaller --chunk-size",
                format_bytes(free),
                paths::display(&quota.dir),
                format_bytes(need)
            );
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct PlanReport {
    source: String,
    grant: Option<String>,
    chunk_size: u64,
    level: i32,
    summary: archive::plan::PlanSummary,
    estimate: Option<archive::Estimate>,
}

fn plan_only(
    cfg: &Config,
    data_dir: &Path,
    s: &Settings,
    estimate: bool,
    format: Format,
) -> Result<ExitCode> {
    eprintln!(
        "stomp: planning lists every file under {}; on a big tree, run this inside a Slurm job.",
        paths::display(&s.source)
    );
    let rules = Rules::new(cfg, data_dir)?;
    let backend = fs::detect(&s.source)?;
    let plan = archive::plan::make_plan(
        &s.source,
        &rules,
        backend.as_ref(),
        s.only_owned,
        s.chunk_size,
    )?;
    let estimate = if estimate {
        Some(archive::estimate(&s.source, &plan, s.level, s.threads)?)
    } else {
        None
    };
    if format == Format::Json {
        let report = PlanReport {
            source: s.source.to_string_lossy().into_owned(),
            grant: s.grant.clone(),
            chunk_size: s.chunk_size,
            level: s.level,
            summary: plan.summary.clone(),
            estimate,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(ExitCode::SUCCESS);
    }
    let p = &plan.summary;
    let grant = s
        .grant
        .as_deref()
        .map_or(String::new(), |g| format!(" (grant {g})"));
    println!("Plan for archiving {}{grant}\n", paths::display(&s.source));
    let mut t = Table::new(&[("", Align::Left), ("", Align::Right), ("", Align::Left)]);
    t.row(vec![
        "files".into(),
        format_count(p.files),
        format_bytes(p.bytes),
    ]);
    t.row(vec![
        "directories".into(),
        format_count(p.dirs),
        String::new(),
    ]);
    t.row(vec![
        "symlinks".into(),
        format_count(p.symlinks),
        String::new(),
    ]);
    t.row(vec![
        "hard links".into(),
        format_count(p.hardlinks),
        String::new(),
    ]);
    t.row(vec![
        "chunks".into(),
        format_count(u64::from(p.chunks)),
        format!(
            "up to {} each, zstd level {}",
            format_bytes(s.chunk_size),
            s.level
        ),
    ]);
    print!(
        "{}",
        t.render()
            .lines()
            .skip(1)
            .map(|l| format!("  {l}\n"))
            .collect::<String>()
    );
    if !p.skipped.is_empty() {
        println!("\nLeft in place:");
        for (reason, (count, bytes)) in &p.skipped {
            println!(
                "  {:<40} {:>8}   {}",
                reason.label(),
                format_count(*count),
                format_bytes(*bytes)
            );
        }
    }
    if let Some(e) = estimate {
        let saved = p.bytes.saturating_sub(e.predicted_bytes);
        println!(
            "\nEstimate, from compressing {} sampled files:",
            e.sampled_files
        );
        println!(
            "  compressed size   about {} ({:.0}% of the original), saving about {}",
            format_bytes(e.predicted_bytes),
            e.ratio * 100.0,
            format_bytes(saved)
        );
        println!(
            "  files             {} → {}",
            format_count(e.files_before),
            format_count(e.files_after)
        );
        println!(
            "  compression time  about {} at {:.0} MB/s per thread with {} thread(s); reading the files may take longer",
            format_duration(e.predicted_seconds),
            e.mb_per_second,
            e.threads
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// The destination an archive's chunks are at, unless they're local.
fn open_remote(cfg: &Config, destination: &str) -> Result<Option<Box<dyn Destination>>> {
    if destination == LOCAL_DESTINATION {
        Ok(None)
    } else {
        dest::open(cfg, destination).map(Some)
    }
}

fn run_archive(
    cfg: &Config,
    data_dir: &Path,
    s: &Settings,
    task: Option<&str>,
) -> Result<ExitCode> {
    let stop = progress::stop_flag()?;
    if let Some(id) = task {
        tasks::update(data_dir, id, |t| {
            t.status = Status::Running;
            t.message = None;
        })?;
        // The task works for the grant whose data it archives (§14.2).
        jobs::log_task(data_dir, id, s.grant.as_deref().unwrap_or(compute::STOMP));
    } else if !in_job() {
        eprintln!(
            "stomp: archiving here instead of in a Slurm task; Ctrl-C stops it safely and `--resume` continues."
        );
    }
    let rules = Rules::new(cfg, data_dir)?;
    let backend = fs::detect(&s.source)?;
    let remote = open_remote(cfg, &s.destination)?;
    let counters = Progress::default();
    let finished = AtomicBool::new(false);
    let file = task.map(|id| tasks::progress_path(data_dir, id));
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            progress::report(&finished, file.as_deref(), || {
                let (files, bytes, chunks, removed) = (
                    counters.files.load(Ordering::Relaxed),
                    counters.bytes.load(Ordering::Relaxed),
                    counters.chunks.load(Ordering::Relaxed),
                    counters.removed.load(Ordering::Relaxed),
                );
                (
                    format!(
                        "archived {} files ({}), {} chunk(s) done, {} originals removed",
                        format_count(files),
                        format_bytes(bytes),
                        chunks,
                        format_count(removed)
                    ),
                    serde_json::json!({"files": files, "bytes": bytes, "chunks": chunks, "removed": removed}),
                )
            })
        });
        let result = archive::archive(
            s,
            &rules,
            backend.as_ref(),
            remote.as_deref(),
            data_dir,
            &stop,
            &counters,
            &|_| false,
        );
        finished.store(true, Ordering::Relaxed);
        result
    });
    match result {
        Err(e) => {
            if let Some(id) = task {
                let _ = tasks::update(data_dir, id, |t| {
                    t.status = Status::Failed;
                    t.message = Some(format!("{e:#}"));
                });
            }
            Err(e)
        }
        Ok(Outcome::Complete(sum)) => {
            let mut text = format!(
                "Archived {}: {} files ({}) compressed to {} in {} chunk(s), in {}.",
                paths::display(&s.source),
                format_count(sum.files),
                format_bytes(sum.bytes),
                format_bytes(sum.compressed),
                sum.chunks,
                format_duration(sum.seconds)
            );
            if sum.kept > 0 {
                text += &format!(
                    " {} file(s) changed while being archived and were left in place.",
                    sum.kept
                );
            }
            if sum.skipped > 0 {
                text += &format!(
                    " {} item(s) were left in place on purpose (see the manifest).",
                    sum.skipped
                );
            }
            println!("{text}");
            println!(
                "Archive {}. Restore it with `stomp restore {}`.",
                s.id, s.id
            );
            match task {
                Some(id) => {
                    tasks::update(data_dir, id, |t| {
                        t.status = Status::Done;
                        t.message = Some(text);
                    })?;
                }
                None => {
                    open_catalog(data_dir)?;
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Ok(Outcome::Incomplete(sum)) => {
            let text = format!(
                "stopped after {} of {} chunk(s); everything removed so far is safe in the archive",
                sum.chunks_done, sum.chunks
            );
            continue_or_resubmit(
                cfg,
                data_dir,
                task,
                &text,
                &format!("stomp archive --resume {}", paths::display(&s.source)),
            )
        }
    }
}

/// After an interruption: a task resubmits itself if allowed; otherwise say how to continue.
fn continue_or_resubmit(
    cfg: &Config,
    data_dir: &Path,
    task: Option<&str>,
    text: &str,
    command: &str,
) -> Result<ExitCode> {
    match task {
        Some(id) => {
            let mut state = tasks::load(data_dir, id)?;
            if cfg.slurm.auto_resubmit.unwrap_or(true) && state.resubmits < tasks::MAX_RESUBMITS {
                let job = tasks::resubmit(data_dir, &mut state)?;
                println!("{text}. Resubmitted as Slurm job {job} to continue.");
            } else {
                tasks::update(data_dir, id, |t| {
                    t.status = Status::Incomplete;
                    t.message = Some(text.to_string());
                })?;
                println!("{text}. Continue with `{command}`.");
            }
        }
        None => println!("{text}. Continue with `{command}`."),
    }
    Ok(ExitCode::from(3))
}

// ---------------------------------------------------------------------------
// stomp archives
// ---------------------------------------------------------------------------

pub fn list(data_dir: &Path, format: Format) -> Result<()> {
    let archives = open_catalog(data_dir)?.archives()?;
    if archives.is_empty() && matches!(format, Format::Table | Format::Md) {
        println!("No archives yet. `stomp archive <dir>` makes one.");
        return Ok(());
    }
    emit(format, &archives, || {
        let mut t = Table::new(&[
            ("ARCHIVE", Align::Left),
            ("STATE", Align::Left),
            ("ORIGINAL", Align::Right),
            ("COMPRESSED", Align::Right),
            ("FILES", Align::Right),
            ("CREATED", Align::Left),
            ("DIRECTORY", Align::Left),
        ]);
        for a in &archives {
            t.row(vec![
                a.id.clone(),
                a.state.clone(),
                format_bytes(a.bytes),
                if a.compressed > 0 {
                    format_bytes(a.compressed)
                } else {
                    "—".into()
                },
                format_count(a.files),
                a.created_at.as_deref().map(short_local).unwrap_or_default(),
                paths::display(Path::new(&a.source)),
            ]);
        }
        t
    })
}

/// Finds an archive by ID (through the catalog) or by the directory holding it.
/// Returns (archive directory, original location).
pub fn locate(data_dir: &Path, what: &str) -> Result<(PathBuf, PathBuf)> {
    let path = PathBuf::from(what);
    if path.exists() {
        let path = std::fs::canonicalize(&path)?;
        if path.join("settings.json").exists() {
            let settings = archive::load_settings(&path)?;
            return Ok((path, settings.source));
        }
        let found: Vec<_> = archive::find_in(&path)
            .into_iter()
            .filter(|f| !f.restored)
            .collect();
        return match found.as_slice() {
            [one] => Ok((one.dir.clone(), path)),
            [] => bail!("there's no stomp archive in {}", paths::display(&path)),
            many => bail!(
                "{} holds several archives; name one: {}",
                paths::display(&path),
                many.iter()
                    .map(|f| f.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
    }
    let catalog = open_catalog(data_dir)?;
    let record = catalog
        .archive(what)?
        .with_context(|| format!("no archive or directory called {what:?}"))?;
    // An archive's records always live in its directory, wherever the chunks are.
    let source = PathBuf::from(&record.source);
    let dir = archive::archive_path(&source, &record.id);
    if !dir.exists() {
        let why = match record.state.as_str() {
            "restored" => " (it was restored)".to_string(),
            "restored-copy-kept" => {
                format!(" (it was restored; a copy remains at {})", record.location)
            }
            _ => String::new(),
        };
        bail!(
            "archive {what}'s records at {} are gone{why}",
            paths::display(&dir)
        );
    }
    Ok((dir, source))
}

pub fn show(data_dir: &Path, what: &str) -> Result<()> {
    let (dir, source) = locate(data_dir, what)?;
    let manifest_path = dir.join(manifest::MANIFEST);
    println!(
        "Archive    {}",
        dir.file_name().unwrap_or_default().to_string_lossy()
    );
    println!("Directory  {}", paths::display(&source));
    println!("Stored in  {}", paths::display(&dir));
    if !manifest_path.exists() {
        let state = archive::journal::Journal::read(&dir.join("journal.jsonl"))?;
        let (chunks, files, bytes) = state.planned.unwrap_or_default();
        let done = state
            .chunks
            .values()
            .filter(|c| c.deleted.is_some())
            .count();
        println!("State      unfinished: {done} of {chunks} chunk(s) done");
        println!(
            "Planned    {} files, {}",
            format_count(files),
            format_bytes(bytes)
        );
        return Ok(());
    }
    let mut skipped: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for record in manifest::read_records(&manifest_path)? {
        match record {
            Record::Header(h) => {
                let t = &h.totals;
                println!("Grant      {}", h.grant.as_deref().unwrap_or("—"));
                println!(
                    "Created    {} by {} on {}",
                    short_local(&h.created_at),
                    h.user.as_deref().unwrap_or("?"),
                    h.host
                );
                println!("Finished   {}", short_local(&h.finished_at));
                println!(
                    "Contents   {} files ({}), {} directories, {} symlinks, {} hard links",
                    format_count(t.files),
                    format_bytes(t.bytes),
                    format_count(t.dirs),
                    format_count(t.symlinks),
                    format_count(t.hardlinks)
                );
                println!(
                    "Stored as  {} chunk(s), {} ({:.0}% of the original), zstd level {}",
                    t.chunks,
                    format_bytes(t.compressed_bytes),
                    if t.bytes > 0 {
                        t.compressed_bytes as f64 * 100.0 / t.bytes as f64
                    } else {
                        0.0
                    },
                    h.zstd_level
                );
                if t.kept > 0 {
                    println!(
                        "Kept       {} original(s) left in place because they changed",
                        t.kept
                    );
                }
            }
            Record::Skipped(s) => {
                let slot = skipped.entry(s.reason.label().to_string()).or_default();
                slot.0 += 1;
                slot.1 += s.bytes;
            }
            Record::Entry(_) => {}
        }
    }
    if !skipped.is_empty() {
        println!("Left in place:");
        for (label, (n, bytes)) in skipped {
            println!(
                "  {label:<40} {:>8}   {}",
                format_count(n),
                format_bytes(bytes)
            );
        }
    }
    Ok(())
}

pub fn verify(cfg: &Config, data_dir: &Path, what: &str, deep: bool) -> Result<ExitCode> {
    let (dir, _) = locate(data_dir, what)?;
    if !dir.join(manifest::MANIFEST).exists() {
        bail!(
            "this archive isn't finished; `stomp archive --resume` verifies each chunk as it goes"
        );
    }
    let settings = archive::load_settings(&dir)?;
    let remote = open_remote(cfg, &settings.destination)?;
    let journal = archive::journal::Journal::read(&dir.join("journal.jsonl"))?;
    let scratch = settings.chunk_dir(&dir);
    let mut by_chunk: BTreeMap<u32, Vec<_>> = BTreeMap::new();
    if deep {
        for record in manifest::read_records(&dir.join(manifest::MANIFEST))? {
            if let Record::Entry(e) = record {
                by_chunk.entry(e.chunk).or_default().push(e);
            }
        }
    }
    let mut failures = 0;
    for (name, sum) in manifest::read_checksums(&dir.join(manifest::CHECKSUMS))? {
        let path = dir.join(&name);
        let chunk = name
            .strip_prefix("chunk-")
            .and_then(|n| n.strip_suffix(".tar.zst"))
            .and_then(|n| n.parse::<u32>().ok());
        let records = chunk.and_then(|k| by_chunk.get(&k)).map_or(&[][..], |v| v);
        let result = match (&remote, chunk, path.exists()) {
            // A remote chunk: checked where it's stored, or downloaded for a deep check.
            (Some(remote), Some(k), false) => {
                let rel = format!("{}/{name}", settings.remote_prefix());
                if deep {
                    let tmp = scratch.join(format!("{name}.verify"));
                    let result = paths::create_private_dir(&scratch)
                        .map_err(anyhow::Error::from)
                        .and_then(|()| remote.get(&rel, &tmp))
                        .and_then(|()| archive::verify_chunk(&tmp, &sum, records, true));
                    let _ = std::fs::remove_file(&tmp);
                    result
                } else {
                    let size = journal.chunk(k).written.map_or(0, |w| w.1);
                    remote.verify(&rel, &sum, size, &scratch)
                }
            }
            (_, Some(_), _) if deep => archive::verify_chunk(&path, &sum, records, true),
            _ => manifest::sha256_file(&path).and_then(|actual| {
                if actual == sum {
                    Ok(())
                } else {
                    bail!("wrong checksum")
                }
            }),
        };
        match result {
            Ok(()) => println!("ok    {name}"),
            Err(e) => {
                failures += 1;
                println!("FAIL  {name}: {e:#}");
            }
        }
    }
    if failures == 0 {
        println!(
            "\nAll checks passed{}.",
            if deep {
                ", including every file's checksum"
            } else {
                ""
            }
        );
        Ok(ExitCode::SUCCESS)
    } else {
        println!("\n{failures} check(s) failed.");
        Ok(ExitCode::FAILURE)
    }
}

// ---------------------------------------------------------------------------
// stomp restore
// ---------------------------------------------------------------------------

pub struct RestoreArgs {
    pub what: String,
    pub only: Vec<String>,
    pub to: Option<PathBuf>,
    pub overwrite: bool,
    pub keep_archive: bool,
    pub delete_remote: bool,
    pub yes: bool,
    pub here: bool,
    pub task: Option<String>,
    pub overrides: Overrides,
}

pub fn restore(
    cfg: &Config,
    config_path: &Path,
    data_dir: &Path,
    args: RestoreArgs,
) -> Result<ExitCode> {
    let (archive_dir, source) = locate(data_dir, &args.what)?;
    let to = args
        .to
        .as_ref()
        .map(|p| -> Result<PathBuf> {
            std::fs::create_dir_all(p)?;
            Ok(std::fs::canonicalize(p)?)
        })
        .transpose()?;
    if args.overwrite && !args.yes && args.task.is_none() {
        println!("Files that already exist where the archive restores to will be replaced.");
        if !confirm("Continue?")? {
            println!("Nothing was changed.");
            return Ok(ExitCode::SUCCESS);
        }
    }
    let running_here = args.task.is_some() || args.here || in_job();
    if !running_here {
        let mut task_args = vec![
            "restore".to_string(),
            archive_dir.to_string_lossy().into_owned(),
            "--yes".into(),
        ];
        for pattern in &args.only {
            task_args.extend(["--only".into(), pattern.clone()]);
        }
        if let Some(to) = &to {
            task_args.extend(["--to".into(), to.to_string_lossy().into_owned()]);
        }
        if args.overwrite {
            task_args.push("--overwrite".into());
        }
        if args.keep_archive {
            task_args.push("--keep-archive".into());
        }
        if args.delete_remote {
            task_args.push("--delete-remote".into());
        }
        let state = tasks::submit(
            cfg,
            config_path,
            data_dir,
            "restore",
            &task_args,
            &args.overrides,
        )?;
        println!(
            "Submitted restore task {} as Slurm job {}. Follow it with `stomp tasks`.",
            state.id,
            state.slurm_jobs.first().map_or("?", String::as_str)
        );
        return Ok(ExitCode::SUCCESS);
    }

    let stop = progress::stop_flag()?;
    let settings = archive::load_settings(&archive_dir).ok();
    if let Some(id) = &args.task {
        tasks::update(data_dir, id, |t| {
            t.status = Status::Running;
            t.message = None;
        })?;
        let grant = settings.as_ref().and_then(|s| s.grant.clone());
        jobs::log_task(data_dir, id, grant.as_deref().unwrap_or(compute::STOMP));
    }
    let options = RestoreOptions {
        only: args.only.clone(),
        to: to.clone(),
        overwrite: args.overwrite,
        keep_archive: args.keep_archive,
        delete_remote: args.delete_remote,
    };
    let remote = match &settings {
        Some(s) => open_remote(cfg, &s.destination)?,
        None => None,
    };
    match archive::restore(
        &archive_dir,
        &source,
        &options,
        remote.as_deref(),
        data_dir,
        &stop,
    ) {
        Err(e) => {
            if let Some(id) = &args.task {
                let _ = tasks::update(data_dir, id, |t| {
                    t.status = Status::Failed;
                    t.message = Some(format!("{e:#}"));
                });
            }
            Err(e)
        }
        Ok(RestoreOutcome::Complete(summary)) => {
            let text = describe_restore(&summary, to.as_deref().unwrap_or(&source));
            println!("{text}");
            match &args.task {
                Some(id) => {
                    tasks::update(data_dir, id, |t| {
                        t.status = Status::Done;
                        t.message = Some(text);
                    })?;
                }
                None => {
                    open_catalog(data_dir)?;
                }
            }
            Ok(if summary.conflicts.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(3)
            })
        }
        Ok(RestoreOutcome::Incomplete(_)) => continue_or_resubmit(
            cfg,
            data_dir,
            args.task.as_deref(),
            "restore stopped early; files restored so far are in place",
            &format!("stomp restore {}", args.what),
        ),
    }
}

// ---------------------------------------------------------------------------
// stomp archives move
// ---------------------------------------------------------------------------

pub struct MoveArgs {
    pub what: String,
    pub to: String,
    pub yes: bool,
    pub here: bool,
    pub task: Option<String>,
    pub overrides: Overrides,
}

/// Moves an archive's chunks to another destination, or back into its directory.
/// Run it on a login or data-transfer node (`--here`) when compute nodes can't
/// reach the destination (§13.4).
pub fn move_archive(
    cfg: &Config,
    config_path: &Path,
    data_dir: &Path,
    args: MoveArgs,
) -> Result<ExitCode> {
    let (archive_dir, _) = locate(data_dir, &args.what)?;
    let settings = archive::load_settings(&archive_dir)?;
    let from = open_remote(cfg, &settings.destination)?;
    let to = open_remote(cfg, &args.to)?;
    if settings.destination == args.to {
        bail!("archive {} is already at {}", settings.id, args.to);
    }
    if args.task.is_none() && !args.yes {
        println!(
            "Move archive {} from {} to {}. Each chunk is verified at {} before any copy is removed.",
            settings.id, settings.destination, args.to, args.to
        );
        if !confirm("Move it?")? {
            println!("Nothing was changed.");
            return Ok(ExitCode::SUCCESS);
        }
    }
    if args.task.is_none() && !args.here && !in_job() {
        let task_args = vec![
            "archives".to_string(),
            "move".into(),
            archive_dir.to_string_lossy().into_owned(),
            "--to".into(),
            args.to.clone(),
            "--yes".into(),
        ];
        let state = tasks::submit(
            cfg,
            config_path,
            data_dir,
            "move",
            &task_args,
            &args.overrides,
        )?;
        println!(
            "Submitted move task {} as Slurm job {}. Follow it with `stomp tasks`.",
            state.id,
            state.slurm_jobs.first().map_or("?", String::as_str)
        );
        let remotes: Vec<&str> = [settings.destination.as_str(), args.to.as_str()]
            .into_iter()
            .filter(|d| *d != crate::config::LOCAL_DESTINATION)
            .collect();
        println!(
            "If compute nodes can't reach {}, cancel it and run this with --here on a login node.",
            remotes.join(" or ")
        );
        return Ok(ExitCode::SUCCESS);
    }
    let stop = progress::stop_flag()?;
    if let Some(id) = &args.task {
        tasks::update(data_dir, id, |t| t.status = Status::Running)?;
        jobs::log_task(
            data_dir,
            id,
            settings.grant.as_deref().unwrap_or(compute::STOMP),
        );
    }
    let result = archive::relocate(
        &archive_dir,
        from.as_deref(),
        to.as_deref(),
        data_dir,
        &stop,
    );
    match result {
        Err(e) => {
            if let Some(id) = &args.task {
                let _ = tasks::update(data_dir, id, |t| {
                    t.status = Status::Failed;
                    t.message = Some(format!("{e:#}"));
                });
            }
            Err(e)
        }
        Ok(archive::RelocateOutcome::Complete(location)) => {
            let text = format!(
                "Moved archive {} to {}.",
                settings.id,
                paths::display(Path::new(&location))
            );
            println!("{text}");
            match &args.task {
                Some(id) => {
                    tasks::update(data_dir, id, |t| {
                        t.status = Status::Done;
                        t.message = Some(text);
                    })?;
                }
                None => {
                    open_catalog(data_dir)?;
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Ok(archive::RelocateOutcome::Incomplete { moved, total }) => continue_or_resubmit(
            cfg,
            data_dir,
            args.task.as_deref(),
            &format!("moved {moved} of {total} chunk(s) before stopping"),
            &format!("stomp archives move {} --to {}", settings.id, args.to),
        ),
    }
}

fn describe_restore(s: &RestoreSummary, into: &Path) -> String {
    let mut text = format!(
        "Restored {} files ({}), {} links and {} directories into {}.",
        format_count(s.files),
        format_bytes(s.bytes),
        format_count(s.links),
        format_count(s.dirs),
        paths::display(into)
    );
    if !s.conflicts.is_empty() {
        let shown: Vec<&str> = s.conflicts.iter().take(5).map(String::as_str).collect();
        text += &format!(
            "\n{} file(s) already existed and were left alone (use --overwrite to replace them): {}{}",
            s.conflicts.len(),
            shown.join(", "),
            if s.conflicts.len() > 5 { ", ..." } else { "" }
        );
    }
    if s.missing_links > 0 {
        text += &format!(
            "\n{} hard link(s) weren't restored because their target wasn't.",
            s.missing_links
        );
    }
    text += if s.archive_removed {
        "\nEverything is back, so the archive was removed."
    } else {
        "\nThe archive was kept."
    };
    text
}
