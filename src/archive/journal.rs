//! The archive journal (§12.4, I4): every step is appended and synced before
//! the next begins, so a killed run resumes exactly where it stopped and never
//! repeats a destructive step.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Event {
    Planned {
        chunks: u32,
        files: u64,
        bytes: u64,
    },
    Written {
        chunk: u32,
        sha256: String,
        compressed: u64,
        files: u64,
        bytes: u64,
    },
    Verified {
        chunk: u32,
    },
    /// A chunk failed verification and was removed; it will be written again.
    Discarded {
        chunk: u32,
        reason: String,
    },
    /// Recorded before removing originals, so an interrupted deletion is redone.
    Deleting {
        chunk: u32,
    },
    Deleted {
        chunk: u32,
        removed: u64,
        kept: u64,
        missing: u64,
    },
    DirsRemoved {
        removed: u64,
        kept: u64,
    },
    /// A chunk is stored and verified at the archive's destination.
    Uploaded {
        chunk: u32,
    },
    /// The manifest, checksums, README and note are written.
    Finalized,
    /// The manifest, checksums and README are stored at the destination.
    MetadataUploaded,
    Finished,
    /// Everything was restored and the archive's files removed.
    Restored,
    /// While moving to `to` (a destination or `local`): this chunk is there and verified.
    Moved {
        chunk: u32,
        to: String,
    },
    /// Every chunk and the metadata are at `to`.
    MoveFinished {
        to: String,
    },
}

#[derive(Serialize, Deserialize)]
struct Line {
    at: String,
    #[serde(flatten)]
    event: Event,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChunkState {
    /// (sha256, compressed bytes, files, bytes)
    pub written: Option<(String, u64, u64, u64)>,
    pub verified: bool,
    pub uploaded: bool,
    pub deleting: bool,
    pub deleted: Option<(u64, u64, u64)>,
}

/// What the journal says has happened.
#[derive(Debug, Clone, Default)]
pub struct JournalState {
    pub created_at: Option<String>,
    /// (chunks, files, bytes)
    pub planned: Option<(u32, u64, u64)>,
    pub chunks: BTreeMap<u32, ChunkState>,
    pub dirs_removed: Option<(u64, u64)>,
    pub finalized: bool,
    pub metadata_uploaded: bool,
    pub finished: Option<String>,
    pub restored: bool,
    /// Chunks already moved, by target.
    pub moved: BTreeMap<String, BTreeSet<u32>>,
    pub moves_finished: BTreeSet<String>,
}

impl JournalState {
    pub fn apply(&mut self, at: &str, event: &Event) {
        if self.created_at.is_none() {
            self.created_at = Some(at.to_string());
        }
        match event {
            Event::Planned {
                chunks,
                files,
                bytes,
            } => self.planned = Some((*chunks, *files, *bytes)),
            Event::Written {
                chunk,
                sha256,
                compressed,
                files,
                bytes,
            } => {
                self.chunks.entry(*chunk).or_default().written =
                    Some((sha256.clone(), *compressed, *files, *bytes));
            }
            Event::Verified { chunk } => self.chunks.entry(*chunk).or_default().verified = true,
            Event::Discarded { chunk, .. } => {
                self.chunks.insert(*chunk, ChunkState::default());
            }
            Event::Deleting { chunk } => self.chunks.entry(*chunk).or_default().deleting = true,
            Event::Deleted {
                chunk,
                removed,
                kept,
                missing,
            } => self.chunks.entry(*chunk).or_default().deleted = Some((*removed, *kept, *missing)),
            Event::DirsRemoved { removed, kept } => self.dirs_removed = Some((*removed, *kept)),
            Event::Uploaded { chunk } => self.chunks.entry(*chunk).or_default().uploaded = true,
            Event::Finalized => self.finalized = true,
            Event::MetadataUploaded => self.metadata_uploaded = true,
            Event::Finished => self.finished = Some(at.to_string()),
            Event::Restored => self.restored = true,
            Event::Moved { chunk, to } => {
                self.moved.entry(to.clone()).or_default().insert(*chunk);
            }
            Event::MoveFinished { to } => {
                self.moves_finished.insert(to.clone());
            }
        }
    }

    /// Whether chunk `k` has already been moved to `to`.
    pub fn is_moved(&self, k: u32, to: &str) -> bool {
        self.moved.get(to).is_some_and(|set| set.contains(&k))
    }

    pub fn chunk(&self, k: u32) -> ChunkState {
        self.chunks.get(&k).cloned().unwrap_or_default()
    }

    /// Chunks whose content is safely written, with their checksums.
    pub fn written_chunks(&self) -> Vec<(u32, String)> {
        self.chunks
            .iter()
            .filter_map(|(k, c)| c.written.as_ref().map(|w| (*k, w.0.clone())))
            .collect()
    }
}

pub struct Journal {
    file: File,
}

impl Journal {
    /// Opens (creating) a journal and replays it. A partly written last line,
    /// from a crash mid-append, is ignored.
    pub fn open(path: &Path) -> Result<(Journal, JournalState)> {
        let mut state = JournalState::default();
        let mut torn = false;
        if path.exists() {
            let text = std::fs::read_to_string(path)?;
            for line in text.lines() {
                if let Ok(parsed) = serde_json::from_str::<Line>(line) {
                    state.apply(&parsed.at, &parsed.event);
                }
            }
            torn = !text.is_empty() && !text.ends_with('\n');
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        if torn {
            // Start the next event on its own line rather than gluing it to the torn one.
            file.write_all(b"\n")?;
            file.sync_data()?;
        }
        Ok((Journal { file }, state))
    }

    /// Reads a journal without opening it for writing.
    pub fn read(path: &Path) -> Result<JournalState> {
        let mut state = JournalState::default();
        let reader = BufReader::new(File::open(path)?);
        for line in reader.lines() {
            if let Ok(parsed) = serde_json::from_str::<Line>(&line?) {
                state.apply(&parsed.at, &parsed.event);
            }
        }
        Ok(state)
    }

    /// Appends an event and waits until it is on disk.
    pub fn append(&mut self, state: &mut JournalState, event: Event) -> Result<()> {
        let at = jiff::Timestamp::now().to_string();
        let mut line = serde_json::to_string(&Line {
            at: at.clone(),
            event: event.clone(),
        })?;
        line.push('\n');
        self.file.write_all(line.as_bytes())?;
        self.file.sync_data()?;
        state.apply(&at, &event);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replays_and_ignores_torn_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("journal.jsonl");
        let (mut j, mut s) = Journal::open(&path).unwrap();
        j.append(
            &mut s,
            Event::Planned {
                chunks: 2,
                files: 3,
                bytes: 4,
            },
        )
        .unwrap();
        j.append(
            &mut s,
            Event::Written {
                chunk: 1,
                sha256: "ab".into(),
                compressed: 9,
                files: 3,
                bytes: 4,
            },
        )
        .unwrap();
        j.append(&mut s, Event::Verified { chunk: 1 }).unwrap();
        j.append(
            &mut s,
            Event::Discarded {
                chunk: 1,
                reason: "x".into(),
            },
        )
        .unwrap();
        drop(j);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"at\":\"x\",\"step\":\"fin")
            .unwrap();

        let (mut j, mut state) = Journal::open(&path).unwrap();
        assert_eq!(state.planned, Some((2, 3, 4)));
        assert_eq!(
            state.chunk(1),
            ChunkState::default(),
            "discarding resets the chunk"
        );
        assert!(state.finished.is_none());
        assert!(state.created_at.is_some());

        // An event appended after the torn line survives the next replay.
        j.append(&mut state, Event::Finished).unwrap();
        drop(j);
        assert!(Journal::read(&path).unwrap().finished.is_some());
    }
}
