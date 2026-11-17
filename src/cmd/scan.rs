//! `stomp scan`: a detailed scan, normally run as a Slurm task (§9, §14).

use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use signal_hook::consts::{SIGINT, SIGTERM, SIGUSR1};

use super::open_catalog;
use crate::config::Config;
use crate::scan::walk::Progress;
use crate::scan::{self, Outcome, Request, Summary};
use crate::tasks::{self, Overrides, Status};
use crate::units::{format_bytes, format_count, format_duration};

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
            "stomp: scanning here instead of in a Slurm task. On a shared login node, please keep this to small trees."
        );
    }

    // Slurm sends USR1 shortly before the time limit (§14.2); TERM and Ctrl-C
    // also stop the scan cleanly, keeping finished work.
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [SIGUSR1, SIGTERM, SIGINT] {
        signal_hook::flag::register(signal, Arc::clone(&stop))?;
    }
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
        scope.spawn(|| report_progress(&progress, &finished, progress_file.as_deref()));
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
        "Follow it with `stomp tasks`. The results are imported the next time you run stomp after it finishes."
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
        "{kind}, {} directories and {} files ({}) in {}",
        format_count(s.dirs),
        format_count(s.files),
        format_bytes(s.bytes),
        format_duration(s.seconds)
    );
    if s.reused > 0 {
        text += &format!("; {} unchanged directories reused", format_count(s.reused));
    }
    if s.resumed {
        text += "; resumed an interrupted scan";
    }
    text
}

/// Shows progress on an interactive terminal, and writes it for `stomp tasks`.
fn report_progress(progress: &Progress, finished: &AtomicBool, file: Option<&Path>) {
    let interactive = std::io::stderr().is_terminal();
    let mut last = Instant::now();
    loop {
        let done = finished.load(Ordering::Relaxed);
        if done || last.elapsed() >= Duration::from_secs(10) {
            last = Instant::now();
            let (dirs, files, bytes) = (
                progress.dirs.load(Ordering::Relaxed),
                progress.files.load(Ordering::Relaxed),
                progress.bytes.load(Ordering::Relaxed),
            );
            if let Some(path) = file {
                let json = serde_json::json!({
                    "dirs": dirs,
                    "files": files,
                    "bytes": bytes,
                    "reused": progress.reused.load(Ordering::Relaxed),
                    "units": progress.units.load(Ordering::Relaxed),
                    "updated_at": jiff::Timestamp::now().to_string(),
                });
                let tmp = path.with_extension("tmp");
                if std::fs::write(&tmp, json.to_string()).is_ok() {
                    let _ = std::fs::rename(&tmp, path);
                }
            }
            if interactive && !done {
                eprint!(
                    "\rstomp: scanned {} directories, {} files ({})   ",
                    format_count(dirs),
                    format_count(files),
                    format_bytes(bytes)
                );
            }
        }
        if done {
            if interactive {
                eprint!("\r\x1b[K");
            }
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
