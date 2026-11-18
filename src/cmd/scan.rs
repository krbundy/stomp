//! `stomp scan`: a detailed scan, normally run as a Slurm task (§9, §14).

use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;

use super::{open_catalog, progress};
use crate::config::Config;
use crate::scan::walk::Progress;
use crate::scan::{self, Outcome, Request, Summary};
use crate::tasks::{self, Overrides, Status};
use crate::units::{counted, format_bytes, format_count, format_duration};

pub struct Args {
    pub full: bool,
    pub resume: bool,
    pub here: bool,
    pub task: Option<String>,
    pub overrides: Overrides,
}

pub fn run(cfg: &Config, config_path: &Path, data_dir: &Path, args: Args) -> Result<ExitCode> {
    let in_job = std::env::var_os("SLURM_JOB_ID").is_some();
    if args.task.is_none() && !args.here && !in_job {
        return submit(cfg, config_path, data_dir, &args);
    }
    if args.here && !in_job {
        eprintln!(
            "stomp: scanning here instead of in a Slurm task. This walks every root in your config; on a shared login node, keep that to small ones."
        );
    }

    let stop = progress::stop_flag()?;
    if let Some(id) = &args.task {
        tasks::update(data_dir, id, |s| {
            s.status = Status::Running;
            s.message = None;
        })?;
        crate::jobs::log_task(data_dir, id, crate::compute::STOMP);
    }

    let request = Request {
        full: args.full,
        resume: args.resume,
        task_id: args.task.clone(),
    };
    let progress = Progress::default();
    let finished = AtomicBool::new(false);
    let progress_file = args
        .task
        .as_deref()
        .map(|id| tasks::progress_path(data_dir, id));
    let result = std::thread::scope(|scope| {
        scope.spawn(|| progress::report(&finished, progress_file.as_deref(), || sample(&progress)));
        let result = scan::run(cfg, data_dir, &request, &stop, &progress);
        finished.store(true, Ordering::Relaxed);
        result
    });

    match result {
        Err(e) => {
            if let Some(id) = &args.task {
                let _ = tasks::update(data_dir, id, |s| {
                    s.status = Status::Failed;
                    s.message = Some(format!("{e:#}"));
                });
            }
            Err(e)
        }
        Ok(Outcome::Complete(summary)) => {
            let text = describe(&summary);
            println!("Scan complete: {text}.");
            match &args.task {
                Some(id) => {
                    tasks::update(data_dir, id, |s| {
                        s.status = Status::Done;
                        s.message = Some(text);
                    })?;
                }
                None => {
                    open_catalog(data_dir)?;
                    println!("Imported into the catalog.");
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Ok(Outcome::Incomplete(summary)) => {
            let text = format!(
                "stopped early after {}; the finished parts are saved",
                describe(&summary)
            );
            match &args.task {
                Some(id) => {
                    let mut state = tasks::load(data_dir, id)?;
                    if cfg.slurm.auto_resubmit.unwrap_or(true)
                        && state.resubmits < tasks::MAX_RESUBMITS
                    {
                        let job = tasks::resubmit(data_dir, &mut state)?;
                        println!("Scan {text}. Resubmitted as Slurm job {job} to continue.");
                    } else {
                        tasks::update(data_dir, id, |s| {
                            s.status = Status::Incomplete;
                            s.message = Some(text.clone());
                        })?;
                        println!("Scan {text}. Continue it with `stomp scan --resume`.");
                    }
                }
                None => println!("Scan {text}. Continue it with `stomp scan --resume --here`."),
            }
            Ok(ExitCode::from(3))
        }
    }
}

fn submit(cfg: &Config, config_path: &Path, data_dir: &Path, args: &Args) -> Result<ExitCode> {
    let mut stomp_args = vec!["scan".to_string()];
    if args.full {
        stomp_args.push("--full".into());
    }
    if args.resume {
        stomp_args.push("--resume".into());
    }
    let state = tasks::submit(
        cfg,
        config_path,
        data_dir,
        "scan",
        &stomp_args,
        &args.overrides,
    )?;
    println!(
        "Submitted scan task {} as Slurm job {}.",
        state.id,
        state.slurm_jobs.first().map_or("?", String::as_str)
    );
    println!(
        "Follow it with `stomp tasks`; once it's done, that (or any report) imports the results."
    );
    Ok(ExitCode::SUCCESS)
}

fn describe(s: &Summary) -> String {
    let kind = if s.full {
        "full scan"
    } else {
        "incremental scan"
    };
    let mut text = format!(
        "{kind}, {} and {} ({}) in {}",
        counted(s.dirs, "directory", "directories"),
        counted(s.files, "file", "files"),
        format_bytes(s.bytes),
        format_duration(s.seconds)
    );
    if s.reused > 0 {
        text += &format!(
            "; {} reused",
            counted(s.reused, "unchanged directory", "unchanged directories")
        );
    }
    if s.resumed {
        text += "; resumed an interrupted scan";
    }
    text
}

fn sample(progress: &Progress) -> (String, serde_json::Value) {
    let (dirs, files, bytes) = (
        progress.dirs.load(Ordering::Relaxed),
        progress.files.load(Ordering::Relaxed),
        progress.bytes.load(Ordering::Relaxed),
    );
    let line = format!(
        "scanned {} directories, {} files ({})",
        format_count(dirs),
        format_count(files),
        format_bytes(bytes)
    );
    let json = serde_json::json!({
        "dirs": dirs,
        "files": files,
        "bytes": bytes,
        "reused": progress.reused.load(Ordering::Relaxed),
        "units": progress.units.load(Ordering::Relaxed),
    });
    (line, json)
}
