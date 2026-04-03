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
        // Create version tracking table
        self.conn
            .execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL)")
            .map_err(|e| LarkNotesError::Storage(format!("创建版本表失败: {e}")))?;

        let current_version: i64 = self.conn
            .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_version", [], |row| row.get(0))
            .unwrap_or(0);

        let migrations: Vec<(i64, &str)> = vec![
            (1, "
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
                    remote_hash TEXT,
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
            "),
            (2, "
                -- Add remote_hash column if upgrading from v0
                ALTER TABLE documents ADD COLUMN remote_hash TEXT;
            "),
        ];

        for (version, sql) in &migrations {
            if *version > current_version {
                // Try to run, ignoring "duplicate column" errors for ALTER TABLE
                let result = self.conn.execute_batch(sql);
                match result {
                    Ok(()) => {}
                    Err(e) => {
                        let msg = e.to_string();
                        // Ignore "duplicate column name" from ALTER TABLE on existing DBs
                        if !msg.contains("duplicate column name") {
                            return Err(LarkNotesError::Storage(format!("迁移v{version}失败: {e}")));
                        }
                    }
                }
                self.conn
                    .execute("INSERT INTO schema_version (version) VALUES (?1)", params![version])
                    .map_err(|e| LarkNotesError::Storage(format!("记录迁移版本失败: {e}")))?;
                tracing::info!("数据库迁移完成: v{version}");
            }
        }

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

    pub fn update_local_path(&self, doc_id: &str, path: &str) -> Result<(), LarkNotesError> {
        self.conn
            .execute(
                "UPDATE documents SET local_path = ?1 WHERE doc_id = ?2",
                params![path, doc_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("更新本地路径失败: {e}")))?;
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

    /// Search documents locally by title (case-insensitive)
    pub fn search_docs_local(&self, query: &str) -> Result<Vec<DocMeta>, LarkNotesError> {
        // Escape LIKE wildcards in user input to prevent unintended matching
        let escaped = query.replace('%', "\\%").replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let mut stmt = self
            .conn
            .prepare(
                "SELECT doc_id, title, doc_type, url, owner_name, created_at, updated_at,
                        local_path, content_hash, sync_status
                 FROM documents WHERE title LIKE ?1 ESCAPE '\\' COLLATE NOCASE
                 ORDER BY updated_at DESC",
            )
            .map_err(|e| LarkNotesError::Storage(format!("查询失败: {e}")))?;

        let docs = stmt
            .query_map(params![pattern], |row| Ok(row_to_doc_meta(row)))
            .map_err(|e| LarkNotesError::Storage(format!("查询失败: {e}")))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(docs)
    }

    /// Update remote content hash for a document
    pub fn update_remote_hash(&self, doc_id: &str, hash: &str) -> Result<(), LarkNotesError> {
        self.conn
            .execute(
                "UPDATE documents SET remote_hash = ?1 WHERE doc_id = ?2",
                params![hash, doc_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("更新远程哈希失败: {e}")))?;
        Ok(())
    }

    /// Get all documents that have local_path set (for pull checking)
    pub fn list_synced_docs(&self) -> Result<Vec<DocMeta>, LarkNotesError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT doc_id, title, doc_type, url, owner_name, created_at, updated_at,
                        local_path, content_hash, sync_status
                 FROM documents WHERE local_path IS NOT NULL
                 ORDER BY updated_at DESC",
            )
            .map_err(|e| LarkNotesError::Storage(format!("查询失败: {e}")))?;

        let docs = stmt
            .query_map([], |row| Ok(row_to_doc_meta(row)))
            .map_err(|e| LarkNotesError::Storage(format!("查询失败: {e}")))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(docs)
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

    // ─── update_content_hash ─────────────────────────────

    #[test]
    fn test_update_content_hash() {
        let s = test_storage();
        s.upsert_doc(&sample_doc("d1")).unwrap();
        assert!(s.get_doc("d1").unwrap().unwrap().content_hash.is_none());

        s.update_content_hash("d1", "abc123").unwrap();
        assert_eq!(s.get_doc("d1").unwrap().unwrap().content_hash.as_deref(), Some("abc123"));
    }

    // ─── update_local_path ────────────────────────────────

    #[test]
    fn test_update_local_path() {
        let s = test_storage();
        let mut doc = sample_doc("d1");
        doc.local_path = Some("/old/path.md".to_string());
        s.upsert_doc(&doc).unwrap();

        s.update_local_path("d1", "/new/path.md").unwrap();
        let fetched = s.get_doc("d1").unwrap().unwrap();
        assert_eq!(fetched.local_path.as_deref(), Some("/new/path.md"));
    }

    #[test]
    fn test_update_local_path_nonexistent_doc() {
        let s = test_storage();
        // Should not error even if doc doesn't exist (0 rows affected)
        s.update_local_path("nonexistent", "/some/path.md").unwrap();
    }

    // ─── update_title ────────────────────────────────────

    #[test]
    fn test_update_title() {
        let s = test_storage();
        s.upsert_doc(&sample_doc("d1")).unwrap();
        s.update_title("d1", "New Title").unwrap();
        assert_eq!(s.get_doc("d1").unwrap().unwrap().title, "New Title");
    }

    #[test]
    fn test_update_title_unicode() {
        let s = test_storage();
        s.upsert_doc(&sample_doc("d1")).unwrap();
        s.update_title("d1", "飞书笔记📝").unwrap();
        assert_eq!(s.get_doc("d1").unwrap().unwrap().title, "飞书笔记📝");
    }

    // ─── update_remote_hash ──────────────────────────────

    #[test]
    fn test_update_remote_hash() {
        let s = test_storage();
        s.upsert_doc(&sample_doc("d1")).unwrap();
        s.update_remote_hash("d1", "remote_xyz").unwrap();
        // remote_hash is not in the standard select (row_to_doc_meta reads 10 cols)
        // but the update should succeed without error
    }

    // ─── reset_stale_syncing ─────────────────────────────

    #[test]
    fn test_reset_stale_syncing() {
        let s = test_storage();
        let mut d1 = sample_doc("d1");
        d1.sync_status = SyncStatus::Syncing;
        s.upsert_doc(&d1).unwrap();

        let mut d2 = sample_doc("d2");
        d2.sync_status = SyncStatus::Syncing;
        s.upsert_doc(&d2).unwrap();

        let mut d3 = sample_doc("d3");
        d3.sync_status = SyncStatus::Synced;
        s.upsert_doc(&d3).unwrap();

        let count = s.reset_stale_syncing().unwrap();
        assert_eq!(count, 2);

        assert_eq!(s.get_doc("d1").unwrap().unwrap().sync_status, SyncStatus::LocalModified);
        assert_eq!(s.get_doc("d2").unwrap().unwrap().sync_status, SyncStatus::LocalModified);
        assert_eq!(s.get_doc("d3").unwrap().unwrap().sync_status, SyncStatus::Synced);
    }

    #[test]
    fn test_reset_stale_syncing_none() {
        let s = test_storage();
        s.upsert_doc(&sample_doc("d1")).unwrap();
        let count = s.reset_stale_syncing().unwrap();
        assert_eq!(count, 0);
    }

    // ─── Sync History ────────────────────────────────────

    #[test]
    fn test_sync_history_crud() {
        let s = test_storage();
        s.add_sync_history("d1", "push", Some("hash1")).unwrap();
        s.add_sync_history("d1", "pull", Some("hash2")).unwrap();
        s.add_sync_history("d1", "conflict", None).unwrap();

        let history = s.get_sync_history("d1", 10).unwrap();
        assert_eq!(history.len(), 3);
        // Ordered DESC by created_at — most recent first
        assert_eq!(history[0].action, "conflict");
        assert_eq!(history[1].action, "pull");
        assert_eq!(history[2].action, "push");
    }

    #[test]
    fn test_sync_history_limit() {
        let s = test_storage();
        for i in 0..10 {
            s.add_sync_history("d1", &format!("action{i}"), None).unwrap();
        }
        let history = s.get_sync_history("d1", 3).unwrap();
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn test_sync_history_empty() {
        let s = test_storage();
        let history = s.get_sync_history("nonexistent", 10).unwrap();
        assert!(history.is_empty());
    }

    #[test]
    fn test_sync_history_content_hash() {
        let s = test_storage();
        s.add_sync_history("d1", "push", Some("abc")).unwrap();
        s.add_sync_history("d1", "conflict", None).unwrap();

        let history = s.get_sync_history("d1", 10).unwrap();
        assert_eq!(history[1].content_hash.as_deref(), Some("abc"));
        assert!(history[0].content_hash.is_none());
    }

    // ─── Version Snapshots ───────────────────────────────

    #[test]
    fn test_snapshot_crud() {
        let s = test_storage();
        s.save_snapshot("d1", "# Hello", "hash1").unwrap();
        s.save_snapshot("d1", "# Hello World", "hash2").unwrap();

        let snaps = s.get_snapshots("d1").unwrap();
        assert_eq!(snaps.len(), 2);
        // Most recent first
        assert_eq!(snaps[0].content_hash, "hash2");
        assert_eq!(snaps[0].content, "# Hello World");
        assert_eq!(snaps[1].content_hash, "hash1");
    }

    #[test]
    fn test_snapshot_by_id() {
        let s = test_storage();
        s.save_snapshot("d1", "content here", "hash1").unwrap();

        let snaps = s.get_snapshots("d1").unwrap();
        let snap_id = snaps[0].id;

        let found = s.get_snapshot_by_id(snap_id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().content, "content here");
    }

    #[test]
    fn test_snapshot_by_id_nonexistent() {
        let s = test_storage();
        assert!(s.get_snapshot_by_id(99999).unwrap().is_none());
    }

    #[test]
    fn test_snapshot_cleanup_keeps_20() {
        let s = test_storage();
        for i in 0..25 {
            s.save_snapshot("d1", &format!("content-{i}"), &format!("hash-{i}")).unwrap();
        }
        let snaps = s.get_snapshots("d1").unwrap();
        assert_eq!(snaps.len(), 20, "should keep only 20 snapshots");
        // Most recent should be content-24
        assert_eq!(snaps[0].content, "content-24");
    }

    #[test]
    fn test_snapshot_per_doc_isolation() {
        let s = test_storage();
        s.save_snapshot("d1", "doc1 content", "h1").unwrap();
        s.save_snapshot("d2", "doc2 content", "h2").unwrap();

        assert_eq!(s.get_snapshots("d1").unwrap().len(), 1);
        assert_eq!(s.get_snapshots("d2").unwrap().len(), 1);
    }

    // ─── search_docs_local ───────────────────────────────

    #[test]
    fn test_search_docs_local_exact() {
        let s = test_storage();
        s.upsert_doc(&sample_doc("d1")).unwrap();
        let results = s.search_docs_local("Test Doc").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].doc_id, "d1");
    }

    #[test]
    fn test_search_docs_local_partial() {
        let s = test_storage();
        s.upsert_doc(&sample_doc("d1")).unwrap();
        let results = s.search_docs_local("Test").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_docs_local_case_insensitive() {
        let s = test_storage();
        s.upsert_doc(&sample_doc("d1")).unwrap();
        let results = s.search_docs_local("test doc").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_docs_local_no_match() {
        let s = test_storage();
        s.upsert_doc(&sample_doc("d1")).unwrap();
        let results = s.search_docs_local("Nonexistent").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_docs_local_wildcard_escape() {
        let s = test_storage();
        let mut doc = sample_doc("d1");
        doc.title = "100% complete".to_string();
        s.upsert_doc(&doc).unwrap();

        let mut doc2 = sample_doc("d2");
        doc2.title = "100 items".to_string();
        s.upsert_doc(&doc2).unwrap();

        // Searching for "100%" should only match doc with literal %
        let results = s.search_docs_local("100%").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].doc_id, "d1");
    }

    #[test]
    fn test_search_docs_local_underscore_escape() {
        let s = test_storage();
        let mut doc = sample_doc("d1");
        doc.title = "test_doc".to_string();
        s.upsert_doc(&doc).unwrap();

        let mut doc2 = sample_doc("d2");
        doc2.title = "testXdoc".to_string();
        s.upsert_doc(&doc2).unwrap();

        // Without escaping, _ matches any char → both match
        // With proper escaping, only literal _ matches
        let results = s.search_docs_local("test_doc").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].doc_id, "d1");
    }

    // ─── list_synced_docs ────────────────────────────────

    #[test]
    fn test_list_synced_docs() {
        let s = test_storage();
        let d1 = sample_doc("d1"); // local_path = None
        s.upsert_doc(&d1).unwrap();

        let mut d2 = sample_doc("d2");
        d2.local_path = Some("/path/to/doc.md".to_string());
        s.upsert_doc(&d2).unwrap();

        let synced = s.list_synced_docs().unwrap();
        assert_eq!(synced.len(), 1);
        assert_eq!(synced[0].doc_id, "d2");
    }

    // ─── list_docs ordering ──────────────────────────────

    #[test]
    fn test_list_docs_empty() {
        let s = test_storage();
        assert!(s.list_docs().unwrap().is_empty());
    }

    #[test]
    fn test_list_docs_order_by_updated_at() {
        let s = test_storage();
        let mut d1 = sample_doc("d1");
        d1.updated_at = "2026-01-01T00:00:00+08:00".to_string();
        s.upsert_doc(&d1).unwrap();

        let mut d2 = sample_doc("d2");
        d2.updated_at = "2026-02-01T00:00:00+08:00".to_string();
        s.upsert_doc(&d2).unwrap();

        let docs = s.list_docs().unwrap();
        assert_eq!(docs[0].doc_id, "d2"); // more recent first
        assert_eq!(docs[1].doc_id, "d1");
    }

    // ─── Migration idempotency ───────────────────────────

    #[test]
    fn test_migrate_idempotent() {
        let s = test_storage();
        // migrate() is called in new_in_memory(), calling it again should be safe
        s.migrate().unwrap();
        s.migrate().unwrap();
        // DB should still work
        s.upsert_doc(&sample_doc("d1")).unwrap();
        assert!(s.get_doc("d1").unwrap().is_some());
    }

    // ─── File-based DB ───────────────────────────────────

    #[test]
    fn test_new_file_based() {
        let tmp = std::env::temp_dir().join("larknotes_test_storage_file");
        let _ = std::fs::remove_dir_all(&tmp);
        let db_path = tmp.join("test.db");

        let s = Storage::new(&db_path).unwrap();
        assert!(db_path.exists());
        s.upsert_doc(&sample_doc("d1")).unwrap();
        assert!(s.get_doc("d1").unwrap().is_some());

        // Reopen same file
        let s2 = Storage::new(&db_path).unwrap();
        assert!(s2.get_doc("d1").unwrap().is_some());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ─── SyncStatus roundtrip ────────────────────────────

    #[test]
    fn test_upsert_doc_error_status_roundtrip() {
        let s = test_storage();
        let mut doc = sample_doc("d1");
        doc.sync_status = SyncStatus::Error("网络异常，第2次重试中...".to_string());
        s.upsert_doc(&doc).unwrap();

        let fetched = s.get_doc("d1").unwrap().unwrap();
        assert_eq!(
            fetched.sync_status,
            SyncStatus::Error("网络异常，第2次重试中...".to_string())
        );
    }

    // ─── Config overwrite ────────────────────────────────

    #[test]
    fn test_config_overwrite() {
        let s = test_storage();
        s.set_config("key", "value1").unwrap();
        s.set_config("key", "value2").unwrap();
        assert_eq!(s.get_config("key").unwrap().unwrap(), "value2");
    }

    // ─── Delete nonexistent ──────────────────────────────

    #[test]
    fn test_delete_nonexistent() {
        let s = test_storage();
        // Should not error
        s.delete_doc("nonexistent").unwrap();
    }
}
