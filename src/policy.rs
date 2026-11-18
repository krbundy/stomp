//! Archive policies (§16): which directories to propose for archiving.
//!
//! Policies are evaluated against the newest detailed scan, which holds
//! per-directory totals. Each policy proposes the outermost directories that
//! meet all its conditions; across policies, only the outermost proposals are
//! kept, since archiving a directory covers everything inside it.
//!
//! Only data inside grant directories is proposed, unless a policy names a
//! `path`. Hidden directories are skipped the same way. Software, settings and
//! reference data are often read every day without being modified, so age
//! alone mustn't put them up for archiving.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};
use globset::{GlobBuilder, GlobMatcher};
use jiff::civil::Date;
use serde::Serialize;

use crate::archive::ARCHIVE_DIR;
use crate::config::{Conditions, Config, Policy, PolicyAction};
use crate::scan::{DirRow, Role};
use crate::units::{format_bytes, format_count};

/// A directory a policy proposes to archive.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Candidate {
    pub path: PathBuf,
    pub policy: String,
    #[serde(skip)]
    pub action: PolicyAction,
    pub grant: Option<String>,
    pub destination: String,
    /// What would be archived: the directory minus excluded directories in it.
    pub bytes: u64,
    pub files: u64,
    /// Environments, caches and other excluded directories inside it, which stay.
    pub kept_bytes: u64,
    /// The newest modification or change time of anything in it.
    pub newest: Option<i64>,
    pub reason: String,
}

/// What evaluation needs besides the scan.
pub struct Inputs<'a> {
    pub cfg: &'a Config,
    /// Unix seconds.
    pub now: i64,
    pub today: Date,
    /// Directories that already hold an archive, or are being archived.
    pub archived: &'a HashSet<PathBuf>,
    /// stomp's data and staging directories, which are never proposed.
    pub protected: &'a [PathBuf],
}

fn glob(pattern: &str) -> GlobMatcher {
    // Validated when the config was loaded.
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map(|g| g.compile_matcher())
        .unwrap_or_else(|_| GlobBuilder::new("").build().unwrap().compile_matcher())
}

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.as_encoded_bytes().first() == Some(&b'.')
}

/// Days since a grant ended, if it has.
fn days_since_end(cfg: &Config, path: &Path, today: Date) -> Option<i64> {
    let end = cfg.grant_for(path)?.end?;
    let days = i64::from((today - end).get_days());
    (days > 0).then_some(days)
}

fn age_text(seconds: i64) -> String {
    let days = seconds / 86_400;
    if days >= 730 {
        format!("{:.1} years", days as f64 / 365.0)
    } else if days >= 60 {
        format!("{} months", days / 30)
    } else {
        format!("{days} days")
    }
}

/// The conditions that don't depend on the directory's contents.
fn matches_place(
    cfg: &Config,
    c: &Conditions,
    glob: Option<&GlobMatcher>,
    path: &Path,
    today: Date,
) -> bool {
    if !c.grants.is_empty()
        && !cfg
            .grant_for(path)
            .is_some_and(|g| c.grants.contains(&g.id))
    {
        return false;
    }
    if let Some(d) = c.grant_ended_for
        && !days_since_end(cfg, path, today).is_some_and(|days| days * 86_400 >= d.as_secs() as i64)
    {
        return false;
    }
    glob.is_none_or(|g| g.is_match(path))
}

/// The conditions on the directory's contents.
fn matches_contents(c: &Conditions, bytes: u64, files: u64, newest: Option<i64>, now: i64) -> bool {
    if let Some(d) = c.unchanged_for
        && !newest.is_some_and(|t| t <= now - d.as_secs() as i64)
    {
        return false;
    }
    if c.min_bytes.is_some_and(|min| bytes < min) || c.min_files.is_some_and(|min| files < min) {
        return false;
    }
    if let Some(max) = c.max_avg_file_size
        && (files == 0 || bytes / files > max)
    {
        return false;
    }
    true
}

fn reason(inputs: &Inputs, policy: &Policy, c: &Candidate) -> String {
    let cond = &policy.conditions;
    let mut parts = Vec::new();
    if cond.grant_ended_for.is_some()
        && let Some(g) = inputs.cfg.grant_for(&c.path)
        && let Some(end) = g.end
    {
        parts.push(format!("grant {} ended {end}", g.id));
    } else if !cond.grants.is_empty()
        && let Some(g) = &c.grant
    {
        parts.push(format!("grant {g}"));
    }
    if cond.unchanged_for.is_some()
        && let Some(t) = c.newest
    {
        parts.push(format!("unchanged for {}", age_text(inputs.now - t)));
    }
    if let Some(p) = &cond.path {
        parts.push(format!("matches {}", crate::paths::display(Path::new(p))));
    }
    if cond.min_files.is_some() || cond.max_avg_file_size.is_some() {
        parts.push(format!(
            "{} files averaging {}",
            format_count(c.files),
            format_bytes(c.bytes / c.files.max(1))
        ));
    } else if cond.min_bytes.is_some() {
        parts.push(format_bytes(c.bytes));
    }
    parts.join("; ")
}

/// Directories each policy proposes, outermost only.
pub fn evaluate(inputs: &Inputs, rows: &[DirRow]) -> Vec<Candidate> {
    let cfg = inputs.cfg;
    let index: HashMap<&Path, &DirRow> = rows.iter().map(|r| (r.path.as_path(), r)).collect();

    // What excluded directories inside each recorded directory hold; those stay in place.
    let mut kept: HashMap<&Path, (u64, u64)> = HashMap::new();
    for r in rows.iter().filter(|r| r.excluded.is_some()) {
        for a in r.path.ancestors().skip(1) {
            if let Some(row) = index.get(a) {
                let k = kept.entry(row.path.as_path()).or_default();
                k.0 += r.agg.bytes;
                k.1 += r.agg.files;
            }
        }
    }
    let contents = |r: &DirRow| {
        let (kb, kf) = kept.get(r.path.as_path()).copied().unwrap_or_default();
        (
            r.agg.bytes.saturating_sub(kb),
            r.agg.files.saturating_sub(kf),
            kb,
        )
    };
    let newest = |r: &DirRow| match (r.agg.newest_mtime, r.agg.newest_change) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };

    let eligible = |r: &DirRow, explicit_path: bool| {
        let path = r.path.as_path();
        if r.role == Some(Role::Root) || r.excluded.is_some() || r.agg.files == 0 {
            return false;
        }
        if path.components().any(|c| c.as_os_str() == ARCHIVE_DIR) {
            return false;
        }
        if inputs.archived.contains(path)
            || inputs
                .protected
                .iter()
                .any(|p| p.starts_with(path) || path.starts_with(p))
            || !cfg.grants_within(path).is_empty()
        {
            return false;
        }
        if explicit_path {
            return true;
        }
        // Without an explicit path: grant data only, and nothing hidden.
        let Some(root) = cfg.roots.iter().find(|root| path.starts_with(root)) else {
            return false;
        };
        cfg.grant_for(path).is_some()
            && !path
                .strip_prefix(root)
                .unwrap_or(path)
                .components()
                .any(|c| matches!(c, Component::Normal(n) if is_hidden(n)))
    };

    // (depth, policy order, candidate), so outer directories come first.
    let mut found: Vec<(usize, usize, Candidate)> = Vec::new();
    for (order, policy) in cfg.policies.iter().enumerate() {
        let c = &policy.conditions;
        let matcher = c.path.as_deref().map(glob);
        let matched: Vec<&DirRow> = rows
            .iter()
            .filter(|r| eligible(r, matcher.is_some()))
            .filter(|r| matches_place(cfg, c, matcher.as_ref(), &r.path, inputs.today))
            .filter(|r| {
                let (bytes, files, _) = contents(r);
                matches_contents(c, bytes, files, newest(r), inputs.now)
            })
            .collect();
        let set: HashSet<&Path> = matched.iter().map(|r| r.path.as_path()).collect();
        for r in matched {
            if r.path.ancestors().skip(1).any(|a| set.contains(a)) {
                continue;
            }
            let (bytes, files, kept_bytes) = contents(r);
            let mut candidate = Candidate {
                path: r.path.clone(),
                policy: policy.name.clone(),
                action: policy.action,
                grant: cfg.grant_for(&r.path).map(|g| g.id.clone()),
                destination: policy.destination.clone(),
                bytes,
                files,
                kept_bytes,
                newest: newest(r),
                reason: String::new(),
            };
            candidate.reason = reason(inputs, policy, &candidate);
            found.push((r.path.components().count(), order, candidate));
        }
    }
    found.sort_by_key(|a| (a.0, a.1));
    let mut kept_paths: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for (_, _, c) in found {
        if kept_paths.iter().any(|k| c.path.starts_with(k)) {
            continue;
        }
        kept_paths.push(c.path.clone());
        out.push(c);
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Checks, just before archiving, that a proposed directory still qualifies:
/// the policy still exists, the directory can be archived, the grant conditions
/// still hold under the current config, and, where the filesystem reports
/// subtree totals instantly (CephFS), nothing in it has changed since.
pub fn recheck(
    cfg: &Config,
    policy: &str,
    path: &Path,
    data_dir: &Path,
    now: i64,
    today: Date,
) -> Result<()> {
    let Some(policy) = cfg.policies.iter().find(|p| p.name == policy) else {
        bail!("policy {policy:?} is no longer in the config");
    };
    crate::archive::check_target(cfg, path, data_dir, false)?;
    let c = &policy.conditions;
    let matcher = c.path.as_deref().map(glob);
    if !matches_place(cfg, c, matcher.as_ref(), path, today) {
        bail!(
            "it no longer matches policy {:?} (has the grant changed?)",
            policy.name
        );
    }
    let backend = crate::fs::detect(path)?;
    if backend.is_instant() {
        let usage = backend.usage(path)?;
        if !matches_contents(c, usage.bytes, usage.files, usage.newest_change, now) {
            bail!(
                "it has changed since the scan, so it no longer matches policy {:?}",
                policy.name
            );
        }
    }
    Ok(())
}

/// Pieces of files compressed for an estimate.
const SAMPLES: usize = 16;
/// Bytes read from the start of each sampled file.
const SAMPLE_BYTES: usize = 1 << 20;

/// A small deterministic random number generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn new(seed: &[u8]) -> Rng {
        let hash = blake3::hash(seed);
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&hash.as_bytes()[..8]);
        Rng(u64::from_le_bytes(bytes) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// An index chosen in proportion to `weights`.
    fn pick(&mut self, weights: &[u64]) -> Option<usize> {
        let total: u64 = weights.iter().sum();
        if total == 0 {
            return None;
        }
        let mut at = self.next() % total;
        for (i, w) in weights.iter().enumerate() {
            if at < *w {
                return Some(i);
            }
            at -= w;
        }
        None
    }
}

/// Estimates a directory's compressed size as a fraction of its size, by
/// compressing pieces of a few files picked in proportion to their size. The
/// scan's per-directory sizes say where to look, so this reads little more
/// than the samples themselves. `None` if nothing could be read.
pub fn estimate_ratio(path: &Path, rows: &[DirRow], level: i32) -> Option<f64> {
    let dirs: Vec<&DirRow> = rows
        .iter()
        .filter(|r| {
            r.path.starts_with(path)
                && r.excluded.is_none()
                && r.direct_bytes > 0
                && !r.path.components().any(|c| c.as_os_str() == ARCHIVE_DIR)
        })
        .collect();
    let weights: Vec<u64> = dirs.iter().map(|r| r.direct_bytes).collect();
    // A row's direct totals include small subdirectories the scan folded
    // into it, so its files are looked for in those too.
    let has_row: HashSet<&Path> = rows.iter().map(|r| r.path.as_path()).collect();
    let mut rng = Rng::new(path.as_os_str().as_encoded_bytes());
    let mut listings: HashMap<usize, Vec<(PathBuf, u64)>> = HashMap::new();
    let mut buf = vec![0u8; SAMPLE_BYTES];
    let (mut read, mut compressed) = (0u64, 0u64);
    for _ in 0..SAMPLES {
        let Some(d) = rng.pick(&weights) else { break };
        let files = listings.entry(d).or_insert_with(|| {
            let mut found = Vec::new();
            files_under(&dirs[d].path, &has_row, &mut found);
            found
        });
        let sizes: Vec<u64> = files.iter().map(|f| f.1).collect();
        let Some(f) = rng.pick(&sizes) else { continue };
        let Ok(mut file) = File::open(&files[f].0) else {
            continue;
        };
        let mut n = 0;
        while n < buf.len() {
            match file.read(&mut buf[n..]) {
                Ok(0) | Err(_) => break,
                Ok(m) => n += m,
            }
        }
        if n == 0 {
            continue;
        }
        let Ok(out) = zstd::bulk::compress(&buf[..n], level) else {
            continue;
        };
        read += n as u64;
        compressed += out.len() as u64;
    }
    (read > 0).then(|| compressed as f64 / read as f64)
}

/// Most files listed per directory when looking for samples.
const LISTING_LIMIT: usize = 2000;

/// Non-empty files in `dir` and in subdirectories without a row of their own
/// (folded into it by the scan), without following symlinks.
fn files_under(dir: &Path, has_row: &HashSet<&Path>, out: &mut Vec<(PathBuf, u64)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        if out.len() >= LISTING_LIMIT {
            return;
        }
        let Ok(kind) = e.file_type() else { continue };
        let path = e.path();
        if kind.is_file() {
            if let Ok(md) = e.metadata()
                && md.len() > 0
            {
                out.push((path, md.len()));
            }
        } else if kind.is_dir() && !has_row.contains(path.as_path()) && e.file_name() != ARCHIVE_DIR
        {
            files_under(&path, has_row, out);
        }
    }
}

#[cfg(test)]
mod tests;
