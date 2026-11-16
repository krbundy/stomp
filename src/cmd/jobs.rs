//! `stomp jobs log`: attributes Slurm jobs to grants (§15.1).

use std::path::Path;
use std::process::ExitCode;

use crate::config::Config;
use crate::jobs;
use crate::paths;

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
    let mut warnings = Vec::new();
    let known_grant = match Config::load(config_path) {
        Ok(cfg) => {
            let known = cfg.grant(grant).is_some();
            if !known {
                warnings.push(format!(
                    "grant {grant:?} is not in {}; the job is logged anyway and will show as an unknown grant until you fix it",
                    paths::display(config_path)
                ));
            }
            Some(known)
        }
        Err(e) => {
            warnings.push(format!(
                "could not read the config, so the grant ID wasn't checked: {e}"
            ));
            None
        }
    };

    match jobs::log(data_dir, grant, explicit, note, known_grant, |k| {
        std::env::var(k).ok()
    }) {
        Ok(messages) => {
            for m in messages {
                eprintln!("stomp: {m}");
            }
        }
        Err(e) => warnings.push(format!("job not logged: {e:#}")),
    }

    for w in &warnings {
        eprintln!("stomp: warning: {w}");
    }
    if strict && !warnings.is_empty() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
