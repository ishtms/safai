//! wire-format types for the deletion engine. serde renames
//! keep this in sync with src/lib/cleaner.ts. change a field here =
//! update TS + the wire_format_is_stable_camel_case test.

use serde::{Deserialize, Serialize};

/// Missing = always protected (nothing to delete). Symlink = allowed
/// but counted as 1 item / 0 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ItemKind {
    File,
    Directory,
    Symlink,
    Missing,
}

/// one candidate. protected = won't move to graveyard (safety policy or
/// stat failure). UI greys it out with the reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingDelete {
    pub path: String,
    pub bytes: u64,
    pub file_count: u64,
    pub kind: ItemKind,
    pub protected: bool,
    pub protected_reason: Option<String>,
}

/// response from preview_delete. items deduped + shadow-free (parent +
/// descendant = only parent survives, no double-count).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletePlan {
    pub token: String,
    pub created_at: u64,
    pub items: Vec<PendingDelete>,
    /// sum of bytes across non-protected items
    pub total_bytes: u64,
    /// non-protected count, what actually moves
    pub total_count: u64,
    /// protected count, surfaces "X items skipped" in the modal
    pub protected_count: u64,
}

/// one failure during commit or restore. path is caller's original,
/// error is human-readable for inline display.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteFailure {
    pub path: String,
    pub error: String,
}

/// response from commit_delete. partial success still returns Ok, UI
/// checks failed to show warnings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteResult {
    pub token: String,
    pub batch_id: String,
    pub committed_at: u64,
    pub committed: Vec<String>,
    pub failed: Vec<DeleteFailure>,
    /// bytes actually moved, excludes failures
    pub bytes_trashed: u64,
}

/// same partial-success shape as DeleteResult
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreResult {
    pub batch_id: String,
    pub restored_at: u64,
    pub restored: Vec<String>,
    pub failed: Vec<DeleteFailure>,
    pub bytes_restored: u64,
}

/// response from purge_graveyard (user-invoked Empty) and the startup
/// ttl sweep. purged holds removed batch ids.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PurgeResult {
    pub purged: Vec<String>,
    pub failed: Vec<DeleteFailure>,
    pub bytes_freed: u64,
    pub purged_at: u64,
}

/// what's in the graveyard, for UI readout + Empty button tooltip
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GraveyardStats {
    pub batch_count: u64,
    pub total_bytes: u64,
    /// unix secs of oldest batch, None if empty
    pub oldest_at: Option<u64>,
    /// unix secs of newest batch, None if empty
    pub newest_at: Option<u64>,
}

/// errors bubbled through the cleaner. stringified at the tauri
/// boundary since the frontend only cares about the message.
#[derive(Debug)]
pub enum CleanerError {
    /// plan token unknown or expired. usually a racing second commit or
    /// the process restarted between preview and commit.
    UnknownToken,
    /// restore_last called but nothing has been committed yet
    NothingToRestore,
    /// fs error during graveyard prep or move
    Io(String),
    /// audit log exists but didn't parse
    Audit(String),
}

impl std::fmt::Display for CleanerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownToken => write!(f, "unknown or expired plan token"),
            Self::NothingToRestore => write!(f, "no batch available to restore"),
            Self::Io(m) => write!(f, "io: {m}"),
            Self::Audit(m) => write!(f, "audit log: {m}"),
        }
    }
}

impl std::error::Error for CleanerError {}

/// lets tauri commands do ? via map_err(Into::into)
impl From<CleanerError> for String {
    fn from(e: CleanerError) -> Self {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format_is_stable_camel_case() {
        let plan = DeletePlan {
            token: "t".into(),
            created_at: 1,
            items: vec![PendingDelete {
                path: "/a".into(),
                bytes: 2,
                file_count: 3,
                kind: ItemKind::File,
                protected: false,
                protected_reason: None,
            }],
            total_bytes: 2,
            total_count: 1,
            protected_count: 0,
        };
        let v = serde_json::to_value(&plan).unwrap();
        assert!(v.get("createdAt").is_some());
        assert!(v.get("totalBytes").is_some());
        assert!(v.get("totalCount").is_some());
        assert!(v.get("protectedCount").is_some());
        let item = &v["items"][0];
        assert!(item.get("fileCount").is_some());
        assert!(item.get("protectedReason").is_some());
        assert_eq!(item.get("kind").unwrap(), "file");
    }

    #[test]
    fn item_kind_uses_kebab_case() {
        for (k, s) in [
            (ItemKind::File, "file"),
            (ItemKind::Directory, "directory"),
            (ItemKind::Symlink, "symlink"),
            (ItemKind::Missing, "missing"),
        ] {
            let v = serde_json::to_value(k).unwrap();
            assert_eq!(v.as_str().unwrap(), s);
        }
    }

    #[test]
    fn delete_result_shape_is_stable() {
        let r = DeleteResult {
            token: "t".into(),
            batch_id: "b".into(),
            committed_at: 1,
            committed: vec!["/a".into()],
            failed: vec![DeleteFailure {
                path: "/b".into(),
                error: "x".into(),
            }],
            bytes_trashed: 42,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["batchId"], "b");
        assert_eq!(v["committedAt"], 1);
        assert_eq!(v["bytesTrashed"], 42);
        assert_eq!(v["failed"][0]["path"], "/b");
    }

    #[test]
    fn error_into_string_round_trip() {
        let s: String = CleanerError::UnknownToken.into();
        assert!(s.contains("unknown"));
        let s: String = CleanerError::NothingToRestore.into();
        assert!(s.contains("restore"));
    }
}
