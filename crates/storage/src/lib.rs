use larknotes_core::*;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

pub struct Storage {
    conn: Connection,
}

impl Storage {
    pub fn new(db_path: &Path) -> Result<Self, LarkNotesError> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| LarkNotesError::Storage(format!("创建数据库目录失败: {e}")))?;
        }
        let conn = Connection::open(db_path)
            .map_err(|e| LarkNotesError::Storage(format!("打开数据库失败: {e}")))?;
        let storage = Self { conn };
        storage.migrate()?;
        Ok(storage)
    }

    pub fn new_in_memory() -> Result<Self, LarkNotesError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| LarkNotesError::Storage(format!("创建内存数据库失败: {e}")))?;
        let storage = Self { conn };
        storage.migrate()?;
        Ok(storage)
    }

    fn migrate(&self) -> Result<(), LarkNotesError> {
        self.conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS documents (
                doc_id TEXT PRIMARY KEY,
                title TEXT NOT NULL DEFAULT 'Untitled',
                doc_type TEXT NOT NULL DEFAULT 'DOCX',
                url TEXT NOT NULL DEFAULT '',
                owner_name TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT '',
                local_path TEXT,
                content_hash TEXT,
                sync_status TEXT NOT NULL DEFAULT 'New',
                last_synced_at TEXT
            );

            CREATE TABLE IF NOT EXISTS app_config (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sync_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                doc_id TEXT NOT NULL,
                action TEXT NOT NULL,
                content_hash TEXT,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS version_snapshots (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                doc_id TEXT NOT NULL,
                content TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            ",
            )
            .map_err(|e| LarkNotesError::Storage(format!("数据库迁移失败: {e}")))?;
        Ok(())
    }

    pub fn upsert_doc(&self, meta: &DocMeta) -> Result<(), LarkNotesError> {
        let sync_status_str = serde_json::to_string(&meta.sync_status).unwrap_or_default();
        self.conn
            .execute(
                "INSERT OR REPLACE INTO documents
                 (doc_id, title, doc_type, url, owner_name, created_at, updated_at,
                  local_path, content_hash, sync_status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    meta.doc_id,
                    meta.title,
                    meta.doc_type,
                    meta.url,
                    meta.owner_name,
                    meta.created_at,
                    meta.updated_at,
                    meta.local_path,
                    meta.content_hash,
                    sync_status_str,
                ],
            )
            .map_err(|e| LarkNotesError::Storage(format!("写入文档失败: {e}")))?;
        Ok(())
    }

    pub fn get_doc(&self, doc_id: &str) -> Result<Option<DocMeta>, LarkNotesError> {
        self.conn
            .query_row(
                "SELECT doc_id, title, doc_type, url, owner_name, created_at, updated_at,
                        local_path, content_hash, sync_status
                 FROM documents WHERE doc_id = ?1",
                params![doc_id],
                |row| Ok(row_to_doc_meta(row)),
            )
            .optional()
            .map_err(|e| LarkNotesError::Storage(format!("查询文档失败: {e}")))
    }

    pub fn list_docs(&self) -> Result<Vec<DocMeta>, LarkNotesError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT doc_id, title, doc_type, url, owner_name, created_at, updated_at,
                        local_path, content_hash, sync_status
                 FROM documents ORDER BY updated_at DESC",
            )
            .map_err(|e| LarkNotesError::Storage(format!("查询失败: {e}")))?;

        let docs = stmt
            .query_map([], |row| Ok(row_to_doc_meta(row)))
            .map_err(|e| LarkNotesError::Storage(format!("查询失败: {e}")))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(docs)
    }

    pub fn update_sync_status(
        &self,
        doc_id: &str,
        status: &SyncStatus,
    ) -> Result<(), LarkNotesError> {
        let status_str = serde_json::to_string(status).unwrap_or_default();
        let now = chrono::Local::now().to_rfc3339();
        self.conn
            .execute(
                "UPDATE documents SET sync_status = ?1, last_synced_at = ?2 WHERE doc_id = ?3",
                params![status_str, now, doc_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("更新状态失败: {e}")))?;
        Ok(())
    }

    pub fn update_content_hash(&self, doc_id: &str, hash: &str) -> Result<(), LarkNotesError> {
        self.conn
            .execute(
                "UPDATE documents SET content_hash = ?1 WHERE doc_id = ?2",
                params![hash, doc_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("更新哈希失败: {e}")))?;
        Ok(())
    }

    pub fn update_title(&self, doc_id: &str, title: &str) -> Result<(), LarkNotesError> {
        self.conn
            .execute(
                "UPDATE documents SET title = ?1 WHERE doc_id = ?2",
                params![title, doc_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("更新标题失败: {e}")))?;
        Ok(())
    }

    pub fn delete_doc(&self, doc_id: &str) -> Result<(), LarkNotesError> {
        self.conn
            .execute("DELETE FROM documents WHERE doc_id = ?1", params![doc_id])
            .map_err(|e| LarkNotesError::Storage(format!("删除文档失败: {e}")))?;
        Ok(())
    }

    pub fn get_config(&self, key: &str) -> Result<Option<String>, LarkNotesError> {
        self.conn
            .query_row(
                "SELECT value FROM app_config WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| LarkNotesError::Storage(format!("查询配置失败: {e}")))
    }

    /// Reset any docs stuck in "Syncing" state (from a crash) back to "LocalModified".
    pub fn reset_stale_syncing(&self) -> Result<usize, LarkNotesError> {
        let syncing_str = serde_json::to_string(&SyncStatus::Syncing).unwrap_or_default();
        let modified_str = serde_json::to_string(&SyncStatus::LocalModified).unwrap_or_default();
        let count = self.conn
            .execute(
                "UPDATE documents SET sync_status = ?1 WHERE sync_status = ?2",
                params![modified_str, syncing_str],
            )
            .map_err(|e| LarkNotesError::Storage(format!("重置同步状态失败: {e}")))?;
        Ok(count)
    }

    // ─── Sync History ─────────────────────────────────────

    pub fn add_sync_history(
        &self,
        doc_id: &str,
        action: &str,
        content_hash: Option<&str>,
    ) -> Result<(), LarkNotesError> {
        let now = chrono::Local::now().to_rfc3339();
        self.conn
            .execute(
                "INSERT INTO sync_history (doc_id, action, content_hash, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![doc_id, action, content_hash, now],
            )
            .map_err(|e| LarkNotesError::Storage(format!("写入同步历史失败: {e}")))?;
        Ok(())
    }

    pub fn get_sync_history(
        &self,
        doc_id: &str,
        limit: usize,
    ) -> Result<Vec<SyncHistoryEntry>, LarkNotesError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, doc_id, action, content_hash, created_at
                 FROM sync_history WHERE doc_id = ?1
                 ORDER BY created_at DESC LIMIT ?2",
            )
            .map_err(|e| LarkNotesError::Storage(format!("查询同步历史失败: {e}")))?;

        let entries = stmt
            .query_map(params![doc_id, limit as i64], |row| {
                Ok(SyncHistoryEntry {
                    id: row.get(0)?,
                    doc_id: row.get(1)?,
                    action: row.get(2)?,
                    content_hash: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })
            .map_err(|e| LarkNotesError::Storage(format!("查询同步历史失败: {e}")))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(entries)
    }

    // ─── Version Snapshots ──────────────────────────────

    pub fn save_snapshot(
        &self,
        doc_id: &str,
        content: &str,
        content_hash: &str,
    ) -> Result<(), LarkNotesError> {
        let now = chrono::Local::now().to_rfc3339();
        self.conn
            .execute(
                "INSERT INTO version_snapshots (doc_id, content, content_hash, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![doc_id, content, content_hash, now],
            )
            .map_err(|e| LarkNotesError::Storage(format!("保存快照失败: {e}")))?;

        // Keep only the latest 20 snapshots per doc
        self.conn
            .execute(
                "DELETE FROM version_snapshots WHERE doc_id = ?1 AND id NOT IN (
                    SELECT id FROM version_snapshots WHERE doc_id = ?1
                    ORDER BY created_at DESC LIMIT 20
                )",
                params![doc_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("清理旧快照失败: {e}")))?;

        Ok(())
    }

    pub fn get_snapshots(
        &self,
        doc_id: &str,
    ) -> Result<Vec<VersionSnapshot>, LarkNotesError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, doc_id, content, content_hash, created_at
                 FROM version_snapshots WHERE doc_id = ?1
                 ORDER BY created_at DESC",
            )
            .map_err(|e| LarkNotesError::Storage(format!("查询快照失败: {e}")))?;

        let snapshots = stmt
            .query_map(params![doc_id], |row| {
                Ok(VersionSnapshot {
                    id: row.get(0)?,
                    doc_id: row.get(1)?,
                    content: row.get(2)?,
                    content_hash: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })
            .map_err(|e| LarkNotesError::Storage(format!("查询快照失败: {e}")))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(snapshots)
    }

    pub fn get_snapshot_by_id(
        &self,
        snapshot_id: i64,
    ) -> Result<Option<VersionSnapshot>, LarkNotesError> {
        self.conn
            .query_row(
                "SELECT id, doc_id, content, content_hash, created_at
                 FROM version_snapshots WHERE id = ?1",
                params![snapshot_id],
                |row| {
                    Ok(VersionSnapshot {
                        id: row.get(0)?,
                        doc_id: row.get(1)?,
                        content: row.get(2)?,
                        content_hash: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(|e| LarkNotesError::Storage(format!("查询快照失败: {e}")))
    }

    // ─── Config ─────────────────────────────────────────

    pub fn set_config(&self, key: &str, value: &str) -> Result<(), LarkNotesError> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO app_config (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .map_err(|e| LarkNotesError::Storage(format!("写入配置失败: {e}")))?;
        Ok(())
    }
}

fn row_to_doc_meta(row: &rusqlite::Row) -> DocMeta {
    let sync_status_str: String = row.get(9).unwrap_or_default();
    let sync_status =
        serde_json::from_str(&sync_status_str).unwrap_or(SyncStatus::New);

    DocMeta {
        doc_id: row.get(0).unwrap_or_default(),
        title: row.get(1).unwrap_or_default(),
        doc_type: row.get(2).unwrap_or_default(),
        url: row.get(3).unwrap_or_default(),
        owner_name: row.get(4).unwrap_or_default(),
        created_at: row.get(5).unwrap_or_default(),
        updated_at: row.get(6).unwrap_or_default(),
        local_path: row.get(7).unwrap_or_default(),
        content_hash: row.get(8).unwrap_or_default(),
        sync_status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_storage() -> Storage {
        Storage::new_in_memory().unwrap()
    }

    fn sample_doc(id: &str) -> DocMeta {
        DocMeta {
            doc_id: id.to_string(),
            title: "Test Doc".to_string(),
            doc_type: "DOCX".to_string(),
            url: format!("https://feishu.cn/docx/{id}"),
            owner_name: "tester".to_string(),
            created_at: "2026-01-01T00:00:00+08:00".to_string(),
            updated_at: "2026-01-01T00:00:00+08:00".to_string(),
            local_path: None,
            content_hash: None,
            sync_status: SyncStatus::Synced,
        }
    }

    #[test]
    fn test_crud() {
        let s = test_storage();
        let doc = sample_doc("doc1");
        s.upsert_doc(&doc).unwrap();

        let fetched = s.get_doc("doc1").unwrap().unwrap();
        assert_eq!(fetched.title, "Test Doc");

        let list = s.list_docs().unwrap();
        assert_eq!(list.len(), 1);

        s.delete_doc("doc1").unwrap();
        assert!(s.get_doc("doc1").unwrap().is_none());
    }

    #[test]
    fn test_upsert_update() {
        let s = test_storage();
        let mut doc = sample_doc("doc1");
        s.upsert_doc(&doc).unwrap();

        doc.title = "Updated".to_string();
        s.upsert_doc(&doc).unwrap();

        let fetched = s.get_doc("doc1").unwrap().unwrap();
        assert_eq!(fetched.title, "Updated");
        assert_eq!(s.list_docs().unwrap().len(), 1);
    }

    #[test]
    fn test_sync_status() {
        let s = test_storage();
        s.upsert_doc(&sample_doc("doc1")).unwrap();
        s.update_sync_status("doc1", &SyncStatus::Syncing).unwrap();

        let doc = s.get_doc("doc1").unwrap().unwrap();
        assert_eq!(doc.sync_status, SyncStatus::Syncing);
    }

    #[test]
    fn test_config() {
        let s = test_storage();
        assert!(s.get_config("editor").unwrap().is_none());
        s.set_config("editor", "typora").unwrap();
        assert_eq!(s.get_config("editor").unwrap().unwrap(), "typora");
    }
}
