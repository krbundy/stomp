//! Filesystem backends (§6 of the spec).
//!
//! A backend measures directory trees. The Ceph backend reads CephFS's
//! recursive statistics, so measuring is instant; the POSIX backend walks
//! the tree.

pub mod ceph;
pub mod posix;

use std::fs::{self, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    Ceph,
    Posix,
}

impl BackendKind {
    /// Name shown to people.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ceph => "CephFS",
            Self::Posix => "POSIX",
        }
    }

    /// Name stored in the catalog.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ceph => "ceph",
            Self::Posix => "posix",
        }
    }
}

/// Usage of a directory subtree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    /// Total logical size of the files.
    pub bytes: u64,
    /// Number of files (anything that isn't a directory).
    pub files: u64,
    /// Newest change time (ctime) anywhere in the subtree, as Unix seconds.
    pub newest_change: Option<i64>,
    /// Entries that couldn't be read while walking, and so aren't counted.
    pub unreadable: u64,
}

impl Usage {
    /// The usage of a single file.
    pub fn file(md: &Metadata) -> Usage {
        Usage {
            bytes: md.len(),
            files: 1,
            newest_change: Some(md.ctime()),
            unreadable: 0,
        }
    }

    pub fn add(&mut self, other: &Usage) {
        self.bytes += other.bytes;
        self.files += other.files;
        self.unreadable += other.unreadable;
        if let Some(t) = other.newest_change {
            self.saw_change(t);
        }
    }

    /// Records a change time, keeping the newest.
    pub fn saw_change(&mut self, t: i64) {
        self.newest_change = Some(self.newest_change.map_or(t, |c| c.max(t)));
    }
}

/// A directory and the usage of its subtree, with child directories down to a requested depth.
#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub path: PathBuf,
    pub usage: Usage,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}

/// A quota and the directory it's set on.
#[derive(Debug, Clone, Serialize)]
pub struct Quota {
    pub dir: PathBuf,
    pub max_bytes: Option<u64>,
    pub max_files: Option<u64>,
    pub used: Usage,
}

pub trait Backend: Send + Sync {
    fn kind(&self) -> BackendKind;

    /// Usage of the whole subtree under `dir`.
    fn usage(&self, dir: &Path) -> io::Result<Usage>;

    /// Usage of `dir`, with child directories down to `depth` levels.
    fn tree(&self, dir: &Path, depth: usize) -> io::Result<Node>;

    /// The quota that applies to `dir`, if the filesystem exposes one.
    fn quota(&self, dir: &Path) -> io::Result<Option<Quota>>;

    /// The newest change time anywhere under `dir`, if known without walking
    /// it. Incremental scans use this to skip unchanged subtrees (§9.2).
    fn newest_change(&self, _dir: &Path) -> io::Result<Option<i64>> {
        Ok(None)
    }

    /// Whether usage is available without walking the tree.
    fn is_instant(&self) -> bool {
        self.kind() == BackendKind::Ceph
    }
}

/// Picks the backend for the filesystem holding `path`.
pub fn detect(path: &Path) -> io::Result<Box<dyn Backend>> {
    if ceph::is_ceph(path)? {
        Ok(Box::new(ceph::CephBackend::new(ceph::SystemXattrs)))
    } else {
        Ok(Box::new(posix::PosixBackend))
    }
}

/// A directory entry with its metadata. Symlinks are not followed.
pub struct Entry {
    pub path: PathBuf,
    pub md: Metadata,
}

impl Entry {
    /// Whether this is a directory on the filesystem `dev`, which a walk should enter.
    pub fn is_dir_on(&self, dev: u64) -> bool {
        self.md.is_dir() && self.md.dev() == dev
    }
}

/// Reads a directory's entries. The count is of entries that couldn't be read.
pub fn read_entries(dir: &Path) -> io::Result<(Vec<Entry>, u64)> {
    let mut entries = Vec::new();
    let mut unreadable = 0;
    for entry in fs::read_dir(dir)? {
        // DirEntry::metadata doesn't follow symlinks.
        match entry.and_then(|e| {
            Ok(Entry {
                md: e.metadata()?,
                path: e.path(),
            })
        }) {
            Ok(e) => entries.push(e),
            Err(_) => unreadable += 1,
        }
    }
    Ok((entries, unreadable))
}
