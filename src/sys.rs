//! Small helpers for facts about the running system.

use std::env;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// This machine's hostname.
pub fn hostname() -> String {
    rustix::system::uname()
        .nodename()
        .to_string_lossy()
        .into_owned()
}

/// The current user's login name, from `$USER` or `$LOGNAME`.
pub fn user_name() -> Option<String> {
    ["USER", "LOGNAME"]
        .iter()
        .find_map(|var| env::var(var).ok().filter(|v| !v.is_empty()))
}

/// Finds an executable on `$PATH`, like `which`.
pub fn which(program: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| {
            candidate
                .metadata()
                .is_ok_and(|md| md.is_file() && md.permissions().mode() & 0o111 != 0)
        })
}

/// How many worker threads stomp should use: the configured maximum, further
/// capped by the CPUs Slurm allocated when running inside a job (§9.2).
pub fn effective_threads(configured: usize) -> usize {
    let slurm_cpus = env::var("SLURM_CPUS_PER_TASK")
        .ok()
        .and_then(|v| v.parse::<usize>().ok());
    slurm_cpus
        .map_or(configured, |cpus| cpus.min(configured))
        .max(1)
}
