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
use crate::dest::Destination;

#[derive(Debug, Clone, Default)]
pub struct RestoreOptions {
    /// Glob patterns relative to the archived directory; a directory pattern includes its contents.
    pub only: Vec<String>,
    /// Restore somewhere else instead of the original location.
    pub to: Option<PathBuf>,
    pub overwrite: bool,
    /// Keep the archive even after a complete restore.
    pub keep_archive: bool,
    /// After a complete restore of a remote archive, delete its copy at the
    /// destination too (it's kept by default).
    pub delete_remote: bool,
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
    /// The archive was unfinished, so restoring everything undid it.
    pub abandoned: bool,
    /// Where a copy of the archive remains, if anywhere.
    pub copy_kept_at: Option<String>,
}

/// What a restore would do, for `--dry-run` and for checking a request
/// before submitting a task.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RestorePlan {
    pub files: u64,
    pub bytes: u64,
    pub dirs: u64,
    pub links: u64,
    /// Files already in place exactly as archived.
    pub already: u64,
    /// Existing files that differ from the archive.
    pub conflicts: Vec<String>,
    /// Chunks to read, and how many of them must be downloaded.
    pub chunks: usize,
    pub downloads: usize,
    /// Whether the archive is finished; restoring all of an unfinished one undoes it.
    pub finished: bool,
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

/// The selected entries (path and record) and every record a restore needs:
/// the selection plus the directories holding it, keyed by tar path.
type Selection<'a> = (
    Vec<(PathBuf, &'a EntryRecord)>,
    HashMap<Vec<u8>, &'a EntryRecord>,
);

fn select<'a>(entries: &'a [EntryRecord], only: &[String]) -> Result<Selection<'a>> {
    let filter = matcher(only)?;
    let mut selected: Vec<(PathBuf, &EntryRecord)> = Vec::new();
    for r in entries {
        let rel = r.rel()?;
        if filter.as_ref().is_none_or(|f| f.is_match(&rel)) {
            selected.push((rel, r));
        }
    }
    // Nothing at all is fine: an unfinished archive may not have written a
    // chunk yet, and restoring it still undoes it. Patterns must match.
    if selected.is_empty() && !only.is_empty() {
        bail!(
            "nothing in the archive matches {}; patterns are relative to the archived directory, \
             e.g. `results` or `results/*.csv`",
            only.iter()
                .map(|p| format!("--only {p:?}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
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
    let wanted = entries
        .iter()
        .filter_map(|r| {
            let rel = r.rel().ok()?;
            (selected_paths.contains(&rel) || needed_dirs.contains(&rel))
                .then(|| (rel.as_os_str().as_bytes().to_vec(), r))
        })
        .collect();
    Ok((selected, wanted))
}

/// Where chunks are downloaded to during a restore. Restoring from kept
/// records (the files were restored before, a copy stays at a destination)
/// downloads next to where the files go, not into stomp's data directory.
fn scratch_dir(
    archive_dir: &Path,
    data_dir: &Path,
    dest: &Path,
    settings: Option<&super::Settings>,
) -> PathBuf {
    match settings {
        Some(s) if super::is_kept_records(archive_dir, data_dir) => s
            .staging
            .as_ref()
            .map_or_else(|| dest.join(ARCHIVE_DIR).join(&s.id), |st| st.join(&s.id)),
        Some(s) => s.chunk_dir(archive_dir),
        None => archive_dir.to_path_buf(),
    }
}

/// Plans a restore without changing anything. `check_targets` also looks at
/// what's already where the files would go (a metadata lookup per file).
pub fn restore_plan(
    archive_dir: &Path,
    source: &Path,
    options: &RestoreOptions,
    data_dir: &Path,
    check_targets: bool,
) -> Result<RestorePlan> {
    let settings = super::load_settings(archive_dir).ok();
    let dest = options.to.clone().unwrap_or_else(|| source.to_path_buf());
    let scratch = scratch_dir(archive_dir, data_dir, &dest, settings.as_ref());
    let (entries, _) = load_entries(archive_dir)?;
    let (selected, wanted) = select(&entries, &options.only)?;
    let mut plan = RestorePlan {
        finished: archive_dir.join(manifest::MANIFEST).exists(),
        ..RestorePlan::default()
    };
    let chunks: BTreeSet<u32> = wanted.values().map(|r| r.chunk).collect();
    plan.chunks = chunks.len();
    plan.downloads = chunks
        .iter()
        .filter(|k| {
            let name = manifest::chunk_name(**k);
            !archive_dir.join(&name).exists() && !scratch.join(&name).exists()
        })
        .count();
    // Directories are counted as the restore counts them: the ones selected
    // and the ones holding the selection, which it recreates too.
    plan.dirs = wanted.values().filter(|r| r.kind == Kind::Dir).count() as u64;
    for (rel, r) in &selected {
        let target = dest.join(rel);
        match r.kind {
            Kind::Dir => {}
            Kind::Symlink | Kind::Hardlink => plan.links += 1,
            Kind::File => {
                plan.files += 1;
                plan.bytes += r.size;
            }
        }
        if !check_targets || r.kind == Kind::Dir || fs::symlink_metadata(&target).is_err() {
            continue;
        }
        if already_restored(&target, r) {
            plan.already += 1;
        } else if !((r.changed || r.kept) && options.to.is_none()) {
            plan.conflicts.push(rel.to_string_lossy().into_owned());
        }
    }
    Ok(plan)
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

/// Restores from the archive in `archive_dir`, whose original location is
/// `source`. Chunks that aren't on disk are downloaded from `remote`.
pub fn restore(
    archive_dir: &Path,
    source: &Path,
    options: &RestoreOptions,
    remote: Option<&dyn Destination>,
    data_dir: &Path,
    stop: &AtomicBool,
) -> Result<RestoreOutcome> {
    let settings = super::load_settings(archive_dir).ok();
    let kept_records = super::is_kept_records(archive_dir, data_dir);
    let (entries, sums) = load_entries(archive_dir)?;
    let (_, wanted) = select(&entries, &options.only)?;
    let dest = options.to.clone().unwrap_or_else(|| source.to_path_buf());
    let scratch = scratch_dir(archive_dir, data_dir, &dest, settings.as_ref());
    fs::create_dir_all(&dest)?;
    let chunks: BTreeSet<u32> = wanted.values().map(|r| r.chunk).collect();

    let mut summary = RestoreSummary::default();
    let mut restored: HashSet<Vec<u8>> = HashSet::new();
    let mut dirs: Vec<(PathBuf, &EntryRecord)> = Vec::new();
    for k in chunks {
        if stop.load(Ordering::Relaxed) {
            return Ok(RestoreOutcome::Incomplete(summary));
        }
        let name = manifest::chunk_name(k);
        let sha = sums
            .get(&k)
            .ok_or_else(|| anyhow!("no checksum recorded for chunk {k}"))?;
        let on_disk = [archive_dir.join(&name), scratch.join(&name)]
            .into_iter()
            .find(|p| p.exists());
        let (path, downloaded) = match on_disk {
            Some(p) => (p, false),
            None => {
                let (remote, s) = remote.zip(settings.as_ref()).with_context(|| {
                    format!("chunk {k} isn't on disk and the archive's destination isn't available")
                })?;
                crate::paths::create_private_dir(&scratch)?;
                let tmp = scratch.join(format!("{name}.download"));
                remote.get(&format!("{}/{name}", s.remote_prefix()), &tmp)?;
                (tmp, true)
            }
        };
        if let Err(e) = write::verify_chunk(&path, sha, &[], false) {
            if downloaded {
                let _ = fs::remove_file(&path);
            }
            return Err(e);
        }
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
        drop(archive);
        if downloaded {
            let _ = fs::remove_file(&path);
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
    if kept_records {
        // Downloads went next to the restored files; tidy up after them.
        let _ = fs::remove_dir(&scratch);
        if let Some(parent) = scratch.parent() {
            let _ = fs::remove_dir(parent);
        }
    }

    // A complete, clean restore to the original place makes the archive redundant.
    let everything = options.only.is_empty() && options.to.is_none();
    let clean = summary.conflicts.is_empty() && summary.missing_links == 0;
    if !(everything && clean) || options.keep_archive {
        if let (Some(remote), Some(s)) = (remote, settings.as_ref())
            && kept_records
        {
            summary.copy_kept_at = Some(remote.location(&s.remote_prefix()));
        }
        return Ok(RestoreOutcome::Complete(summary));
    }
    let finished = archive_dir.join(manifest::MANIFEST).exists();
    let remote_copy = remote.zip(settings.as_ref().filter(|s| s.is_remote()));
    let id = archive_dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let state;
    let mut location = source.to_string_lossy().into_owned();
    if !finished {
        // Everything archived so far is back, and the rest never left: the
        // unfinished archive is undone. Uploaded chunks go too.
        if let Some((remote, s)) = remote_copy {
            crate::dest::delete_archive(remote, &s.remote_prefix())?;
        }
        if let Some(s) = &settings
            && s.is_remote()
            && s.staging.is_some()
        {
            let _ = fs::remove_dir_all(s.chunk_dir(archive_dir));
        }
        remove_archive(archive_dir, source, None)?;
        summary.archive_removed = true;
        summary.abandoned = true;
        state = "abandoned";
    } else if let Some((remote, s)) = remote_copy {
        let prefix = s.remote_prefix();
        if options.delete_remote {
            crate::dest::delete_archive(remote, &prefix)?;
            if kept_records {
                fs::remove_dir_all(archive_dir)?;
            } else {
                remove_archive(archive_dir, source, None)?;
            }
            summary.archive_removed = true;
            state = "restored";
        } else {
            // The copy at the destination stays, as a backup until deleted.
            // Its records move to stomp's data directory, so it can still be
            // listed, checked and restored from.
            location = remote.location(&prefix);
            summary.copy_kept_at = Some(location.clone());
            if !kept_records {
                remove_archive(
                    archive_dir,
                    source,
                    Some(&super::kept_records_dir(data_dir, &id)),
                )?;
                summary.archive_removed = true;
            }
            state = "restored-copy-kept";
        }
    } else {
        remove_archive(archive_dir, source, None)?;
        summary.archive_removed = true;
        state = "restored";
    }
    super::send_record(
        data_dir,
        &ArchiveRecord {
            id,
            source: source.to_string_lossy().into_owned(),
            grant: settings.as_ref().and_then(|s| s.grant.clone()),
            destination: settings
                .as_ref()
                .map_or_else(|| "local".to_string(), |s| s.destination.clone()),
            location,
            state: state.into(),
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

/// Removes an archive's files after everything in it was restored. With
/// `keep_to`, its records (not its chunks) are copied there first.
fn remove_archive(archive_dir: &Path, source: &Path, keep_to: Option<&Path>) -> Result<()> {
    let (mut journal, mut state) = Journal::open(&archive_dir.join("journal.jsonl"))?;
    journal.append(&mut state, Event::Restored)?;
    drop(journal);
    if let Some(kept) = keep_to {
        keep_records(archive_dir, kept)?;
    }
    fs::remove_dir_all(archive_dir)?;
    let parent = source.join(ARCHIVE_DIR);
    let _ = fs::remove_dir(&parent); // only if no other archive is there
    if !parent.exists() {
        let _ = fs::remove_file(source.join(NOTE_FILE));
    }
    Ok(())
}

/// Copies an archive's records (everything but chunks) to `kept`, leaving
/// out a manifest bigger than [`super::KEEP_MANIFEST_LIMIT`].
fn keep_records(archive_dir: &Path, kept: &Path) -> Result<()> {
    crate::paths::create_private_dir(kept)?;
    for name in [
        "settings.json",
        "journal.jsonl",
        manifest::CHECKSUMS,
        manifest::README,
        manifest::MANIFEST,
    ] {
        let from = archive_dir.join(name);
        let Ok(md) = fs::metadata(&from) else {
            continue;
        };
        if name == manifest::MANIFEST && md.len() > super::KEEP_MANIFEST_LIMIT {
            continue;
        }
        fs::copy(&from, kept.join(name))
            .with_context(|| format!("cannot keep {} in {}", name, kept.display()))?;
    }
    Ok(())
}
