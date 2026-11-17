//! `stomp report grant <ID>`: one grant's storage and compute together,
//! suitable for a progress report or data management plan (§18).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use serde::Serialize;

use super::totals_by_grant;
use crate::cmd::{Format, jobs::synced_catalog, local_date, local_time};
use crate::compute::Attributor;
use crate::config::Config;
use crate::paths;
use crate::scan::hist::{AGE_LABELS, Agg, Count, SIZE_LABELS, age_profile};
use crate::table::{Align, Document, Table};
use crate::units::{format_bytes, format_count, format_hours};

const YEAR: i64 = 365 * 86_400;
const TOP: usize = 10;

#[derive(Serialize)]
struct Report {
    id: String,
    title: Option<String>,
    pi: Option<String>,
    start: Option<String>,
    end: Option<String>,
    storage: Option<Storage>,
    storage_by_month: Vec<MonthStorage>,
    scan: Option<ScanSection>,
    compute_by_month: Vec<MonthCompute>,
}

#[derive(Serialize)]
struct Storage {
    measured_at: Timestamp,
    bytes: u64,
    files: u64,
    dirs: Vec<DirValue>,
}

#[derive(Serialize)]
struct DirValue {
    path: String,
    bytes: u64,
    files: u64,
}

#[derive(Serialize)]
struct MonthStorage {
    month: String,
    bytes: u64,
}

#[derive(Serialize)]
struct ScanSection {
    scanned_at: Timestamp,
    ages: Vec<Bucket>,
    sizes: Vec<Bucket>,
    excluded: Vec<ExcludedDir>,
    file_types_from: Option<Timestamp>,
    file_types: Vec<FileType>,
    largest_files: Vec<LargeFile>,
}

#[derive(Serialize)]
struct Bucket {
    label: &'static str,
    files: u64,
    bytes: u64,
}

#[derive(Serialize)]
struct ExcludedDir {
    path: String,
    kind: &'static str,
    bytes: u64,
}

#[derive(Serialize)]
struct FileType {
    extension: String,
    files: u64,
    bytes: u64,
}

#[derive(Serialize)]
struct LargeFile {
    path: String,
    bytes: u64,
    modified: String,
}

#[derive(Serialize, Default)]
struct MonthCompute {
    month: String,
    jobs: u64,
    cpu_hours: f64,
    gpu_hours: f64,
}

pub fn grant(cfg: &Config, data_dir: &Path, id: &str, format: Format) -> Result<()> {
    if format == Format::Csv {
        bail!("the grant report has several sections; use --format table, md or json");
    }
    let grant = cfg
        .grant(id)
        .with_context(|| format!("no grant {id:?} in {}", paths::display(&cfg.source)))?;
    let (catalog, _) = synced_catalog(cfg, data_dir, true)?;
    let now = Timestamp::now().as_second();

    // Storage now, from the newest measurement.
    let storage = match catalog.measurement_at_or_before(now)? {
        Some((measurement, taken_at)) => {
            let samples = catalog.measurement_samples(measurement)?;
            let dirs: Vec<DirValue> = grant
                .dirs
                .iter()
                .filter_map(|d| {
                    let path = d.path.to_string_lossy();
                    samples
                        .iter()
                        .find(|s| s.kind == "grant_dir" && s.path == path)
                        .map(|s| DirValue {
                            path: paths::display(&d.path),
                            bytes: s.bytes,
                            files: s.files,
                        })
                })
                .collect();
            Some(Storage {
                measured_at: Timestamp::from_second(taken_at)?,
                bytes: dirs.iter().map(|d| d.bytes).sum(),
                files: dirs.iter().map(|d| d.files).sum(),
                dirs,
            })
        }
        None => None,
    };

    // The last measurement of each month over the past year.
    let mut by_measurement: BTreeMap<(i64, i64), Vec<_>> = BTreeMap::new();
    for s in catalog.samples(Some(now - YEAR), None)? {
        by_measurement
            .entry((s.taken_at, s.measurement_id))
            .or_default()
            .push(s);
    }
    let mut monthly: BTreeMap<String, u64> = BTreeMap::new();
    for ((taken_at, _), samples) in by_measurement {
        let month = local_date(taken_at)
            .get(..7)
            .unwrap_or_default()
            .to_string();
        let bytes = totals_by_grant(cfg, &samples).get(id).map_or(0, |v| v.0);
        monthly.insert(month, bytes); // later measurements overwrite earlier ones
    }

    // Age and size profiles from the newest scan; grant rows are kept even in pruned scans.
    let scan = match catalog.latest_scan(false)? {
        Some(info) => {
            let mut agg = Agg::default();
            let mut excluded = Vec::new();
            for d in &grant.dirs {
                if let Some(row) = catalog.dir_row(info.id, &d.path)? {
                    agg.merge(&row.agg);
                }
                if info.detail != "pruned" {
                    for row in catalog.dir_rows(info.id, Some(&d.path))? {
                        if let Some(kind) = row.excluded {
                            excluded.push(ExcludedDir {
                                path: paths::display(&row.path),
                                kind: kind.as_str(),
                                bytes: row.agg.bytes,
                            });
                        }
                    }
                }
            }
            excluded.sort_by_key(|e| std::cmp::Reverse(e.bytes));
            let bucket = |labels: &[&'static str; 5], counts: [Count; 5]| -> Vec<Bucket> {
                labels
                    .iter()
                    .zip(counts)
                    .map(|(label, c)| Bucket {
                        label,
                        files: c.files,
                        bytes: c.bytes,
                    })
                    .collect()
            };
            let stats_scan = catalog.latest_file_stats_scan()?;
            let (file_types, largest_files) = match &stats_scan {
                Some(s) => (
                    catalog
                        .scan_extensions(s.id, "grant", id)?
                        .into_iter()
                        .take(TOP)
                        .map(|(extension, c)| FileType {
                            extension,
                            files: c.files,
                            bytes: c.bytes,
                        })
                        .collect(),
                    catalog
                        .scan_largest(s.id, "grant", id)?
                        .into_iter()
                        // Small files would only pad the list.
                        .filter(|(_, bytes, _)| *bytes >= 1 << 20)
                        .take(TOP)
                        .map(|(path, bytes, mtime)| LargeFile {
                            path: paths::display(&path),
                            bytes,
                            modified: mtime.map(local_date).unwrap_or_default(),
                        })
                        .collect(),
                ),
                None => (Vec::new(), Vec::new()),
            };
            Some(ScanSection {
                scanned_at: Timestamp::from_second(info.started_at)?,
                ages: bucket(&AGE_LABELS, age_profile(&agg.months, now)),
                sizes: bucket(&SIZE_LABELS, agg.sizes),
                excluded,
                file_types_from: stats_scan
                    .map(|s| Timestamp::from_second(s.started_at))
                    .transpose()?,
                file_types,
                largest_files,
            })
        }
        None => None,
    };

    // Compute per month over the past year.
    let attributor = Attributor::new(cfg, catalog.tags()?);
    let mut compute: BTreeMap<String, MonthCompute> = BTreeMap::new();
    for job in catalog.jobs_in_range(Some(now - YEAR), None)? {
        if attributor.attribute(&job).0 != id {
            continue;
        }
        let when = job.end_time.or(job.start_time).unwrap_or(now);
        let month = local_date(when).get(..7).unwrap_or_default().to_string();
        let entry = compute
            .entry(month.clone())
            .or_insert_with(|| MonthCompute {
                month,
                ..MonthCompute::default()
            });
        entry.jobs += 1;
        entry.cpu_hours += job.cpu_hours();
        entry.gpu_hours += job.gpu_hours();
    }

    let report = Report {
        id: grant.id.clone(),
        title: grant.title.clone(),
        pi: grant.pi.clone(),
        start: grant.start.map(|d| d.to_string()),
        end: grant.end.map(|d| d.to_string()),
        storage,
        storage_by_month: monthly
            .into_iter()
            .map(|(month, bytes)| MonthStorage { month, bytes })
            .collect(),
        scan,
        compute_by_month: compute.into_values().collect(),
    };
    match format {
        Format::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        _ => print!("{}", render(&report).render(format == Format::Md)),
    }
    Ok(())
}

fn share(part: u64, whole: u64) -> String {
    if whole == 0 {
        "—".to_string()
    } else {
        format!("{:.0}%", part as f64 * 100.0 / whole as f64)
    }
}

fn render(r: &Report) -> Document {
    let mut doc = Document::default();
    doc.title(match &r.title {
        Some(t) => format!("Grant {}: {t}", r.id),
        None => format!("Grant {}", r.id),
    });
    let mut facts = Vec::new();
    if let Some(pi) = &r.pi {
        facts.push(format!("PI: {pi}"));
    }
    match (&r.start, &r.end) {
        (Some(s), Some(e)) => facts.push(format!("{s} to {e}")),
        (None, Some(e)) => facts.push(format!("ends {e}")),
        (Some(s), None) => facts.push(format!("from {s}")),
        (None, None) => {}
    }
    if !facts.is_empty() {
        doc.text(facts.join(" · "));
    }

    doc.heading("Storage");
    match &r.storage {
        Some(s) => {
            doc.text(format!("Measured {}.", local_time(s.measured_at)));
            let mut t = Table::new(&[
                ("DIRECTORY", Align::Left),
                ("SIZE", Align::Right),
                ("FILES", Align::Right),
            ]);
            for d in &s.dirs {
                t.row(vec![
                    d.path.clone(),
                    format_bytes(d.bytes),
                    format_count(d.files),
                ]);
            }
            if s.dirs.len() > 1 {
                t.rule();
                t.row(vec![
                    "TOTAL".into(),
                    format_bytes(s.bytes),
                    format_count(s.files),
                ]);
            }
            doc.table(t);
        }
        None => doc.text("No measurements yet; run `stomp usage`."),
    }
    if r.storage_by_month.len() > 1 {
        doc.heading("Storage by month");
        let mut t = Table::new(&[("MONTH", Align::Left), ("SIZE", Align::Right)]);
        for m in &r.storage_by_month {
            t.row(vec![m.month.clone(), format_bytes(m.bytes)]);
        }
        doc.table(t);
    }

    match &r.scan {
        Some(scan) => {
            let total: u64 = scan.ages.iter().map(|b| b.bytes).sum();
            doc.heading("Data age");
            doc.text(format!(
                "By last modification, from the scan of {}. Software environments and caches aren't included.",
                local_time(scan.scanned_at)
            ));
            let mut t = Table::new(&[
                ("LAST MODIFIED", Align::Left),
                ("SIZE", Align::Right),
                ("FILES", Align::Right),
                ("SHARE", Align::Right),
            ]);
            for b in &scan.ages {
                t.row(vec![
                    b.label.to_string(),
                    format_bytes(b.bytes),
                    format_count(b.files),
                    share(b.bytes, total),
                ]);
            }
            doc.table(t);

            doc.heading("File sizes");
            let mut t = Table::new(&[
                ("FILE SIZE", Align::Left),
                ("FILES", Align::Right),
                ("TOTAL SIZE", Align::Right),
            ]);
            for b in &scan.sizes {
                t.row(vec![
                    b.label.to_string(),
                    format_count(b.files),
                    format_bytes(b.bytes),
                ]);
            }
            doc.table(t);

            if !scan.excluded.is_empty() {
                doc.heading("Environments and caches");
                doc.text("Counted in the grant's storage but not scanned or archived.");
                let mut t = Table::new(&[
                    ("KIND", Align::Left),
                    ("SIZE", Align::Right),
                    ("PATH", Align::Left),
                ]);
                for e in &scan.excluded {
                    t.row(vec![
                        e.kind.to_string(),
                        format_bytes(e.bytes),
                        e.path.clone(),
                    ]);
                }
                doc.table(t);
            }
            if !scan.file_types.is_empty() {
                doc.heading("File types");
                if let Some(from) = scan.file_types_from {
                    doc.text(format!(
                        "Top {TOP}, from the full scan of {}.",
                        local_time(from)
                    ));
                }
                let mut t = Table::new(&[
                    ("EXTENSION", Align::Left),
                    ("FILES", Align::Right),
                    ("SIZE", Align::Right),
                ]);
                for f in &scan.file_types {
                    t.row(vec![
                        f.extension.clone(),
                        format_count(f.files),
                        format_bytes(f.bytes),
                    ]);
                }
                doc.table(t);
            }
            if !scan.largest_files.is_empty() {
                doc.heading("Largest files");
                let mut t = Table::new(&[
                    ("SIZE", Align::Right),
                    ("MODIFIED", Align::Left),
                    ("PATH", Align::Left),
                ]);
                for f in &scan.largest_files {
                    t.row(vec![
                        format_bytes(f.bytes),
                        f.modified.clone(),
                        f.path.clone(),
                    ]);
                }
                doc.table(t);
            }
        }
        None => {
            doc.heading("Data age");
            doc.text(
                "No scan yet; run `stomp scan` for age profiles, file types and largest files.",
            );
        }
    }

    doc.heading("Compute by month");
    if r.compute_by_month.is_empty() {
        doc.text("No jobs attributed to this grant in the past year.");
    } else {
        let mut t = Table::new(&[
            ("MONTH", Align::Left),
            ("JOBS", Align::Right),
            ("CPU-HOURS", Align::Right),
            ("GPU-HOURS", Align::Right),
        ]);
        for m in &r.compute_by_month {
            t.row(vec![
                m.month.clone(),
                m.jobs.to_string(),
                format_hours(m.cpu_hours),
                format_hours(m.gpu_hours),
            ]);
        }
        doc.table(t);
    }
    doc
}
