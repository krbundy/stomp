//! `stomp archives move` (§12.1): moving a finished archive's chunks between
//! the archived directory and remote destinations, or between destinations.
//!
//! Each chunk is copied and verified at its new home (journaled) before any
//! copy is removed, and the old destination is cleared only after everything,
//! metadata included, has arrived. Interrupted moves resume.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};

use super::journal::{Event, Journal};
use super::manifest;
use super::{ArchiveRecord, LOCAL_DESTINATION, Settings, load_settings, update_settings};
use crate::dest::Destination;

pub enum RelocateOutcome {
    /// Every chunk is at the new destination; holds where that is.
    Complete(String),
    Incomplete {
        moved: usize,
        total: usize,
    },
}

/// Moves the archive in `archive_dir` from `from` (its current destination,
/// `None` when local) to `to` (`None` to bring it back into the directory).
pub fn relocate(
    archive_dir: &Path,
    from: Option<&dyn Destination>,
    to: Option<&dyn Destination>,
    data_dir: &Path,
    stop: &AtomicBool,
) -> Result<RelocateOutcome> {
    let settings = load_settings(archive_dir)?;
    let target_name = to.map_or(LOCAL_DESTINATION, |d| d.name()).to_string();
    if settings.destination == target_name {
        bail!("archive {} is already at {target_name}", settings.id);
    }
    if settings.is_remote() != from.is_some() {
        bail!(
            "archive {} is stored at {:?}, but that destination wasn't given",
            settings.id,
            settings.destination
        );
    }
    if !archive_dir.join(manifest::MANIFEST).exists() {
        bail!(
            "archive {} isn't finished; finish it with `stomp archive --resume` first",
            settings.id
        );
    }
    let (mut journal, mut state) = Journal::open(&archive_dir.join("journal.jsonl"))?;
    let sums = manifest::read_checksums(&archive_dir.join(manifest::CHECKSUMS))?;
    let chunks: Vec<(u32, String, String)> = sums
        .iter()
        .filter_map(|(name, sum)| {
            let k = name
                .strip_prefix("chunk-")?
                .strip_suffix(".tar.zst")?
                .parse()
                .ok()?;
            Some((k, name.clone(), sum.clone()))
        })
        .collect();
    let prefix = settings.remote_prefix();
    let staging = Settings {
        destination: target_name.clone(),
        ..settings.clone()
    }
    .chunk_dir(archive_dir);
    crate::paths::create_private_dir(&staging)?;

    let mut moved = chunks
        .iter()
        .filter(|(k, _, _)| state.is_moved(*k, &target_name))
        .count();
    for (k, name, sha) in &chunks {
        if state.is_moved(*k, &target_name) {
            continue;
        }
        if stop.load(Ordering::Relaxed) {
            return Ok(RelocateOutcome::Incomplete {
                moved,
                total: chunks.len(),
            });
        }
        let rel = format!("{prefix}/{name}");
        // A verified copy on disk: the local chunk, or a download of the remote one.
        let local = archive_dir.join(name);
        let path = if local.exists() {
            local.clone()
        } else {
            let tmp = staging.join(format!("{name}.download"));
            if !tmp.exists() || manifest::sha256_file(&tmp)? != *sha {
                from.context("chunk is missing locally")?.get(&rel, &tmp)?;
            }
            tmp
        };
        if manifest::sha256_file(&path)? != *sha {
            let _ = fs::remove_file(&path);
            bail!("{name} has the wrong checksum; nothing was moved");
        }
        match to {
            Some(dest) => {
                dest.put(&path, &rel, sha, &staging)?;
                remove(&path)?; // the new copy is verified; this one is no longer needed
            }
            None if path != local => fs::rename(&path, &local)?,
            None => {}
        }
        journal.append(
            &mut state,
            Event::Moved {
                chunk: *k,
                to: target_name.clone(),
            },
        )?;
        moved += 1;
    }

    if !state.moves_finished.contains(&target_name) {
        if let Some(dest) = to {
            let mut header = manifest::read_header(&archive_dir.join(manifest::MANIFEST))?;
            header.location = Some(dest.location(&prefix));
            let readme = manifest::readme_text(&header, header.location.as_deref());
            manifest::write_atomic(&archive_dir.join(manifest::README), |out| {
                out.write_all(readme.as_bytes())?;
                Ok(())
            })?;
            for name in [manifest::README, manifest::CHECKSUMS, manifest::MANIFEST] {
                let path = archive_dir.join(name);
                dest.put(
                    &path,
                    &format!("{prefix}/{name}"),
                    &manifest::sha256_file(&path)?,
                    &staging,
                )?;
            }
        }
        journal.append(
            &mut state,
            Event::MoveFinished {
                to: target_name.clone(),
            },
        )?;
    }
    // Only now, with everything safe at the new place, clear the old one.
    if let Some(old) = from {
        for (_, name, _) in &chunks {
            old.delete(&format!("{prefix}/{name}"))?;
        }
        for name in [manifest::README, manifest::CHECKSUMS, manifest::MANIFEST] {
            old.delete(&format!("{prefix}/{name}"))?;
        }
        old.delete_dir(&prefix)?;
    }
    let updated = Settings {
        destination: target_name.clone(),
        ..settings
    };
    update_settings(archive_dir, &updated)?;

    let location = to.map_or_else(
        || archive_dir.to_string_lossy().into_owned(),
        |d| d.location(&prefix),
    );
    let header = manifest::read_header(&archive_dir.join(manifest::MANIFEST))?;
    let note = manifest::note_text(
        &updated.id,
        &updated.source,
        &header.created_at,
        header.user.as_deref(),
        Some(&header),
        to.map(|d| d.location(&prefix)).as_deref(),
    );
    manifest::write_atomic(&updated.source.join(super::NOTE_FILE), |out| {
        out.write_all(note.as_bytes())?;
        Ok(())
    })?;
    super::send_record(
        data_dir,
        &ArchiveRecord {
            id: updated.id.clone(),
            source: updated.source.to_string_lossy().into_owned(),
            grant: updated.grant.clone(),
            destination: target_name,
            location: location.clone(),
            state: "complete".into(),
            created_at: Some(header.created_at.clone()),
            finished_at: Some(header.finished_at.clone()),
            files: header.totals.files,
            bytes: header.totals.bytes,
            compressed: header.totals.compressed_bytes,
            chunks: header.totals.chunks,
            kept: header.totals.kept,
            skipped: header.totals.skipped,
        },
    )?;
    Ok(RelocateOutcome::Complete(location))
}

fn remove(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}
