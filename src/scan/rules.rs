//! Which directories a scan counts but doesn't enter (§9.4).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use super::ExcludeKind;
use crate::config::Config;
use crate::fs::Entry;
use crate::paths;

/// Known package and container caches, relative to the home directory.
pub const KNOWN_CACHES: [&str; 8] = [
    ".cache",
    ".apptainer/cache",
    ".singularity/cache",
    ".conda/pkgs",
    // conda's package cache inside the usual installs.
    "miniconda3/pkgs",
    "anaconda3/pkgs",
    "miniforge3/pkgs",
    "mambaforge/pkgs",
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
        Self::environment_kind(entries).is_some()
    }

    /// Which kind of software environment a directory's entries mark it as.
    pub fn environment_kind(entries: &[Entry]) -> Option<Environment> {
        entries.iter().find_map(|e| {
            let name = e.path.file_name().unwrap_or_default();
            if e.md.is_dir() && name == ".singularity.d" {
                Some(Environment::Apptainer)
            } else if e.md.is_dir() && name == "conda-meta" {
                Some(Environment::Conda)
            } else if e.md.is_file() && name == "pyvenv.cfg" {
                Some(Environment::Venv)
            } else {
                None
            }
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    Apptainer,
    Conda,
    Venv,
}

impl Environment {
    /// How to make the environment smaller or rebuildable, for `dir`.
    pub fn hint(self, dir: &str) -> String {
        match self {
            Environment::Apptainer => {
                format!(
                    "an Apptainer sandbox can be turned into one file with `apptainer build image.sif {dir}`"
                )
            }
            Environment::Conda => format!(
                "a conda environment can be rebuilt from `conda env export --prefix {dir} > environment.yml`"
            ),
            Environment::Venv => format!(
                "a Python venv can be rebuilt from `{dir}/bin/pip freeze > requirements.txt`"
            ),
        }
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
