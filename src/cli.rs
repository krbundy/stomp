//! Command-line interface (§17 of the spec).

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::cmd::{self, Format};
use crate::config::{Config, ConfigError};
use crate::{paths, sys};

#[derive(Parser)]
#[command(
    name = "stomp",
    version,
    about = "Track storage and compute use per research grant on HPC systems"
)]
struct Cli {
    /// Config file to use [default: ~/.config/stomp/config.toml]
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Print JSON instead of a table
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a config file from your current directory layout
    Init {
        /// Top-level directory to manage (repeatable) [default: ~]
        #[arg(long = "root", value_name = "DIR")]
        roots: Vec<String>,
        /// Print the generated config instead of writing it
        #[arg(long)]
        print: bool,
    },
    /// Check stomp's configuration and environment
    Doctor,
    /// Show current storage use per grant
    Usage {
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
        /// Don't save this measurement to the usage history
        #[arg(long)]
        no_record: bool,
    },
    /// Show directory sizes (instant on CephFS)
    Du {
        /// Directory to show [default: the current directory]
        path: Option<PathBuf>,
        /// How many levels of subdirectories to show
        #[arg(short, long, default_value_t = 1)]
        depth: usize,
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },
    /// Reports for grant budgets and planning
    #[command(subcommand)]
    Report(ReportCommand),
    /// Attribute Slurm compute jobs to grants
    #[command(subcommand)]
    Jobs(JobsCommand),
}

#[derive(Subcommand)]
enum ReportCommand {
    /// Current storage use per grant (same as `stomp usage`)
    Usage {
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
        /// Don't save this measurement to the usage history
        #[arg(long)]
        no_record: bool,
    },
    /// Storage per grant at each recorded measurement
    History {
        /// First day to include (YYYY-MM-DD)
        #[arg(long, value_name = "DATE")]
        from: Option<String>,
        /// Last day to include (YYYY-MM-DD)
        #[arg(long, value_name = "DATE")]
        to: Option<String>,
        /// Only show this grant
        #[arg(long)]
        grant: Option<String>,
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },
}

#[derive(Subcommand)]
enum JobsCommand {
    /// Record that a Slurm job belongs to a grant
    ///
    /// Inside a job script, this logs the current job; a job array is logged
    /// once for all its tasks. With --job, it tags jobs that already ran.
    /// It never makes a job fail: problems are printed as warnings and it
    /// exits 0, unless --strict is given.
    Log {
        /// Grant ID from your config
        grant: String,
        /// Tag this job ID instead of the current job (repeatable)
        #[arg(long = "job", value_name = "ID")]
        jobs: Vec<String>,
        /// A note to store with the record
        #[arg(long)]
        note: Option<String>,
        /// Exit with an error instead of a warning when something goes wrong
        #[arg(long)]
        strict: bool,
    },
}

pub fn run() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(e) => {
            // `stomp jobs log` must not fail a job, even over a typo in its arguments.
            if e.use_stderr() && is_lenient_jobs_log(&args) {
                eprintln!("stomp: warning: {}", e.to_string().trim_end());
                eprintln!("stomp: warning: job not logged (use --strict to make this an error)");
                return ExitCode::SUCCESS;
            }
            e.exit();
        }
    };
    match dispatch(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("stomp: error: {e:#}");
            if e.downcast_ref::<ConfigError>().is_some() {
                ExitCode::from(2)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn is_lenient_jobs_log(args: &[OsString]) -> bool {
    args.windows(2).any(|w| w[0] == "jobs" && w[1] == "log")
        && !args.iter().any(|a| a == "--strict")
}

fn dispatch(cli: Cli) -> Result<ExitCode> {
    let config_path = match &cli.config {
        Some(p) => p.clone(),
        None => paths::config_file()?,
    };
    let data_dir = paths::data_dir()?;
    let format = |f: Format| if cli.json { Format::Json } else { f };

    match cli.command {
        Command::Init { roots, print } => {
            start_threads(None);
            cmd::init::run(&config_path, &roots, print)?;
        }
        Command::Doctor => {
            let ok = cmd::doctor::run(&config_path, &data_dir)?;
            return Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            });
        }
        Command::Usage {
            format: f,
            no_record,
        }
        | Command::Report(ReportCommand::Usage {
            format: f,
            no_record,
        }) => {
            let cfg = load_config(&config_path)?;
            start_threads(Some(&cfg));
            cmd::usage::run(&cfg, &data_dir, format(f), !no_record)?;
        }
        Command::Du {
            path,
            depth,
            format: f,
        } => {
            let cfg = load_optional_config(&config_path);
            start_threads(cfg.as_ref());
            cmd::du::run(cfg.as_ref(), path.as_deref(), depth, format(f))?;
        }
        Command::Report(ReportCommand::History {
            from,
            to,
            grant,
            format: f,
        }) => {
            let cfg = load_config(&config_path)?;
            cmd::report::history(
                &cfg,
                &data_dir,
                from.as_deref(),
                to.as_deref(),
                grant.as_deref(),
                format(f),
            )?;
        }
        Command::Jobs(JobsCommand::Log {
            grant,
            jobs,
            note,
            strict,
        }) => {
            return Ok(cmd::jobs::log(
                &config_path,
                &data_dir,
                &grant,
                &jobs,
                note.as_deref(),
                strict,
            ));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Loads the config, printing its warnings.
fn load_config(path: &std::path::Path) -> Result<Config> {
    let cfg = Config::load(path)?;
    for w in &cfg.warnings {
        cmd::warn(w);
    }
    Ok(cfg)
}

/// For commands that work without a config: a missing config is fine, a broken one is a warning.
fn load_optional_config(path: &std::path::Path) -> Option<Config> {
    if !path.exists() {
        return None;
    }
    match Config::load(path) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            cmd::warn(format_args!("ignoring the config: {e}"));
            None
        }
    }
}

/// Sizes the global thread pool used for parallel walks (§9.2).
fn start_threads(cfg: Option<&Config>) {
    let configured = cfg.map_or(8, |c| c.scan.max_threads);
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(sys::effective_threads(configured))
        // Deep directory trees recurse deeply.
        .stack_size(16 * 1024 * 1024)
        .build_global();
}
