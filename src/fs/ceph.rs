//! CephFS backend (§6.1): reads recursive statistics from virtual extended attributes.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use rayon::prelude::*;

use super::{Backend, BackendKind, Node, Quota, Usage, read_entries};

/// `statfs` filesystem type of the CephFS kernel client.
pub const CEPH_SUPER_MAGIC: u64 = 0x00c3_6400;

/// Source of extended attributes. Tests substitute a fake.
pub trait XattrSource: Send + Sync {
    /// The attribute's value, or `None` if it isn't set or the filesystem doesn't support it.
    fn get(&self, path: &Path, name: &str) -> io::Result<Option<Vec<u8>>>;
}

/// Reads attributes from the real filesystem.
pub struct SystemXattrs;

impl XattrSource for SystemXattrs {
    fn get(&self, path: &Path, name: &str) -> io::Result<Option<Vec<u8>>> {
        use rustix::io::Errno;
        match xattr::get(path, name) {
            Ok(value) => Ok(value),
            Err(e)
                if e.raw_os_error() == Some(Errno::NODATA.raw_os_error())
                    || e.raw_os_error() == Some(Errno::NOTSUP.raw_os_error()) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
}

/// Whether `path` is on CephFS.
pub fn is_ceph(path: &Path) -> io::Result<bool> {
    let st = rustix::fs::statfs(path)?;
    if st.f_type as u64 == CEPH_SUPER_MAGIC {
        return Ok(true);
    }
    // ceph-fuse reports a FUSE filesystem type but still serves the attributes.
    Ok(SystemXattrs.get(path, "ceph.dir.rbytes")?.is_some())
}

pub struct CephBackend<X: XattrSource> {
    xattrs: X,
}

impl<X: XattrSource> CephBackend<X> {
    pub fn new(xattrs: X) -> Self {
        Self { xattrs }
    }

    fn text(&self, path: &Path, name: &str) -> io::Result<Option<String>> {
        Ok(self.xattrs.get(path, name)?.map(|value| {
            String::from_utf8_lossy(&value)
                .trim_end_matches('\0')
                .trim()
                .to_string()
        }))
    }

    fn number(&self, path: &Path, name: &str) -> io::Result<Option<u64>> {
        match self.text(path, name)? {
            None => Ok(None),
            Some(text) => text.parse().map(Some).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: unexpected value {text:?} for {name}", path.display()),
                )
            }),
        }
    }

    fn required(&self, path: &Path, name: &str) -> io::Result<u64> {
        self.number(path, name)?.ok_or_else(|| {
            io::Error::other(format!(
                "{}: CephFS attribute {name} is not available",
                path.display()
            ))
        })
    }

    /// Newest change time in the subtree. Ceph formats it as `<seconds>.<nanoseconds>`;
    /// only the seconds are needed.
    fn rctime(&self, path: &Path) -> io::Result<Option<i64>> {
        Ok(self
            .text(path, "ceph.dir.rctime")?
            .and_then(|t| t.split('.').next()?.parse().ok()))
    }
}

impl<X: XattrSource> Backend for CephBackend<X> {
    fn kind(&self) -> BackendKind {
        BackendKind::Ceph
    }

    fn usage(&self, dir: &Path) -> io::Result<Usage> {
        Ok(Usage {
            bytes: self.required(dir, "ceph.dir.rbytes")?,
            files: self.required(dir, "ceph.dir.rfiles")?,
            newest_change: self.rctime(dir)?,
            unreadable: 0,
        })
    }

    fn tree(&self, dir: &Path, depth: usize) -> io::Result<Node> {
        let usage = self.usage(dir)?;
        let mut children = Vec::new();
        if depth > 0 {
            let dev = fs::symlink_metadata(dir)?.dev();
            // A directory we can't list still has known totals; it just shows no children.
            if let Ok((entries, _)) = read_entries(dir) {
                children = entries
                    .par_iter()
                    .filter(|e| e.is_dir_on(dev))
                    .filter_map(|e| self.tree(&e.path, depth - 1).ok())
                    .collect();
            }
        }
        Ok(Node {
            path: dir.to_path_buf(),
            usage,
            children,
        })
    }

    /// Finds the nearest quota on `dir` or one of its ancestors on the same filesystem.
    fn quota(&self, dir: &Path) -> io::Result<Option<Quota>> {
        let dev = fs::metadata(dir)?.dev();
        for ancestor in dir.ancestors() {
            if !fs::metadata(ancestor).is_ok_and(|md| md.dev() == dev) {
                break;
            }
            // Unset quotas read as missing or as 0, depending on the Ceph version.
            let read = |name| {
                self.number(ancestor, name)
                    .ok()
                    .flatten()
                    .filter(|&n| n > 0)
            };
            let max_bytes = read("ceph.quota.max_bytes");
            let max_files = read("ceph.quota.max_files");
            if max_bytes.is_some() || max_files.is_some() {
                return Ok(Some(Quota {
                    dir: ancestor.to_path_buf(),
                    max_bytes,
                    max_files,
                    used: self.usage(ancestor)?,
                }));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A fake attribute source for testing the Ceph backend off-cluster.

    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct FakeXattrs {
        values: Mutex<HashMap<(PathBuf, String), String>>,
    }

    impl FakeXattrs {
        pub fn set(&self, path: &Path, name: &str, value: &str) {
            self.values
                .lock()
                .unwrap()
                .insert((path.to_path_buf(), name.to_string()), value.to_string());
        }

        /// Sets the recursive totals Ceph would report for a directory.
        pub fn set_dir(&self, path: &Path, bytes: u64, files: u64, rctime: i64) {
            self.set(path, "ceph.dir.rbytes", &bytes.to_string());
            self.set(path, "ceph.dir.rfiles", &files.to_string());
            self.set(path, "ceph.dir.rctime", &format!("{rctime}.123456789"));
        }
    }

    impl XattrSource for FakeXattrs {
        fn get(&self, path: &Path, name: &str) -> io::Result<Option<Vec<u8>>> {
            Ok(self
                .values
                .lock()
                .unwrap()
                .get(&(path.to_path_buf(), name.to_string()))
                .map(|v| v.clone().into_bytes()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeXattrs;
    use super::*;

    #[test]
    fn reads_recursive_stats() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = FakeXattrs::default();
        fake.set_dir(tmp.path(), 5000, 12, 1_700_000_000);
        let backend = CephBackend::new(fake);
        let usage = backend.usage(tmp.path()).unwrap();
        assert_eq!(usage.bytes, 5000);
        assert_eq!(usage.files, 12);
        assert_eq!(usage.newest_change, Some(1_700_000_000));
    }

    #[test]
    fn missing_stats_are_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let backend = CephBackend::new(FakeXattrs::default());
        assert!(backend.usage(tmp.path()).is_err());
    }

    #[test]
    fn builds_tree_to_requested_depth() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = a.join("b");
        fs::create_dir_all(&b).unwrap();
        let fake = FakeXattrs::default();
        fake.set_dir(tmp.path(), 300, 3, 10);
        fake.set_dir(&a, 200, 2, 10);
        fake.set_dir(&b, 100, 1, 10);
        let backend = CephBackend::new(fake);

        let shallow = backend.tree(tmp.path(), 1).unwrap();
        assert_eq!(shallow.children.len(), 1);
        assert!(shallow.children[0].children.is_empty());

        let deep = backend.tree(tmp.path(), 2).unwrap();
        assert_eq!(deep.children[0].children[0].usage.bytes, 100);
    }

    #[test]
    fn finds_quota_on_an_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        fs::create_dir(&sub).unwrap();
        let fake = FakeXattrs::default();
        fake.set_dir(tmp.path(), 600, 6, 10);
        fake.set(tmp.path(), "ceph.quota.max_bytes", "1000");
        fake.set(&sub, "ceph.quota.max_bytes", "0"); // unset
        let backend = CephBackend::new(fake);

        let quota = backend.quota(&sub).unwrap().unwrap();
        assert_eq!(quota.dir, tmp.path());
        assert_eq!(quota.max_bytes, Some(1000));
        assert_eq!(quota.max_files, None);
        assert_eq!(quota.used.bytes, 600);
    }
}
