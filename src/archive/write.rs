//! Writing an archive (§12.4). For each chunk, in order:
//!
//! ```text
//! WRITE → VERIFY → DELETE_SOURCES
//! ```
//!
//! Every step is journaled before the next begins. Originals are removed only
//! after their chunk has been read back and every file's hash checked (I1),
//! and only if they haven't changed since they were read (I2). Directories are
//! removed only when empty (I3), and a resumed run never repeats a step (I4).

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;

use super::journal::{Event, Journal, JournalState};
use super::manifest::{
    self, EntryRecord, HashWriter, Header, Kind, PathField, Record, SkipReason, SkipRecord,
    TargetField, Totals,
};
use super::plan::{self, PlanEntry};
use super::{ArchiveRecord, NOTE_FILE, Settings, archive_path};
use crate::dest::Destination;
use crate::fs::Backend;
use crate::scan::rules::Rules;

#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub bytes: AtomicU64,
    pub chunks: AtomicU64,
    pub removed: AtomicU64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub id: String,
    pub archive_dir: PathBuf,
    pub files: u64,
    pub bytes: u64,
    pub compressed: u64,
    pub chunks: u32,
    pub chunks_done: u32,
    pub removed: u64,
    pub kept: u64,
    pub skipped: u64,
    pub seconds: u64,
}

pub enum Outcome {
    Complete(Summary),
    /// Stopped early; `stomp archive --resume` continues.
    Incomplete(Summary),
}

/// Called after each journaled step; returning true stops the run there.
/// Tests use it to simulate a crash after every step.
pub type Hook<'a> = &'a (dyn Fn(&Event) -> bool + Sync);

#[derive(Debug)]
struct Interrupted;

impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("interrupted")
    }
}

impl std::error::Error for Interrupted {}

pub fn mtime_ns(md: &fs::Metadata) -> i64 {
    md.mtime() * 1_000_000_000 + md.mtime_nsec()
}

pub fn ctime_ns(md: &fs::Metadata) -> i64 {
    md.ctime() * 1_000_000_000 + md.ctime_nsec()
}

/// Archives `settings.source`, or continues an archive started earlier.
/// `dest` is the destination named in the settings, for remote archives.
#[allow(clippy::too_many_arguments)]
pub fn archive(
    settings: &Settings,
    rules: &Rules,
    backend: &dyn Backend,
    dest: Option<&dyn Destination>,
    data_dir: &Path,
    stop: &AtomicBool,
    progress: &Progress,
    hook: Hook,
) -> Result<Outcome> {
    if settings.is_remote() != dest.is_some() {
        bail!(
            "archive {} is stored at {:?}, but no matching destination was given",
            settings.id,
            settings.destination
        );
    }
    let started = Instant::now();
    let dir = archive_path(&settings.source, &settings.id);
    crate::paths::create_private_dir(&dir)?;
    let lock_path = dir.join(".lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)?;
    if lock.try_lock().is_err() {
        bail!(
            "another stomp process is working on archive {}",
            settings.id
        );
    }
    super::save_settings(&dir, settings)?;
    let (journal, state) = Journal::open(&dir.join("journal.jsonl"))?;
    let mut run = Run {
        settings,
        dir,
        journal,
        state,
        stop,
        progress,
        hook,
        data_dir,
        dest,
    };
    let result = run.go(rules, backend);
    let summary = run.summary(started)?;
    match result {
        Ok(()) => {
            drop(lock);
            let _ = fs::remove_file(lock_path);
            Ok(Outcome::Complete(summary))
        }
        Err(e) if e.is::<Interrupted>() => Ok(Outcome::Incomplete(summary)),
        Err(e) => Err(e),
    }
}

struct Run<'a> {
    settings: &'a Settings,
    dir: PathBuf,
    journal: Journal,
    state: JournalState,
    stop: &'a AtomicBool,
    progress: &'a Progress,
    hook: Hook<'a>,
    data_dir: &'a Path,
    dest: Option<&'a dyn Destination>,
}

enum Added {
    Entry(EntryRecord),
    Skip(SkipRecord),
}

impl Run<'_> {
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Journals a step, then stops if asked to.
    fn step(&mut self, event: Event) -> Result<()> {
        self.journal.append(&mut self.state, event.clone())?;
        if (self.hook)(&event) || self.stopping() {
            return Err(Interrupted.into());
        }
        Ok(())
    }

    fn manifest_dir(&self) -> PathBuf {
        self.dir.join("manifest.d")
    }

    fn records_path(&self, k: u32) -> PathBuf {
        self.manifest_dir().join(format!("chunk-{k:04}.jsonl"))
    }

    fn kept_path(&self, k: u32) -> PathBuf {
        self.manifest_dir().join(format!("chunk-{k:04}.kept.jsonl"))
    }

    fn chunk_dir(&self) -> PathBuf {
        self.settings.chunk_dir(&self.dir)
    }

    fn chunk_path(&self, k: u32) -> PathBuf {
        self.chunk_dir().join(manifest::chunk_name(k))
    }

    /// Where a file of this archive goes at its destination.
    fn remote_rel(&self, name: &str) -> String {
        format!("{}/{name}", self.settings.remote_prefix())
    }

    fn remote_location(&self) -> Option<String> {
        self.dest
            .map(|d| d.location(&self.settings.remote_prefix()))
    }

    fn go(&mut self, rules: &Rules, backend: &dyn Backend) -> Result<()> {
        if self.state.planned.is_none() {
            let s = self.settings;
            let plan = plan::make_plan(&s.source, rules, backend, s.only_owned, s.chunk_size)?;
            if plan.entries.is_empty() {
                bail!("there's nothing to archive in {}", s.source.display());
            }
            plan::save(&self.dir, &plan)?;
            self.write_note(None)?;
            self.send_record("in-progress", None)?;
            let summary = &plan.summary;
            self.step(Event::Planned {
                chunks: summary.chunks,
                files: summary.files,
                bytes: summary.bytes,
            })?;
        }
        let (chunks, _, _) = self.state.planned.expect("planned");
        for k in 1..=chunks {
            self.chunk(k)?;
        }
        if self.state.dirs_removed.is_none() {
            let (removed, kept) = self.remove_dirs(chunks)?;
            self.step(Event::DirsRemoved { removed, kept })?;
        }
        if self.state.finished.is_none() {
            let header = self.finalize(chunks)?;
            // The manifest goes last: a remote archive without one is unfinished (§12.4).
            if let Some(dest) = self.dest
                && !self.state.metadata_uploaded
            {
                for name in [manifest::README, manifest::CHECKSUMS, manifest::MANIFEST] {
                    let path = self.dir.join(name);
                    dest.put(
                        &path,
                        &self.remote_rel(name),
                        &manifest::sha256_file(&path)?,
                        &self.dir,
                    )?;
                }
                self.step(Event::MetadataUploaded)?;
            }
            self.journal.append(&mut self.state, Event::Finished)?;
            self.send_record("complete", Some(&header))?;
        }
        Ok(())
    }

    fn chunk(&mut self, k: u32) -> Result<()> {
        if self.state.chunk(k).deleted.is_some() {
            self.progress.chunks.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        if self.stopping() {
            return Err(Interrupted.into());
        }
        if self.state.chunk(k).written.is_none() {
            let (sha256, compressed, files, bytes) = self.write_chunk(k)?;
            self.step(Event::Written {
                chunk: k,
                sha256,
                compressed,
                files,
                bytes,
            })?;
        }
        if !self.state.chunk(k).verified {
            let sha = self.state.chunk(k).written.expect("written").0;
            let records = entry_records(&self.records_path(k))?;
            let chunk_path = self.chunk_path(k);
            if let Err(e) = verify_chunk(&chunk_path, &sha, &records, true) {
                // Never delete anything on the strength of a bad chunk: discard it and stop.
                let _ = fs::remove_file(&chunk_path);
                self.journal.append(
                    &mut self.state,
                    Event::Discarded {
                        chunk: k,
                        reason: format!("{e:#}"),
                    },
                )?;
                return Err(e.context(format!(
                    "chunk {k} failed verification and was discarded; nothing from it was deleted, and it will be written again next time"
                )));
            }
            self.step(Event::Verified { chunk: k })?;
        }
        if let Some(dest) = self.dest {
            // I1: originals go only once the chunk is verified where it will stay.
            if !self.state.chunk(k).uploaded {
                let sha = self.state.chunk(k).written.expect("written").0;
                dest.put(
                    &self.chunk_path(k),
                    &self.remote_rel(&manifest::chunk_name(k)),
                    &sha,
                    &self.chunk_dir(),
                )?;
                self.step(Event::Uploaded { chunk: k })?;
            }
            match fs::remove_file(self.chunk_path(k)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        self.step(Event::Deleting { chunk: k })?;
        let (removed, kept, missing) = self.delete_chunk(k)?;
        self.step(Event::Deleted {
            chunk: k,
            removed,
            kept,
            missing,
        })?;
        self.progress.chunks.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Writes chunk `k` and returns (sha256, compressed bytes, files, bytes).
    fn write_chunk(&mut self, k: u32) -> Result<(String, u64, u64, u64)> {
        let entries = plan::load_chunk(&self.dir, k)?;
        fs::create_dir_all(self.manifest_dir())?;
        crate::paths::create_private_dir(&self.chunk_dir())?;
        let final_path = self.chunk_path(k);
        let partial = self
            .chunk_dir()
            .join(format!("{}.partial", manifest::chunk_name(k)));
        let file = File::create(&partial)?;
        let mut encoder = zstd::Encoder::new(
            HashWriter::new(BufWriter::with_capacity(1 << 20, file)),
            self.settings.level,
        )?;
        encoder.include_checksum(true)?;
        if self.settings.threads > 1 {
            encoder.multithread(self.settings.threads)?;
        }
        let mut tar = tar::Builder::new(encoder);
        let mut lines: Vec<serde_json::Value> = Vec::with_capacity(entries.len());
        let (mut files, mut bytes) = (0, 0);
        for entry in &entries {
            if self.stopping() {
                drop(tar);
                let _ = fs::remove_file(&partial);
                return Err(Interrupted.into());
            }
            match self.add(&mut tar, entry, k)? {
                Added::Entry(record) => {
                    if record.kind == Kind::File {
                        files += 1;
                        bytes += record.size;
                    }
                    lines.push(serde_json::to_value(record)?);
                }
                Added::Skip(skip) => lines.push(serde_json::to_value(skip)?),
            }
        }
        let encoder = tar.into_inner()?;
        let (buffered, sha256, compressed) = encoder.finish()?.finish();
        buffered
            .into_inner()
            .map_err(|e| e.into_error())?
            .sync_all()?;
        manifest::write_jsonl(&self.records_path(k), lines)?;
        fs::rename(&partial, &final_path)?;
        manifest::sync_dir(&self.chunk_dir())?;
        Ok((sha256, compressed, files, bytes))
    }

    fn add<W: io::Write>(&self, tar: &mut tar::Builder<W>, e: &PlanEntry, k: u32) -> Result<Added> {
        let abs = self.settings.source.join(&e.rel);
        let skip = |reason: SkipReason, bytes: u64| {
            Ok(Added::Skip(SkipRecord::new(&e.rel, reason, bytes)))
        };
        let lstat = || match fs::symlink_metadata(&abs) {
            Ok(md) => Ok(md),
            Err(err) => Err(classify(&err)),
        };
        match e.kind {
            Kind::Dir => {
                let md = match lstat() {
                    Ok(md) if md.is_dir() => md,
                    Ok(_) => return skip(SkipReason::ChangedType, 0),
                    Err(reason) => return skip(reason, 0),
                };
                let mut h = header(&md, tar::EntryType::Directory, 0);
                tar.append_data(&mut h, &e.rel, io::empty())?;
                Ok(Added::Entry(record(e, k, &md, None, None)))
            }
            Kind::File => {
                let nofollow = rustix::fs::OFlags::NOFOLLOW.bits() as i32;
                let file = match OpenOptions::new()
                    .read(true)
                    .custom_flags(nofollow)
                    .open(&abs)
                {
                    Ok(f) => f,
                    Err(err) => return skip(classify(&err), e.size),
                };
                let md = file.metadata()?;
                if !md.is_file() {
                    return skip(SkipReason::ChangedType, 0);
                }
                let size = md.len();
                let mut h = header(&md, tar::EntryType::Regular, size);
                let mut reader = ExactReader::new(BufReader::with_capacity(1 << 20, &file), size);
                tar.append_data(&mut h, &e.rel, &mut reader)?;
                let longer = reader.has_more()?;
                let after = file.metadata()?;
                let changed = reader.short
                    || longer
                    || after.len() != size
                    || mtime_ns(&after) != mtime_ns(&md)
                    || ctime_ns(&after) != ctime_ns(&md);
                self.progress.files.fetch_add(1, Ordering::Relaxed);
                self.progress.bytes.fetch_add(size, Ordering::Relaxed);
                let mut rec = record(e, k, &md, Some(reader.hash()), None);
                rec.changed = changed;
                Ok(Added::Entry(rec))
            }
            Kind::Symlink => {
                let md = match lstat() {
                    Ok(md) if md.file_type().is_symlink() => md,
                    Ok(_) => return skip(SkipReason::ChangedType, 0),
                    Err(reason) => return skip(reason, 0),
                };
                let target = fs::read_link(&abs)?;
                let mut h = header(&md, tar::EntryType::Symlink, 0);
                tar.append_link(&mut h, &e.rel, &target)?;
                Ok(Added::Entry(record(e, k, &md, None, Some(&target))))
            }
            Kind::Hardlink => {
                let md = match lstat() {
                    Ok(md) if md.is_file() => md,
                    Ok(_) => return skip(SkipReason::ChangedType, 0),
                    Err(reason) => return skip(reason, 0),
                };
                let target = e.link.as_ref().context("hard link without a target")?;
                let mut h = header(&md, tar::EntryType::Link, 0);
                tar.append_link(&mut h, &e.rel, target)?;
                Ok(Added::Entry(record(e, k, &md, None, Some(target))))
            }
        }
    }

    /// Removes the originals of chunk `k`. Returns (removed, kept, missing).
    fn delete_chunk(&mut self, k: u32) -> Result<(u64, u64, u64)> {
        let records = entry_records(&self.records_path(k))?;
        // Check everything before removing anything: the links of a hard-linked
        // file share an inode, and removing one changes the inode's ctime.
        let mut doomed: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut kept: Vec<PathBuf> = Vec::new();
        let mut missing = 0;
        for r in records.iter().filter(|r| r.kind != Kind::Dir) {
            let rel = r.rel()?;
            if r.changed {
                kept.push(rel);
                continue;
            }
            let abs = self.settings.source.join(&rel);
            match fs::symlink_metadata(&abs) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => missing += 1,
                Err(_) => kept.push(rel),
                Ok(md) if unchanged(&md, r) => doomed.push((abs, rel)),
                Ok(_) => kept.push(rel),
            }
        }
        let mut removed = 0;
        for (abs, rel) in doomed {
            match fs::remove_file(&abs) {
                Ok(()) => {
                    removed += 1;
                    self.progress.removed.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => missing += 1,
                Err(_) => kept.push(rel),
            }
        }
        manifest::write_jsonl(&self.kept_path(k), kept.iter().map(|p| PathField::new(p)))?;
        Ok((removed, kept.len() as u64, missing))
    }

    /// Removes directories that are now empty, deepest first. Returns (removed, kept).
    fn remove_dirs(&mut self, chunks: u32) -> Result<(u64, u64)> {
        let mut dirs = Vec::new();
        for k in 1..=chunks {
            for r in entry_records(&self.records_path(k))? {
                if r.kind == Kind::Dir {
                    dirs.push(r.rel()?);
                }
            }
        }
        let (mut removed, mut kept) = (0, 0);
        for rel in dirs.iter().rev() {
            match fs::remove_dir(self.settings.source.join(rel)) {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(_) => kept += 1,
            }
        }
        Ok((removed, kept))
    }

    /// Writes the manifest, checksums, README and note once (journaled), then
    /// tidies the work files. Safe to repeat after an interruption.
    fn finalize(&mut self, chunks: u32) -> Result<Header> {
        if !self.state.finalized {
            self.write_final_files(chunks)?;
            self.step(Event::Finalized)?;
        }
        if self.manifest_dir().exists() {
            fs::remove_dir_all(self.manifest_dir())?;
        }
        plan::remove(&self.dir)?;
        manifest::read_header(&self.dir.join(manifest::MANIFEST))
    }

    fn write_final_files(&mut self, chunks: u32) -> Result<()> {
        let mut kept: HashSet<PathBuf> = HashSet::new();
        for k in 1..=chunks {
            let path = self.kept_path(k);
            if path.exists() {
                for p in manifest::read_jsonl::<PathField>(&path)? {
                    kept.insert(p.get()?);
                }
            }
        }
        let plan_skips = plan::load_skipped(&self.dir)?;

        // First pass: totals for the header.
        let mut totals = Totals {
            chunks,
            ..Totals::default()
        };
        for k in 1..=chunks {
            totals.compressed_bytes += self.state.chunk(k).written.map_or(0, |w| w.1);
            for record in manifest::read_records(&self.records_path(k))? {
                match record {
                    Record::Entry(r) => {
                        match r.kind {
                            Kind::File => {
                                totals.files += 1;
                                totals.bytes += r.size;
                            }
                            Kind::Dir => totals.dirs += 1,
                            Kind::Symlink => totals.symlinks += 1,
                            Kind::Hardlink => totals.hardlinks += 1,
                        }
                        if r.kind != Kind::Dir && (r.changed || kept.contains(&r.rel()?)) {
                            totals.kept += 1;
                        }
                    }
                    Record::Skipped(s) => {
                        totals.skipped += 1;
                        totals.skipped_bytes += s.bytes;
                    }
                    Record::Header(_) => {}
                }
            }
        }
        for s in &plan_skips {
            totals.skipped += 1;
            totals.skipped_bytes += s.bytes;
        }
        let s = self.settings;
        let header = Header {
            record: "header".into(),
            format: manifest::FORMAT.into(),
            version: manifest::VERSION,
            archive_id: s.id.clone(),
            source: s.source.to_string_lossy().into_owned(),
            grant: s.grant.clone(),
            user: crate::sys::user_name(),
            host: crate::sys::hostname(),
            stomp_version: env!("CARGO_PKG_VERSION").into(),
            created_at: self.state.created_at.clone().unwrap_or_default(),
            finished_at: jiff::Timestamp::now().to_string(),
            zstd_level: s.level,
            chunk_size: s.chunk_size,
            totals,
            destination: Some(s.destination.clone()),
            location: self.remote_location(),
        };

        // Second pass: the manifest itself, header first, then entries, then what was left in place.
        let manifest_path = self.dir.join(manifest::MANIFEST);
        manifest::write_atomic(&manifest_path, |out| {
            let mut enc = zstd::Encoder::new(out, 9)?;
            let mut line = |value: &dyn erased::Json| -> Result<()> {
                value.write_line(&mut enc)?;
                Ok(())
            };
            line(&header)?;
            let mut chunk_skips = Vec::new();
            for k in 1..=chunks {
                for record in manifest::read_records(&self.records_path(k))? {
                    match record {
                        Record::Entry(mut r) => {
                            r.kept = r.kind != Kind::Dir && (r.changed || kept.contains(&r.rel()?));
                            line(&r)?;
                        }
                        Record::Skipped(s) => chunk_skips.push(s),
                        Record::Header(_) => {}
                    }
                }
            }
            for s in plan_skips.iter().chain(&chunk_skips) {
                line(s)?;
            }
            enc.finish()?;
            Ok(())
        })?;

        let mut sums: Vec<(String, String)> = (1..=chunks)
            .map(|k| {
                let sha = self.state.chunk(k).written.map(|w| w.0).unwrap_or_default();
                (manifest::chunk_name(k), sha)
            })
            .collect();
        sums.push((
            manifest::MANIFEST.into(),
            manifest::sha256_file(&manifest_path)?,
        ));
        let checksums = manifest::checksums_text(&sums);
        manifest::write_atomic(&self.dir.join(manifest::CHECKSUMS), |out| {
            out.write_all(checksums.as_bytes())?;
            Ok(())
        })?;
        let readme = manifest::readme_text(&header, header.location.as_deref());
        manifest::write_atomic(&self.dir.join(manifest::README), |out| {
            out.write_all(readme.as_bytes())?;
            Ok(())
        })?;
        self.write_note(Some(&header))
    }

    fn write_note(&self, finished: Option<&Header>) -> Result<()> {
        let created = self
            .state
            .created_at
            .clone()
            .unwrap_or_else(|| jiff::Timestamp::now().to_string());
        let text = manifest::note_text(
            &self.settings.id,
            &self.settings.source,
            &created,
            crate::sys::user_name().as_deref(),
            finished,
            self.remote_location().as_deref(),
        );
        manifest::write_atomic(&self.settings.source.join(NOTE_FILE), |out| {
            out.write_all(text.as_bytes())?;
            Ok(())
        })
    }

    fn send_record(&self, state: &str, header: Option<&Header>) -> Result<()> {
        let s = self.settings;
        let (chunks, files, bytes) = self.state.planned.unwrap_or_default();
        let record = ArchiveRecord {
            id: s.id.clone(),
            source: s.source.to_string_lossy().into_owned(),
            grant: s.grant.clone(),
            destination: s.destination.clone(),
            location: self
                .remote_location()
                .unwrap_or_else(|| self.dir.to_string_lossy().into_owned()),
            state: state.into(),
            created_at: self.state.created_at.clone(),
            finished_at: header.map(|h| h.finished_at.clone()),
            files: header.map_or(files, |h| h.totals.files),
            bytes: header.map_or(bytes, |h| h.totals.bytes),
            compressed: header.map_or(0, |h| h.totals.compressed_bytes),
            chunks: header.map_or(chunks, |h| h.totals.chunks),
            kept: header.map_or(0, |h| h.totals.kept),
            skipped: header.map_or(0, |h| h.totals.skipped),
        };
        super::send_record(self.data_dir, &record)
    }

    fn summary(&self, started: Instant) -> Result<Summary> {
        let (chunks, files, bytes) = self.state.planned.unwrap_or_default();
        let mut s = Summary {
            id: self.settings.id.clone(),
            archive_dir: self.dir.clone(),
            files,
            bytes,
            compressed: 0,
            chunks,
            chunks_done: 0,
            removed: 0,
            kept: 0,
            skipped: 0,
            seconds: started.elapsed().as_secs(),
        };
        for c in self.state.chunks.values() {
            s.compressed += c.written.as_ref().map_or(0, |w| w.1);
            if let Some((removed, kept, _)) = c.deleted {
                s.chunks_done += 1;
                s.removed += removed;
                s.kept += kept;
            }
        }
        if let Ok(summary) = plan::load_summary(&self.dir) {
            s.skipped = summary.skipped.values().map(|(n, _)| n).sum();
        }
        Ok(s)
    }
}

/// Writing heterogeneous records as JSON lines.
mod erased {
    use std::io::Write;

    pub trait Json {
        fn write_line(&self, out: &mut dyn Write) -> anyhow::Result<()>;
    }

    impl<T: serde::Serialize> Json for T {
        fn write_line(&self, out: &mut dyn Write) -> anyhow::Result<()> {
            serde_json::to_writer(&mut *out, self)?;
            out.write_all(b"\n")?;
            Ok(())
        }
    }
}

fn classify(err: &io::Error) -> SkipReason {
    if err.kind() == io::ErrorKind::NotFound {
        SkipReason::Vanished
    } else if err.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) {
        // O_NOFOLLOW refused: the file was replaced by a symlink.
        SkipReason::ChangedType
    } else {
        SkipReason::Unreadable
    }
}

fn header(md: &fs::Metadata, kind: tar::EntryType, size: u64) -> tar::Header {
    let mut h = tar::Header::new_gnu();
    h.set_metadata_in_mode(md, tar::HeaderMode::Complete);
    h.set_entry_type(kind);
    h.set_size(size);
    h.set_mtime(md.mtime().max(0) as u64);
    h
}

fn record(
    e: &PlanEntry,
    k: u32,
    md: &fs::Metadata,
    blake3: Option<String>,
    target: Option<&Path>,
) -> EntryRecord {
    EntryRecord {
        record: "entry".into(),
        path: PathField::new(&e.rel),
        kind: e.kind,
        chunk: k,
        size: if matches!(e.kind, Kind::File | Kind::Hardlink) {
            md.len()
        } else {
            0
        },
        mode: md.mode() & 0o7777,
        mtime: md.mtime(),
        uid: md.uid(),
        gid: md.gid(),
        blake3,
        target: TargetField::new(target),
        changed: false,
        kept: false,
        ino: md.ino(),
        mtime_ns: mtime_ns(md),
        ctime_ns: ctime_ns(md),
    }
}

/// Whether an original is exactly as it was when archived (§12.4, I2).
fn unchanged(md: &fs::Metadata, r: &EntryRecord) -> bool {
    let same_inode = md.ino() == r.ino;
    match r.kind {
        Kind::File => {
            md.is_file()
                && same_inode
                && md.len() == r.size
                && mtime_ns(md) == r.mtime_ns
                && ctime_ns(md) == r.ctime_ns
        }
        // Removing another link to the same file changes its ctime, so hard
        // links are checked by inode, size and modification time.
        Kind::Hardlink => {
            md.is_file() && same_inode && md.len() == r.size && mtime_ns(md) == r.mtime_ns
        }
        Kind::Symlink => md.file_type().is_symlink() && same_inode && ctime_ns(md) == r.ctime_ns,
        Kind::Dir => false,
    }
}

/// Entry records from a per-chunk manifest file.
pub fn entry_records(path: &Path) -> Result<Vec<EntryRecord>> {
    Ok(manifest::read_records(path)?
        .into_iter()
        .filter_map(|r| match r {
            Record::Entry(e) => Some(e),
            _ => None,
        })
        .collect())
}

/// Checks a chunk's SHA-256 and, with `deep`, decompresses it and checks every
/// file against the manifest's BLAKE3 hashes.
pub fn verify_chunk(path: &Path, sha256: &str, records: &[EntryRecord], deep: bool) -> Result<()> {
    let actual = manifest::sha256_file(path)?;
    if actual != sha256 {
        bail!("{} has the wrong checksum", path.display());
    }
    if !deep {
        return Ok(());
    }
    let mut expected: HashMap<Vec<u8>, &EntryRecord> = HashMap::new();
    for r in records {
        expected.insert(r.rel()?.as_os_str().as_bytes().to_vec(), r);
    }
    let mut archive = tar::Archive::new(zstd::Decoder::new(File::open(path)?)?);
    let mut seen = 0usize;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let mut name = entry.path_bytes().into_owned();
        while name.len() > 1 && name.ends_with(b"/") {
            name.pop();
        }
        let shown = String::from_utf8_lossy(&name).into_owned();
        let record = expected.get(&name).ok_or_else(|| {
            anyhow!(
                "{} contains {shown:?}, which the manifest doesn't list",
                path.display()
            )
        })?;
        let kind = match entry.header().entry_type() {
            tar::EntryType::Regular | tar::EntryType::Continuous => Kind::File,
            tar::EntryType::Directory => Kind::Dir,
            tar::EntryType::Symlink => Kind::Symlink,
            tar::EntryType::Link => Kind::Hardlink,
            other => bail!("{shown:?} has an unexpected tar entry type {other:?}"),
        };
        if kind != record.kind {
            bail!(
                "{shown:?} is a {kind:?} in the chunk but a {:?} in the manifest",
                record.kind
            );
        }
        if kind == Kind::File && !record.changed {
            let mut hasher = blake3::Hasher::new();
            let copied = io::copy(&mut entry, &mut hasher)?;
            if copied != record.size
                || Some(hasher.finalize().to_hex().to_string()) != record.blake3
            {
                bail!("{shown:?} in {} doesn't match its checksum", path.display());
            }
        }
        seen += 1;
    }
    if seen != expected.len() {
        bail!(
            "{} has {seen} entries but the manifest lists {}",
            path.display(),
            expected.len()
        );
    }
    Ok(())
}

/// Reads a file's first `size` bytes for a tar entry, hashing them. If the
/// file shrank while being read, the rest is zero-filled so the entry keeps
/// its declared size, and `short` is set.
struct ExactReader<R: Read> {
    inner: R,
    remaining: u64,
    hasher: blake3::Hasher,
    short: bool,
}

impl<R: Read> ExactReader<R> {
    fn new(inner: R, size: u64) -> Self {
        ExactReader {
            inner,
            remaining: size,
            hasher: blake3::Hasher::new(),
            short: false,
        }
    }

    fn hash(&self) -> String {
        self.hasher.finalize().to_hex().to_string()
    }

    /// Whether the file has grown beyond the size that was archived.
    fn has_more(&mut self) -> io::Result<bool> {
        if self.short {
            return Ok(false);
        }
        let mut byte = [0u8; 1];
        loop {
            match self.inner.read(&mut byte) {
                Ok(n) => return Ok(n > 0),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

impl<R: Read> Read for ExactReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 || buf.is_empty() {
            return Ok(0);
        }
        let want = buf
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let n = if self.short {
            0
        } else {
            self.inner.read(&mut buf[..want])?
        };
        if n == 0 {
            self.short = true;
            buf[..want].fill(0);
            self.remaining -= want as u64;
            return Ok(want);
        }
        self.hasher.update(&buf[..n]);
        self.remaining -= n as u64;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_reader_pads_short_files() {
        let mut r = ExactReader::new(&b"abc"[..], 5);
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"abc\0\0");
        assert!(r.short);

        let mut r = ExactReader::new(&b"abcdef"[..], 4);
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"abcd");
        assert!(!r.short);
        assert!(r.has_more().unwrap());
        assert_eq!(r.hash(), blake3::hash(b"abcd").to_hex().to_string());
    }
}
