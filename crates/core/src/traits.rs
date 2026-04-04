use crate::{AuthStatus, DocMeta, LarkNotesError, ReadOutput, WriteMeta};

// ─── File-system–style document provider ─────────────────────

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
}

// ─── Auth introspection (separate from FS operations) ────────

#[async_trait::async_trait]
pub trait ProviderAuth: Send + Sync {
    async fn auth_status(&self) -> Result<AuthStatus, LarkNotesError>;
}
