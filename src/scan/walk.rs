//! The parallel scan walk (§9.2).
//!
//! The tree is walked depth-first in parallel. Directories at `unit_depth` are
//! work units: each is committed to the result file when it finishes, so an
//! interrupted scan resumes without redoing them. On CephFS, a subtree whose
//! newest change predates the previous scan is not walked at all; its rows
//! are reused from that scan. Small directories are folded into their parent
//! to keep the catalog compact (§9.3).

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rayon::prelude::*;

use super::hist::{Agg, Count};
use super::rules::Rules;
use super::{DirRow, ExcludeKind, Role};
use crate::fs::{Backend, Entry, Usage, read_entries};

/// Why a walk stopped early.
#[derive(Debug)]
pub enum Stop {
    /// A stop was requested (a signal); finished units are saved.
    Interrupted,
    Failed(anyhow::Error),
}

/// Live counters, read by the progress reporter.
#[derive(Default)]
pub struct Progress {
    pub dirs: AtomicU64,
    pub files: AtomicU64,
    pub bytes: AtomicU64,
    pub reused: AtomicU64,
    pub units: AtomicU64,
}

impl Progress {
    fn add(counter: &AtomicU64, n: u64) {
        counter.fetch_add(n, Ordering::Relaxed);
    }
}

/// A walked directory, before its parent decides whether to fold it.
#[derive(Debug, Clone)]
pub struct Walked {
    pub row: DirRow,
    /// Whether any rows below this one were kept; such a directory is never folded.
    pub has_rows_below: bool,
}

/// Rows from the previous scan, for reusing unchanged subtrees.
pub trait PrevRows: Sync {
    fn get(&self, path: &Path) -> Option<DirRow>;
}

/// Where finished units go, and where a resumed scan finds them.
pub trait UnitStore: Sync {
    fn done(&self, path: &Path) -> Option<Walked>;
    fn commit(&self, unit: &Walked, rows: Vec<DirRow>) -> anyhow::Result<()>;
}

// ---------------------------------------------------------------------------
// File-type and largest-file statistics (full scans only)
// ---------------------------------------------------------------------------

pub const LARGEST_KEPT: usize = 100;

#[derive(Default)]
pub struct ScopeStats {
    pub extensions: HashMap<String, Count>,
    /// Min-heap of the largest files: (bytes, path, mtime).
    pub largest: BinaryHeap<Reverse<(u64, PathBuf, i64)>>,
}

impl ScopeStats {
    /// Whether a file of this size would make the largest-files list.
    fn qualifies(&self, bytes: u64) -> bool {
        self.largest.len() < LARGEST_KEPT
            || self
                .largest
                .peek()
                .is_some_and(|Reverse((min, _, _))| bytes > *min)
    }

    fn push_largest(&mut self, bytes: u64, path: PathBuf, mtime: i64) {
        if !self.qualifies(bytes) {
            return;
        }
        if self.largest.len() == LARGEST_KEPT {
            self.largest.pop();
        }
        self.largest.push(Reverse((bytes, path, mtime)));
    }

    fn merge(&mut self, other: &ScopeStats) {
        for (ext, count) in &other.extensions {
            self.extensions
                .entry(ext.clone())
                .or_default()
                .merge(*count);
        }
        for Reverse((bytes, path, mtime)) in &other.largest {
            // Check before cloning: most candidates don't qualify.
            if self.qualifies(*bytes) {
                self.push_largest(*bytes, path.clone(), *mtime);
            }
        }
    }
}

/// Statistics per scope: `("root", <root path>)` and `("grant", <grant id>)`.
#[derive(Default)]
pub struct FileStats {
    pub scopes: Mutex<HashMap<(&'static str, String), ScopeStats>>,
}

impl FileStats {
    fn record(&self, root: &str, grant: Option<&str>, local: &ScopeStats) {
        let mut scopes = self.scopes.lock().unwrap();
        scopes
            .entry(("root", root.to_string()))
            .or_default()
            .merge(local);
        if let Some(g) = grant {
            scopes
                .entry(("grant", g.to_string()))
                .or_default()
                .merge(local);
        }
    }
}

/// A file's extension for statistics: lowercase, or a placeholder.
pub fn extension(name: &OsStr) -> String {
    let bytes = name.as_bytes();
    match bytes.iter().rposition(|&b| b == b'.') {
        Some(dot) if dot > 0 && dot + 1 < bytes.len() => {
            let ext = &bytes[dot + 1..];
            if ext.len() > 12 || !ext.iter().all(u8::is_ascii_alphanumeric) {
                "(other)".to_string()
            } else if ext.iter().all(u8::is_ascii_digit) {
                "(numbered)".to_string()
            } else {
                String::from_utf8_lossy(ext).to_ascii_lowercase()
            }
        }
        _ => "(none)".to_string(),
    }
}

// ---------------------------------------------------------------------------
// The walker
// ---------------------------------------------------------------------------

pub struct Walker<'a> {
    pub backend: &'a dyn Backend,
    pub rules: &'a Rules,
    /// Grant directories and their grant IDs; these are never folded or excluded.
    pub grant_dirs: &'a HashMap<PathBuf, String>,
    /// The previous scan and the reuse threshold: subtrees whose newest change
    /// is older than this (Unix seconds) are reused.
    pub prev: Option<(&'a dyn PrevRows, i64)>,
    pub fold_bytes: u64,
    pub fold_files: u64,
    pub unit_depth: usize,
    pub units: &'a dyn UnitStore,
    pub stop: &'a AtomicBool,
    pub progress: &'a Progress,
    /// Collected only on full scans.
    pub file_stats: Option<&'a FileStats>,
    /// The root's path as text, naming its file-statistics scope.
    pub root_name: String,
}

impl Walker<'_> {
    /// Walks a root. Returns its row and the rows not committed as part of a unit.
    pub fn walk_root(&self, root: &Path) -> Result<(DirRow, Vec<DirRow>), Stop> {
        let md = std::fs::symlink_metadata(root).map_err(|e| Stop::Failed(e.into()))?;
        let grant = self.grant_dirs.get(root).map(String::as_str);
        let sink = Mutex::new(Vec::new());
        let walked = self.walk_dir(root, md.dev(), md.ctime(), 0, grant, &sink)?;
        Ok((walked.row, sink.into_inner().unwrap()))
    }

    fn role(&self, dir: &Path, depth: usize) -> Option<Role> {
        if depth == 0 {
            Some(Role::Root)
        } else if self.grant_dirs.contains_key(dir) {
            Some(Role::Grant)
        } else {
            None
        }
    }

    fn walk_dir(
        &self,
        dir: &Path,
        dev: u64,
        own_ctime: i64,
        depth: usize,
        grant: Option<&str>,
        sink: &Mutex<Vec<DirRow>>,
    ) -> Result<Walked, Stop> {
        if self.stop.load(Ordering::Relaxed) {
            return Err(Stop::Interrupted);
        }
        let role = self.role(dir, depth);
        let mut agg = Agg::default();
        agg.saw_change(own_ctime);
        let (entries, unreadable) = match read_entries(dir) {
            Ok(found) => found,
            Err(_) => {
                agg.unreadable = 1;
                return Ok(Walked {
                    row: DirRow::new(dir, role, None, 0, 0, agg),
                    has_rows_below: false,
                });
            }
        };
        Progress::add(&self.progress.dirs, 1);
        if role.is_none() && Rules::is_environment(&entries) {
            return Ok(self.excluded(dir, ExcludeKind::Environment));
        }
        agg.unreadable = unreadable;

        let (mut direct_bytes, mut direct_files) = (0, 0);
        let mut local = self.file_stats.map(|_| ScopeStats::default());
        let mut subdirs = Vec::new();
        for entry in entries {
            if entry.md.is_dir() {
                // Other filesystems mounted inside are neither entered nor counted.
                if entry.md.dev() == dev {
                    subdirs.push(entry);
                }
                continue;
            }
            let (len, mtime) = (entry.md.len(), entry.md.mtime());
            agg.add_file(len, mtime, entry.md.ctime());
            direct_bytes += len;
            direct_files += 1;
            if let Some(stats) = &mut local {
                let name = entry.path.file_name().unwrap_or_default();
                stats
                    .extensions
                    .entry(extension(name))
                    .or_default()
                    .add(1, len);
                if stats.qualifies(len) {
                    stats.push_largest(len, entry.path, mtime);
                }
            }
        }
        Progress::add(&self.progress.files, direct_files);
        Progress::add(&self.progress.bytes, direct_bytes);
        if let (Some(stats), Some(local)) = (self.file_stats, &local) {
            stats.record(&self.root_name, grant, local);
        }

        let children: Vec<Result<Walked, Stop>> = subdirs
            .par_iter()
            .map(|entry| self.child(entry, dev, depth + 1, grant, sink))
            .collect();
        let mut has_rows_below = false;
        for child in children {
            let child = child?;
            agg.merge(&child.row.agg);
            if self.foldable(&child) {
                direct_bytes += child.row.agg.bytes;
                direct_files += child.row.agg.files;
            } else {
                has_rows_below = true;
                sink.lock().unwrap().push(child.row);
            }
        }
        Ok(Walked {
            row: DirRow::new(dir, role, None, direct_bytes, direct_files, agg),
            has_rows_below,
        })
    }

    fn child(
        &self,
        entry: &Entry,
        dev: u64,
        depth: usize,
        parent_grant: Option<&str>,
        sink: &Mutex<Vec<DirRow>>,
    ) -> Result<Walked, Stop> {
        let path = &entry.path;
        let own_grant = self.grant_dirs.get(path).map(String::as_str);
        if own_grant.is_none()
            && let Some(kind) = self.rules.by_path(path)
        {
            return Ok(self.excluded(path, kind));
        }
        if let Some((prev, threshold)) = self.prev
            && let Some(mut row) = self.reusable(prev, threshold, path)
        {
            Progress::add(&self.progress.reused, 1);
            row.role = self.role(path, depth);
            return Ok(Walked {
                row,
                has_rows_below: true,
            });
        }
        let grant = own_grant.or(parent_grant);
        if depth != self.unit_depth {
            return self.walk_dir(path, dev, entry.md.ctime(), depth, grant, sink);
        }
        if let Some(done) = self.units.done(path) {
            return Ok(done);
        }
        let unit_sink = Mutex::new(Vec::new());
        let walked = self.walk_dir(path, dev, entry.md.ctime(), depth, grant, &unit_sink)?;
        self.units
            .commit(&walked, unit_sink.into_inner().unwrap())
            .map_err(Stop::Failed)?;
        Progress::add(&self.progress.units, 1);
        Ok(walked)
    }

    /// The previous scan's row for `path`, if nothing under it has changed since.
    fn reusable(&self, prev: &dyn PrevRows, threshold: i64, path: &Path) -> Option<DirRow> {
        let newest = self.backend.newest_change(path).ok().flatten()?;
        if newest >= threshold {
            return None;
        }
        let mut row = prev.get(path)?;
        row.reused = true;
        Some(row)
    }

    fn excluded(&self, path: &Path, kind: ExcludeKind) -> Walked {
        let usage = self.backend.usage(path).unwrap_or(Usage {
            unreadable: 1,
            ..Usage::default()
        });
        // Counted like everything else, so the scan's totals match `stomp du`.
        Progress::add(&self.progress.files, usage.files);
        Progress::add(&self.progress.bytes, usage.bytes);
        Walked {
            row: DirRow::new(path, None, Some(kind), 0, 0, Agg::from_usage(&usage)),
            has_rows_below: false,
        }
    }

    fn foldable(&self, child: &Walked) -> bool {
        let row = &child.row;
        !child.has_rows_below
            && row.role.is_none()
            && row.excluded.is_none()
            && !row.reused
            && row.agg.bytes < self.fold_bytes
            && row.agg.files < self.fold_files
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_extensions() {
        for (name, ext) in [
            ("data.CSV", "csv"),
            ("archive.tar.gz", "gz"),
            ("out.0001", "(numbered)"),
            ("README", "(none)"),
            (".bashrc", "(none)"),
            ("trailing.", "(none)"),
            ("weird.ext-with-dash", "(other)"),
        ] {
            assert_eq!(extension(OsStr::new(name)), ext, "{name}");
        }
    }

    #[test]
    fn keeps_largest_files() {
        let mut s = ScopeStats::default();
        for i in 0..(LARGEST_KEPT as u64 + 50) {
            s.push_largest(i, PathBuf::from(format!("/f{i}")), 0);
        }
        assert_eq!(s.largest.len(), LARGEST_KEPT);
        let smallest = s.largest.peek().unwrap().0.0;
        assert_eq!(smallest, 50);
    }
}
