//! `stomp report lifecycle`: grants that have ended or are about to, and what
//! they still hold on the HPC (§18).

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::Serialize;

use super::totals_by_grant;
use crate::cmd::{Format, emit, jobs::synced_catalog, local_time};
use crate::compute::Attributor;
use crate::config::Config;
use crate::table::{Align, Table};
use crate::units::{format_bytes, format_hours};

/// Grants ending within this many days are flagged.
const ENDING_SOON_DAYS: i64 = 90;

#[derive(Serialize)]
struct Row {
    grant: String,
    end: Option<String>,
    status: String,
    days: Option<i64>,
    storage_bytes: Option<u64>,
    /// Stored size of archives held at remote destinations.
    archived_remote_bytes: u64,
    jobs_after_end: u64,
    #[serde(serialize_with = "crate::units::round2")]
    cpu_hours_after_end: f64,
}

pub fn lifecycle(cfg: &Config, data_dir: &Path, format: Format) -> Result<()> {
    let (catalog, _) = synced_catalog(cfg, data_dir, true)?;
    let now = Timestamp::now();
    let tz = TimeZone::system();
    let today = now.to_zoned_datetime(tz.clone()).date();

    let measured = catalog.measurement_at_or_before(now.as_second())?;
    let storage: HashMap<String, (u64, u64)> = match measured {
        Some((id, _)) => totals_by_grant(cfg, &catalog.measurement_samples(id)?)
            .into_iter()
            .collect(),
        None => HashMap::new(),
    };
    let attributor = Attributor::new(cfg, catalog.tags()?);
    let jobs = catalog.jobs_in_range(None, None)?;
    let mut remote: HashMap<String, u64> = HashMap::new();
    for h in crate::cmd::usage::remote_holdings(&catalog.archives()?) {
        *remote.entry(h.grant).or_default() += h.compressed;
    }

    let mut rows: Vec<Row> = cfg
        .grants
        .iter()
        .map(|g| {
            let days = g.end.map(|end| (end - today).get_days() as i64);
            let status = match days {
                None => "no end date".to_string(),
                Some(d) if d < 0 => format!("ended {} days ago", -d),
                Some(d) if d <= ENDING_SOON_DAYS => format!("ends in {d} days"),
                Some(d) => format!("active ({d} days left)"),
            };
            let end_ts = g.end.and_then(|e| {
                e.tomorrow()
                    .ok()?
                    .to_zoned_datetime(tz.clone())
                    .ok()
                    .map(|z| z.timestamp().as_second())
            });
            let (mut jobs_after, mut cpu_after) = (0, 0.0);
            if let Some(end) = end_ts {
                for job in &jobs {
                    if job.start_time.is_some_and(|s| s >= end)
                        && attributor.attribute(job).0 == g.id
                    {
                        jobs_after += 1;
                        cpu_after += job.cpu_hours();
                    }
                }
            }
            Row {
                grant: g.id.clone(),
                end: g.end.map(|d| d.to_string()),
                status,
                days,
                storage_bytes: storage.get(&g.id).map(|v| v.0),
                archived_remote_bytes: remote.get(&g.id).copied().unwrap_or(0),
                jobs_after_end: jobs_after,
                cpu_hours_after_end: cpu_after,
            }
        })
        .collect();
    // Ended grants first (longest ago first), then by days remaining; no end date last.
    rows.sort_by_key(|r| r.days.unwrap_or(i64::MAX));

    if matches!(format, Format::Table | Format::Md) {
        match measured {
            Some((_, at)) => println!(
                "Grant end dates, with storage as of {}.\n",
                local_time(Timestamp::from_second(at)?)
            ),
            None => println!("Grant end dates. Run `stomp usage` to include storage.\n"),
        }
        if rows.is_empty() {
            println!("No grants are defined.");
            return Ok(());
        }
    }
    emit(format, &rows, || {
        let mut t = Table::new(&[
            ("GRANT", Align::Left),
            ("ENDS", Align::Left),
            ("STATUS", Align::Left),
            ("STORAGE ON HPC", Align::Right),
            ("ARCHIVED ELSEWHERE", Align::Right),
            ("JOBS AFTER END", Align::Right),
            ("CPU-H AFTER END", Align::Right),
        ]);
        for r in &rows {
            let after = r.days.is_some_and(|d| d < 0);
            t.row(vec![
                r.grant.clone(),
                r.end.clone().unwrap_or_else(|| "—".into()),
                r.status.clone(),
                r.storage_bytes.map_or("—".into(), format_bytes),
                if r.archived_remote_bytes > 0 {
                    format_bytes(r.archived_remote_bytes)
                } else {
                    "—".into()
                },
                if after {
                    r.jobs_after_end.to_string()
                } else {
                    "—".into()
                },
                if after {
                    format_hours(r.cpu_hours_after_end)
                } else {
                    "—".into()
                },
            ]);
        }
        t
    })
}
