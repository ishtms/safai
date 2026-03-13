//! append-only jsonl audit log at `<data_dir>/audit.log`.
//!
//! one line per commit or restore. uses O_APPEND so a crashing process
//! can't corrupt earlier lines. unparseable lines are skipped on read so
//! someone poking the file with vim can't brick restore_last.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::types::{CleanerError, DeleteResult, RestoreResult};

/// one record in the audit log. forwards-compatible: unknown `op` values
/// are tolerated on read so new ops can be added without migrating.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditRecord {
    pub op: String, // commit | restore | purge
    pub batch_id: String,
    pub timestamp: u64,
    pub items: u64,
    pub failed: u64,
    pub bytes: u64,
}

pub fn append_commit(data_dir: &Path, result: &DeleteResult) -> Result<(), CleanerError> {
    append(
        data_dir,
        &AuditRecord {
            op: "commit".into(),
            batch_id: result.batch_id.clone(),
            timestamp: result.committed_at,
            items: result.committed.len() as u64,
            failed: result.failed.len() as u64,
            bytes: result.bytes_trashed,
        },
    )
}

pub fn append_restore(data_dir: &Path, result: &RestoreResult) -> Result<(), CleanerError> {
    append(
        data_dir,
        &AuditRecord {
            op: "restore".into(),
            batch_id: result.batch_id.clone(),
            timestamp: result.restored_at,
            items: result.restored.len() as u64,
            failed: result.failed.len() as u64,
            bytes: result.bytes_restored,
        },
    )
}

/// purged batches aren't restorable. record keeps latest_commit from
/// returning a batch whose graveyard dir is gone.
pub fn append_purge(
    data_dir: &Path,
    batch_id: &str,
    timestamp: u64,
    bytes: u64,
) -> Result<(), CleanerError> {
    append(
        data_dir,
        &AuditRecord {
            op: "purge".into(),
            batch_id: batch_id.into(),
            timestamp,
            items: 0,
            failed: 0,
            bytes,
        },
    )
}

fn append(data_dir: &Path, record: &AuditRecord) -> Result<(), CleanerError> {
    fs::create_dir_all(data_dir)
        .map_err(|e| CleanerError::Io(format!("create data dir: {e}")))?;
    let path = data_dir.join("audit.log");
    let line = serde_json::to_string(record)
        .map_err(|e| CleanerError::Audit(format!("encode: {e}")))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| CleanerError::Io(format!("open audit log: {e}")))?;
    writeln!(file, "{line}").map_err(|e| CleanerError::Io(format!("write: {e}")))?;
    Ok(())
}

/// most recent commit that's still restorable. skips batches we've
/// already restored or purged.
pub fn latest_commit(data_dir: &Path) -> Result<Option<AuditRecord>, CleanerError> {
    let all = read_all(data_dir)?;
    // restored or purged = not in graveyard anymore, skip
    let mut consumed: std::collections::HashSet<String> = std::collections::HashSet::new();
    for r in all.iter().rev() {
        if r.op == "restore" || r.op == "purge" {
            consumed.insert(r.batch_id.clone());
            continue;
        }
        if r.op == "commit" && !consumed.contains(&r.batch_id) {
            return Ok(Some(r.clone()));
        }
    }
    Ok(None)
}

/// parseable records in append order. bad lines silently skipped.
pub fn read_all(data_dir: &Path) -> Result<Vec<AuditRecord>, CleanerError> {
    let path = data_dir.join("audit.log");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let s = fs::read_to_string(&path)
        .map_err(|e| CleanerError::Io(format!("read audit log: {e}")))?;
    let mut out = Vec::new();
    for line in s.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(rec) = serde_json::from_str::<AuditRecord>(trimmed) {
            out.push(rec);
        }
        // skip bad lines, don't let hand-edits break undo
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cleaner::types::{DeleteFailure, DeleteResult, RestoreResult};

    fn delete_result(batch_id: &str, bytes: u64) -> DeleteResult {
        DeleteResult {
            token: "t".into(),
            batch_id: batch_id.into(),
            committed_at: 111,
            committed: vec!["/a".into(), "/b".into()],
            failed: Vec::new(),
            bytes_trashed: bytes,
        }
    }

    fn restore_result(batch_id: &str, bytes: u64) -> RestoreResult {
        RestoreResult {
            batch_id: batch_id.into(),
            restored_at: 222,
            restored: vec!["/a".into()],
            failed: Vec::new(),
            bytes_restored: bytes,
        }
    }

    #[test]
    fn append_commit_writes_a_jsonl_line() {
        let tmp = tempfile::tempdir().unwrap();
        append_commit(tmp.path(), &delete_result("b1", 100)).unwrap();
        let s = fs::read_to_string(tmp.path().join("audit.log")).unwrap();
        assert_eq!(s.lines().count(), 1);
        let rec: AuditRecord = serde_json::from_str(s.lines().next().unwrap()).unwrap();
        assert_eq!(rec.op, "commit");
        assert_eq!(rec.batch_id, "b1");
        assert_eq!(rec.bytes, 100);
    }

    #[test]
    fn multiple_appends_produce_multiple_lines() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..3 {
            append_commit(tmp.path(), &delete_result(&format!("b{i}"), i as u64 * 100)).unwrap();
        }
        let all = read_all(tmp.path()).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[2].batch_id, "b2");
    }

    #[test]
    fn latest_commit_returns_most_recent_commit_when_no_restores() {
        let tmp = tempfile::tempdir().unwrap();
        append_commit(tmp.path(), &delete_result("b1", 1)).unwrap();
        append_commit(tmp.path(), &delete_result("b2", 2)).unwrap();
        append_commit(tmp.path(), &delete_result("b3", 3)).unwrap();
        let latest = latest_commit(tmp.path()).unwrap().unwrap();
        assert_eq!(latest.batch_id, "b3");
    }

    #[test]
    fn latest_commit_skips_batches_already_restored() {
        let tmp = tempfile::tempdir().unwrap();
        append_commit(tmp.path(), &delete_result("b1", 1)).unwrap();
        append_commit(tmp.path(), &delete_result("b2", 2)).unwrap();
        // restored b2, latest should now be b1
        append_restore(tmp.path(), &restore_result("b2", 2)).unwrap();
        let latest = latest_commit(tmp.path()).unwrap().unwrap();
        assert_eq!(latest.batch_id, "b1");
    }

    #[test]
    fn latest_commit_is_none_after_all_batches_restored() {
        let tmp = tempfile::tempdir().unwrap();
        append_commit(tmp.path(), &delete_result("b1", 1)).unwrap();
        append_restore(tmp.path(), &restore_result("b1", 1)).unwrap();
        assert!(latest_commit(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn latest_commit_is_none_when_log_missing() {
        let tmp = tempfile::tempdir().unwrap();
        // no log yet
        assert!(latest_commit(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn read_all_skips_unparseable_lines() {
        let tmp = tempfile::tempdir().unwrap();
        append_commit(tmp.path(), &delete_result("b1", 1)).unwrap();
        // jam in a corrupt line
        let path = tmp.path().join("audit.log");
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{{ this is not json").unwrap();
        append_commit(tmp.path(), &delete_result("b2", 2)).unwrap();

        let all = read_all(tmp.path()).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].batch_id, "b1");
        assert_eq!(all[1].batch_id, "b2");
    }

    #[test]
    fn data_dir_is_created_if_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("nested/notyet");
        append_commit(&data, &delete_result("b1", 1)).unwrap();
        assert!(data.join("audit.log").exists());
    }

    #[test]
    fn latest_commit_skips_purged_batches() {
        let tmp = tempfile::tempdir().unwrap();
        append_commit(tmp.path(), &delete_result("b1", 1)).unwrap();
        append_commit(tmp.path(), &delete_result("b2", 2)).unwrap();
        append_purge(tmp.path(), "b2", 99, 2).unwrap();
        // b2 purged, fall back to b1
        let latest = latest_commit(tmp.path()).unwrap().unwrap();
        assert_eq!(latest.batch_id, "b1");
    }

    #[test]
    fn latest_commit_none_when_all_batches_purged_or_restored() {
        let tmp = tempfile::tempdir().unwrap();
        append_commit(tmp.path(), &delete_result("b1", 1)).unwrap();
        append_commit(tmp.path(), &delete_result("b2", 2)).unwrap();
        append_restore(tmp.path(), &restore_result("b1", 1)).unwrap();
        append_purge(tmp.path(), "b2", 99, 2).unwrap();
        assert!(latest_commit(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn record_fields_round_trip_through_jsonl() {
        let tmp = tempfile::tempdir().unwrap();
        append_commit(
            tmp.path(),
            &DeleteResult {
                token: "t".into(),
                batch_id: "b1".into(),
                committed_at: 42,
                committed: vec!["/x".into()],
                failed: vec![DeleteFailure {
                    path: "/y".into(),
                    error: "nope".into(),
                }],
                bytes_trashed: 1024,
            },
        )
        .unwrap();
        let r = latest_commit(tmp.path()).unwrap().unwrap();
        assert_eq!(r.timestamp, 42);
        assert_eq!(r.items, 1);
        assert_eq!(r.failed, 1);
        assert_eq!(r.bytes, 1024);
    }
}
