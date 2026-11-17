//! Remote destinations (§13): where archives go to free space on the cluster.
//!
//! Every destination stores an archive's files under the same layout,
//! `<base>/<grant>/<archive-id>/<file>`, and every upload is verified at the
//! destination before it counts (§12.4, I1).

pub mod s3;
pub mod sftp;
pub mod sigv4;

use std::path::Path;

use anyhow::{Result, bail};

use crate::config::{Config, Destination as DestinationConfig};

/// One line of a destination's self-check, for `stomp doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    Pass(String),
    Info(String),
    Warn(String),
    Fail(String),
}

pub trait Destination: Send + Sync {
    fn name(&self) -> &str;

    /// `sftp` or `s3`.
    fn kind(&self) -> &'static str;

    /// Where a path under the destination is, for people and the catalog.
    fn location(&self, rel: &str) -> String;

    /// Uploads a file and verifies it at the destination, so it exists there
    /// only once it is complete and correct. `scratch` is a directory for
    /// temporary files, such as a copy read back for verification.
    fn put(&self, local: &Path, rel: &str, sha256: &str, scratch: &Path) -> Result<()>;

    /// Checks a stored file against its SHA-256 and size.
    fn verify(&self, rel: &str, sha256: &str, size: u64, scratch: &Path) -> Result<()>;

    /// Downloads a file. The caller verifies it.
    fn get(&self, rel: &str, local: &Path) -> Result<()>;

    fn delete(&self, rel: &str) -> Result<()>;

    /// Removes an archive's (now empty) directory, where that means anything.
    fn delete_dir(&self, rel: &str) -> Result<()>;

    /// Files under a directory, with their sizes.
    fn list(&self, rel: &str) -> Result<Vec<(String, u64)>>;

    /// Checks that the destination works, for `stomp doctor`.
    fn check(&self) -> Vec<Check>;
}

/// The directory for an archive's files: `<grant>/<archive id>`.
pub fn archive_prefix(grant: Option<&str>, id: &str) -> String {
    format!("{}/{id}", grant.unwrap_or("unassigned"))
}

/// Opens a configured destination by name.
pub fn open(cfg: &Config, name: &str) -> Result<Box<dyn Destination>> {
    let Some(d) = cfg.destinations.iter().find(|d| d.name() == name) else {
        let names: Vec<&str> = cfg.destinations.iter().map(|d| d.name()).collect();
        bail!(
            "no destination named {name:?}; configured: {}",
            if names.is_empty() {
                "none".to_string()
            } else {
                names.join(", ")
            }
        );
    };
    Ok(match d {
        DestinationConfig::Sftp(s) => Box::new(sftp::Sftp::new(s)?),
        DestinationConfig::S3(s) => Box::new(s3::S3::new(s)?),
    })
}
