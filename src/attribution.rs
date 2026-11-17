//! Attributing storage usage to grants (§8 of the spec).
//!
//! Each grant directory is measured with its root's backend. Unassigned usage
//! is measured directly: stomp walks down from the root, around claimed
//! directories, and measures everything else. Computing it as total minus
//! grants instead could go negative while Ceph's lazily updated totals lag.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rayon::prelude::*;
use serde::Serialize;

use crate::config::Config;
use crate::fs::{Backend, BackendKind, Quota, Usage, read_entries};
use crate::paths;

#[derive(Debug, Serialize)]
pub struct Measurement {
    pub taken_at: Timestamp,
    pub roots: Vec<RootUsage>,
    pub grants: Vec<GrantUsage>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RootUsage {
    pub path: PathBuf,
    pub backend: BackendKind,
    /// Everything under the root: the grants' directories plus unassigned.
    pub total: Usage,
    pub unassigned: Usage,
    pub quota: Option<Quota>,
}

#[derive(Debug, Serialize)]
pub struct GrantUsage {
    pub id: String,
    pub total: Usage,
    pub dirs: Vec<DirUsage>,
}

#[derive(Debug, Serialize)]
pub struct DirUsage {
    pub path: PathBuf,
    /// Index of the root containing this directory.
    pub root: usize,
    /// `None` when the directory is missing or couldn't be measured.
    pub usage: Option<Usage>,
}

impl Measurement {
    /// Usage summed over all roots.
    pub fn total(&self) -> Usage {
        let mut total = Usage::default();
        for r in &self.roots {
            total.add(&r.total);
        }
        total
    }

    pub fn unreadable(&self) -> u64 {
        self.total().unreadable
    }
}

/// Measures every root and grant directory. `backends[i]` must serve `cfg.roots[i]`.
pub fn measure(cfg: &Config, backends: &[Box<dyn Backend>]) -> Measurement {
    assert_eq!(cfg.roots.len(), backends.len(), "one backend per root");
    let taken_at = Timestamp::now();
    let mut warnings = Vec::new();

    let targets: Vec<(usize, usize)> = cfg
        .grants
        .iter()
        .enumerate()
        .flat_map(|(gi, g)| (0..g.dirs.len()).map(move |di| (gi, di)))
        .collect();
    let results: Vec<io::Result<Usage>> = targets
        .par_iter()
        .map(|&(gi, di)| {
            let dir = &cfg.grants[gi].dirs[di];
            if dir.exists {
                backends[dir.root].usage(&dir.path)
            } else {
                Err(io::Error::new(io::ErrorKind::NotFound, "does not exist"))
            }
        })
        .collect();

    let mut grants: Vec<GrantUsage> = cfg
        .grants
        .iter()
        .map(|g| GrantUsage {
            id: g.id.clone(),
            total: Usage::default(),
            dirs: Vec::new(),
        })
        .collect();
    for (&(gi, di), result) in targets.iter().zip(results) {
        let dir = &cfg.grants[gi].dirs[di];
        let usage = match result {
            Ok(u) => {
                grants[gi].total.add(&u);
                Some(u)
            }
            // Config validation already warned about missing directories.
            Err(_) if !dir.exists => None,
            Err(e) => {
                warnings.push(format!(
                    "grant {}: {}: {e}",
                    cfg.grants[gi].id,
                    paths::display(&dir.path)
                ));
                None
            }
        };
        grants[gi].dirs.push(DirUsage {
            path: dir.path.clone(),
            root: dir.root,
            usage,
        });
    }

    let mut roots = Vec::new();
    for (ri, root) in cfg.roots.iter().enumerate() {
        let backend = backends[ri].as_ref();
        let claimed: Vec<&Path> = cfg
            .grants
            .iter()
            .flat_map(|g| &g.dirs)
            .filter(|d| d.root == ri && d.exists)
            .map(|d| d.path.as_path())
            .collect();
        let unassigned = unassigned_usage(root, &claimed, backend).unwrap_or_else(|e| {
            warnings.push(format!(
                "{}: could not measure unassigned usage: {e}",
                paths::display(root)
            ));
            Usage::default()
        });
        let mut total = unassigned;
        for dir in grants.iter().flat_map(|g| &g.dirs).filter(|d| d.root == ri) {
            if let Some(u) = &dir.usage {
                total.add(u);
            }
        }
        let quota = backend.quota(root).unwrap_or_else(|e| {
            warnings.push(format!(
                "{}: could not read quota: {e}",
                paths::display(root)
            ));
            None
        });
        roots.push(RootUsage {
            path: root.clone(),
            backend: backend.kind(),
            total,
            unassigned,
            quota,
        });
    }

    Measurement {
        taken_at,
        roots,
        grants,
        warnings,
    }
}

/// Usage under `root` that isn't inside any of the `claimed` directories.
pub fn unassigned_usage(
    root: &Path,
    claimed: &[&Path],
    backend: &dyn Backend,
) -> io::Result<Usage> {
    if claimed.contains(&root) {
        return Ok(Usage::default());
    }
    if claimed.is_empty() {
        return backend.usage(root);
    }
    let md = fs::symlink_metadata(root)?;
    let mut usage = frontier(root, md.dev(), claimed, backend)?;
    usage.saw_change(md.ctime());
    Ok(usage)
}

/// Measures `dir` while skipping claimed directories. Subtrees with no claimed
/// directory inside are measured whole by the backend (instant on Ceph);
/// directories that contain claimed ones are descended into.
fn frontier(dir: &Path, dev: u64, claimed: &[&Path], backend: &dyn Backend) -> io::Result<Usage> {
    let (entries, unreadable) = read_entries(dir)?;
    let parts: Vec<Usage> = entries
        .par_iter()
        .map(|e| {
            if !e.md.is_dir() {
                return Usage::file(&e.md);
            }
            if e.md.dev() != dev || claimed.contains(&e.path.as_path()) {
                return Usage::default();
            }
            let measured = if claimed.iter().any(|c| c.starts_with(&e.path)) {
                frontier(&e.path, dev, claimed, backend).map(|mut u| {
                    u.saw_change(e.md.ctime());
                    u
                })
            } else {
                backend.usage(&e.path)
            };
            measured.unwrap_or(Usage {
                unreadable: 1,
                ..Usage::default()
            })
        })
        .collect();
    let mut usage = Usage {
        unreadable,
        ..Usage::default()
    };
    for part in &parts {
        usage.add(part);
    }
    Ok(usage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::ceph::{CephBackend, fake::FakeXattrs};
    use crate::fs::posix::PosixBackend;
    use std::path::Path;

    fn write(path: &Path, len: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![b'x'; len]).unwrap();
    }

    /// root/
    ///   loose            1 byte    (unassigned)
    ///   alpha/a          10 bytes  (grant A)
    ///   beta/b           100       (grant B, first dir)
    ///   lab/beta2/c      1000      (grant B, second dir, nested in an unclaimed dir)
    ///   lab/notes        10000     (unassigned, beside a claimed dir)
    ///   other/d          100000    (unassigned)
    fn fixture() -> (tempfile::TempDir, Config) {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        write(&root.join("loose"), 1);
        write(&root.join("alpha/a"), 10);
        write(&root.join("beta/b"), 100);
        write(&root.join("lab/beta2/c"), 1000);
        write(&root.join("lab/notes"), 10000);
        write(&root.join("other/d"), 100000);
        let text = format!(
            r#"
            general.roots = ["{r}"]
            [[grant]]
            id = "A"
            dirs = ["{r}/alpha"]
            [[grant]]
            id = "B"
            dirs = ["{r}/beta", "{r}/lab/beta2"]
            [[grant]]
            id = "C"
            dirs = ["{r}/gone"]
            "#,
            r = root.display()
        );
        let cfg = Config::parse(&text, Path::new("t.toml")).unwrap();
        (tmp, cfg)
    }

    #[test]
    fn attributes_usage_with_posix_backend() {
        let (_tmp, cfg) = fixture();
        let backends: Vec<Box<dyn Backend>> = vec![Box::new(PosixBackend)];
        let m = measure(&cfg, &backends);

        assert_eq!(m.grants[0].total.bytes, 10);
        assert_eq!(m.grants[1].total.bytes, 1100);
        assert_eq!(m.grants[1].total.files, 2);
        assert_eq!(m.grants[2].total.bytes, 0);
        assert!(m.grants[2].dirs[0].usage.is_none());
        assert_eq!(m.roots[0].unassigned.bytes, 110_001);
        assert_eq!(m.roots[0].unassigned.files, 3);
        assert_eq!(m.roots[0].total.bytes, 111_111);
        // The missing directory is reported by config validation, not again here.
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    }

    #[test]
    fn attributes_usage_with_ceph_backend() {
        let (_tmp, cfg) = fixture();
        let root = cfg.roots[0].clone();
        let fake = FakeXattrs::default();
        // Ceph's totals, deliberately different from the real file sizes, to
        // prove that the instant path reads attributes instead of walking.
        fake.set_dir(&root.join("alpha"), 7_000, 70, 100);
        fake.set_dir(&root.join("beta"), 8_000, 80, 200);
        fake.set_dir(&root.join("lab/beta2"), 9_000, 90, 300);
        fake.set_dir(&root.join("other"), 5_000, 50, 400);
        let backends: Vec<Box<dyn Backend>> = vec![Box::new(CephBackend::new(fake))];
        let m = measure(&cfg, &backends);

        assert_eq!(m.grants[0].total.bytes, 7_000);
        assert_eq!(m.grants[1].total.bytes, 17_000);
        assert_eq!(m.grants[1].total.newest_change, Some(300));
        // Unassigned: other/ (from Ceph) + loose and lab/notes (stat'd directly).
        assert_eq!(m.roots[0].unassigned.bytes, 5_000 + 1 + 10_000);
        assert_eq!(m.roots[0].unassigned.files, 52);
        assert_eq!(m.roots[0].total.bytes, 7_000 + 17_000 + 15_001);
    }

    #[test]
    fn grant_claiming_the_whole_root_leaves_nothing_unassigned() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        write(&root.join("f"), 5);
        let text = format!(
            "general.roots = [\"{r}\"]\n[[grant]]\nid = \"ALL\"\ndirs = [\"{r}\"]\n",
            r = root.display()
        );
        let cfg = Config::parse(&text, Path::new("t.toml")).unwrap();
        let backends: Vec<Box<dyn Backend>> = vec![Box::new(PosixBackend)];
        let m = measure(&cfg, &backends);
        assert_eq!(m.roots[0].unassigned, Usage::default());
        assert_eq!(m.roots[0].total.bytes, 5);
    }
}
