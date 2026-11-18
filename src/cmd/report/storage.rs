//! `stomp report stale` and `stomp report reclaimable`: storage worth
//! archiving or cleaning, from the newest scan (§18).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use serde::Serialize;

use crate::catalog::{Catalog, ScanInfo};
use crate::cmd::{Format, age, emit, local_time, open_catalog, parse_since, rfc3339};
use crate::config::Config;
use crate::paths;
use crate::scan::{DirRow, ExcludeKind, Role};
use crate::table::{Align, Table};
use crate::units::{format_bytes, format_count};

fn detailed_scan(catalog: &Catalog) -> Result<ScanInfo> {
    catalog.latest_scan(true)?.context(
        "no scan with directory detail yet; run `stomp scan` (it runs as a Slurm task), then try again",
    )
}

fn grant_label(cfg: &Config, path: &Path) -> String {
    cfg.grant_for(path)
        .map_or("—".to_string(), |g| g.id.clone())
}

#[derive(Serialize)]
struct StaleRow {
    grant: String,
    path: String,
    bytes: u64,
    files: u64,
    newest_mtime: String,
}

pub fn stale(
    cfg: &Config,
    data_dir: &Path,
    older_than: &str,
    grant: Option<&str>,
    top: usize,
    format: Format,
) -> Result<()> {
    let cutoff = parse_since(older_than)?;
    if let Some(id) = grant
        && cfg.grant(id).is_none()
    {
        bail!("no grant {id:?} in {}", paths::display(&cfg.source));
    }
    let catalog = open_catalog(data_dir)?;
    let scan = detailed_scan(&catalog)?;
    let rows = catalog.dir_rows(scan.id, None)?;
    let index: HashMap<&Path, &DirRow> = rows.iter().map(|r| (r.path.as_path(), r)).collect();
    let is_stale = |r: &DirRow| {
        r.role != Some(Role::Root)
            && r.excluded.is_none()
            && r.agg.files > 0
            && r.agg.newest_mtime.is_some_and(|m| m < cutoff)
            // Archives are old by design; they're already dealt with.
            && !r.path.components().any(|c| c.as_os_str() == crate::archive::ARCHIVE_DIR)
    };
    // Report only the topmost stale directories: everything inside a stale
    // directory is stale too. Checking the nearest recorded ancestor is
    // enough, since an ancestor's newest file is at least as new.
    let mut found: Vec<&DirRow> = rows
        .iter()
        .filter(|r| is_stale(r))
        .filter(|r| {
            r.path
                .ancestors()
                .skip(1)
                .find_map(|a| index.get(a))
                .is_none_or(|parent| !is_stale(parent))
        })
        .filter(|r| grant.is_none_or(|id| cfg.grant_for(&r.path).is_some_and(|g| g.id == id)))
        .collect();
    found.sort_by_key(|r| std::cmp::Reverse(r.agg.bytes));
    let total_bytes: u64 = found.iter().map(|r| r.agg.bytes).sum();
    let shown: Vec<StaleRow> = found
        .iter()
        .take(top)
        .map(|r| StaleRow {
            grant: grant_label(cfg, &r.path),
            path: paths::display(&r.path),
            bytes: r.agg.bytes,
            files: r.agg.files,
            newest_mtime: rfc3339(r.agg.newest_mtime),
        })
        .collect();

    if matches!(format, Format::Table | Format::Md) {
        println!(
            "Directories with no file modified since {}, from the scan of {}.\n",
            crate::cmd::local_date(cutoff),
            local_time(Timestamp::from_second(scan.started_at)?)
        );
        if shown.is_empty() {
            println!("None found.");
            return Ok(());
        }
    }
    emit(format, &shown, || {
        let mut t = Table::new(&[
            ("SIZE", Align::Right),
            ("FILES", Align::Right),
            ("NEWEST FILE", Align::Left),
            ("GRANT", Align::Left),
            ("PATH", Align::Left),
        ]);
        for (row, r) in shown.iter().zip(&found) {
            t.row(vec![
                format_bytes(row.bytes),
                format_count(row.files),
                age(r.agg.newest_mtime),
                row.grant.clone(),
                row.path.clone(),
            ]);
        }
        if found.len() > shown.len() {
            t.rule();
            t.row(vec![
                format_bytes(total_bytes),
                String::new(),
                String::new(),
                String::new(),
                format!("in all {} stale directories", found.len()),
            ]);
        }
        t
    })
}

#[derive(Serialize)]
struct ReclaimRow {
    kind: &'static str,
    grant: String,
    path: String,
    bytes: u64,
    files: u64,
    suggestion: String,
}

/// What to do about an excluded directory.
fn suggestion(kind: ExcludeKind, path: &Path) -> String {
    let has = |name: &str| path.join(name).exists();
    match kind {
        ExcludeKind::Cache => {
            let p = path.to_string_lossy();
            if p.ends_with(".apptainer/cache") || p.ends_with(".singularity/cache") {
                "apptainer cache clean".to_string()
            } else if p.ends_with("/pkgs") {
                "conda's package cache: `conda clean --all`".to_string()
            } else {
                "usually safe to clear; check what's in it first (pip: pip cache purge)".to_string()
            }
        }
        ExcludeKind::Environment if has(".singularity.d") => {
            "Apptainer sandbox: `apptainer build image.sif <dir>` makes it one file".to_string()
        }
        ExcludeKind::Environment if has("conda-meta") => {
            "conda environment: can be rebuilt from an environment.yml".to_string()
        }
        ExcludeKind::Environment => "Python venv: can be rebuilt from requirements".to_string(),
        ExcludeKind::User => "excluded by scan.exclude in your config".to_string(),
        ExcludeKind::Stomp => "stomp's own data".to_string(),
    }
}

pub fn reclaimable(cfg: &Config, data_dir: &Path, format: Format) -> Result<()> {
    let catalog = open_catalog(data_dir)?;
    let scan = detailed_scan(&catalog)?;
    let mut rows: Vec<(PathBuf, ReclaimRow)> = catalog
        .dir_rows(scan.id, None)?
        .into_iter()
        .filter_map(|r| {
            // stomp's own data isn't something to reclaim.
            let kind = r.excluded.filter(|k| *k != ExcludeKind::Stomp)?;
            Some((
                r.path.clone(),
                ReclaimRow {
                    kind: kind.as_str(),
                    grant: grant_label(cfg, &r.path),
                    path: paths::display(&r.path),
                    bytes: r.agg.bytes,
                    files: r.agg.files,
                    suggestion: suggestion(kind, &r.path),
                },
            ))
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1.bytes));
    let rows: Vec<ReclaimRow> = rows.into_iter().map(|(_, r)| r).collect();
    if matches!(format, Format::Table | Format::Md) {
        println!(
            "Caches and software environments, from the scan of {}.\n",
            local_time(Timestamp::from_second(scan.started_at)?)
        );
        if rows.is_empty() {
            println!("None found.");
            return Ok(());
        }
    }
    emit(format, &rows, || {
        let mut t = Table::new(&[
            ("KIND", Align::Left),
            ("SIZE", Align::Right),
            ("FILES", Align::Right),
            ("GRANT", Align::Left),
            ("PATH", Align::Left),
            ("SUGGESTION", Align::Left),
        ]);
        for r in &rows {
            t.row(vec![
                r.kind.to_string(),
                format_bytes(r.bytes),
                format_count(r.files),
                r.grant.clone(),
                r.path.clone(),
                r.suggestion.clone(),
            ]);
        }
        let caches: u64 = rows
            .iter()
            .filter(|r| r.kind == "cache")
            .map(|r| r.bytes)
            .sum();
        t.rule();
        t.row(vec![
            "cache".into(),
            format_bytes(caches),
            String::new(),
            String::new(),
            "total in caches".into(),
            String::new(),
        ]);
        t
    })
}
