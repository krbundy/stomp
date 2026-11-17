//! `stomp usage`: current storage use per grant (§8.4).

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};

use super::{Format, age, local_time, open_catalog, rfc3339, warn};
use crate::attribution::{self, GrantUsage, Measurement, RootUsage};
use crate::config::Config;
use crate::fs::{self, Backend, BackendKind};
use crate::paths;
use crate::table::{Align, Table, csv_row};
use crate::units::{format_bytes, format_count};

pub fn run(cfg: &Config, data_dir: &Path, format: Format, record: bool) -> Result<()> {
    let backends = detect_backends(cfg)?;
    for (root, backend) in cfg.roots.iter().zip(&backends) {
        if !backend.is_instant() {
            eprintln!(
                "stomp: {} is not on CephFS, so measuring it means walking every file. This can take a while.",
                paths::display(root)
            );
        }
    }
    let m = attribution::measure(cfg, &backends);
    for w in &m.warnings {
        warn(w);
    }
    if record {
        let saved = open_catalog(data_dir).and_then(|mut c| c.record(&m, &crate::sys::hostname()));
        if let Err(e) = saved {
            warn(format_args!(
                "this measurement was not saved to the usage history: {e:#}"
            ));
        }
    }
    let remote = open_catalog(data_dir)
        .and_then(|c| c.archives())
        .map(|archives| remote_holdings(&archives))
        .unwrap_or_default();
    match format {
        Format::Table => print!(
            "{}{}",
            render_table(cfg, &m, false),
            remote_table(&remote, false)
        ),
        Format::Md => print!(
            "{}{}",
            render_table(cfg, &m, true),
            remote_table(&remote, true)
        ),
        Format::Csv => print!("{}", render_csv(&m)),
        Format::Json => println!("{}", serde_json::to_string_pretty(&m)?),
    }
    Ok(())
}

/// One backend per configured root.
pub fn detect_backends(cfg: &Config) -> Result<Vec<Box<dyn Backend>>> {
    cfg.roots
        .iter()
        .map(|root| {
            fs::detect(root).with_context(|| format!("cannot examine {}", paths::display(root)))
        })
        .collect()
}

fn render_table(cfg: &Config, m: &Measurement, markdown: bool) -> String {
    let mut out = format!("Measured {}\n\n", local_time(m.taken_at));

    let mut roots = Table::new(&[
        ("ROOT", Align::Left),
        ("FILESYSTEM", Align::Left),
        ("SIZE", Align::Right),
        ("FILES", Align::Right),
        ("QUOTA", Align::Left),
    ]);
    for r in &m.roots {
        roots.row(vec![
            paths::display(&r.path),
            r.backend.label().to_string(),
            format_bytes(r.total.bytes),
            format_count(r.total.files),
            quota_text(r),
        ]);
    }
    out += &roots.render_as(markdown);
    out.push('\n');

    let mut grants = Table::new(&[
        ("GRANT", Align::Left),
        ("DIRS", Align::Right),
        ("SIZE", Align::Right),
        ("FILES", Align::Right),
        ("LAST CHANGE", Align::Left),
    ]);
    let mut order: Vec<&GrantUsage> = m.grants.iter().collect();
    order.sort_by(|a, b| {
        b.total
            .bytes
            .cmp(&a.total.bytes)
            .then_with(|| a.id.cmp(&b.id))
    });
    for g in order {
        let measured = g.dirs.iter().filter(|d| d.usage.is_some()).count();
        let dirs = if measured == g.dirs.len() {
            g.dirs.len().to_string()
        } else {
            format!("{measured} of {}", g.dirs.len())
        };
        if measured == 0 {
            grants.row(vec![g.id.clone(), dirs, "—".into(), "—".into(), "—".into()]);
        } else {
            grants.row(vec![
                g.id.clone(),
                dirs,
                format_bytes(g.total.bytes),
                format_count(g.total.files),
                age(g.total.newest_change),
            ]);
        }
    }
    let several_roots = m.roots.len() > 1;
    for r in &m.roots {
        let label = if several_roots {
            format!("(unassigned in {})", paths::display(&r.path))
        } else {
            "(unassigned)".to_string()
        };
        grants.row(vec![
            label,
            "—".into(),
            format_bytes(r.unassigned.bytes),
            format_count(r.unassigned.files),
            age(r.unassigned.newest_change),
        ]);
    }
    grants.rule();
    let total = m.total();
    grants.row(vec![
        "TOTAL".into(),
        String::new(),
        format_bytes(total.bytes),
        format_count(total.files),
        age(total.newest_change),
    ]);
    out += &grants.render_as(markdown);

    if cfg.grants.is_empty() {
        out += "\nNo grants are defined yet: add [[grant]] blocks to your config.\n";
    }
    if m.unreadable() > 0 {
        out += &format!(
            "\n{} entries could not be read and are not counted.\n",
            format_count(m.unreadable())
        );
    }
    out
}

/// Archived data held at remote destinations (§8.3).
#[derive(Debug, Default, Clone)]
pub struct Holding {
    pub destination: String,
    pub grant: String,
    pub archives: u64,
    pub bytes: u64,
    pub compressed: u64,
}

/// Remote holdings by destination and grant, from the catalog's archives.
pub fn remote_holdings(archives: &[crate::catalog::CatalogArchive]) -> Vec<Holding> {
    let mut grouped: std::collections::BTreeMap<(String, String), Holding> = Default::default();
    for a in archives {
        let held = a.destination != crate::config::LOCAL_DESTINATION
            && matches!(a.state.as_str(), "complete" | "restored-copy-kept");
        if !held {
            continue;
        }
        let grant = a.grant.clone().unwrap_or_else(|| "(unassigned)".into());
        let h = grouped
            .entry((a.destination.clone(), grant.clone()))
            .or_insert_with(|| Holding {
                destination: a.destination.clone(),
                grant,
                ..Holding::default()
            });
        h.archives += 1;
        h.bytes += a.bytes;
        h.compressed += a.compressed;
    }
    grouped.into_values().collect()
}

fn remote_table(holdings: &[Holding], markdown: bool) -> String {
    if holdings.is_empty() {
        return String::new();
    }
    let mut t = Table::new(&[
        ("DESTINATION", Align::Left),
        ("GRANT", Align::Left),
        ("ARCHIVES", Align::Right),
        ("ORIGINAL", Align::Right),
        ("STORED", Align::Right),
    ]);
    for h in holdings {
        t.row(vec![
            h.destination.clone(),
            h.grant.clone(),
            h.archives.to_string(),
            format_bytes(h.bytes),
            format_bytes(h.compressed),
        ]);
    }
    format!("\nArchived elsewhere\n\n{}", t.render_as(markdown))
}

fn quota_text(r: &RootUsage) -> String {
    let Some(q) = &r.quota else {
        return match r.backend {
            BackendKind::Ceph => "none set".to_string(),
            BackendKind::Posix => "—".to_string(),
        };
    };
    let percent = |used: u64, max: u64| used as f64 * 100.0 / max as f64;
    let mut parts = Vec::new();
    if let Some(max) = q.max_bytes {
        parts.push(format!(
            "{} ({:.0}% used)",
            format_bytes(max),
            percent(q.used.bytes, max)
        ));
    }
    if let Some(max) = q.max_files {
        parts.push(format!(
            "{} files ({:.0}% used)",
            format_count(max),
            percent(q.used.files, max)
        ));
    }
    let on = if q.dir == r.path {
        String::new()
    } else {
        format!(" on {}", paths::display(&q.dir))
    };
    format!("{}{on}", parts.join(", "))
}

fn render_csv(m: &Measurement) -> String {
    let when = m.taken_at.to_string();
    let mut out = csv_row(&[
        "measured_at",
        "grant",
        "roots",
        "bytes",
        "files",
        "last_change",
    ]);
    out.push('\n');
    for g in &m.grants {
        let roots: BTreeSet<String> = g
            .dirs
            .iter()
            .map(|d| m.roots[d.root].path.display().to_string())
            .collect();
        out += &csv_row(&[
            when.clone(),
            g.id.clone(),
            roots.into_iter().collect::<Vec<_>>().join(";"),
            g.total.bytes.to_string(),
            g.total.files.to_string(),
            rfc3339(g.total.newest_change),
        ]);
        out.push('\n');
    }
    for r in &m.roots {
        out += &csv_row(&[
            when.clone(),
            "unassigned".to_string(),
            r.path.display().to_string(),
            r.unassigned.bytes.to_string(),
            r.unassigned.files.to_string(),
            rfc3339(r.unassigned.newest_change),
        ]);
        out.push('\n');
    }
    out
}
