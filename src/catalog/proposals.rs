//! Archive proposals made by policies (§16).
//!
//! Open proposals are recomputed from each new evaluation: new ones are added,
//! ones that no longer match are removed, and the rest keep their ID and when
//! they were first proposed. Applied and dismissed proposals stay as history
//! and aren't proposed again.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use super::{Catalog, to_db};
use crate::policy::Candidate;
use crate::scan::{path_bytes, path_from_bytes};

#[derive(Debug, Clone, Serialize)]
pub struct Proposal {
    pub id: i64,
    pub path: PathBuf,
    pub policy: String,
    /// `propose` or `apply`.
    pub action: String,
    pub grant: Option<String>,
    pub destination: String,
    pub bytes: u64,
    pub files: u64,
    pub kept_bytes: u64,
    pub newest: Option<i64>,
    pub reason: String,
    /// Estimated compressed size as a fraction of `bytes`.
    pub ratio: Option<f64>,
    pub scan_id: Option<i64>,
    pub first_seen: i64,
    pub updated_at: i64,
    /// `open`, `applied` or `dismissed`.
    pub state: String,
    pub task_id: Option<String>,
    pub archive_id: Option<String>,
    pub applied_at: Option<i64>,
}

/// What an evaluation changed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ProposalSync {
    pub new: usize,
    pub removed: usize,
    pub open: usize,
}

const COLUMNS: &str = "id, path, policy, action, grant_id, destination, bytes, files, kept_bytes, \
     newest, reason, ratio, scan_id, first_seen, updated_at, state, task_id, archive_id, applied_at";

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Proposal> {
    let n = |i: usize| -> rusqlite::Result<u64> { Ok(r.get::<_, i64>(i)?.max(0) as u64) };
    Ok(Proposal {
        id: r.get(0)?,
        path: path_from_bytes(r.get(1)?),
        policy: r.get(2)?,
        action: r.get(3)?,
        grant: r.get(4)?,
        destination: r.get(5)?,
        bytes: n(6)?,
        files: n(7)?,
        kept_bytes: n(8)?,
        newest: r.get(9)?,
        reason: r.get(10)?,
        ratio: r.get(11)?,
        scan_id: r.get(12)?,
        first_seen: r.get(13)?,
        updated_at: r.get(14)?,
        state: r.get(15)?,
        task_id: r.get(16)?,
        archive_id: r.get(17)?,
        applied_at: r.get(18)?,
    })
}

/// A ratio estimated for a size this far off is estimated again.
const RATIO_STALE_FRACTION: f64 = 0.25;

impl Catalog {
    /// Replaces the open proposals with the result of an evaluation.
    pub fn sync_proposals(
        &mut self,
        candidates: &[Candidate],
        scan_id: i64,
        now: i64,
    ) -> Result<ProposalSync> {
        let tx = self.conn.transaction()?;
        let mut existing: HashMap<Vec<u8>, (i64, String, Option<i64>)> = HashMap::new();
        {
            let mut stmt = tx.prepare("SELECT path, id, state, ratio_bytes FROM proposals")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, Vec<u8>>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
            })?;
            for row in rows {
                let (path, value) = row?;
                existing.insert(path, value);
            }
        }
        let mut sync = ProposalSync::default();
        let mut seen: HashSet<Vec<u8>> = HashSet::new();
        for c in candidates {
            let path = path_bytes(&c.path);
            seen.insert(path.clone());
            match existing.get(&path) {
                Some((id, state, ratio_bytes)) if state == "open" => {
                    let stale = ratio_bytes.is_some_and(|b| {
                        let b = b.max(1) as f64;
                        (c.bytes as f64 - b).abs() / b > RATIO_STALE_FRACTION
                    });
                    tx.execute(
                        "UPDATE proposals SET policy = ?2, action = ?3, grant_id = ?4, destination = ?5,
                             bytes = ?6, files = ?7, kept_bytes = ?8, newest = ?9, reason = ?10,
                             scan_id = ?11, updated_at = ?12,
                             ratio = CASE WHEN ?13 THEN NULL ELSE ratio END,
                             ratio_bytes = CASE WHEN ?13 THEN NULL ELSE ratio_bytes END
                         WHERE id = ?1",
                        params![
                            id,
                            c.policy,
                            c.action.as_str(),
                            c.grant,
                            c.destination,
                            to_db(c.bytes),
                            to_db(c.files),
                            to_db(c.kept_bytes),
                            c.newest,
                            c.reason,
                            scan_id,
                            now,
                            stale
                        ],
                    )?;
                }
                // Applied and dismissed proposals aren't proposed again.
                Some(_) => {}
                None => {
                    tx.execute(
                        "INSERT INTO proposals (path, policy, action, grant_id, destination, bytes, files,
                             kept_bytes, newest, reason, scan_id, first_seen, updated_at, state)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12, 'open')",
                        params![
                            path,
                            c.policy,
                            c.action.as_str(),
                            c.grant,
                            c.destination,
                            to_db(c.bytes),
                            to_db(c.files),
                            to_db(c.kept_bytes),
                            c.newest,
                            c.reason,
                            scan_id,
                            now
                        ],
                    )?;
                    sync.new += 1;
                }
            }
        }
        for (path, (id, state, _)) in &existing {
            if state == "open" && !seen.contains(path) {
                tx.execute("DELETE FROM proposals WHERE id = ?1", [id])?;
                sync.removed += 1;
            }
        }
        sync.open = tx.query_row(
            "SELECT count(*) FROM proposals WHERE state = 'open'",
            [],
            |r| r.get::<_, i64>(0),
        )? as usize;
        tx.commit()?;
        Ok(sync)
    }

    /// Open proposals, largest first; with `all`, applied and dismissed ones too.
    pub fn proposals(&self, all: bool) -> Result<Vec<Proposal>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM proposals {} ORDER BY state != 'open', bytes DESC, id",
            if all { "" } else { "WHERE state = 'open'" }
        ))?;
        let rows = stmt.query_map([], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn proposal(&self, id: i64) -> Result<Option<Proposal>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM proposals WHERE id = ?1"),
                [id],
                from_row,
            )
            .optional()?)
    }

    pub fn open_proposal_count(&self) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM proposals WHERE state = 'open'",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }

    /// Records an estimated compression ratio for a proposal of `bytes`.
    pub fn set_proposal_ratio(&self, id: i64, ratio: f64, bytes: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE proposals SET ratio = ?2, ratio_bytes = ?3 WHERE id = ?1",
            params![id, ratio, to_db(bytes)],
        )?;
        Ok(())
    }

    /// Records that a proposal is being archived, as `archive_id`, by `task_id` if submitted.
    pub fn mark_proposal_applied(
        &self,
        id: i64,
        task_id: Option<&str>,
        archive_id: &str,
        now: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE proposals SET state = 'applied', task_id = ?2, archive_id = ?3, applied_at = ?4,
                 updated_at = ?4
             WHERE id = ?1",
            params![id, task_id, archive_id, now],
        )?;
        Ok(())
    }

    /// Dismisses open proposals so they aren't proposed again. Returns how many changed.
    pub fn dismiss_proposals(&self, ids: &[i64], now: i64) -> Result<usize> {
        let mut changed = 0;
        for id in ids {
            changed += self.conn.execute(
                "UPDATE proposals SET state = 'dismissed', updated_at = ?2
                 WHERE id = ?1 AND state = 'open'",
                params![id, now],
            )?;
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PolicyAction;

    fn candidate(path: &str, bytes: u64) -> Candidate {
        Candidate {
            path: PathBuf::from(path),
            policy: "p".into(),
            action: PolicyAction::Propose,
            grant: Some("G".into()),
            destination: "local".into(),
            bytes,
            files: 10,
            kept_bytes: 0,
            newest: Some(1),
            reason: "r".into(),
        }
    }

    #[test]
    fn sync_keeps_ids_and_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = Catalog::open(dir.path()).unwrap();
        let s = c
            .sync_proposals(&[candidate("/h/a", 100), candidate("/h/b", 200)], 1, 10)
            .unwrap();
        assert_eq!(
            s,
            ProposalSync {
                new: 2,
                removed: 0,
                open: 2
            }
        );
        let ids: HashMap<PathBuf, i64> = c
            .proposals(false)
            .unwrap()
            .into_iter()
            .map(|p| (p.path, p.id))
            .collect();
        let a = ids[&PathBuf::from("/h/a")];
        let b = ids[&PathBuf::from("/h/b")];
        c.set_proposal_ratio(a, 0.4, 100).unwrap();
        assert_eq!(c.dismiss_proposals(&[b], 11).unwrap(), 1);

        // a grew a lot, b is dismissed, c is new.
        let s = c
            .sync_proposals(
                &[
                    candidate("/h/a", 1000),
                    candidate("/h/b", 200),
                    candidate("/h/c", 1),
                ],
                2,
                20,
            )
            .unwrap();
        assert_eq!(
            s,
            ProposalSync {
                new: 1,
                removed: 0,
                open: 2
            }
        );
        let pa = c.proposal(a).unwrap().unwrap();
        assert_eq!((pa.bytes, pa.first_seen, pa.ratio), (1000, 10, None));
        assert_eq!(c.proposal(b).unwrap().unwrap().state, "dismissed");

        // c no longer matches and goes; a is applied and stays.
        c.mark_proposal_applied(a, Some("archive-1"), "G-1", 30)
            .unwrap();
        let s = c.sync_proposals(&[], 3, 40).unwrap();
        assert_eq!(
            s,
            ProposalSync {
                new: 0,
                removed: 1,
                open: 0
            }
        );
        let all = c.proposals(true).unwrap();
        assert_eq!(all.len(), 2);
        assert!(
            all.iter()
                .any(|p| p.state == "applied" && p.task_id.as_deref() == Some("archive-1"))
        );
    }
}
