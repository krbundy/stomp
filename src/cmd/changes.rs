//! `stomp changes`: how storage changed (§11).
//!
//! By default this compares a fresh measurement (instant on CephFS) with an
//! earlier recorded one, per grant. With `--dirs` it compares two scans
//! directory by directory.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use serde::Serialize;

use super::report::{TOTAL, UNASSIGNED, totals_by_grant};
use super::{
    Format, age, emit, local_time, open_catalog, parse_since, rfc3339, usage::detect_backends,
};
use crate::attribution;
use crate::config::Config;
use crate::paths;
use crate::table::{Align, Table};
use crate::units::{format_bytes, format_bytes_delta, format_count};

pub struct Args {
    pub since: Option<String>,
    pub dirs: bool,
    pub grant: Option<String>,
    pub depth: usize,
    pub top: usize,
    pub format: Format,
}

pub fn run(cfg: &Config, data_dir: &Path, args: &Args) -> Result<()> {
    if let Some(id) = &args.grant
        && cfg.grant(id).is_none()
    {
        bail!("no grant {id:?} in {}", paths::display(&cfg.source));
    }
    if args.dirs {
        directories(cfg, data_dir, args)
    } else {
        grants(cfg, data_dir, args)
    }
}

#[derive(Serialize)]
struct GrantChange {
    grant: String,
    before_bytes: Option<u64>,
    now_bytes: u64,
    change_bytes: i128,
    before_files: Option<u64>,
    now_files: u64,
    change_files: i128,
    last_change: String,
    #[serde(skip)]
    changed_at: Option<i64>,
}

fn grants(cfg: &Config, data_dir: &Path, args: &Args) -> Result<()> {
    let catalog = open_catalog(data_dir)?;
    let cutoff = match &args.since {
        Some(s) => parse_since(s)?,
        None => Timestamp::now().as_second(),
    };
    let Some((baseline, taken_at)) = catalog.measurement_at_or_before(cutoff)? else {
        bail!(match args.since {
            Some(_) => "no measurement that old; `stomp report history` lists what's recorded",
            None => "nothing to compare with yet; run `stomp usage` to record a measurement",
        });
    };
    let before = totals_by_grant(cfg, &catalog.measurement_samples(baseline)?);

    let backends = detect_backends(cfg)?;
    if backends.iter().any(|b| !b.is_instant()) {
        eprintln!(
            "stomp: measuring roots that aren't on CephFS means walking them; this can take a while."
        );
    }
    let now = attribution::measure(cfg, &backends);
    let mut current: BTreeMap<String, (u64, u64, Option<i64>)> = BTreeMap::new();
    for g in &now.grants {
        current.insert(
            g.id.clone(),
            (g.total.bytes, g.total.files, g.total.newest_change),
        );
    }
    let (mut unassigned, mut total) = ((0, 0, None), (0, 0, None));
    for r in &now.roots {
        unassigned = (
            unassigned.0 + r.unassigned.bytes,
            unassigned.1 + r.unassigned.files,
            r.unassigned.newest_change.max(unassigned.2),
        );
        total = (
            total.0 + r.total.bytes,
            total.1 + r.total.files,
            r.total.newest_change.max(total.2),
        );
    }

    let mut labels: Vec<String> = current.keys().cloned().collect();
    labels.sort_by_key(|g| std::cmp::Reverse(current[g].0));
    labels.extend([UNASSIGNED.to_string(), TOTAL.to_string()]);
    let rows: Vec<GrantChange> = labels
        .into_iter()
        .filter(|label| args.grant.as_ref().is_none_or(|g| g == label))
        .map(|label| {
            let (now_bytes, now_files, changed) = match label.as_str() {
                UNASSIGNED => unassigned,
                TOTAL => total,
                g => current[g],
            };
            let then = before.get(&label);
            GrantChange {
                before_bytes: then.map(|v| v.0),
                before_files: then.map(|v| v.1),
                change_bytes: i128::from(now_bytes) - i128::from(then.map_or(0, |v| v.0)),
                change_files: i128::from(now_files) - i128::from(then.map_or(0, |v| v.1)),
                now_bytes,
                now_files,
                last_change: rfc3339(changed),
                changed_at: changed,
                grant: label,
            }
        })
        .collect();

    if matches!(args.format, Format::Table | Format::Md) {
        println!(
            "Changes since {} (a recorded measurement) to now.\n",
            local_time(Timestamp::from_second(taken_at)?)
        );
    }
    emit(args.format, &rows, || {
        let mut t = Table::new(&[
            ("GRANT", Align::Left),
            ("THEN", Align::Right),
            ("NOW", Align::Right),
            ("CHANGE", Align::Right),
            ("FILES CHANGE", Align::Right),
            ("LAST CHANGE", Align::Left),
        ]);
        for r in &rows {
            if r.grant == TOTAL {
                t.rule();
            }
            let files = match r.change_files {
                0 => "0".to_string(),
                n if n > 0 => format!("+{}", format_count(n as u64)),
                n => format!("-{}", format_count(n.unsigned_abs() as u64)),
            };
            t.row(vec![
                r.grant.clone(),
                r.before_bytes.map_or("—".into(), format_bytes),
                format_bytes(r.now_bytes),
                format_bytes_delta(r.change_bytes),
                files,
                age(r.changed_at),
            ]);
        }
        t
    })
}

#[derive(Serialize)]
struct DirChange {
    path: String,
    status: &'static str,
    before_bytes: u64,
    now_bytes: u64,
    change_bytes: i128,
    change_files: i128,
}

fn directories(cfg: &Config, data_dir: &Path, args: &Args) -> Result<()> {
    let catalog = open_catalog(data_dir)?;
    let after = catalog
        .latest_scan(true)?
        .context("no scan with directory detail yet; run `stomp scan`")?;
    let before = match &args.since {
        Some(s) => catalog.scan_at_or_before(parse_since(s)?, true)?,
        None => catalog.scan_before(&after, true)?,
    }
    .context("no earlier scan with directory detail to compare with")?;
    if before.id == after.id {
        bail!("the newest scan is also the one to compare with; pick an earlier --since");
    }

    let scopes: Vec<PathBuf> = match &args.grant {
        Some(id) => cfg
            .grant(id)
            .map(|g| g.dirs.iter().map(|d| d.path.clone()).collect())
            .unwrap_or_default(),
        None => cfg.roots.clone(),
    };
    let mut sizes: BTreeMap<PathBuf, [(u64, u64); 2]> = BTreeMap::new();
    for scope in &scopes {
        let base = scope.components().count();
        for (side, scan) in [(0, &before), (1, &after)] {
            for row in catalog.dir_rows(scan.id, Some(scope))? {
                if row.path.components().count() - base <= args.depth {
                    sizes.entry(row.path.clone()).or_default()[side] =
                        (row.agg.bytes, row.agg.files);
                }
            }
        }
    }
    let present: [BTreeSet<&PathBuf>; 2] = [0, 1].map(|side| {
        sizes
            .iter()
            .filter(|(_, v)| v[side] != (0, 0))
            .map(|(p, _)| p)
            .collect()
    });
    let mut rows: Vec<DirChange> = sizes
        .iter()
        .filter_map(|(path, [(b0, f0), (b1, f1)])| {
            let change_bytes = i128::from(*b1) - i128::from(*b0);
            let change_files = i128::from(*f1) - i128::from(*f0);
            if change_bytes == 0 && change_files == 0 {
                return None;
            }
            let status = match (present[0].contains(path), present[1].contains(path)) {
                (false, true) => "new",
                (true, false) => "gone",
                _ => "changed",
            };
            Some(DirChange {
                path: paths::display(path),
                status,
                before_bytes: *b0,
                now_bytes: *b1,
                change_bytes,
                change_files,
            })
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.change_bytes.unsigned_abs()));
    rows.truncate(args.top);

    if matches!(args.format, Format::Table | Format::Md) {
        println!(
            "Directory changes between the scans of {} and {}, down to depth {}.",
            local_time(Timestamp::from_second(before.started_at)?),
            local_time(Timestamp::from_second(after.started_at)?),
            args.depth
        );
        println!("Small directories are folded into their parents, so they show up there.\n");
        if rows.is_empty() {
            println!("No changes.");
            return Ok(());
        }
    }
    emit(args.format, &rows, || {
        let mut t = Table::new(&[
            ("CHANGE", Align::Right),
            ("NOW", Align::Right),
            ("BEFORE", Align::Right),
            ("FILES CHANGE", Align::Right),
            ("STATUS", Align::Left),
            ("PATH", Align::Left),
        ]);
        for r in &rows {
            let files = if r.change_files >= 0 {
                format!("+{}", format_count(r.change_files as u64))
            } else {
                format!("-{}", format_count(r.change_files.unsigned_abs() as u64))
            };
            t.row(vec![
                format_bytes_delta(r.change_bytes),
                format_bytes(r.now_bytes),
                format_bytes(r.before_bytes),
                files,
                r.status.to_string(),
                r.path.clone(),
            ]);
        }
        t
    })
}
