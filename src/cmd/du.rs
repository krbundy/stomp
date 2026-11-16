//! `stomp du`: directory sizes, instant on CephFS.

use std::path::Path;

use anyhow::{Context, Result};

use super::{Format, age, rfc3339};
use crate::config::Config;
use crate::fs::{self, Node, Usage};
use crate::paths;
use crate::table::{Align, Table, csv_row};
use crate::units::{format_bytes, format_count};

struct Line {
    label: String,
    /// The directory, or for a "(files here)" line, the directory holding the files.
    path: String,
    loose_files: bool,
    grant: String,
    usage: Usage,
}

pub fn run(cfg: Option<&Config>, path: Option<&Path>, depth: usize, format: Format) -> Result<()> {
    let start = match path {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir()?,
    };
    let dir = std::fs::canonicalize(&start)
        .with_context(|| format!("cannot open {}", start.display()))?;
    let backend = fs::detect(&dir)?;
    if !backend.is_instant() {
        eprintln!(
            "stomp: {} is not on CephFS, so stomp has to walk it to measure sizes. This can take a while.",
            paths::display(&dir)
        );
    }
    let node = backend
        .tree(&dir, depth)
        .with_context(|| format!("cannot measure {}", paths::display(&dir)))?;

    if format == Format::Json {
        println!("{}", serde_json::to_string_pretty(&node)?);
        return Ok(());
    }
    let mut lines = Vec::new();
    collect(cfg, &node, 0, depth, &mut lines);
    match format {
        Format::Table => print!("{}", render_table(cfg.is_some(), &lines)),
        Format::Csv => print!("{}", render_csv(&lines)),
        Format::Json => unreachable!(),
    }
    Ok(())
}

/// The grant a directory belongs to, or which grants it contains.
fn grant_label(cfg: Option<&Config>, path: &Path) -> String {
    let Some(cfg) = cfg else { return String::new() };
    if let Some(g) = cfg.grant_for(path) {
        return g.id.clone();
    }
    let within = cfg.grants_within(path);
    if within.is_empty() {
        "—".to_string()
    } else {
        let ids: Vec<&str> = within.iter().map(|g| g.id.as_str()).collect();
        format!("contains {}", ids.join(", "))
    }
}

fn collect(cfg: Option<&Config>, node: &Node, level: usize, depth: usize, out: &mut Vec<Line>) {
    let indent = "  ".repeat(level);
    let label = if level == 0 {
        paths::display(&node.path)
    } else {
        let name = node.path.file_name().unwrap_or_default().to_string_lossy();
        format!("{indent}{name}/")
    };
    out.push(Line {
        label,
        path: node.path.display().to_string(),
        loose_files: false,
        grant: grant_label(cfg, &node.path),
        usage: node.usage.clone(),
    });
    if level >= depth || node.children.is_empty() {
        return;
    }
    let mut children: Vec<&Node> = node.children.iter().collect();
    children.sort_by(|a, b| {
        b.usage
            .bytes
            .cmp(&a.usage.bytes)
            .then_with(|| a.path.cmp(&b.path))
    });
    for child in &children {
        collect(cfg, child, level + 1, depth, out);
    }
    // Whatever the subdirectories don't account for is in files directly in this directory.
    let bytes = node
        .usage
        .bytes
        .saturating_sub(children.iter().map(|c| c.usage.bytes).sum());
    let files = node
        .usage
        .files
        .saturating_sub(children.iter().map(|c| c.usage.files).sum());
    if bytes > 0 || files > 0 {
        let grant = match cfg {
            Some(cfg) => cfg
                .grant_for(&node.path)
                .map_or_else(|| "—".to_string(), |g| g.id.clone()),
            None => String::new(),
        };
        out.push(Line {
            label: format!("{indent}  (files here)"),
            path: node.path.display().to_string(),
            loose_files: true,
            grant,
            usage: Usage {
                bytes,
                files,
                newest_change: None,
                unreadable: 0,
            },
        });
    }
}

fn render_table(with_grants: bool, lines: &[Line]) -> String {
    let mut columns = vec![
        ("SIZE", Align::Right),
        ("FILES", Align::Right),
        ("LAST CHANGE", Align::Left),
    ];
    if with_grants {
        columns.push(("GRANT", Align::Left));
    }
    columns.push(("PATH", Align::Left));
    let mut table = Table::new(&columns);
    for line in lines {
        let mut cells = vec![
            format_bytes(line.usage.bytes),
            format_count(line.usage.files),
            if line.loose_files {
                String::new()
            } else {
                age(line.usage.newest_change)
            },
        ];
        if with_grants {
            cells.push(line.grant.clone());
        }
        cells.push(line.label.clone());
        table.row(cells);
    }
    let mut out = table.render();
    let unreadable = lines.first().map_or(0, |l| l.usage.unreadable);
    if unreadable > 0 {
        out += &format!(
            "\n{} entries could not be read and are not counted.\n",
            format_count(unreadable)
        );
    }
    out
}

fn render_csv(lines: &[Line]) -> String {
    let mut out = csv_row(&["path", "kind", "bytes", "files", "last_change", "grant"]);
    out.push('\n');
    for line in lines {
        out += &csv_row(&[
            line.path.clone(),
            if line.loose_files {
                "files-here"
            } else {
                "dir"
            }
            .to_string(),
            line.usage.bytes.to_string(),
            line.usage.files.to_string(),
            rfc3339(line.usage.newest_change),
            line.grant.clone(),
        ]);
        out.push('\n');
    }
    out
}
