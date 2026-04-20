use crate::{AuthStatus, DocMeta, LarkNotesError, ReadOutput, WriteMeta};

// ─── File-system–style document provider ─────────────────────

/// Lightweight remote metadata used by sync to detect changes without
/// fetching content. `modify_time` is Lark's `latest_modify_time` (Unix
/// seconds); `modify_user` is the editor's `open_id`.
#[derive(Debug, Clone)]
pub struct RemoteMeta {
    pub remote_id: String,
    pub modify_time: i64,
    pub modify_user: String,
}

/// Result of a batch metas query. `gone` lists tokens for which Lark
/// reports the doc is no longer accessible — either deleted (970005) or
/// permission revoked (970003). Callers should treat these as "remote
/// removed" rather than transient failures so the local note can be
/// flagged for the user to act on.
#[derive(Debug, Clone, Default)]
pub struct BatchMetas {
    pub found: Vec<RemoteMeta>,
    pub gone: Vec<String>,
}

#[async_trait::async_trait]
pub trait DocProvider: Send + Sync {
    /// Create a new document. Returns full metadata including the new ID.
    async fn create(&self, name: &str, content: &str) -> Result<DocMeta, LarkNotesError>;

    /// Read document content + metadata in one round trip.
    async fn read(&self, id: &str) -> Result<ReadOutput, LarkNotesError>;

    /// Overwrite document content. Returns post-write metadata (hash, timestamp).
    async fn write(&self, id: &str, content: &str) -> Result<WriteMeta, LarkNotesError>;

    /// Delete a document.
    async fn delete(&self, id: &str) -> Result<(), LarkNotesError>;

    /// Rename a document (title only, no content change).
    async fn rename(&self, id: &str, new_name: &str) -> Result<(), LarkNotesError>;

    /// List all documents, optionally scoped to a folder.
    async fn list(&self, folder: Option<&str>) -> Result<Vec<DocMeta>, LarkNotesError>;

    /// Full-text search.
    async fn search(&self, query: &str) -> Result<Vec<DocMeta>, LarkNotesError>;

    /// Batch-fetch lightweight metadata for the given remote IDs. Sync uses
    /// this in the poll loop to skip docs whose `modify_time` hasn't moved
    /// AND to detect docs that were deleted/unshared remotely (returned in
    /// `BatchMetas::gone`). Implementations should batch (Lark caps at 200
    /// per call) and surface per-doc errors rather than aborting the batch.
    async fn query_metas(&self, remote_ids: &[String])
        -> Result<BatchMetas, LarkNotesError>;
}

// ─── Auth introspection (separate from FS operations) ────────

#[async_trait::async_trait]
pub trait ProviderAuth: Send + Sync {
    async fn auth_status(&self) -> Result<AuthStatus, LarkNotesError>;
}
