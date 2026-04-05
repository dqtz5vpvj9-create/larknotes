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
                ALTER TABLE documents ADD COLUMN remote_hash TEXT;
            "),
            (3, "
                ALTER TABLE documents ADD COLUMN folder_path TEXT NOT NULL DEFAULT '';
                CREATE TABLE IF NOT EXISTS folders (
                    folder_path TEXT PRIMARY KEY,
                    remote_token TEXT,
                    created_at TEXT NOT NULL DEFAULT (datetime('now'))
                );
            "),
            (4, "
                ALTER TABLE folders RENAME COLUMN remote_token TO remote_id;
            "),
            (5, "
                CREATE INDEX IF NOT EXISTS idx_local_path ON documents(local_path);
            "),
            (6, "
                ALTER TABLE documents ADD COLUMN pending_rename INTEGER NOT NULL DEFAULT 0;
            "),
        ];

        for (version, sql) in &migrations {
            if *version > current_version {
                let result = self.conn.execute_batch(sql);
                match result {
                    Ok(()) => {}
                    Err(e) => {
                        let msg = e.to_string();
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

        // ── v7: note_id / remote_id split, new tables ──────────────
        if current_version < 7 {
            self.migrate_v7()?;
            self.conn
                .execute("INSERT INTO schema_version (version) VALUES (7)", [])
                .map_err(|e| LarkNotesError::Storage(format!("记录迁移版本失败: {e}")))?;
            tracing::info!("数据库迁移完成: v7 (note_id / sync_ops / worktree_snapshot)");
        }

        Ok(())
    }

    /// v7 migration: introduce note_id as immutable local identity.
    ///
    /// - Recreates `documents` → `notes` with note_id PK and remote_id column.
    /// - Migrates sync_history and version_snapshots to use note_id.
    /// - Creates new worktree_snapshot and sync_ops tables.
    fn migrate_v7(&self) -> Result<(), LarkNotesError> {
        let tx = self.conn.unchecked_transaction()
            .map_err(|e| LarkNotesError::Storage(format!("v7事务开始失败: {e}")))?;

        // 1. Create the new notes table
        tx.execute_batch("
            CREATE TABLE IF NOT EXISTS notes (
                note_id         TEXT PRIMARY KEY,
                remote_id       TEXT,
                title           TEXT NOT NULL DEFAULT 'Untitled',
                desired_title   TEXT,
                doc_type        TEXT NOT NULL DEFAULT 'DOCX',
                url             TEXT NOT NULL DEFAULT '',
                owner_name      TEXT NOT NULL DEFAULT '',
                created_at      TEXT NOT NULL DEFAULT '',
                updated_at      TEXT NOT NULL DEFAULT '',
                local_path      TEXT,
                desired_path    TEXT,
                local_base_hash TEXT,
                remote_base_hash TEXT,
                sync_state      TEXT NOT NULL DEFAULT 'Synced',
                sync_status     TEXT NOT NULL DEFAULT 'New',
                last_synced_at  TEXT,
                folder_path     TEXT NOT NULL DEFAULT '',
                title_mode      TEXT NOT NULL DEFAULT 'manual'
            );
        ").map_err(|e| LarkNotesError::Storage(format!("v7创建notes表失败: {e}")))?;

        // 2. Check if old documents table exists (fresh DB won't have it after v7)
        let has_documents: bool = tx.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='documents'",
            [], |row| row.get::<_, i64>(0),
        ).unwrap_or(0) > 0;

        if has_documents {
            // Check if notes table is empty (not already migrated)
            let notes_count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM notes", [], |row| row.get(0),
            ).unwrap_or(0);

            if notes_count == 0 {
                // Migrate data: generate UUID note_id, doc_id becomes remote_id
                // Use hex(randomblob(16)) as UUID surrogate since we can't call Rust uuid in SQL
                let mut stmt = tx.prepare(
                    "SELECT doc_id, title, doc_type, url, owner_name, created_at, updated_at,
                            local_path, content_hash, remote_hash, sync_status, last_synced_at,
                            folder_path
                     FROM documents"
                ).map_err(|e| LarkNotesError::Storage(format!("v7读取documents失败: {e}")))?;

                #[allow(clippy::type_complexity)]
                let rows: Vec<(String, String, String, String, String, String, String,
                               Option<String>, Option<String>, Option<String>, String,
                               Option<String>, String)> = stmt.query_map([], |row| {
                    Ok((
                        row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                        row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?,
                        row.get(8)?, row.get(9)?, row.get(10)?, row.get(11)?,
                        row.get(12)?,
                    ))
                }).map_err(|e| LarkNotesError::Storage(format!("v7查询失败: {e}")))?
                .filter_map(|r| r.ok())
                .collect();

                for (doc_id, title, doc_type, url, owner, created, updated,
                     local_path, content_hash, remote_hash, sync_status,
                     last_synced, folder_path) in &rows
                {
                    let note_id = new_note_id();
                    tx.execute(
                        "INSERT INTO notes (note_id, remote_id, title, doc_type, url, owner_name,
                         created_at, updated_at, local_path, local_base_hash, remote_base_hash,
                         sync_state, sync_status, last_synced_at, folder_path)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                        params![
                            note_id, doc_id, title, doc_type, url, owner,
                            created, updated, local_path, content_hash, remote_hash,
                            "Synced", sync_status, last_synced, folder_path,
                        ],
                    ).map_err(|e| LarkNotesError::Storage(format!("v7插入notes失败: {e}")))?;

                    // Migrate sync_history
                    tx.execute(
                        "UPDATE sync_history SET doc_id = ?1 WHERE doc_id = ?2",
                        params![note_id, doc_id],
                    ).map_err(|e| LarkNotesError::Storage(format!("v7迁移sync_history失败: {e}")))?;

                    // Migrate version_snapshots
                    tx.execute(
                        "UPDATE version_snapshots SET doc_id = ?1 WHERE doc_id = ?2",
                        params![note_id, doc_id],
                    ).map_err(|e| LarkNotesError::Storage(format!("v7迁移snapshots失败: {e}")))?;
                }

                // Drop old table
                tx.execute_batch("DROP TABLE IF EXISTS documents;")
                    .map_err(|e| LarkNotesError::Storage(format!("v7删除旧表失败: {e}")))?;
            }
        }

        // 3. Create indexes on notes
        tx.execute_batch("
            CREATE UNIQUE INDEX IF NOT EXISTS idx_remote_id ON notes(remote_id) WHERE remote_id IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_notes_local_path ON notes(local_path);
            CREATE INDEX IF NOT EXISTS idx_sync_state ON notes(sync_state);
        ").map_err(|e| LarkNotesError::Storage(format!("v7创建索引失败: {e}")))?;

        // 4. Create worktree_snapshot table
        tx.execute_batch("
            CREATE TABLE IF NOT EXISTS worktree_snapshot (
                note_id       TEXT PRIMARY KEY,
                observed_path TEXT NOT NULL,
                mtime_ns      INTEGER,
                size          INTEGER,
                content_hash  TEXT,
                scan_gen      INTEGER NOT NULL DEFAULT 0,
                present       INTEGER NOT NULL DEFAULT 1
            );
        ").map_err(|e| LarkNotesError::Storage(format!("v7创建worktree_snapshot失败: {e}")))?;

        // 5. Create sync_ops table
        tx.execute_batch("
            CREATE TABLE IF NOT EXISTS sync_ops (
                op_id         INTEGER PRIMARY KEY AUTOINCREMENT,
                note_id       TEXT NOT NULL,
                op_kind       TEXT NOT NULL,
                op_state      TEXT NOT NULL DEFAULT 'pending',
                op_key        TEXT NOT NULL,
                payload       TEXT,
                precondition  TEXT,
                retry_count   INTEGER NOT NULL DEFAULT 0,
                max_retries   INTEGER NOT NULL DEFAULT 3,
                next_retry_at TEXT,
                claimed_at    TEXT,
                error_msg     TEXT,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_op_key_active
                ON sync_ops(op_key) WHERE op_state IN ('pending', 'claimed');
            CREATE INDEX IF NOT EXISTS idx_note_ops
                ON sync_ops(note_id, op_state);
        ").map_err(|e| LarkNotesError::Storage(format!("v7创建sync_ops失败: {e}")))?;

        // Rename sync_history.doc_id column conceptually (SQLite can't rename columns < 3.25)
        // The column is already updated to contain note_id values above.
        // We keep the column name as doc_id for now to avoid ALTER TABLE complexity,
        // but it now stores note_id values. Code references use note_id semantics.

        tx.commit()
            .map_err(|e| LarkNotesError::Storage(format!("v7事务提交失败: {e}")))?;

        Ok(())
    }

    // ─── Notes CRUD ──────────────────────────────────────────

    pub fn upsert_doc(&self, meta: &DocMeta) -> Result<(), LarkNotesError> {
        let sync_status_str = serde_json::to_string(&meta.sync_status).unwrap_or_default();
        let sync_state_str = serde_json::to_string(&meta.sync_state).unwrap_or_default();
        self.conn
            .execute(
                "INSERT INTO notes
                 (note_id, remote_id, title, desired_title, desired_path, doc_type, url, owner_name,
                  created_at, updated_at, local_path, local_base_hash, sync_state, sync_status,
                  folder_path, title_mode)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
                 ON CONFLICT(note_id) DO UPDATE SET
                  remote_id = excluded.remote_id,
                  title = excluded.title,
                  desired_title = excluded.desired_title,
                  desired_path = excluded.desired_path,
                  doc_type = excluded.doc_type,
                  url = excluded.url,
                  owner_name = excluded.owner_name,
                  created_at = excluded.created_at,
                  updated_at = excluded.updated_at,
                  local_path = excluded.local_path,
                  local_base_hash = excluded.local_base_hash,
                  sync_state = excluded.sync_state,
                  sync_status = excluded.sync_status,
                  folder_path = excluded.folder_path,
                  title_mode = excluded.title_mode",
                params![
                    meta.note_id,
                    meta.remote_id,
                    meta.title,
                    meta.desired_title,
                    meta.desired_path,
                    meta.doc_type,
                    meta.url,
                    meta.owner_name,
                    meta.created_at,
                    meta.updated_at,
                    meta.local_path,
                    meta.content_hash,
                    sync_state_str,
                    sync_status_str,
                    meta.folder_path,
                    meta.title_mode,
                ],
            )
            .map_err(|e| LarkNotesError::Storage(format!("写入文档失败: {e}")))?;
        Ok(())
    }

    pub fn get_doc(&self, id: &str) -> Result<Option<DocMeta>, LarkNotesError> {
        // Try note_id first, then remote_id for backward compat
        let result = self.conn
            .query_row(
                "SELECT note_id, remote_id, title, doc_type, url, owner_name, created_at, updated_at,
                        local_path, local_base_hash, sync_state, sync_status, folder_path, title_mode,
                        desired_title, desired_path
                 FROM notes WHERE note_id = ?1",
                params![id],
                |row| Ok(row_to_doc_meta(row)),
            )
            .optional()
            .map_err(|e| LarkNotesError::Storage(format!("查询文档失败: {e}")))?;

        if result.is_some() {
            return Ok(result);
        }

        // Fallback: look up by remote_id (for callers still passing doc_id)
        self.conn
            .query_row(
                "SELECT note_id, remote_id, title, doc_type, url, owner_name, created_at, updated_at,
                        local_path, local_base_hash, sync_state, sync_status, folder_path, title_mode,
                        desired_title, desired_path
                 FROM notes WHERE remote_id = ?1",
                params![id],
                |row| Ok(row_to_doc_meta(row)),
            )
            .optional()
            .map_err(|e| LarkNotesError::Storage(format!("查询文档失败: {e}")))
    }

    /// Look up by note_id only (no remote_id fallback).
    pub fn get_note(&self, note_id: &str) -> Result<Option<DocMeta>, LarkNotesError> {
        self.conn
            .query_row(
                "SELECT note_id, remote_id, title, doc_type, url, owner_name, created_at, updated_at,
                        local_path, local_base_hash, sync_state, sync_status, folder_path, title_mode,
                        desired_title, desired_path
                 FROM notes WHERE note_id = ?1",
                params![note_id],
                |row| Ok(row_to_doc_meta(row)),
            )
            .optional()
            .map_err(|e| LarkNotesError::Storage(format!("查询文档失败: {e}")))
    }

    /// Look up by remote_id only.
    pub fn get_note_by_remote_id(&self, remote_id: &str) -> Result<Option<DocMeta>, LarkNotesError> {
        self.conn
            .query_row(
                "SELECT note_id, remote_id, title, doc_type, url, owner_name, created_at, updated_at,
                        local_path, local_base_hash, sync_state, sync_status, folder_path, title_mode,
                        desired_title, desired_path
                 FROM notes WHERE remote_id = ?1",
                params![remote_id],
                |row| Ok(row_to_doc_meta(row)),
            )
            .optional()
            .map_err(|e| LarkNotesError::Storage(format!("按remote_id查询失败: {e}")))
    }

    pub fn list_docs(&self) -> Result<Vec<DocMeta>, LarkNotesError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT note_id, remote_id, title, doc_type, url, owner_name, created_at, updated_at,
                        local_path, local_base_hash, sync_state, sync_status, folder_path, title_mode,
                        desired_title, desired_path
                 FROM notes ORDER BY updated_at DESC",
            )
            .map_err(|e| LarkNotesError::Storage(format!("查询失败: {e}")))?;

        let docs = stmt
            .query_map([], |row| Ok(row_to_doc_meta(row)))
            .map_err(|e| LarkNotesError::Storage(format!("查询失败: {e}")))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(docs)
    }

    pub fn get_doc_by_path(&self, path: &str) -> Result<Option<DocMeta>, LarkNotesError> {
        self.conn
            .query_row(
                "SELECT note_id, remote_id, title, doc_type, url, owner_name, created_at, updated_at,
                        local_path, local_base_hash, sync_state, sync_status, folder_path, title_mode,
                        desired_title, desired_path
                 FROM notes WHERE local_path = ?1",
                params![path],
                |row| Ok(row_to_doc_meta(row)),
            )
            .optional()
            .map_err(|e| LarkNotesError::Storage(format!("按路径查询文档失败: {e}")))
    }

    pub fn update_sync_status(
        &self,
        note_id: &str,
        status: &SyncStatus,
    ) -> Result<(), LarkNotesError> {
        let status_str = serde_json::to_string(status).unwrap_or_default();
        let now = chrono::Local::now().to_rfc3339();
        self.conn
            .execute(
                "UPDATE notes SET sync_status = ?1, last_synced_at = ?2 WHERE note_id = ?3",
                params![status_str, now, note_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("更新状态失败: {e}")))?;
        Ok(())
    }

    pub fn update_sync_state(
        &self,
        note_id: &str,
        state: &SyncState,
    ) -> Result<(), LarkNotesError> {
        let state_str = serde_json::to_string(state).unwrap_or_default();
        self.conn
            .execute(
                "UPDATE notes SET sync_state = ?1 WHERE note_id = ?2",
                params![state_str, note_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("更新sync_state失败: {e}")))?;
        Ok(())
    }

    pub fn update_content_hash(&self, note_id: &str, hash: &str) -> Result<(), LarkNotesError> {
        self.conn
            .execute(
                "UPDATE notes SET local_base_hash = ?1 WHERE note_id = ?2",
                params![hash, note_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("更新哈希失败: {e}")))?;
        Ok(())
    }

    pub fn update_local_path(&self, note_id: &str, path: &str) -> Result<(), LarkNotesError> {
        self.conn
            .execute(
                "UPDATE notes SET local_path = ?1 WHERE note_id = ?2",
                params![path, note_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("更新本地路径失败: {e}")))?;
        Ok(())
    }

    pub fn update_title(&self, note_id: &str, title: &str) -> Result<(), LarkNotesError> {
        self.conn
            .execute(
                "UPDATE notes SET title = ?1 WHERE note_id = ?2",
                params![title, note_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("更新标题失败: {e}")))?;
        Ok(())
    }

    /// Clear the desired_title and desired_path after a rename is fulfilled.
    pub fn clear_desired_title(&self, note_id: &str) -> Result<(), LarkNotesError> {
        self.conn
            .execute(
                "UPDATE notes SET desired_title = NULL, desired_path = NULL WHERE note_id = ?1",
                params![note_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("清除desired_title失败: {e}")))?;
        Ok(())
    }

    /// Update the title_mode for a note (e.g. derive_once → manual after title derived).
    pub fn update_title_mode(&self, note_id: &str, mode: &str) -> Result<(), LarkNotesError> {
        self.conn
            .execute(
                "UPDATE notes SET title_mode = ?1 WHERE note_id = ?2",
                params![mode, note_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("更新title_mode失败: {e}")))?;
        Ok(())
    }

    pub fn delete_doc(&self, note_id: &str) -> Result<(), LarkNotesError> {
        self.conn
            .execute("DELETE FROM notes WHERE note_id = ?1", params![note_id])
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

    pub fn reset_stale_syncing(&self) -> Result<usize, LarkNotesError> {
        let modified_str = serde_json::to_string(&SyncStatus::LocalModified).unwrap_or_default();
        let stale_states = [
            serde_json::to_string(&SyncStatus::Syncing).unwrap_or_default(),
            serde_json::to_string(&SyncStatus::Pulling).unwrap_or_default(),
        ];
        let mut count = 0usize;
        for stale_str in &stale_states {
            count += self.conn
                .execute(
                    "UPDATE notes SET sync_status = ?1 WHERE sync_status = ?2",
                    params![modified_str, stale_str],
                )
                .map_err(|e| LarkNotesError::Storage(format!("重置同步状态失败: {e}")))?;
        }
        Ok(count)
    }

    /// Reset notes stuck in Executing state (sync_state) back to LocalModified.
    /// Called at startup to recover from interrupted sync operations.
    pub fn reset_stale_executing(&self) -> Result<usize, LarkNotesError> {
        let executing_str = serde_json::to_string(&SyncState::Executing).unwrap_or_default();
        let local_modified_str = serde_json::to_string(&SyncState::LocalModified).unwrap_or_default();
        let count = self.conn
            .execute(
                "UPDATE notes SET sync_state = ?1 WHERE sync_state = ?2",
                params![local_modified_str, executing_str],
            )
            .map_err(|e| LarkNotesError::Storage(format!("重置执行状态失败: {e}")))?;
        Ok(count)
    }

    // ─── Sync History ─────────────────────────────────────
    // The sync_history table still has a column named `doc_id` but it now stores note_id values.

    pub fn add_sync_history(
        &self,
        note_id: &str,
        action: &str,
        content_hash: Option<&str>,
    ) -> Result<(), LarkNotesError> {
        let now = chrono::Local::now().to_rfc3339();
        self.conn
            .execute(
                "INSERT INTO sync_history (doc_id, action, content_hash, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![note_id, action, content_hash, now],
            )
            .map_err(|e| LarkNotesError::Storage(format!("写入同步历史失败: {e}")))?;
        Ok(())
    }

    pub fn get_sync_history(
        &self,
        note_id: &str,
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
            .query_map(params![note_id, limit as i64], |row| {
                let nid: String = row.get(1)?;
                Ok(SyncHistoryEntry {
                    id: row.get(0)?,
                    note_id: nid.clone(),
                    doc_id: nid,
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
        note_id: &str,
        content: &str,
        content_hash: &str,
    ) -> Result<(), LarkNotesError> {
        let now = chrono::Local::now().to_rfc3339();
        self.conn
            .execute(
                "INSERT INTO version_snapshots (doc_id, content, content_hash, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![note_id, content, content_hash, now],
            )
            .map_err(|e| LarkNotesError::Storage(format!("保存快照失败: {e}")))?;

        self.conn
            .execute(
                "DELETE FROM version_snapshots WHERE doc_id = ?1 AND id NOT IN (
                    SELECT id FROM version_snapshots WHERE doc_id = ?1
                    ORDER BY created_at DESC LIMIT 20
                )",
                params![note_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("清理旧快照失败: {e}")))?;

        Ok(())
    }

    pub fn get_snapshots(
        &self,
        note_id: &str,
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
            .query_map(params![note_id], |row| {
                let nid: String = row.get(1)?;
                Ok(VersionSnapshot {
                    id: row.get(0)?,
                    note_id: nid.clone(),
                    doc_id: nid,
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
                    let nid: String = row.get(1)?;
                    Ok(VersionSnapshot {
                        id: row.get(0)?,
                        note_id: nid.clone(),
                        doc_id: nid,
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

    pub fn search_docs_local(&self, query: &str) -> Result<Vec<DocMeta>, LarkNotesError> {
        let escaped = query.replace('%', "\\%").replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let mut stmt = self
            .conn
            .prepare(
                "SELECT note_id, remote_id, title, doc_type, url, owner_name, created_at, updated_at,
                        local_path, local_base_hash, sync_state, sync_status, folder_path, title_mode,
                        desired_title, desired_path
                 FROM notes WHERE title LIKE ?1 ESCAPE '\\' COLLATE NOCASE
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

    pub fn set_pending_rename(&self, note_id: &str, pending: bool) -> Result<(), LarkNotesError> {
        // In v7, pending_rename is replaced by title_mode='derive_once'.
        // For backward compat, map pending=true to derive_once, false to manual.
        let mode = if pending { "derive_once" } else { "manual" };
        self.conn
            .execute(
                "UPDATE notes SET title_mode = ?1 WHERE note_id = ?2",
                params![mode, note_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("设置title_mode失败: {e}")))?;
        Ok(())
    }

    pub fn list_pending_rename_docs(&self) -> Result<Vec<DocMeta>, LarkNotesError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT note_id, remote_id, title, doc_type, url, owner_name, created_at, updated_at,
                        local_path, local_base_hash, sync_state, sync_status, folder_path, title_mode,
                        desired_title, desired_path
                 FROM notes WHERE title_mode = 'derive_once'",
            )
            .map_err(|e| LarkNotesError::Storage(format!("查询失败: {e}")))?;

        let docs = stmt
            .query_map([], |row| Ok(row_to_doc_meta(row)))
            .map_err(|e| LarkNotesError::Storage(format!("查询失败: {e}")))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(docs)
    }

    pub fn find_orphan_by_hash(&self, hash: &str) -> Result<Option<DocMeta>, LarkNotesError> {
        let mut stmt = self.conn
            .prepare(
                "SELECT note_id, remote_id, title, doc_type, url, owner_name, created_at, updated_at,
                        local_path, local_base_hash, sync_state, sync_status, folder_path, title_mode,
                        desired_title, desired_path
                 FROM notes WHERE local_base_hash = ?1",
            )
            .map_err(|e| LarkNotesError::Storage(format!("查询失败: {e}")))?;

        let docs: Vec<DocMeta> = stmt
            .query_map(params![hash], |row| Ok(row_to_doc_meta(row)))
            .map_err(|e| LarkNotesError::Storage(format!("查询失败: {e}")))?
            .filter_map(|r| r.ok())
            .collect();

        for doc in docs {
            match &doc.local_path {
                None => return Ok(Some(doc)),
                Some(p) if !std::path::Path::new(p).exists() => return Ok(Some(doc)),
                _ => continue,
            }
        }
        Ok(None)
    }

    pub fn title_exists_in_folder(
        &self,
        title: &str,
        folder: &str,
        exclude_note_id: Option<&str>,
    ) -> Result<bool, LarkNotesError> {
        let exists: bool = match exclude_note_id {
            Some(exclude) => self.conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM notes WHERE title = ?1 AND folder_path = ?2 AND note_id != ?3)",
                    params![title, folder, exclude],
                    |row| row.get(0),
                ),
            None => self.conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM notes WHERE title = ?1 AND folder_path = ?2)",
                    params![title, folder],
                    |row| row.get(0),
                ),
        }
        .map_err(|e| LarkNotesError::Storage(format!("查询标题唯一性失败: {e}")))?;
        Ok(exists)
    }

    pub fn unique_title(
        &self,
        title: &str,
        folder: &str,
        exclude_note_id: Option<&str>,
    ) -> Result<String, LarkNotesError> {
        if !self.title_exists_in_folder(title, folder, exclude_note_id)? {
            return Ok(title.to_string());
        }
        for n in 2..=999 {
            let candidate = format!("{title} ({n})");
            if !self.title_exists_in_folder(&candidate, folder, exclude_note_id)? {
                return Ok(candidate);
            }
        }
        Ok(format!(
            "{title} ({})",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ))
    }

    // ─── Baselines (P5: separate hash domains) ──────────

    /// Read the cached remote_base_hash for a note.
    pub fn get_remote_hash(&self, note_id: &str) -> Result<Option<String>, LarkNotesError> {
        self.conn
            .query_row(
                "SELECT remote_base_hash FROM notes WHERE note_id = ?1",
                params![note_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(|opt| opt.flatten())
            .map_err(|e| LarkNotesError::Storage(format!("查询远程哈希失败: {e}")))
    }

    /// Set both baselines independently after a successful sync.
    pub fn set_baselines(
        &self,
        note_id: &str,
        local_base: &str,
        remote_base: &str,
    ) -> Result<(), LarkNotesError> {
        self.conn
            .execute(
                "UPDATE notes SET local_base_hash = ?1, remote_base_hash = ?2 WHERE note_id = ?3",
                params![local_base, remote_base, note_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("更新基线失败: {e}")))?;
        Ok(())
    }

    /// Legacy compat: set both hashes to the same value.
    pub fn set_synced_hashes(&self, note_id: &str, hash: &str) -> Result<(), LarkNotesError> {
        self.set_baselines(note_id, hash, hash)
    }

    pub fn update_remote_hash(&self, note_id: &str, hash: &str) -> Result<(), LarkNotesError> {
        self.conn
            .execute(
                "UPDATE notes SET remote_base_hash = ?1 WHERE note_id = ?2",
                params![hash, note_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("更新远程哈希失败: {e}")))?;
        Ok(())
    }

    pub fn update_url(&self, note_id: &str, url: &str) -> Result<(), LarkNotesError> {
        self.conn
            .execute(
                "UPDATE notes SET url = ?1 WHERE note_id = ?2",
                params![url, note_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("更新URL失败: {e}")))?;
        Ok(())
    }

    /// Update the remote_id for a note (e.g. after re-creating on remote).
    /// Replaces the old replace_doc_id() — no more cascade needed since
    /// all tables use note_id as the stable key.
    pub fn update_remote_id(&self, note_id: &str, new_remote_id: &str) -> Result<(), LarkNotesError> {
        self.conn
            .execute(
                "UPDATE notes SET remote_id = ?1 WHERE note_id = ?2",
                params![new_remote_id, note_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("更新remote_id失败: {e}")))?;
        Ok(())
    }

    /// Legacy compat: replace_doc_id now just updates remote_id.
    pub fn replace_doc_id(&self, _old_id: &str, new_id: &str) -> Result<(), LarkNotesError> {
        // In v7, find the note by old remote_id and update to new remote_id.
        self.conn
            .execute(
                "UPDATE notes SET remote_id = ?1 WHERE remote_id = ?2",
                params![new_id, _old_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("替换remote_id失败: {e}")))?;
        Ok(())
    }

    // ─── Folder operations ──────────────────────────────

    pub fn update_folder_path(&self, note_id: &str, folder_path: &str) -> Result<(), LarkNotesError> {
        self.conn
            .execute(
                "UPDATE notes SET folder_path = ?1 WHERE note_id = ?2",
                params![folder_path, note_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("更新文件夹路径失败: {e}")))?;
        Ok(())
    }

    pub fn list_docs_in_folder(&self, folder_path: &str) -> Result<Vec<DocMeta>, LarkNotesError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT note_id, remote_id, title, doc_type, url, owner_name, created_at, updated_at,
                        local_path, local_base_hash, sync_state, sync_status, folder_path, title_mode,
                        desired_title, desired_path
                 FROM notes WHERE folder_path = ?1
                 ORDER BY updated_at DESC",
            )
            .map_err(|e| LarkNotesError::Storage(format!("查询失败: {e}")))?;

        let docs = stmt
            .query_map(params![folder_path], |row| Ok(row_to_doc_meta(row)))
            .map_err(|e| LarkNotesError::Storage(format!("查询失败: {e}")))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(docs)
    }

    pub fn upsert_folder(
        &self,
        folder_path: &str,
        remote_id: Option<&str>,
    ) -> Result<(), LarkNotesError> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO folders (folder_path, remote_id) VALUES (?1, ?2)",
                params![folder_path, remote_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("写入文件夹失败: {e}")))?;
        Ok(())
    }

    pub fn delete_folder(&self, folder_path: &str) -> Result<(), LarkNotesError> {
        self.conn
            .execute(
                "DELETE FROM folders WHERE folder_path = ?1",
                params![folder_path],
            )
            .map_err(|e| LarkNotesError::Storage(format!("删除文件夹失败: {e}")))?;
        Ok(())
    }

    pub fn list_folders(&self) -> Result<Vec<FolderInfo>, LarkNotesError> {
        let mut stmt = self
            .conn
            .prepare("SELECT folder_path, remote_id FROM folders ORDER BY folder_path")
            .map_err(|e| LarkNotesError::Storage(format!("查询文件夹失败: {e}")))?;

        let folders = stmt
            .query_map([], |row| {
                Ok(FolderInfo {
                    folder_path: row.get(0)?,
                    remote_id: row.get(1)?,
                })
            })
            .map_err(|e| LarkNotesError::Storage(format!("查询文件夹失败: {e}")))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(folders)
    }

    pub fn rename_folder(&self, old_path: &str, new_path: &str) -> Result<usize, LarkNotesError> {
        self.conn
            .execute(
                "UPDATE folders SET folder_path = ?1 WHERE folder_path = ?2",
                params![new_path, old_path],
            )
            .map_err(|e| LarkNotesError::Storage(format!("重命名文件夹失败: {e}")))?;

        let old_prefix = format!("{old_path}/");
        let new_prefix = format!("{new_path}/");
        self.conn
            .execute(
                "UPDATE folders SET folder_path = ?1 || SUBSTR(folder_path, ?2)
                 WHERE folder_path LIKE ?3 || '%'",
                params![new_prefix, old_prefix.len() as i64 + 1, old_prefix],
            )
            .map_err(|e| LarkNotesError::Storage(format!("重命名子文件夹失败: {e}")))?;

        let doc_count = self.conn
            .execute(
                "UPDATE notes SET folder_path = ?1 WHERE folder_path = ?2",
                params![new_path, old_path],
            )
            .map_err(|e| LarkNotesError::Storage(format!("更新文档文件夹失败: {e}")))?;

        self.conn
            .execute(
                "UPDATE notes SET folder_path = ?1 || SUBSTR(folder_path, ?2)
                 WHERE folder_path LIKE ?3 || '%'",
                params![new_prefix, old_prefix.len() as i64 + 1, old_prefix],
            )
            .map_err(|e| LarkNotesError::Storage(format!("更新子文档文件夹失败: {e}")))?;

        Ok(doc_count)
    }

    pub fn list_synced_docs(&self) -> Result<Vec<DocMeta>, LarkNotesError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT note_id, remote_id, title, doc_type, url, owner_name, created_at, updated_at,
                        local_path, local_base_hash, sync_state, sync_status, folder_path, title_mode,
                        desired_title, desired_path
                 FROM notes WHERE local_path IS NOT NULL
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

    // ─── worktree_snapshot CRUD ─────────────────────────

    /// Upsert a worktree snapshot entry for a note.
    pub fn upsert_worktree_snapshot(
        &self,
        note_id: &str,
        observed_path: &str,
        mtime_ns: Option<i64>,
        size: Option<i64>,
        content_hash: Option<&str>,
        scan_gen: i64,
    ) -> Result<(), LarkNotesError> {
        self.conn
            .execute(
                "INSERT INTO worktree_snapshot (note_id, observed_path, mtime_ns, size, content_hash, scan_gen, present)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)
                 ON CONFLICT(note_id) DO UPDATE SET
                   observed_path = excluded.observed_path,
                   mtime_ns = excluded.mtime_ns,
                   size = excluded.size,
                   content_hash = excluded.content_hash,
                   scan_gen = excluded.scan_gen,
                   present = 1",
                rusqlite::params![note_id, observed_path, mtime_ns, size, content_hash, scan_gen],
            )
            .map_err(|e| LarkNotesError::Storage(format!("upsert worktree_snapshot失败: {e}")))?;
        Ok(())
    }

    /// Get the worktree snapshot entry for a note.
    pub fn get_worktree_snapshot(&self, note_id: &str) -> Result<Option<WorktreeEntry>, LarkNotesError> {
        let mut stmt = self.conn
            .prepare(
                "SELECT note_id, observed_path, mtime_ns, size, content_hash, scan_gen, present
                 FROM worktree_snapshot WHERE note_id = ?1",
            )
            .map_err(|e| LarkNotesError::Storage(format!("查询worktree_snapshot失败: {e}")))?;
        let entry = stmt
            .query_row(rusqlite::params![note_id], |row| {
                Ok(WorktreeEntry {
                    note_id: row.get(0)?,
                    observed_path: row.get(1)?,
                    mtime_ns: row.get(2)?,
                    size: row.get(3)?,
                    content_hash: row.get(4)?,
                    scan_gen: row.get(5)?,
                    present: row.get(6)?,
                })
            })
            .optional()
            .map_err(|e| LarkNotesError::Storage(format!("查询worktree_snapshot失败: {e}")))?;
        Ok(entry)
    }

    /// Get the worktree snapshot entry by observed path.
    pub fn get_worktree_by_path(&self, path: &str) -> Result<Option<WorktreeEntry>, LarkNotesError> {
        let mut stmt = self.conn
            .prepare(
                "SELECT note_id, observed_path, mtime_ns, size, content_hash, scan_gen, present
                 FROM worktree_snapshot WHERE observed_path = ?1",
            )
            .map_err(|e| LarkNotesError::Storage(format!("查询worktree_snapshot by path失败: {e}")))?;
        let entry = stmt
            .query_row(rusqlite::params![path], |row| {
                Ok(WorktreeEntry {
                    note_id: row.get(0)?,
                    observed_path: row.get(1)?,
                    mtime_ns: row.get(2)?,
                    size: row.get(3)?,
                    content_hash: row.get(4)?,
                    scan_gen: row.get(5)?,
                    present: row.get(6)?,
                })
            })
            .optional()
            .map_err(|e| LarkNotesError::Storage(format!("查询worktree_snapshot by path失败: {e}")))?;
        Ok(entry)
    }

    /// Get all worktree snapshot entries.
    pub fn list_worktree_snapshots(&self) -> Result<Vec<WorktreeEntry>, LarkNotesError> {
        let mut stmt = self.conn
            .prepare(
                "SELECT note_id, observed_path, mtime_ns, size, content_hash, scan_gen, present
                 FROM worktree_snapshot",
            )
            .map_err(|e| LarkNotesError::Storage(format!("查询worktree_snapshot列表失败: {e}")))?;
        let entries = stmt
            .query_map([], |row| {
                Ok(WorktreeEntry {
                    note_id: row.get(0)?,
                    observed_path: row.get(1)?,
                    mtime_ns: row.get(2)?,
                    size: row.get(3)?,
                    content_hash: row.get(4)?,
                    scan_gen: row.get(5)?,
                    present: row.get(6)?,
                })
            })
            .map_err(|e| LarkNotesError::Storage(format!("查询worktree_snapshot列表失败: {e}")))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(entries)
    }

    /// Mark entries from an older scan_gen as not present (stale).
    pub fn mark_stale_worktree(&self, current_gen: i64) -> Result<usize, LarkNotesError> {
        let count = self.conn
            .execute(
                "UPDATE worktree_snapshot SET present = 0 WHERE scan_gen < ?1 AND present = 1",
                rusqlite::params![current_gen],
            )
            .map_err(|e| LarkNotesError::Storage(format!("标记stale worktree失败: {e}")))?;
        Ok(count)
    }

    /// Get entries that are marked as not present (file missing at last scan).
    pub fn get_missing_worktree_entries(&self) -> Result<Vec<WorktreeEntry>, LarkNotesError> {
        let mut stmt = self.conn
            .prepare(
                "SELECT note_id, observed_path, mtime_ns, size, content_hash, scan_gen, present
                 FROM worktree_snapshot WHERE present = 0",
            )
            .map_err(|e| LarkNotesError::Storage(format!("查询missing worktree失败: {e}")))?;
        let entries = stmt
            .query_map([], |row| {
                Ok(WorktreeEntry {
                    note_id: row.get(0)?,
                    observed_path: row.get(1)?,
                    mtime_ns: row.get(2)?,
                    size: row.get(3)?,
                    content_hash: row.get(4)?,
                    scan_gen: row.get(5)?,
                    present: row.get(6)?,
                })
            })
            .map_err(|e| LarkNotesError::Storage(format!("查询missing worktree失败: {e}")))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(entries)
    }

    // ─── sync_ops CRUD ──────────────────────────────────

    /// Enqueue a sync operation. If an active op with the same op_key exists,
    /// supersede it and insert the new one.
    pub fn enqueue_op(
        &self,
        note_id: &str,
        op_kind: &str,
        payload: Option<&str>,
        precondition: Option<&str>,
    ) -> Result<i64, LarkNotesError> {
        let now = chrono::Local::now().to_rfc3339();
        let op_key = format!("{note_id}:{op_kind}");

        // Supersede any existing active op with same key
        self.conn
            .execute(
                "UPDATE sync_ops SET op_state = 'superseded', updated_at = ?1
                 WHERE op_key = ?2 AND op_state IN ('pending', 'claimed')",
                params![now, op_key],
            )
            .map_err(|e| LarkNotesError::Storage(format!("标记旧操作失败: {e}")))?;

        self.conn
            .execute(
                "INSERT INTO sync_ops (note_id, op_kind, op_state, op_key, payload, precondition, created_at, updated_at)
                 VALUES (?1, ?2, 'pending', ?3, ?4, ?5, ?6, ?6)",
                params![note_id, op_kind, op_key, payload, precondition, now],
            )
            .map_err(|e| LarkNotesError::Storage(format!("入队操作失败: {e}")))?;

        let op_id = self.conn.last_insert_rowid();
        Ok(op_id)
    }

    /// Claim a pending op for execution.
    pub fn claim_op(&self, op_id: i64) -> Result<bool, LarkNotesError> {
        let now = chrono::Local::now().to_rfc3339();
        let changed = self.conn
            .execute(
                "UPDATE sync_ops SET op_state = 'claimed', claimed_at = ?1, updated_at = ?1
                 WHERE op_id = ?2 AND op_state = 'pending'",
                params![now, op_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("认领操作失败: {e}")))?;
        Ok(changed > 0)
    }

    /// Mark an op as done.
    pub fn complete_op(&self, op_id: i64) -> Result<(), LarkNotesError> {
        let now = chrono::Local::now().to_rfc3339();
        self.conn
            .execute(
                "UPDATE sync_ops SET op_state = 'done', updated_at = ?1 WHERE op_id = ?2",
                params![now, op_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("完成操作失败: {e}")))?;
        Ok(())
    }

    /// Mark an op as failed with error message and increment retry.
    pub fn fail_op(&self, op_id: i64, error: &str) -> Result<(), LarkNotesError> {
        let now = chrono::Local::now().to_rfc3339();
        self.conn
            .execute(
                "UPDATE sync_ops SET op_state = 'failed', error_msg = ?1, retry_count = retry_count + 1, updated_at = ?2
                 WHERE op_id = ?3",
                params![error, now, op_id],
            )
            .map_err(|e| LarkNotesError::Storage(format!("标记操作失败: {e}")))?;
        Ok(())
    }

    /// Get all pending ops, ordered by creation time.
    pub fn get_pending_ops(&self) -> Result<Vec<SyncOp>, LarkNotesError> {
        let now = chrono::Local::now().to_rfc3339();
        let mut stmt = self.conn
            .prepare(
                "SELECT op_id, note_id, op_kind, op_state, op_key, payload, precondition,
                        retry_count, max_retries, next_retry_at, claimed_at, error_msg,
                        created_at, updated_at
                 FROM sync_ops
                 WHERE op_state = 'pending'
                   AND (next_retry_at IS NULL OR next_retry_at <= ?1)
                 ORDER BY created_at ASC",
            )
            .map_err(|e| LarkNotesError::Storage(format!("查询pending ops失败: {e}")))?;

        let ops = stmt
            .query_map(params![now], |row| Ok(row_to_sync_op(row)))
            .map_err(|e| LarkNotesError::Storage(format!("查询pending ops失败: {e}")))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(ops)
    }

    /// Reset stale claimed ops (crashed mid-execution) back to pending.
    pub fn reset_stale_claimed_ops(&self, stale_seconds: i64) -> Result<usize, LarkNotesError> {
        let cutoff = (chrono::Local::now() - chrono::Duration::seconds(stale_seconds)).to_rfc3339();
        let count = self.conn
            .execute(
                "UPDATE sync_ops SET op_state = 'pending', claimed_at = NULL, updated_at = ?1
                 WHERE op_state = 'claimed' AND claimed_at < ?2",
                params![chrono::Local::now().to_rfc3339(), cutoff],
            )
            .map_err(|e| LarkNotesError::Storage(format!("重置stale ops失败: {e}")))?;
        Ok(count)
    }
}

// ─── Worktree entry struct ───────────────────────────────

#[derive(Debug, Clone)]
pub struct WorktreeEntry {
    pub note_id: String,
    pub observed_path: String,
    pub mtime_ns: Option<i64>,
    pub size: Option<i64>,
    pub content_hash: Option<String>,
    pub scan_gen: i64,
    pub present: bool,
}

// ─── Sync op struct ──────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SyncOp {
    pub op_id: i64,
    pub note_id: String,
    pub op_kind: String,
    pub op_state: String,
    pub op_key: String,
    pub payload: Option<String>,
    pub precondition: Option<String>,
    pub retry_count: i64,
    pub max_retries: i64,
    pub next_retry_at: Option<String>,
    pub claimed_at: Option<String>,
    pub error_msg: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

fn row_to_sync_op(row: &rusqlite::Row) -> SyncOp {
    SyncOp {
        op_id: row.get(0).unwrap_or_default(),
        note_id: row.get(1).unwrap_or_default(),
        op_kind: row.get(2).unwrap_or_default(),
        op_state: row.get(3).unwrap_or_default(),
        op_key: row.get(4).unwrap_or_default(),
        payload: row.get(5).unwrap_or_default(),
        precondition: row.get(6).unwrap_or_default(),
        retry_count: row.get(7).unwrap_or_default(),
        max_retries: row.get(8).unwrap_or_default(),
        next_retry_at: row.get(9).unwrap_or_default(),
        claimed_at: row.get(10).unwrap_or_default(),
        error_msg: row.get(11).unwrap_or_default(),
        created_at: row.get(12).unwrap_or_default(),
        updated_at: row.get(13).unwrap_or_default(),
    }
}

fn row_to_doc_meta(row: &rusqlite::Row) -> DocMeta {
    let sync_state_str: String = row.get(10).unwrap_or_default();
    let sync_state = serde_json::from_str(&sync_state_str).unwrap_or(SyncState::Synced);

    let sync_status_str: String = row.get(11).unwrap_or_default();
    let sync_status = serde_json::from_str(&sync_status_str).unwrap_or(SyncStatus::New);

    let note_id: String = row.get(0).unwrap_or_default();
    let remote_id: Option<String> = row.get(1).unwrap_or_default();

    DocMeta {
        note_id: note_id.clone(),
        remote_id: remote_id.clone(),
        // doc_id always mirrors note_id — NEVER set to remote_id.
        // Use the `remote_id` field for remote identity.
        doc_id: note_id.clone(),
        title: row.get(2).unwrap_or_default(),
        doc_type: row.get(3).unwrap_or_default(),
        url: row.get(4).unwrap_or_default(),
        owner_name: row.get(5).unwrap_or_default(),
        created_at: row.get(6).unwrap_or_default(),
        updated_at: row.get(7).unwrap_or_default(),
        local_path: row.get(8).unwrap_or_default(),
        content_hash: row.get(9).unwrap_or_default(),
        sync_status,
        folder_path: row.get(12).unwrap_or_default(),
        file_size: None,
        word_count: None,
        sync_state,
        title_mode: row.get(13).unwrap_or_else(|_| "manual".to_string()),
        desired_title: row.get(14).unwrap_or_default(),
        desired_path: row.get(15).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_storage() -> Storage {
        Storage::new_in_memory().unwrap()
    }

    /// Create a sample doc, `id` used as both note_id and remote_id for test convenience.
    fn sample_doc(id: &str) -> DocMeta {
        DocMeta {
            note_id: id.to_string(),
            remote_id: Some(id.to_string()),
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
            folder_path: String::new(),
            file_size: None,
            word_count: None,
            sync_state: SyncState::Synced,
            title_mode: "manual".to_string(),
            desired_title: None,
            desired_path: None,
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
        let rh = s.get_remote_hash("d1").unwrap();
        assert_eq!(rh.as_deref(), Some("remote_xyz"));
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
        assert_eq!(results[0].note_id, "d1");
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
        assert_eq!(results[0].note_id, "d1");
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
        assert_eq!(results[0].note_id, "d1");
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
        assert_eq!(synced[0].note_id, "d2");
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
        assert_eq!(docs[0].note_id, "d2"); // more recent first
        assert_eq!(docs[1].note_id, "d1");
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

    // ─── Folder operations ──────────────────────────────

    #[test]
    fn test_folder_crud() {
        let s = test_storage();
        s.upsert_folder("project-a", None).unwrap();
        s.upsert_folder("project-a/sub", Some("fld_abc")).unwrap();

        let folders = s.list_folders().unwrap();
        assert_eq!(folders.len(), 2);
        assert_eq!(folders[0].folder_path, "project-a");
        assert!(folders[0].remote_id.is_none());
        assert_eq!(folders[1].folder_path, "project-a/sub");
        assert_eq!(folders[1].remote_id.as_deref(), Some("fld_abc"));

        s.delete_folder("project-a/sub").unwrap();
        assert_eq!(s.list_folders().unwrap().len(), 1);
    }

    #[test]
    fn test_doc_folder_path() {
        let s = test_storage();
        let mut doc = sample_doc("d1");
        doc.folder_path = "project-a".to_string();
        s.upsert_doc(&doc).unwrap();

        let fetched = s.get_doc("d1").unwrap().unwrap();
        assert_eq!(fetched.folder_path, "project-a");

        // list_docs_in_folder
        let in_root = s.list_docs_in_folder("").unwrap();
        assert!(in_root.is_empty());
        let in_project = s.list_docs_in_folder("project-a").unwrap();
        assert_eq!(in_project.len(), 1);
        assert_eq!(in_project[0].note_id, "d1");
    }

    #[test]
    fn test_update_folder_path() {
        let s = test_storage();
        s.upsert_doc(&sample_doc("d1")).unwrap();
        assert_eq!(s.get_doc("d1").unwrap().unwrap().folder_path, "");

        s.update_folder_path("d1", "new-folder").unwrap();
        assert_eq!(s.get_doc("d1").unwrap().unwrap().folder_path, "new-folder");
    }

    #[test]
    fn test_rename_folder() {
        let s = test_storage();
        s.upsert_folder("old", None).unwrap();
        s.upsert_folder("old/child", None).unwrap();

        let mut d1 = sample_doc("d1");
        d1.folder_path = "old".to_string();
        s.upsert_doc(&d1).unwrap();

        let mut d2 = sample_doc("d2");
        d2.folder_path = "old/child".to_string();
        s.upsert_doc(&d2).unwrap();

        s.rename_folder("old", "new").unwrap();

        // Folders renamed
        let folders = s.list_folders().unwrap();
        let paths: Vec<&str> = folders.iter().map(|f| f.folder_path.as_str()).collect();
        assert!(paths.contains(&"new"));
        assert!(paths.contains(&"new/child"));
        assert!(!paths.contains(&"old"));

        // Documents updated
        assert_eq!(s.get_doc("d1").unwrap().unwrap().folder_path, "new");
        assert_eq!(s.get_doc("d2").unwrap().unwrap().folder_path, "new/child");
    }
}
