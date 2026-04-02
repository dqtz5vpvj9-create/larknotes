use crate::{AuthStatus, DocMeta, LarkNotesError};

#[async_trait::async_trait]
pub trait DocProvider: Send + Sync {
    async fn auth_status(&self) -> Result<AuthStatus, LarkNotesError>;
    async fn search_docs(&self, query: &str) -> Result<Vec<DocMeta>, LarkNotesError>;
    async fn create_doc(&self, title: &str, markdown: &str) -> Result<DocMeta, LarkNotesError>;
    async fn fetch_doc(&self, doc_id: &str) -> Result<String, LarkNotesError>;
    async fn update_doc(&self, doc_id: &str, markdown: &str) -> Result<(), LarkNotesError>;
}
