//! Generic POSIX backend (§6.2): measures usage by walking the tree in parallel.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use super::{Backend, BackendKind, Entry, Node, Quota, Usage, read_entries};

pub struct PosixBackend;

impl Backend for PosixBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Posix
    }

    fn usage(&self, dir: &Path) -> io::Result<Usage> {
        Ok(walk(dir, 0)?.usage)
    }

    fn tree(&self, dir: &Path, depth: usize) -> io::Result<Node> {
        walk(dir, depth)
    }

    fn quota(&self, _dir: &Path) -> io::Result<Option<Quota>> {
        Ok(None)
    }
}

/// Walks `dir` in parallel on the global rayon pool, keeping child nodes down to `keep_depth`.
///
/// Never follows symlinks and never enters another filesystem. Entries that
/// can't be read are counted in [`Usage::unreadable`] instead of failing the walk.
pub fn walk(dir: &Path, keep_depth: usize) -> io::Result<Node> {
    let md = fs::symlink_metadata(dir)?;
    if !md.is_dir() {
        return Ok(Node {
            path: dir.to_path_buf(),
            usage: Usage::file(&md),
            children: Vec::new(),
        });
    }
    let (entries, unreadable) = read_entries(dir)?;
    let mut node = walk_entries(dir, entries, unreadable, md.dev(), 0, keep_depth);
    node.usage.saw_change(md.ctime());
    Ok(node)
}

fn walk_dir(dir: &Path, dev: u64, depth: usize, keep_depth: usize) -> Node {
    match read_entries(dir) {
        Ok((entries, unreadable)) => walk_entries(dir, entries, unreadable, dev, depth, keep_depth),
        Err(_) => Node {
            path: dir.to_path_buf(),
            usage: Usage {
                unreadable: 1,
                ..Usage::default()
            },
            children: Vec::new(),
        },
    }
}

fn walk_entries(
    dir: &Path,
    entries: Vec<Entry>,
    unreadable: u64,
    dev: u64,
    depth: usize,
    keep_depth: usize,
) -> Node {
    let mut usage = Usage {
        unreadable,
        ..Usage::default()
    };
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for entry in entries {
        if entry.md.is_dir() {
            if entry.md.dev() == dev {
                usage.saw_change(entry.md.ctime());
                subdirs.push(entry.path);
            }
        } else {
            usage.add(&Usage::file(&entry.md));
        }
    }
    let children: Vec<Node> = subdirs
        .par_iter()
        .map(|sub| walk_dir(sub, dev, depth + 1, keep_depth))
        .collect();
    for child in &children {
        usage.add(&child.usage);
    }
    Node {
        path: dir.to_path_buf(),
        usage,
        children: if depth < keep_depth {
            children
        } else {
            Vec::new()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn write(path: &Path, len: usize) {
        fs::write(path, vec![b'x'; len]).unwrap();
    }

    #[test]
    fn counts_bytes_and_files_without_following_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("a/b")).unwrap();
        write(&root.join("top.txt"), 10);
        write(&root.join("a/one"), 100);
        write(&root.join("a/b/two"), 1000);
        // A symlink to a directory must not be followed (it counts as a small file).
        symlink(root.join("a"), root.join("link")).unwrap();

        let node = walk(root, 0).unwrap();
        let link_len = fs::symlink_metadata(root.join("link")).unwrap().len();
        assert_eq!(node.usage.bytes, 1110 + link_len);
        assert_eq!(node.usage.files, 4);
        assert_eq!(node.usage.unreadable, 0);
        assert!(node.usage.newest_change.is_some());
        assert!(node.children.is_empty());
    }

    #[test]
    fn keeps_children_to_depth() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("a/b")).unwrap();
        write(&root.join("a/b/f"), 5);

        let one = walk(root, 1).unwrap();
        assert_eq!(one.children.len(), 1);
        assert_eq!(one.children[0].usage.bytes, 5);
        assert!(one.children[0].children.is_empty());

        let two = walk(root, 2).unwrap();
        assert_eq!(two.children[0].children.len(), 1);
    }
}
