//! Command-line interface (§17 of the spec).

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::cmd::{self, Format};
use crate::config::{Config, ConfigError};
use crate::tasks::Overrides;
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
    /// Scan in detail: data ages, file types, environments, archive candidates
    ///
    /// Outside a Slurm job this submits a Slurm task and returns. The first
    /// scan, and one every scan.full_every, walks everything; on CephFS the
    /// others skip directories that haven't changed.
    Scan {
        /// Walk everything instead of skipping unchanged directories
        #[arg(long)]
        full: bool,
        /// Continue the most recent interrupted scan
        #[arg(long)]
        resume: bool,
        /// Run here instead of submitting a Slurm task
        #[arg(long)]
        here: bool,
        /// The task this run belongs to (set in scripts stomp generates)
        #[arg(long, hide = true)]
        task: Option<String>,
        #[command(flatten)]
        slurm: SlurmArgs,
    },
    /// How storage changed: per grant, or per directory with --dirs
    Changes {
        /// Compare with this long ago (e.g. 30d) or this date [default: the last measurement or scan]
        #[arg(long, value_name = "AGE|DATE")]
        since: Option<String>,
        /// Compare two scans directory by directory
        #[arg(long)]
        dirs: bool,
        /// Only this grant
        #[arg(long)]
        grant: Option<String>,
        /// With --dirs: how many levels below each root or grant directory
        #[arg(long, default_value_t = 2)]
        depth: usize,
        /// With --dirs: how many changes to show
        #[arg(long, default_value_t = 20)]
        top: usize,
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },
    /// Reports for grant budgets and planning
    #[command(subcommand)]
    Report(ReportCommand),
    /// Attribute Slurm compute jobs to grants
    #[command(subcommand)]
    Jobs(JobsCommand),
    /// stomp's own Slurm tasks, such as scans
    Tasks {
        #[command(subcommand)]
        action: Option<TasksCommand>,
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },
    /// Pack a directory into compressed, verified chunks and remove the originals
    ///
    /// Originals are removed one chunk at a time, only after the chunk has been
    /// read back and every file's checksum compared, and only if they haven't
    /// changed since. Runs as a Slurm task unless --here; asks first unless --yes.
    Archive {
        /// The directory to archive
        dir: PathBuf,
        /// Where to keep the archive: local (in the directory itself)
        #[arg(long, value_name = "DEST")]
        to: Option<String>,
        /// Don't ask for confirmation
        #[arg(long, short)]
        yes: bool,
        /// Show the plan without archiving anything
        #[arg(long)]
        dry_run: bool,
        /// Show the plan and estimate the compressed size, without archiving anything
        #[arg(long)]
        estimate: bool,
        /// Run here instead of submitting a Slurm task
        #[arg(long)]
        here: bool,
        /// Continue an archive that stopped partway
        #[arg(long)]
        resume: bool,
        /// zstd compression level, 1–22 [default: archive.zstd_level, 3]
        #[arg(long, value_name = "N")]
        level: Option<i32>,
        /// Largest chunk, e.g. 50GiB [default: archive.chunk_size]
        #[arg(long, value_name = "SIZE")]
        chunk_size: Option<String>,
        /// The archive this run continues (set in scripts stomp generates)
        #[arg(long, hide = true)]
        id: Option<String>,
        #[arg(long, hide = true)]
        task: Option<String>,
        #[command(flatten)]
        slurm: SlurmArgs,
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },
    /// List, inspect and verify archives
    Archives {
        #[command(subcommand)]
        action: Option<ArchivesCommand>,
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },
    /// Put archived files back
    ///
    /// Restores into the original directory unless --to is given, never
    /// replacing existing files unless --overwrite. After a complete restore
    /// of everything, the archive is removed (unless --keep-archive).
    Restore {
        /// Archive ID, or the archived directory
        archive: String,
        /// Only paths matching this pattern, relative to the archived directory (repeatable)
        #[arg(long, value_name = "PATTERN")]
        only: Vec<String>,
        /// Restore into this directory instead
        #[arg(long, value_name = "DIR")]
        to: Option<PathBuf>,
        /// Replace files that already exist
        #[arg(long)]
        overwrite: bool,
        /// Keep the archive even after restoring everything
        #[arg(long)]
        keep_archive: bool,
        /// Don't ask for confirmation
        #[arg(long, short)]
        yes: bool,
        /// Run here instead of submitting a Slurm task
        #[arg(long)]
        here: bool,
        #[arg(long, hide = true)]
        task: Option<String>,
        #[command(flatten)]
        slurm: SlurmArgs,
    },
    /// Import finished scans, job logs and archive records now (normally automatic)
    Ingest,
}

#[derive(Subcommand)]
enum ArchivesCommand {
    /// List archives (the default)
    List,
    /// Details of one archive
    Show {
        /// Archive ID, or the archived directory
        archive: String,
    },
    /// Check an archive's checksums
    Verify {
        /// Archive ID, or the archived directory
        archive: String,
        /// Also decompress every chunk and check each file's checksum
        #[arg(long)]
        deep: bool,
    },
}

impl SlurmArgs {
    fn overrides(self) -> Overrides {
        Overrides {
            time: self.time,
            cpus: self.cpus,
            mem: self.mem,
            partition: self.partition,
            account: self.account,
        }
    }
}

/// Overrides for the [slurm] settings of a submitted task.
#[derive(clap::Args)]
struct SlurmArgs {
    /// Time limit for the Slurm task, e.g. 12:00:00
    #[arg(long, value_name = "TIME")]
    time: Option<String>,
    /// CPUs for the Slurm task
    #[arg(long, value_name = "N")]
    cpus: Option<u32>,
    /// Memory for the Slurm task, e.g. 16G
    #[arg(long, value_name = "SIZE")]
    mem: Option<String>,
    /// Slurm partition
    #[arg(long)]
    partition: Option<String>,
    /// Slurm account
    #[arg(long)]
    account: Option<String>,
}

#[derive(Subcommand)]
enum TasksCommand {
    /// List tasks (the default)
    List,
    /// Details and log locations for a task
    Show { id: String },
    /// Cancel a task's Slurm job
    Cancel { id: String },
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
    /// CPU and GPU hours per grant
    Compute {
        /// First day to include (YYYY-MM-DD) [default: 30 days ago]
        #[arg(long, value_name = "DATE")]
        from: Option<String>,
        /// Last day to include (YYYY-MM-DD) [default: today]
        #[arg(long, value_name = "DATE")]
        to: Option<String>,
        /// One row per grant per month
        #[arg(long)]
        by_month: bool,
        /// Use already-imported accounting without running sacct
        #[arg(long)]
        no_sync: bool,
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },
    /// One grant's storage and compute together
    Grant {
        /// Grant ID
        id: String,
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },
    /// Large directories with nothing modified recently
    Stale {
        /// No file modified within this long (e.g. 180d) or since this date
        #[arg(long, value_name = "AGE|DATE", default_value = "180d")]
        older_than: String,
        /// Only this grant
        #[arg(long)]
        grant: Option<String>,
        /// How many directories to show
        #[arg(long, default_value_t = 20)]
        top: usize,
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },
    /// Caches and software environments, and how to slim them down
    Reclaimable {
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },
    /// Grants that have ended or are about to, and what they still use
    Lifecycle {
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
    /// Import job accounting from sacct now (normally automatic)
    Sync,
    /// Recent jobs and the grant each belongs to
    List {
        /// Only this grant
        #[arg(long)]
        grant: Option<String>,
        /// Only jobs not attributed to any grant
        #[arg(long, conflicts_with = "grant")]
        unassigned: bool,
        /// Jobs since this long ago (e.g. 7d) or this date [default: 30d]
        #[arg(long, value_name = "AGE|DATE")]
        since: Option<String>,
        /// Use already-imported accounting without running sacct
        #[arg(long)]
        no_sync: bool,
        #[arg(long, value_enum, default_value_t = Format::Table)]
        format: Format,
    },
    /// Assign jobs to a grant, overriding logs and working-directory inference
    Retag {
        /// Grant ID from your config
        grant: String,
        /// Job IDs
        #[arg(required = true)]
        jobs: Vec<String>,
    },
    /// Remove jobs' explicit grant tags
    Untag {
        /// Job IDs
        #[arg(required = true)]
        jobs: Vec<String>,
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
        Command::Jobs(JobsCommand::Sync) => {
            cmd::jobs::sync(&load_config(&config_path)?, &data_dir)?;
        }
        Command::Jobs(JobsCommand::List {
            grant,
            unassigned,
            since,
            no_sync,
            format: f,
        }) => {
            let args = cmd::jobs::ListArgs {
                grant,
                since,
                unassigned,
                no_sync,
                format: format(f),
            };
            cmd::jobs::list(&load_config(&config_path)?, &data_dir, &args)?;
        }
        Command::Jobs(JobsCommand::Retag { grant, jobs }) => {
            cmd::jobs::retag(&load_config(&config_path)?, &data_dir, &grant, &jobs)?;
        }
        Command::Jobs(JobsCommand::Untag { jobs }) => {
            cmd::jobs::untag(&data_dir, &jobs)?;
        }
        Command::Scan {
            full,
            resume,
            here,
            task,
            slurm,
        } => {
            let cfg = load_config(&config_path)?;
            start_threads(Some(&cfg));
            let args = cmd::scan::Args {
                full,
                resume,
                here,
                task,
                overrides: slurm.overrides(),
            };
            return cmd::scan::run(&cfg, &config_path, &data_dir, args);
        }
        Command::Changes {
            since,
            dirs,
            grant,
            depth,
            top,
            format: f,
        } => {
            let cfg = load_config(&config_path)?;
            start_threads(Some(&cfg));
            let args = cmd::changes::Args {
                since,
                dirs,
                grant,
                depth,
                top,
                format: format(f),
            };
            cmd::changes::run(&cfg, &data_dir, &args)?;
        }
        Command::Report(ReportCommand::Compute {
            from,
            to,
            by_month,
            no_sync,
            format: f,
        }) => {
            let args = cmd::report::ComputeArgs {
                from,
                to,
                by_month,
                no_sync,
                format: format(f),
            };
            cmd::report::compute(&load_config(&config_path)?, &data_dir, &args)?;
        }
        Command::Report(ReportCommand::Grant { id, format: f }) => {
            cmd::report::grant(&load_config(&config_path)?, &data_dir, &id, format(f))?;
        }
        Command::Report(ReportCommand::Stale {
            older_than,
            grant,
            top,
            format: f,
        }) => {
            cmd::report::stale(
                &load_config(&config_path)?,
                &data_dir,
                &older_than,
                grant.as_deref(),
                top,
                format(f),
            )?;
        }
        Command::Report(ReportCommand::Reclaimable { format: f }) => {
            cmd::report::reclaimable(&load_config(&config_path)?, &data_dir, format(f))?;
        }
        Command::Report(ReportCommand::Lifecycle { format: f }) => {
            cmd::report::lifecycle(&load_config(&config_path)?, &data_dir, format(f))?;
        }
        Command::Tasks { action, format: f } => match action {
            None | Some(TasksCommand::List) => cmd::tasks::list(&data_dir, format(f))?,
            Some(TasksCommand::Show { id }) => cmd::tasks::show(&data_dir, &id)?,
            Some(TasksCommand::Cancel { id }) => cmd::tasks::cancel(&data_dir, &id)?,
        },
        Command::Archive {
            dir,
            to,
            yes,
            dry_run,
            estimate,
            here,
            resume,
            level,
            chunk_size,
            id,
            task,
            slurm,
            format: f,
        } => {
            let cfg = load_config(&config_path)?;
            start_threads(Some(&cfg));
            let args = cmd::archive::ArchiveArgs {
                dir,
                to,
                yes,
                dry_run,
                estimate,
                here,
                resume,
                level,
                chunk_size,
                id,
                task,
                overrides: slurm.overrides(),
                format: format(f),
            };
            return cmd::archive::archive(&cfg, &config_path, &data_dir, args);
        }
        Command::Archives { action, format: f } => match action {
            None | Some(ArchivesCommand::List) => cmd::archive::list(&data_dir, format(f))?,
            Some(ArchivesCommand::Show { archive }) => cmd::archive::show(&data_dir, &archive)?,
            Some(ArchivesCommand::Verify { archive, deep }) => {
                return cmd::archive::verify(&data_dir, &archive, deep);
            }
        },
        Command::Restore {
            archive,
            only,
            to,
            overwrite,
            keep_archive,
            yes,
            here,
            task,
            slurm,
        } => {
            let cfg = load_config(&config_path)?;
            let args = cmd::archive::RestoreArgs {
                what: archive,
                only,
                to,
                overwrite,
                keep_archive,
                yes,
                here,
                task,
                overrides: slurm.overrides(),
            };
            return cmd::archive::restore(&cfg, &config_path, &data_dir, args);
        }
        Command::Ingest => {
            let catalog = cmd::open_catalog(&data_dir)?;
            println!(
                "The catalog has {} measurement(s), {} scan(s), {} job record(s) and {} archive(s).",
                catalog.measurement_count()?,
                catalog.scan_count()?,
                catalog.job_count()?,
                catalog.archives()?.len()
            );
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
