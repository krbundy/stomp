//! `stomp tasks`: stomp's own Slurm work (§14.3).

use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use super::{Format, emit, short_local};
use crate::table::{Align, Table};
use crate::tasks::{self, Status, TaskState};
use crate::units::{format_bytes, format_count};
use crate::{paths, scan};

#[derive(Serialize)]
struct Row {
    id: String,
    kind: String,
    status: String,
    slurm_job: String,
    queue: Option<String>,
    updated_at: String,
    progress: Option<serde_json::Value>,
    message: Option<String>,
}

fn progress(data_dir: &Path, id: &str) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(tasks::progress_path(data_dir, id)).ok()?;
    serde_json::from_str(&text).ok()
}

fn progress_text(p: &serde_json::Value) -> String {
    let n = |k: &str| p.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    format!(
        "{} files, {}",
        format_count(n("files")),
        format_bytes(n("bytes"))
    )
}

/// The status to show: a task that stopped without reporting is probably dead.
fn shown_status(task: &TaskState, queue: Option<&String>) -> String {
    match (task.status, queue) {
        (s, _) if s.is_final() || s == Status::Incomplete => s.label().to_string(),
        (_, Some(q)) => q.to_lowercase(),
        (s, None) => format!("{} (not in queue; see its log)", s.label()),
    }
}

pub fn list(data_dir: &Path, format: Format) -> Result<()> {
    // This is where people look once a task is done, so its results are
    // imported here too. Best effort: the listing matters more.
    let _ = super::open_catalog(data_dir);
    let all = tasks::list(data_dir)?;
    let active: Vec<String> = all
        .iter()
        .filter(|t| !t.status.is_final())
        .filter_map(|t| t.slurm_jobs.last().cloned())
        .collect();
    let queue = tasks::queue_states(&active);
    let rows: Vec<Row> = all
        .iter()
        .map(|t| {
            let job = t.slurm_jobs.last().cloned().unwrap_or_default();
            let q = queue.get(&job);
            Row {
                id: t.id.clone(),
                kind: t.kind.clone(),
                status: shown_status(t, q),
                queue: q.cloned(),
                slurm_job: job,
                updated_at: t.updated_at.clone(),
                progress: progress(data_dir, &t.id),
                message: t.message.clone(),
            }
        })
        .collect();

    let pending = scan::list_results(&scan::inbox_dir(data_dir))?
        .into_iter()
        .filter(|r| r.meta.status != "complete")
        .count();
    if matches!(format, Format::Table | Format::Md) {
        if rows.is_empty() {
            println!("No tasks yet. `stomp scan` submits one.");
        }
        if pending > 0 {
            println!(
                "{pending} interrupted scan(s) can be continued with `stomp scan --resume`.\n"
            );
        }
        if rows.is_empty() {
            return Ok(());
        }
    }
    emit(format, &rows, || {
        let mut t = Table::new(&[
            ("TASK", Align::Left),
            ("STATUS", Align::Left),
            ("SLURM JOB", Align::Left),
            ("UPDATED", Align::Left),
            ("PROGRESS", Align::Left),
        ]);
        for r in &rows {
            t.row(vec![
                r.id.clone(),
                r.status.clone(),
                r.slurm_job.clone(),
                short_local(&r.updated_at),
                r.progress.as_ref().map(progress_text).unwrap_or_default(),
            ]);
        }
        t
    })
}

pub fn show(data_dir: &Path, id: &str) -> Result<()> {
    let task = tasks::load(data_dir, id)?;
    let dir = tasks::task_dir(data_dir, id);
    let queue = tasks::queue_states(&task.slurm_jobs);
    println!("Task      {}", task.id);
    println!("Kind      {}", task.kind);
    println!(
        "Status    {}",
        shown_status(&task, task.slurm_jobs.last().and_then(|j| queue.get(j)))
    );
    println!("Created   {}", short_local(&task.created_at));
    println!("Updated   {}", short_local(&task.updated_at));
    println!("Jobs      {}", task.slurm_jobs.join(", "));
    if task.resubmits > 0 {
        println!("Resubmits {}", task.resubmits);
    }
    if let Some(p) = progress(data_dir, id) {
        println!("Progress  {}", progress_text(&p));
    }
    if let Some(m) = &task.message {
        println!("Message   {m}");
    }
    println!("Script    {}", paths::display(&dir.join("task.sh")));
    for job in &task.slurm_jobs {
        let log = dir.join(format!("slurm-{job}.out"));
        if log.exists() {
            println!("Log       {}", paths::display(&log));
        }
    }
    Ok(())
}

pub fn cancel(data_dir: &Path, id: &str) -> Result<()> {
    let task = tasks::cancel(data_dir, id)?;
    let text = format!(
        "Cancelled task {} (Slurm job {}). {}",
        task.id,
        task.slurm_jobs.last().map_or("?", String::as_str),
        after_cancel(&task.kind)
    );
    println!("{}", text.trim_end());
    Ok(())
}

/// What's left after cancelling a task of this kind, and how to go on.
fn after_cancel(kind: &str) -> &'static str {
    match kind {
        "scan" => "Its finished parts are kept; `stomp scan --resume` continues it.",
        "archive" => {
            "Everything removed so far is safe in the archive; `stomp archive --resume DIR` \
             continues it, and `stomp restore DIR` undoes it."
        }
        "restore" => "Files restored so far stay; running the restore again continues it.",
        "move" => {
            "Every chunk is still in one place or the other; running the move again continues it."
        }
        _ => "",
    }
}
