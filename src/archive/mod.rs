//! Archiving (§12 of the spec): packing a directory into verified, compressed
//! chunks, removing the originals, and restoring them.
//!
//! An archive's records live inside the directory it came from, and so do its
//! chunks unless they were sent to a remote destination (§13):
//!
//! ```text
//! <dir>/STOMP_ARCHIVED.txt
//! <dir>/.stomp-archive/<id>/chunk-0001.tar.zst ...     (local archives only)
//!                          manifest.jsonl.zst  SHA256SUMS  README.txt  journal.jsonl
//! ```

mod estimate;
pub mod journal;
pub mod manifest;
pub mod plan;
mod relocate;
mod restore;
mod write;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub use estimate::{Estimate, estimate};
pub use relocate::{RelocateOutcome, relocate};
pub use restore::{
    RestoreOptions, RestoreOutcome, RestorePlan, RestoreSummary, restore, restore_plan,
};
pub use write::{Hook, Outcome, Progress, Summary, archive, entry_records, verify_chunk};

use crate::config::{Config, LOCAL_DESTINATION};
use crate::paths;
use crate::scan::rules::Rules;

pub const ARCHIVE_DIR: &str = ".stomp-archive";
/// Files an archived directory holds besides any chunks: the note, and the
/// manifest, SHA256SUMS, README, settings and journal.
pub const RECORD_FILES: u64 = 6;
pub const NOTE_FILE: &str = "STOMP_ARCHIVED.txt";

/// What an archive run needs to know. Saved with the archive so a resumed
/// run uses the same settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub id: String,
    pub source: PathBuf,
    pub grant: Option<String>,
    pub chunk_size: u64,
    pub level: i32,
    pub threads: u32,
    pub only_owned: bool,
    /// `local`, or the destination holding the chunks (§13).
    #[serde(default = "local")]
    pub destination: String,
    /// Where chunks are written before upload; by default the archive directory.
    #[serde(default)]
    pub staging: Option<PathBuf>,
}

fn local() -> String {
    LOCAL_DESTINATION.to_string()
}

impl Settings {
    pub fn is_remote(&self) -> bool {
        self.destination != LOCAL_DESTINATION
    }

    /// The archive's directory at a destination: `<grant>/<id>`.
    pub fn remote_prefix(&self) -> String {
        crate::dest::archive_prefix(self.grant.as_deref(), &self.id)
    }

    /// Where chunk files are written or downloaded to.
    pub fn chunk_dir(&self, archive_dir: &Path) -> PathBuf {
        match &self.staging {
            Some(staging) if self.is_remote() => staging.join(&self.id),
            _ => archive_dir.to_path_buf(),
        }
    }
}

/// Rewrites saved settings, e.g. after an archive moves to another destination.
pub fn update_settings(archive_dir: &Path, settings: &Settings) -> Result<()> {
    manifest::write_atomic(&archive_dir.join("settings.json"), |out| {
        serde_json::to_writer_pretty(out, settings)?;
        Ok(())
    })
}

pub fn archive_path(source: &Path, id: &str) -> PathBuf {
    source.join(ARCHIVE_DIR).join(id)
}

/// Where an archive's records are kept after its files were restored while
/// a copy stays at a destination, so that copy can still be listed, checked
/// and restored from.
pub fn kept_records_dir(data_dir: &Path, id: &str) -> PathBuf {
    data_dir.join("manifests").join(id)
}

/// Kept records include the manifest only up to this size; a bigger one
/// (millions of files) is fetched from the destination when it's needed,
/// rather than taking up the user's quota.
pub const KEEP_MANIFEST_LIMIT: u64 = 64 << 20;

/// Whether `archive_dir` holds kept records rather than a live archive.
pub fn is_kept_records(archive_dir: &Path, data_dir: &Path) -> bool {
    archive_dir.starts_with(data_dir.join("manifests"))
}

/// A new archive ID: `<grant>-<yyyymmdd>-<6 hex>`.
pub fn new_id(grant: Option<&str>) -> String {
    let now = jiff::Timestamp::now();
    let noise = (now.subsec_nanosecond() as u32) ^ std::process::id().rotate_left(13);
    format!(
        "{}-{}-{:06x}",
        grant.unwrap_or("unassigned"),
        now.strftime("%Y%m%d"),
        noise & 0xff_ffff
    )
}

pub fn save_settings(archive_dir: &Path, settings: &Settings) -> Result<()> {
    let path = archive_dir.join("settings.json");
    if path.exists() {
        return Ok(());
    }
    manifest::write_atomic(&path, |out| {
        serde_json::to_writer_pretty(out, settings)?;
        Ok(())
    })
}

pub fn load_settings(archive_dir: &Path) -> Result<Settings> {
    let text = std::fs::read_to_string(archive_dir.join("settings.json"))
        .with_context(|| format!("{} isn't a stomp archive", archive_dir.display()))?;
    Ok(serde_json::from_str(&text)?)
}

/// An archive found inside a directory.
#[derive(Debug, Clone)]
pub struct Found {
    pub id: String,
    pub dir: PathBuf,
    pub finished: bool,
    pub restored: bool,
}

/// The archives in `<dir>/.stomp-archive`.
pub fn find_in(dir: &Path) -> Vec<Found> {
    let Ok(entries) = std::fs::read_dir(dir.join(ARCHIVE_DIR)) else {
        return Vec::new();
    };
    let mut found: Vec<Found> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let state = journal::Journal::read(&e.path().join("journal.jsonl")).ok()?;
            Some(Found {
                id: e.file_name().to_string_lossy().into_owned(),
                dir: e.path(),
                finished: state.finished.is_some(),
                restored: state.restored,
            })
        })
        .collect();
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found
}

/// Refuses directories stomp must not archive (§12): roots, directories
/// holding other grants, archives, environments, and directories already archived.
pub fn check_target(cfg: &Config, dir: &Path, data_dir: &Path, resuming: bool) -> Result<()> {
    let shown = paths::display(dir);
    if !dir.is_dir() {
        bail!("{shown} is not a directory");
    }
    if cfg.roots.iter().any(|r| r == dir) {
        bail!("{shown} is a root; archive the directories inside it instead");
    }
    if !cfg.roots.iter().any(|r| dir.starts_with(r)) {
        bail!("{shown} is not inside any of your roots (general.roots)");
    }
    if dir.components().any(|c| c.as_os_str() == ARCHIVE_DIR) {
        bail!("{shown} is part of a stomp archive");
    }
    let within = cfg.grants_within(dir);
    if !within.is_empty() {
        let ids: Vec<&str> = within.iter().map(|g| g.id.as_str()).collect();
        bail!(
            "{shown} contains the directories of grant(s) {}; archive each grant's directories separately",
            ids.join(", ")
        );
    }
    let rules = Rules::new(cfg, data_dir)?;
    if let Some(kind) = rules.by_path(dir) {
        bail!(
            "{shown} is excluded from scanning and archiving ({})",
            kind.as_str()
        );
    }
    if data_dir.starts_with(dir) {
        bail!("{shown} contains stomp's own data directory");
    }
    if let Ok((entries, _)) = crate::fs::read_entries(dir)
        && Rules::is_environment(&entries)
    {
        bail!(
            "{shown} is a software environment; stomp doesn't archive these (an Apptainer sandbox can be turned into one file with `apptainer build image.sif {shown}`)"
        );
    }
    if !resuming && dir.join(NOTE_FILE).exists() {
        let unfinished = find_in(dir)
            .into_iter()
            .find(|f| !f.finished && !f.restored);
        match unfinished {
            Some(f) => bail!(
                "archiving {shown} was started earlier ({}); continue it with `stomp archive --resume {shown}`",
                f.id
            ),
            None => bail!(
                "{shown} is already archived; see {}",
                paths::display(&dir.join(NOTE_FILE))
            ),
        }
    }
    Ok(())
}

/// What the catalog learns about an archive, sent through the inbox (§10.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchiveRecord {
    pub id: String,
    pub source: String,
    pub grant: Option<String>,
    pub destination: String,
    pub location: String,
    /// `in-progress`, `complete` or `restored`.
    pub state: String,
    pub created_at: Option<String>,
    pub finished_at: Option<String>,
    pub files: u64,
    pub bytes: u64,
    pub compressed: u64,
    pub chunks: u32,
    pub kept: u64,
    pub skipped: u64,
}

pub fn inbox_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("inbox").join("archives")
}

/// Leaves a record for the catalog. The newest record for an archive wins.
pub fn send_record(data_dir: &Path, record: &ArchiveRecord) -> Result<()> {
    let inbox = inbox_dir(data_dir);
    paths::create_private_dir(&inbox)?;
    let stamp = jiff::Timestamp::now().as_nanosecond();
    let path = inbox.join(format!("{}-{stamp}.json", record.id));
    manifest::write_atomic(&path, |out| {
        serde_json::to_writer_pretty(out, record)?;
        Ok(())
    })
}
