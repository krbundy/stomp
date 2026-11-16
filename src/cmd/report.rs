//! `stomp report`: reports built from the recorded usage history (§18).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use anyhow::{Context, Result};
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::Serialize;

use super::{Format, local_time};
use crate::catalog::Catalog;
use crate::config::Config;
use crate::table::{Align, Table, csv_row};
use crate::units::format_bytes;

const UNASSIGNED: &str = "(unassigned)";
const TOTAL: &str = "TOTAL";

#[derive(Debug, Serialize)]
struct Point {
    measured_at: Timestamp,
    grant: String,
    bytes: u64,
    files: u64,
}

/// `stomp report history`: storage per grant at each recorded measurement.
pub fn history(
    cfg: &Config,
    data_dir: &Path,
    from: Option<&str>,
    to: Option<&str>,
    grant: Option<&str>,
    format: Format,
) -> Result<()> {
    let tz = TimeZone::system();
    let start = from.map(|d| day_start(d, &tz, 0)).transpose()?;
    // `--to` is inclusive, so the range ends at the start of the next day.
    let end = to.map(|d| day_start(d, &tz, 1)).transpose()?;

    let catalog = Catalog::open(data_dir)?;
    let points = points(cfg, &catalog, start, end, grant)?;
    if points.is_empty() {
        eprintln!("No recorded usage matches. Each run of `stomp usage` records a measurement.");
        return Ok(());
    }
    match format {
        Format::Json => println!("{}", serde_json::to_string_pretty(&points)?),
        Format::Csv => {
            println!("{}", csv_row(&["measured_at", "grant", "bytes", "files"]));
            for p in &points {
                println!(
                    "{}",
                    csv_row(&[
                        p.measured_at.to_string(),
                        p.grant.clone(),
                        p.bytes.to_string(),
                        p.files.to_string(),
                    ])
                );
            }
        }
        Format::Table => print!("{}", pivot(&points)),
    }
    Ok(())
}

fn day_start(text: &str, tz: &TimeZone, offset_days: i64) -> Result<i64> {
    let date: Date = text
        .parse()
        .with_context(|| format!("{text:?} is not a date; use YYYY-MM-DD"))?;
    let date = if offset_days == 0 {
        date
    } else {
        date.tomorrow()?
    };
    Ok(date.to_zoned_datetime(tz.clone())?.timestamp().as_second())
}

/// Totals per measurement and label, attributing directories with the current config (§8.2).
fn points(
    cfg: &Config,
    catalog: &Catalog,
    start: Option<i64>,
    end: Option<i64>,
    only: Option<&str>,
) -> Result<Vec<Point>> {
    let current: HashMap<String, &str> = cfg
        .grants
        .iter()
        .flat_map(|g| {
            g.dirs
                .iter()
                .map(move |d| (d.path.to_string_lossy().into_owned(), g.id.as_str()))
        })
        .collect();

    let mut totals: BTreeMap<(i64, i64), BTreeMap<String, (u64, u64)>> = BTreeMap::new();
    for s in catalog.samples(start, end)? {
        let label = match s.kind.as_str() {
            "grant_dir" => current
                .get(&s.path)
                .map(|g| g.to_string())
                .or(s.grant_id)
                .unwrap_or_else(|| "?".to_string()),
            "unassigned" => UNASSIGNED.to_string(),
            "root" => TOTAL.to_string(),
            _ => continue,
        };
        let entry = totals
            .entry((s.taken_at, s.measurement_id))
            .or_default()
            .entry(label)
            .or_default();
        entry.0 += s.bytes;
        entry.1 += s.files;
    }

    let mut points = Vec::new();
    for ((taken_at, _), labels) in totals {
        let measured_at = Timestamp::from_second(taken_at)?;
        for (grant, (bytes, files)) in labels {
            if only.is_none_or(|g| g == grant) {
                points.push(Point {
                    measured_at,
                    grant,
                    bytes,
                    files,
                });
            }
        }
    }
    Ok(points)
}

/// One row per measurement, one column per grant.
fn pivot(points: &[Point]) -> String {
    let mut labels: Vec<&str> = points
        .iter()
        .map(|p| p.grant.as_str())
        .filter(|l| *l != UNASSIGNED && *l != TOTAL)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    for special in [UNASSIGNED, TOTAL] {
        if points.iter().any(|p| p.grant == special) {
            labels.push(special);
        }
    }
    let mut columns = vec![("MEASURED", Align::Left)];
    columns.extend(labels.iter().map(|l| (*l, Align::Right)));
    let mut table = Table::new(&columns);

    let mut rows: BTreeMap<Timestamp, HashMap<&str, u64>> = BTreeMap::new();
    for p in points {
        rows.entry(p.measured_at)
            .or_default()
            .insert(p.grant.as_str(), p.bytes);
    }
    for (when, values) in rows {
        let mut cells = vec![local_time(when)];
        cells.extend(labels.iter().map(|l| {
            values
                .get(l)
                .map_or_else(|| "—".to_string(), |b| format_bytes(*b))
        }));
        table.row(cells);
    }
    table.render()
}
