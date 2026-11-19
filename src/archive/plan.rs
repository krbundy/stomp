//! Planning an archive (§12.3): which entries go in, in what order and chunk,
//! and what is left in place and why.
//!
//! Entries are in depth-first order with each directory's children sorted by
//! name, so a subdirectory lands in as few chunks as possible. The plan is
//! fixed when archiving starts and saved in the archive directory; files
//! created afterwards are not archived and simply stay where they are.

use std::collections::{BTreeMap, HashMap};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::manifest::{self, Kind, PathField, SkipReason, SkipRecord, TargetField};
use super::{ARCHIVE_DIR, NOTE_FILE};
use crate::fs::{Backend, Entry, read_entries};
use crate::scan::ExcludeKind;
use crate::scan::rules::Rules;

#[derive(Debug, Clone, PartialEq)]
pub struct PlanEntry {
    /// Relative to the archived directory.
    pub rel: PathBuf,
    pub kind: Kind,
    /// Size when planned (files only).
    pub size: u64,
    /// For hard links, the earlier entry with the same content.
    pub link: Option<PathBuf>,
    pub chunk: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Skip {
    pub rel: PathBuf,
    pub reason: SkipReason,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanSummary {
    pub files: u64,
    pub bytes: u64,
    pub dirs: u64,
    pub symlinks: u64,
    pub hardlinks: u64,
    pub chunks: u32,
    /// Count and bytes per reason.
    pub skipped: BTreeMap<SkipReason, (u64, u64)>,
}

pub struct Plan {
    pub entries: Vec<PlanEntry>,
    pub skipped: Vec<Skip>,
    pub summary: PlanSummary,
}

struct Raw {
    rel: PathBuf,
    kind: Kind,
    size: u64,
    /// (dev, inode) for files with more than one link.
    inode: Option<(u64, u64)>,
}

#[derive(Default)]
struct Found {
    entries: Vec<Raw>,
    skipped: Vec<Skip>,
}

impl Found {
    fn extend(&mut self, other: Found) {
        self.entries.extend(other.entries);
        self.skipped.extend(other.skipped);
    }

    fn skip(rel: PathBuf, reason: SkipReason, bytes: u64) -> Found {
        Found {
            entries: Vec::new(),
            skipped: vec![Skip { rel, reason, bytes }],
        }
    }
}

struct Walk<'a> {
    dev: u64,
    rules: &'a Rules,
    backend: &'a dyn Backend,
    /// When set, entries owned by anyone else are left in place.
    owner: Option<u32>,
}

impl Walk<'_> {
    /// Everything below a directory, in archive order.
    fn below(&self, rel: &Path, mut children: Vec<Entry>) -> Found {
        children.sort_by(|a, b| a.path.file_name().cmp(&b.path.file_name()));
        let parts: Vec<Found> = children
            .into_par_iter()
            .map(|e| self.entry(e, rel))
            .collect();
        let mut found = Found::default();
        for part in parts {
            found.extend(part);
        }
        found
    }

    fn size_of(&self, dir: &Path) -> u64 {
        self.backend.usage(dir).map_or(0, |u| u.bytes)
    }

    fn entry(&self, e: Entry, parent: &Path) -> Found {
        let name = e.path.file_name().unwrap_or_default();
        let rel = parent.join(name);
        let ft = e.md.file_type();
        if (ft.is_dir() && name == ARCHIVE_DIR) || (ft.is_file() && name == NOTE_FILE) {
            if parent.as_os_str().is_empty() {
                // This archive's own files, at the top of the directory being archived.
                return Found::default();
            }
            let bytes = if ft.is_dir() {
                self.size_of(&e.path)
            } else {
                e.md.len()
            };
            return Found::skip(rel, SkipReason::NestedArchive, bytes);
        }
        if ft.is_dir() {
            if e.md.dev() != self.dev {
                return Found::skip(rel, SkipReason::OtherFilesystem, 0);
            }
            if let Some(kind) = self.rules.by_path(&e.path) {
                let reason = match kind {
                    ExcludeKind::Environment => SkipReason::Environment,
                    ExcludeKind::Cache => SkipReason::Cache,
                    ExcludeKind::User => SkipReason::Excluded,
                    ExcludeKind::Stomp => SkipReason::StompData,
                };
                return Found::skip(rel, reason, self.size_of(&e.path));
            }
            let children = match read_entries(&e.path) {
                Ok((children, _)) => children,
                Err(_) => return Found::skip(rel, SkipReason::Unreadable, 0),
            };
            if Rules::is_environment(&children) {
                return Found::skip(rel, SkipReason::Environment, self.size_of(&e.path));
            }
            let mut found = Found {
                entries: vec![Raw {
                    rel: rel.clone(),
                    kind: Kind::Dir,
                    size: 0,
                    inode: None,
                }],
                skipped: Vec::new(),
            };
            found.extend(self.below(&rel, children));
            return found;
        }
        if !(ft.is_file() || ft.is_symlink()) {
            return Found::skip(rel, SkipReason::SpecialFile, 0);
        }
        if self.owner.is_some_and(|uid| e.md.uid() != uid) {
            return Found::skip(rel, SkipReason::NotOwned, e.md.len());
        }
        Found {
            entries: vec![if ft.is_symlink() {
                Raw {
                    rel,
                    kind: Kind::Symlink,
                    size: 0,
                    inode: None,
                }
            } else {
                Raw {
                    rel,
                    kind: Kind::File,
                    size: e.md.len(),
                    inode: (e.md.nlink() > 1).then(|| (e.md.dev(), e.md.ino())),
                }
            }],
            skipped: Vec::new(),
        }
    }
}

/// Plans an archive of `source`.
pub fn make_plan(
    source: &Path,
    rules: &Rules,
    backend: &dyn Backend,
    only_owned: bool,
    chunk_size: u64,
) -> Result<Plan> {
    let md = std::fs::symlink_metadata(source)
        .with_context(|| format!("cannot open {}", source.display()))?;
    if !md.is_dir() {
        bail!("{} is not a directory", source.display());
    }
    let (children, _) = read_entries(source)?;
    let walk = Walk {
        dev: md.dev(),
        rules,
        backend,
        owner: only_owned.then(|| rustix::process::geteuid().as_raw()),
    };
    let found = walk.below(Path::new(""), children);

    // The first path of each multiply-linked file holds the content; later ones are links to it.
    let mut first: HashMap<(u64, u64), PathBuf> = HashMap::new();
    let mut entries: Vec<PlanEntry> = Vec::with_capacity(found.entries.len());
    for raw in found.entries {
        let mut entry = PlanEntry {
            rel: raw.rel,
            kind: raw.kind,
            size: raw.size,
            link: None,
            chunk: 0,
        };
        if let Some(inode) = raw.inode {
            match first.get(&inode) {
                Some(target) => {
                    entry.kind = Kind::Hardlink;
                    entry.link = Some(target.clone());
                    entry.size = 0;
                }
                None => {
                    first.insert(inode, entry.rel.clone());
                }
            }
        }
        entries.push(entry);
    }

    // Fill chunks up to chunk_size; a file larger than that gets a chunk to itself.
    let (mut chunk, mut chunk_bytes, mut chunk_files) = (1u32, 0u64, 0u64);
    for e in &mut entries {
        if e.kind == Kind::File {
            if chunk_files > 0 && chunk_bytes + e.size > chunk_size {
                chunk += 1;
                chunk_bytes = 0;
                chunk_files = 0;
            }
            chunk_bytes += e.size;
            chunk_files += 1;
        }
        e.chunk = chunk;
    }

    let mut summary = PlanSummary {
        chunks: if entries.is_empty() { 0 } else { chunk },
        ..PlanSummary::default()
    };
    for e in &entries {
        match e.kind {
            Kind::File => {
                summary.files += 1;
                summary.bytes += e.size;
            }
            Kind::Dir => summary.dirs += 1,
            Kind::Symlink => summary.symlinks += 1,
            Kind::Hardlink => summary.hardlinks += 1,
        }
    }
    for s in &found.skipped {
        let slot = summary.skipped.entry(s.reason).or_default();
        slot.0 += 1;
        slot.1 += s.bytes;
    }
    Ok(Plan {
        entries,
        skipped: found.skipped,
        summary,
    })
}

#[derive(Serialize, Deserialize)]
struct PlanLine {
    #[serde(flatten)]
    path: PathField,
    kind: Kind,
    size: u64,
    #[serde(flatten)]
    link: TargetField,
}

fn plan_dir(archive_dir: &Path) -> PathBuf {
    archive_dir.join("plan")
}

fn chunk_file(archive_dir: &Path, k: u32) -> PathBuf {
    plan_dir(archive_dir).join(format!("chunk-{k:04}.jsonl.zst"))
}

/// Saves a plan, all or nothing: it's written to `plan.tmp` and renamed into place.
pub fn save(archive_dir: &Path, plan: &Plan) -> Result<()> {
    let tmp = archive_dir.join("plan.tmp");
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)?;
    }
    std::fs::create_dir(&tmp)?;
    for k in 1..=plan.summary.chunks {
        let lines = plan
            .entries
            .iter()
            .filter(|e| e.chunk == k)
            .map(|e| PlanLine {
                path: PathField::new(&e.rel),
                kind: e.kind,
                size: e.size,
                link: TargetField::new(e.link.as_deref()),
            });
        manifest::write_jsonl(&tmp.join(format!("chunk-{k:04}.jsonl.zst")), lines)?;
    }
    manifest::write_jsonl(
        &tmp.join("skipped.jsonl.zst"),
        plan.skipped
            .iter()
            .map(|s| SkipRecord::new(&s.rel, s.reason, s.bytes)),
    )?;
    manifest::write_atomic(&tmp.join("summary.json"), |out| {
        serde_json::to_writer_pretty(out, &plan.summary)?;
        Ok(())
    })?;
    manifest::sync_dir(&tmp)?;
    std::fs::rename(&tmp, plan_dir(archive_dir))?;
    manifest::sync_dir(archive_dir)?;
    Ok(())
}

pub fn load_summary(archive_dir: &Path) -> Result<PlanSummary> {
    let text = std::fs::read_to_string(plan_dir(archive_dir).join("summary.json"))?;
    Ok(serde_json::from_str(&text)?)
}

pub fn load_chunk(archive_dir: &Path, k: u32) -> Result<Vec<PlanEntry>> {
    manifest::read_jsonl::<PlanLine>(&chunk_file(archive_dir, k))?
        .into_iter()
        .map(|l| {
            Ok(PlanEntry {
                rel: l.path.get()?,
                kind: l.kind,
                size: l.size,
                link: l.link.get()?,
                chunk: k,
            })
        })
        .collect()
}

pub fn load_skipped(archive_dir: &Path) -> Result<Vec<SkipRecord>> {
    manifest::read_jsonl(&plan_dir(archive_dir).join("skipped.jsonl.zst"))
}

/// Removes the plan once the manifest has replaced it.
pub fn remove(archive_dir: &Path) -> Result<()> {
    let dir = plan_dir(archive_dir);
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    Ok(())
}
