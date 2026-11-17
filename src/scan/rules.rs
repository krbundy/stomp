//! Which directories a scan counts but doesn't enter (§9.4).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use super::ExcludeKind;
use crate::config::Config;
use crate::fs::Entry;
use crate::paths;

/// Known package and container caches, relative to the home directory.
pub const KNOWN_CACHES: [&str; 4] = [
    ".cache",
    ".apptainer/cache",
    ".singularity/cache",
    ".conda/pkgs",
];

pub struct Rules {
    stomp: Vec<PathBuf>,
    caches: Vec<PathBuf>,
    globs: GlobSet,
}

impl Rules {
    pub fn new(cfg: &Config, data_dir: &Path) -> Result<Rules> {
        Self::build(
            &paths::home_dir()?,
            data_dir,
            cfg.staging_dir.as_deref(),
            &cfg.scan.exclude,
        )
    }

    pub fn build(
        home: &Path,
        data_dir: &Path,
        staging_dir: Option<&Path>,
        patterns: &[String],
    ) -> Result<Rules> {
        let caches = KNOWN_CACHES
            .iter()
            .map(|c| paths::resolve(&home.join(c)).0)
            .collect();
        let stomp = std::iter::once(data_dir)
            .chain(staging_dir)
            .map(|p| paths::resolve(p).0)
            .collect();
        let mut globs = GlobSetBuilder::new();
        for pattern in patterns {
            let glob = GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()
                .with_context(|| format!("scan.exclude: {pattern:?}"))?;
            globs.add(glob);
        }
        Ok(Rules {
            stomp,
            caches,
            globs: globs.build()?,
        })
    }

    /// Exclusion decided by a directory's path alone.
    pub fn by_path(&self, path: &Path) -> Option<ExcludeKind> {
        if self.stomp.iter().any(|p| p == path) {
            Some(ExcludeKind::Stomp)
        } else if self.caches.iter().any(|p| p == path) {
            Some(ExcludeKind::Cache)
        } else if self.globs.is_match(path) {
            Some(ExcludeKind::User)
        } else {
            None
        }
    }

    /// Whether a directory's entries mark it as a software environment: an
    /// Apptainer/Singularity sandbox, a conda environment or a Python venv.
    pub fn is_environment(entries: &[Entry]) -> bool {
        entries.iter().any(|e| {
            let name = e.path.file_name().unwrap_or_default();
            (e.md.is_dir() && (name == ".singularity.d" || name == "conda-meta"))
                || (e.md.is_file() && name == "pyvenv.cfg")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_absolute_paths() {
        let mut b = GlobSetBuilder::new();
        for p in ["**/node_modules", "/data/*/tmp"] {
            b.add(GlobBuilder::new(p).literal_separator(true).build().unwrap());
        }
        let set = b.build().unwrap();
        assert!(set.is_match("/home/u/proj/node_modules"));
        assert!(set.is_match("/data/x/tmp"));
        assert!(!set.is_match("/data/x/y/tmp"));
        assert!(!set.is_match("/home/u/node_modules_old"));
    }
}
