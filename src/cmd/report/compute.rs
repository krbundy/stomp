//! `stomp report compute`: compute use per grant (§15.3, §15.4).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use jiff::Timestamp;
use serde::Serialize;

use crate::cmd::{Format, day_start, emit, jobs::synced_catalog, local_date, local_time};
use crate::compute::{Attributor, By, Job, STOMP, UNASSIGNED};
use crate::config::{Config, Rates};
use crate::table::{Align, Table};
use crate::units::format_hours;

pub struct ComputeArgs {
    pub from: Option<String>,
    pub to: Option<String>,
    pub by_month: bool,
    pub no_sync: bool,
    pub format: Format,
}

#[derive(Debug, Default, Serialize)]
pub struct Row {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub month: Option<String>,
    pub grant: String,
    pub jobs: u64,
    /// Jobs that ended in any state but COMPLETED (failed, cancelled, timed out…).
    pub not_completed: u64,
    pub running: u64,
    pub cpu_hours: f64,
    pub gpu_hours: f64,
    pub billing_hours: Option<f64>,
    pub estimated_cost: Option<f64>,
    pub by_log: u64,
    pub by_task: u64,
    pub by_workdir: u64,
}

impl Row {
    fn add(&mut self, job: &Job, by: By, rates: &Rates) {
        self.jobs += 1;
        if !job.finished() {
            self.running += 1;
        } else if job.state != "COMPLETED" {
            self.not_completed += 1;
        }
        self.cpu_hours += job.cpu_hours();
        self.gpu_hours += job.gpu_hours();
        if let Some(b) = job.billing_hours() {
            *self.billing_hours.get_or_insert(0.0) += b;
        }
        if rates.per_cpu_hour.is_some() || rates.per_gpu_hour.is_some() {
            *self.estimated_cost.get_or_insert(0.0) += job.cpu_hours()
                * rates.per_cpu_hour.unwrap_or(0.0)
                + job.gpu_hours() * rates.per_gpu_hour.unwrap_or(0.0);
        }
        match by {
            By::Log => self.by_log += 1,
            By::Task => self.by_task += 1,
            By::Workdir => self.by_workdir += 1,
            By::None => {}
        }
    }

    fn attributed_by(&self) -> String {
        let parts: Vec<String> = [
            ("log", self.by_log),
            ("workdir", self.by_workdir),
            ("task", self.by_task),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(label, n)| format!("{label} {n}"))
        .collect();
        if parts.is_empty() {
            "—".to_string()
        } else {
            parts.join(", ")
        }
    }
}

/// Builds per-grant (and optionally per-month) rows for the jobs in range.
pub fn rows(cfg: &Config, jobs: &[Job], attributor: &Attributor, by_month: bool) -> Vec<Row> {
    let mut grouped: BTreeMap<(Option<String>, String), Row> = BTreeMap::new();
    for job in jobs {
        let (grant, by) = attributor.attribute(job);
        let month = by_month.then(|| {
            let when = job.end_time.or(job.start_time).unwrap_or_default();
            local_date(when).get(..7).unwrap_or_default().to_string()
        });
        let row = grouped
            .entry((month.clone(), grant.clone()))
            .or_insert_with(|| Row {
                month,
                grant,
                ..Row::default()
            });
        row.add(job, by, &cfg.rates);
    }
    // Largest first within each month; stomp's overhead and unassigned last.
    let mut rows: Vec<Row> = grouped.into_values().collect();
    let rank = |r: &Row| match r.grant.as_str() {
        UNASSIGNED => 2,
        STOMP => 1,
        _ => 0,
    };
    rows.sort_by(|a, b| {
        a.month
            .cmp(&b.month)
            .then(rank(a).cmp(&rank(b)))
            .then(b.cpu_hours.total_cmp(&a.cpu_hours))
            .then(a.grant.cmp(&b.grant))
    });
    rows
}

pub fn compute(cfg: &Config, data_dir: &Path, args: &ComputeArgs) -> Result<()> {
    let now = Timestamp::now().as_second();
    let from = match &args.from {
        Some(d) => day_start(d, false)?,
        None => now - 30 * 86_400,
    };
    let to = match &args.to {
        Some(d) => day_start(d, true)?,
        None => now + 1,
    };
    let (catalog, synced) = synced_catalog(cfg, data_dir, !args.no_sync)?;
    let jobs = catalog.jobs_in_range(Some(from), Some(to))?;
    let attributor = Attributor::new(cfg, catalog.tags()?);
    let rows = rows(cfg, &jobs, &attributor, args.by_month);

    if matches!(args.format, Format::Table | Format::Md) {
        let synced = synced.map_or("never".to_string(), |t| {
            local_time(Timestamp::from_second(t).unwrap_or_default())
        });
        let heading = format!(
            "Compute by grant, {} to {} (jobs that ended in the period, plus running jobs; sacct synced {synced})",
            local_date(from),
            local_date(to - 1),
        );
        println!("{heading}\n");
        if rows.is_empty() {
            println!("No jobs in this period.");
            return Ok(());
        }
    }
    emit(args.format, &rows, || table(&rows, args.by_month))
}

fn table(rows: &[Row], by_month: bool) -> Table {
    let billing = rows.iter().any(|r| r.billing_hours.is_some());
    let cost = rows.iter().any(|r| r.estimated_cost.is_some());
    let mut columns = Vec::new();
    if by_month {
        columns.push(("MONTH", Align::Left));
    }
    columns.extend([
        ("GRANT", Align::Left),
        ("JOBS", Align::Right),
        ("NOT COMPLETED", Align::Right),
        ("CPU-HOURS", Align::Right),
        ("GPU-HOURS", Align::Right),
    ]);
    if billing {
        columns.push(("BILLING-HOURS", Align::Right));
    }
    if cost {
        columns.push(("EST. COST", Align::Right));
    }
    columns.push(("ATTRIBUTED BY", Align::Left));
    let mut t = Table::new(&columns);
    let mut total = Row::default();
    for r in rows {
        let mut cells = Vec::new();
        if by_month {
            cells.push(r.month.clone().unwrap_or_default());
        }
        cells.extend([
            r.grant.clone(),
            r.jobs.to_string(),
            r.not_completed.to_string(),
            format_hours(r.cpu_hours),
            format_hours(r.gpu_hours),
        ]);
        if billing {
            cells.push(r.billing_hours.map(format_hours).unwrap_or_default());
        }
        if cost {
            cells.push(
                r.estimated_cost
                    .map(|c| format!("{c:.2}"))
                    .unwrap_or_default(),
            );
        }
        cells.push(r.attributed_by());
        t.row(cells);
        total.jobs += r.jobs;
        total.not_completed += r.not_completed;
        total.cpu_hours += r.cpu_hours;
        total.gpu_hours += r.gpu_hours;
        if let Some(b) = r.billing_hours {
            *total.billing_hours.get_or_insert(0.0) += b;
        }
        if let Some(c) = r.estimated_cost {
            *total.estimated_cost.get_or_insert(0.0) += c;
        }
    }
    t.rule();
    let mut cells = Vec::new();
    if by_month {
        cells.push(String::new());
    }
    cells.extend([
        "TOTAL".to_string(),
        total.jobs.to_string(),
        total.not_completed.to_string(),
        format_hours(total.cpu_hours),
        format_hours(total.gpu_hours),
    ]);
    if billing {
        cells.push(total.billing_hours.map(format_hours).unwrap_or_default());
    }
    if cost {
        cells.push(
            total
                .estimated_cost
                .map(|c| format!("{c:.2}"))
                .unwrap_or_default(),
        );
    }
    cells.push(String::new());
    t.row(cells);
    t
}
