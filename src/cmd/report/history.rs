//! `stomp report history`: storage per grant at each recorded measurement,
//! with TB-months over the period (§8.2).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use anyhow::Result;
use jiff::Timestamp;
use serde::Serialize;

use crate::catalog::{Catalog, Sample};
use crate::cmd::{Format, day_start, local_time, open_catalog};
use crate::config::Config;
use crate::table::{Align, Document, Table, csv_from_rows};
use crate::units::format_bytes;

pub const UNASSIGNED: &str = "(unassigned)";
pub const TOTAL: &str = "TOTAL";

/// Seconds in an average month, for TB-months.
const MONTH_SECONDS: f64 = 30.436_875 * 86_400.0;

#[derive(Debug, Serialize)]
struct Point {
    measured_at: Timestamp,
    grant: String,
    bytes: u64,
    files: u64,
}

/// Sums samples per grant (by the current config, §8.2), plus unassigned and total.
pub fn totals_by_grant(cfg: &Config, samples: &[Sample]) -> BTreeMap<String, (u64, u64)> {
    let current: HashMap<String, &str> = cfg
        .grants
        .iter()
        .flat_map(|g| {
            g.dirs
                .iter()
                .map(move |d| (d.path.to_string_lossy().into_owned(), g.id.as_str()))
        })
        .collect();
    let mut totals: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for s in samples {
        let label = match s.kind.as_str() {
            "grant_dir" => current
                .get(&s.path)
                .map(|g| g.to_string())
                .or_else(|| s.grant_id.clone())
                .unwrap_or_else(|| "?".to_string()),
            "unassigned" => UNASSIGNED.to_string(),
            "root" => TOTAL.to_string(),
            _ => continue,
        };
        let entry = totals.entry(label).or_default();
        entry.0 += s.bytes;
        entry.1 += s.files;
    }
    totals
}

pub fn history(
    cfg: &Config,
    data_dir: &Path,
    from: Option<&str>,
    to: Option<&str>,
    grant: Option<&str>,
    format: Format,
) -> Result<()> {
    let start = from.map(|d| day_start(d, false)).transpose()?;
    // `--to` is inclusive, so the range ends at the start of the next day.
    let end = to.map(|d| day_start(d, true)).transpose()?;
    let catalog = open_catalog(data_dir)?;
    let points = points(cfg, &catalog, start, end, grant)?;
    if points.is_empty() {
        eprintln!("No recorded usage matches. Each run of `stomp usage` records a measurement.");
        return Ok(());
    }
    match format {
        Format::Json => println!("{}", serde_json::to_string_pretty(&points)?),
        Format::Csv => print!("{}", csv_from_rows(&points)?),
        Format::Table | Format::Md => {
            let markdown = format == Format::Md;
            let mut doc = Document::default();
            doc.table(pivot(&points));
            let months = tb_months(&points);
            if !months.is_empty() {
                doc.heading("TB-months over this period");
                let rate = cfg.rates.per_tb_month;
                let mut columns = vec![("GRANT", Align::Left), ("TB-MONTHS", Align::Right)];
                if rate.is_some() {
                    columns.push(("ESTIMATED COST", Align::Right));
                }
                let mut table = Table::new(&columns);
                for (grant, value) in months {
                    let mut cells = vec![grant, format!("{value:.2}")];
                    if let Some(rate) = rate {
                        cells.push(format!("{:.2}", value * rate));
                    }
                    table.row(cells);
                }
                doc.table(table);
                doc.text("Storage integrated over time between measurements; run `stomp usage` regularly (or schedule it) for accurate numbers.");
            }
            print!("{}", doc.render(markdown));
        }
    }
    Ok(())
}

fn points(
    cfg: &Config,
    catalog: &Catalog,
    start: Option<i64>,
    end: Option<i64>,
    only: Option<&str>,
) -> Result<Vec<Point>> {
    let mut by_measurement: BTreeMap<(i64, i64), Vec<Sample>> = BTreeMap::new();
    for s in catalog.samples(start, end)? {
        by_measurement
            .entry((s.taken_at, s.measurement_id))
            .or_default()
            .push(s);
    }
    let mut points = Vec::new();
    for ((taken_at, _), samples) in by_measurement {
        let measured_at = Timestamp::from_second(taken_at)?;
        for (grant, (bytes, files)) in totals_by_grant(cfg, &samples) {
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

/// Storage integrated over time per label (trapezoid rule), in TB-months.
fn tb_months(points: &[Point]) -> Vec<(String, f64)> {
    let mut series: BTreeMap<&str, Vec<(i64, u64)>> = BTreeMap::new();
    for p in points {
        series
            .entry(p.grant.as_str())
            .or_default()
            .push((p.measured_at.as_second(), p.bytes));
    }
    let mut out: Vec<(String, f64)> = series
        .into_iter()
        .filter(|(_, s)| s.len() >= 2)
        .map(|(grant, s)| {
            let byte_seconds: f64 = s
                .windows(2)
                .map(|w| (w[0].1 as f64 + w[1].1 as f64) / 2.0 * (w[1].0 - w[0].0) as f64)
                .sum();
            (grant.to_string(), byte_seconds / 1e12 / MONTH_SECONDS)
        })
        .collect();
    // Grants first, then unassigned and the total, as in the table above.
    out.sort_by_key(|(label, _)| match label.as_str() {
        UNASSIGNED => 1,
        TOTAL => 2,
        _ => 0,
    });
    out
}

/// One row per measurement, one column per grant.
fn pivot(points: &[Point]) -> Table {
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
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integrates_tb_months() {
        let point = |t: i64, bytes: u64| Point {
            measured_at: Timestamp::from_second(t).unwrap(),
            grant: "A".into(),
            bytes,
            files: 0,
        };
        // A steady 2 TB for one average month is 2 TB-months.
        let month = MONTH_SECONDS as i64;
        let result = tb_months(&[point(0, 2_000_000_000_000), point(month, 2_000_000_000_000)]);
        assert_eq!(result.len(), 1);
        assert!((result[0].1 - 2.0).abs() < 1e-6, "{result:?}");
        // Growing linearly from 0 to 2 TB averages 1 TB.
        let result = tb_months(&[point(0, 0), point(month, 2_000_000_000_000)]);
        assert!((result[0].1 - 1.0).abs() < 1e-6);
        assert!(tb_months(&[point(0, 5)]).is_empty());
    }
}
