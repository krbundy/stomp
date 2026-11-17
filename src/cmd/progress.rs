//! Progress for long-running commands: a line on an interactive terminal,
//! and a JSON file that `stomp tasks` reads for Slurm tasks.

use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use signal_hook::consts::{SIGINT, SIGTERM, SIGUSR1};

/// A flag set by the signals that should stop work cleanly: USR1, which Slurm
/// sends shortly before a task's time limit (§14.2), TERM and Ctrl-C.
pub fn stop_flag() -> Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [SIGUSR1, SIGTERM, SIGINT] {
        signal_hook::flag::register(signal, Arc::clone(&stop))?;
    }
    Ok(stop)
}

/// Reports progress every 10 seconds until `finished` is set. `sample`
/// returns the line to show and the JSON to write.
pub fn report(
    finished: &AtomicBool,
    file: Option<&Path>,
    sample: impl Fn() -> (String, serde_json::Value),
) {
    let interactive = std::io::stderr().is_terminal();
    let mut last = Instant::now();
    loop {
        let done = finished.load(Ordering::Relaxed);
        if done || last.elapsed() >= Duration::from_secs(10) {
            last = Instant::now();
            let (line, mut json) = sample();
            if let Some(path) = file {
                json["updated_at"] = jiff::Timestamp::now().to_string().into();
                let tmp = path.with_extension("tmp");
                if std::fs::write(&tmp, json.to_string()).is_ok() {
                    let _ = std::fs::rename(&tmp, path);
                }
            }
            if interactive && !done {
                eprint!("\rstomp: {line}   ");
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
