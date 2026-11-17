//! Restoring an archive (§12.5).
//!
//! Only the chunks holding the requested files are read. Each chunk's
//! SHA-256 is checked first and each file's BLAKE3 as it is written. Files
//! are written under a temporary name and renamed into place, and existing
//! files are never replaced without `overwrite`. A file that already matches
//! the archive (from an earlier, interrupted restore) counts as restored.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{self, BufReader, BufWriter, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Serialize;

use super::journal::{Event, Journal};
use super::manifest::{self, EntryRecord, Kind, Record};
use super::{ARCHIVE_DIR, ArchiveRecord, NOTE_FILE, write};

#[derive(Debug, Clone, Default)]
pub struct RestoreOptions {
    /// Glob patterns relative to the archived directory; a directory pattern includes its contents.
    pub only: Vec<String>,
    /// Restore somewhere else instead of the original location.
    pub to: Option<PathBuf>,
    pub overwrite: bool,
    /// Keep the archive even after a complete restore.
    pub keep_archive: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RestoreSummary {
    pub files: u64,
    pub bytes: u64,
    pub dirs: u64,
    pub links: u64,
    /// Files that already existed and differ from the archive; left alone.
    pub conflicts: Vec<String>,
    /// Hard links whose target wasn't restored.
    pub missing_links: u64,
    pub archive_removed: bool,
}

pub enum RestoreOutcome {
    Complete(RestoreSummary),
    Incomplete(RestoreSummary),
}

/// Everything the manifest knows, whether the archive finished or not.
fn load_entries(archive_dir: &Path) -> Result<(Vec<EntryRecord>, HashMap<u32, String>)> {
    let manifest_path = archive_dir.join(manifest::MANIFEST);
    let mut sums = HashMap::new();
    if manifest_path.exists() {
        // A finished archive: the manifest and SHA256SUMS.
        for (name, sum) in manifest::read_checksums(&archive_dir.join(manifest::CHECKSUMS))? {
            if let Some(k) = name
                .strip_prefix("chunk-")
                .and_then(|n| n.strip_suffix(".tar.zst"))
                .and_then(|n| n.parse().ok())
            {
                sums.insert(k, sum);
            }
        }
        let entries = manifest::read_records(&manifest_path)?
            .into_iter()
            .filter_map(|r| match r {
                Record::Entry(e) => Some(e),
                _ => None,
            })
            .collect();
        return Ok((entries, sums));
    }
    // An unfinished archive: per-chunk records for every chunk that was written.
    let state = Journal::read(&archive_dir.join("journal.jsonl"))?;
    let mut entries = Vec::new();
    for (k, sha) in state.written_chunks() {
        let path = archive_dir
            .join("manifest.d")
            .join(format!("chunk-{k:04}.jsonl"));
        if path.exists() {
            entries.extend(write::entry_records(&path)?);
            sums.insert(k, sha);
        }
    }
    Ok((entries, sums))
}

fn matcher(patterns: &[String]) -> Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut set = GlobSetBuilder::new();
    for p in patterns {
        let p = p.trim_end_matches('/');
        for pattern in [p.to_string(), format!("{p}/**")] {
            set.add(
                GlobBuilder::new(&pattern)
                    .literal_separator(true)
                    .build()
                    .with_context(|| format!("bad pattern {p:?}"))?,
            );
        }
    }
    Ok(Some(set.build()?))
}

/// Whether `path` already holds exactly what the record describes.
fn already_restored(path: &Path, r: &EntryRecord) -> bool {
    let Ok(md) = fs::symlink_metadata(path) else {
        return false;
    };
    use std::os::unix::fs::MetadataExt;
    match r.kind {
        Kind::File | Kind::Hardlink => md.is_file() && md.len() == r.size && md.mtime() == r.mtime,
        Kind::Symlink => {
            md.file_type().is_symlink()
                && r.target
                    .get()
                    .ok()
                    .flatten()
                    .is_some_and(|t| fs::read_link(path).is_ok_and(|l| l == t))
        }
        Kind::Dir => md.is_dir(),
    }
}

fn set_mtime(file: &File, mtime: i64) -> io::Result<()> {
    let when = if mtime >= 0 {
        UNIX_EPOCH + Duration::from_secs(mtime as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs(mtime.unsigned_abs())
    };
    file.set_times(FileTimes::new().set_modified(when))
}

/// Restores from the archive in `archive_dir`, whose original location is `source`.
pub fn restore(
    archive_dir: &Path,
    source: &Path,
    options: &RestoreOptions,
    data_dir: &Path,
    stop: &AtomicBool,
) -> Result<RestoreOutcome> {
    let (entries, sums) = load_entries(archive_dir)?;
    let filter = matcher(&options.only)?;
    let dest = options.to.clone().unwrap_or_else(|| source.to_path_buf());
    fs::create_dir_all(&dest)?;

    let mut selected: Vec<(PathBuf, &EntryRecord)> = Vec::new();
    for r in &entries {
        let rel = r.rel()?;
        if filter.as_ref().is_none_or(|f| f.is_match(&rel)) {
            selected.push((rel, r));
        }
    }
    if selected.is_empty() {
        bail!("nothing in the archive matches");
    }
    // Directories holding selected entries are recreated too.
    let selected_paths: HashSet<PathBuf> = selected.iter().map(|(p, _)| p.clone()).collect();
    let mut needed_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for p in &selected_paths {
        for a in p.ancestors().skip(1) {
            if !a.as_os_str().is_empty() {
                needed_dirs.insert(a.to_path_buf());
            }
        }
    }
    let wanted: HashMap<Vec<u8>, &EntryRecord> = entries
        .iter()
        .filter_map(|r| {
            let rel = r.rel().ok()?;
            (selected_paths.contains(&rel) || needed_dirs.contains(&rel))
                .then(|| (rel.as_os_str().as_bytes().to_vec(), r))
        })
        .collect();
    let chunks: BTreeSet<u32> = wanted.values().map(|r| r.chunk).collect();

    let mut summary = RestoreSummary::default();
    let mut restored: HashSet<Vec<u8>> = HashSet::new();
    let mut dirs: Vec<(PathBuf, &EntryRecord)> = Vec::new();
    for k in chunks {
        if stop.load(Ordering::Relaxed) {
            return Ok(RestoreOutcome::Incomplete(summary));
        }
        let path = archive_dir.join(manifest::chunk_name(k));
        let sha = sums
            .get(&k)
            .ok_or_else(|| anyhow!("no checksum recorded for chunk {k}"))?;
        write::verify_chunk(&path, sha, &[], false)?;
        let mut archive =
            tar::Archive::new(zstd::Decoder::new(BufReader::new(File::open(&path)?))?);
        for entry in archive.entries()? {
            let mut entry = entry?;
            let mut name = entry.path_bytes().into_owned();
            while name.len() > 1 && name.ends_with(b"/") {
                name.pop();
            }
            let Some(record) = wanted.get(&name).copied() else {
                continue;
            };
            let rel = record.rel()?;
            let target = dest.join(&rel);
            if record.kind != Kind::Dir
                && (record.changed || record.kept)
                && options.to.is_none()
                && fs::symlink_metadata(&target).is_ok()
            {
                // The original was left in place on purpose, and is newer than the archive.
                continue;
            }
            match record.kind {
                Kind::Dir => {
                    fs::create_dir_all(&target)?;
                    dirs.push((target, record));
                }
                Kind::File => {
                    if already_restored(&target, record) {
                        summary.files += 1;
                        summary.bytes += record.size;
                        restored.insert(name);
                        continue;
                    }
                    if fs::symlink_metadata(&target).is_ok() && !options.overwrite {
                        summary.conflicts.push(rel.to_string_lossy().into_owned());
                        continue;
                    }
                    write_file(&mut entry, &target, record)?;
                    summary.files += 1;
                    summary.bytes += record.size;
                    restored.insert(name);
                }
                Kind::Symlink => {
                    let link_target = record.target.get()?.context("symlink without a target")?;
                    if already_restored(&target, record) {
                        summary.links += 1;
                        continue;
                    }
                    if fs::symlink_metadata(&target).is_ok() {
                        if !options.overwrite {
                            summary.conflicts.push(rel.to_string_lossy().into_owned());
                            continue;
                        }
                        fs::remove_file(&target)?;
                    }
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    std::os::unix::fs::symlink(&link_target, &target)?;
                    summary.links += 1;
                }
                Kind::Hardlink => {
                    let link_rel = record.target.get()?.context("hard link without a target")?;
                    let source_path = dest.join(&link_rel);
                    if already_restored(&target, record) {
                        summary.links += 1;
                        continue;
                    }
                    if !restored.contains(link_rel.as_os_str().as_bytes()) && !source_path.is_file()
                    {
                        summary.missing_links += 1;
                        continue;
                    }
                    if fs::symlink_metadata(&target).is_ok() {
                        if !options.overwrite {
                            summary.conflicts.push(rel.to_string_lossy().into_owned());
                            continue;
                        }
                        fs::remove_file(&target)?;
                    }
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::hard_link(&source_path, &target)?;
                    summary.links += 1;
                }
            }
        }
    }
    // Directory permissions and times last, deepest first, so files could be written into them.
    dirs.sort_by(|a, b| b.0.cmp(&a.0));
    for (path, record) in &dirs {
        fs::set_permissions(path, fs::Permissions::from_mode(record.mode))?;
        if let Ok(dir) = File::open(path) {
            let _ = set_mtime(&dir, record.mtime);
        }
        summary.dirs += 1;
    }

    // A complete, clean restore to the original place makes the archive redundant.
    let everything = options.only.is_empty() && options.to.is_none();
    let finished = archive_dir.join(manifest::MANIFEST).exists();
    if everything
        && finished
        && summary.conflicts.is_empty()
        && summary.missing_links == 0
        && !options.keep_archive
    {
        remove_archive(archive_dir, source)?;
        summary.archive_removed = true;
        let settings = super::load_settings(archive_dir).ok();
        super::send_record(
            data_dir,
            &ArchiveRecord {
                id: archive_dir
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                source: source.to_string_lossy().into_owned(),
                grant: settings.and_then(|s| s.grant),
                destination: "local".into(),
                location: archive_dir.to_string_lossy().into_owned(),
                state: "restored".into(),
                created_at: None,
                finished_at: Some(jiff::Timestamp::now().to_string()),
                files: summary.files,
                bytes: summary.bytes,
                compressed: 0,
                chunks: 0,
                kept: 0,
                skipped: 0,
            },
        )?;
    }
    Ok(RestoreOutcome::Complete(summary))
}

fn write_file(entry: &mut impl io::Read, target: &Path, record: &EntryRecord) -> Result<()> {
    let parent = target.parent().context("restore target has no parent")?;
    fs::create_dir_all(parent)?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(target.file_name().unwrap_or_default());
    tmp_name.push(format!(".stomp-restore-{}", std::process::id()));
    let tmp = parent.join(tmp_name);
    let result = (|| -> Result<()> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        let mut out = BufWriter::with_capacity(1 << 20, file);
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut written = 0u64;
        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n])?;
            written += n as u64;
        }
        let file = out.into_inner().map_err(|e| e.into_error())?;
        if !record.changed
            && (written != record.size
                || Some(hasher.finalize().to_hex().to_string()) != record.blake3)
        {
            bail!(
                "{} doesn't match its checksum in the archive",
                target.display()
            );
        }
        file.set_permissions(fs::Permissions::from_mode(record.mode))?;
        set_mtime(&file, record.mtime)?;
        file.sync_all()?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            fs::rename(&tmp, target)?;
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Removes an archive's files after everything in it was restored.
fn remove_archive(archive_dir: &Path, source: &Path) -> Result<()> {
    let (mut journal, mut state) = Journal::open(&archive_dir.join("journal.jsonl"))?;
    journal.append(&mut state, Event::Restored)?;
    drop(journal);
    fs::remove_dir_all(archive_dir)?;
    let parent = source.join(ARCHIVE_DIR);
    let _ = fs::remove_dir(&parent); // only if no other archive is there
    if !parent.exists() {
        let _ = fs::remove_file(source.join(NOTE_FILE));
    }
    Ok(())
}
