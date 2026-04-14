use crate::hasher::hash_content;
use crate::planner::SyncAction;
use crate::write_guard::WriteGuard;
use larknotes_core::*;
use larknotes_storage::Storage;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;

/// Status update emitted by the executor for each note.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SyncStatusUpdate {
    pub note_id: String,
    pub status: SyncStatus,
    pub title: Option<String>,
    /// When remote_id changes (e.g. recreate), include old note_id so frontend can remap.
    pub new_remote_id: Option<String>,
}

/// The Executor consumes SyncActions and performs the actual I/O.
pub struct Executor {
    provider: Arc<dyn DocProvider>,
    storage: Arc<Mutex<Storage>>,
    workspace_dir: PathBuf,
    write_guard: WriteGuard,
    status_tx: broadcast::Sender<SyncStatusUpdate>,
}

impl Executor {
    pub fn new(
        provider: Arc<dyn DocProvider>,
        storage: Arc<Mutex<Storage>>,
        workspace_dir: PathBuf,
        write_guard: WriteGuard,
        status_tx: broadcast::Sender<SyncStatusUpdate>,
    ) -> Self {
        Self {
            provider,
            storage,
            workspace_dir,
            write_guard,
            status_tx,
        }
    }

    /// Execute a single SyncAction.
    pub async fn execute(&self, action: SyncAction) {
        match action {
            SyncAction::Push { note_id, content, title, local_hash } => {
                self.execute_push(&note_id, &content, &title, &local_hash).await;
            }
            SyncAction::Pull { note_id, remote_content } => {
                self.execute_pull(&note_id, &remote_content).await;
            }
            SyncAction::CreateRemote { note_id, content, title } => {
                self.execute_create_remote(&note_id, &content, &title).await;
            }
            SyncAction::DeleteRemote { note_id, remote_id } => {
                self.execute_delete_remote(&note_id, &remote_id).await;
            }
            SyncAction::RenameRemote { note_id, new_title } => {
                self.execute_rename_remote(&note_id, &new_title).await;
            }
            SyncAction::MarkConflict { note_id } => {
                self.execute_mark_conflict(&note_id).await;
            }
            SyncAction::ReclaimOrphan { note_id, new_path } => {
                self.execute_reclaim_orphan(&note_id, &new_path);
            }
            SyncAction::AdoptNewFile { path } => {
                self.execute_adopt_new_file(&path).await;
            }
            SyncAction::MarkFileMissing { note_id } => {
                self.execute_mark_file_missing(&note_id);
            }
            SyncAction::DeriveTitleRename { note_id, new_title } => {
                self.execute_derive_title_rename(&note_id, &new_title).await;
            }
        }
    }

    // ─── Push ───────────────────────────────────────────

    async fn execute_push(&self, note_id: &str, content: &str, title: &str, local_hash: &str) {
        let remote_id = match self.get_remote_id(note_id) {
            Some(id) => id,
            None => {
                // No remote doc yet — fall back to create_remote instead of silently dropping.
                tracing::info!("push: no remote_id for {note_id}, falling back to create_remote");
                self.execute_create_remote(note_id, content, title).await;
                return;
            }
        };

        // Enqueue op
        let op_id = self.enqueue("push", note_id, Some(content));
        if op_id.is_none() { return; }
        let op_id = op_id.unwrap();

        self.emit(note_id, SyncStatus::Syncing, None);
        self.set_sync_state(note_id, &SyncState::Executing);

        // Retry loop
        let retry_delays = [
            Duration::from_secs(5),
            Duration::from_secs(15),
            Duration::from_secs(45),
        ];
        let mut attempts = 0;

        loop {
            match self.provider.write(&remote_id, content).await {
                Ok(write_meta) => {
                    // If write was performed via reimport, update remote_id and url
                    let effective_remote_id = if let Some(ref new_id) = write_meta.new_remote_id {
                        if let Ok(store) = self.storage.lock() {
                            let _ = store.update_remote_id(note_id, new_id);
                            if let Some(ref new_url) = write_meta.new_url {
                                let _ = store.update_url(note_id, new_url);
                            }
                        }
                        tracing::info!("push: reimported {note_id} → new remote_id {new_id}");
                        new_id.clone()
                    } else {
                        remote_id.clone()
                    };

                    // Title changed? Rename on remote too.
                    let old_title = self.get_title(note_id);
                    if old_title.as_deref() != Some(title) {
                        if let Err(e) = self.provider.rename(&effective_remote_id, title).await {
                            tracing::error!("push: remote rename failed for {note_id}: {e}");
                        }
                    }

                    // Read back remote content to set remote_base_hash (P5)
                    let remote_hash = match self.provider.read(&effective_remote_id).await {
                        Ok(read_output) => Some(hash_content(read_output.content.as_bytes())),
                        Err(e) => {
                            tracing::warn!("push: post-write read failed for {note_id}: {e}");
                            None
                        }
                    };

                    // Update baselines
                    if let Ok(store) = self.storage.lock() {
                        if let Some(ref rh) = remote_hash {
                            let _ = store.set_baselines(note_id, local_hash, rh);
                        } else {
                            let _ = store.update_content_hash(note_id, local_hash);
                        }
                        let _ = store.update_sync_status(note_id, &SyncStatus::Synced);
                        let _ = store.update_sync_state(note_id, &SyncState::Synced);
                        let _ = store.add_sync_history(note_id, "push", Some(local_hash));
                        let _ = store.save_snapshot(note_id, content, local_hash);
                        let _ = store.complete_op(op_id);
                    }
                    self.emit(note_id, SyncStatus::Synced, None);
                    tracing::info!("push成功: {note_id}");
                    return;
                }
                Err(e) => {
                    if e.is_not_found() {
                        // Remote doc deleted — recreate
                        tracing::warn!("push: remote doc deleted, recreating: {note_id}");
                        self.execute_recreate(note_id, title, content, local_hash).await;
                        if let Ok(store) = self.storage.lock() {
                            let _ = store.complete_op(op_id);
                        }
                        return;
                    }
                    if !e.is_transient() || attempts >= retry_delays.len() {
                        tracing::error!("push failed permanently: {note_id}: {e}");
                        self.set_sync_state(note_id, &SyncState::Error(e.to_string()));
                        if let Ok(store) = self.storage.lock() {
                            let _ = store.update_sync_status(note_id, &SyncStatus::Error(e.to_string()));
                            let _ = store.fail_op(op_id, &e.to_string());
                        }
                        self.emit(note_id, SyncStatus::Error(e.to_string()), None);
                        return;
                    }
                    tracing::warn!("push transient error, retrying ({}/{}): {note_id}: {e}",
                        attempts + 1, retry_delays.len());
                    tokio::time::sleep(retry_delays[attempts]).await;
                    attempts += 1;
                }
            }
        }
    }

    // ─── Pull ───────────────────────────────────────────

    async fn execute_pull(&self, note_id: &str, remote_content: &str) {
        let local_path = match self.get_local_path(note_id) {
            Some(p) => p,
            None => {
                tracing::warn!("pull: no local_path for note {note_id}");
                return;
            }
        };

        let op_id = self.enqueue("pull", note_id, None);
        if op_id.is_none() { return; }
        let op_id = op_id.unwrap();

        self.emit(note_id, SyncStatus::Pulling, None);
        self.set_sync_state(note_id, &SyncState::Executing);

        // Write under guard to prevent watcher feedback
        let _guard = self.write_guard.guard(&local_path);
        if let Err(e) = tokio::fs::write(&local_path, remote_content).await {
            tracing::error!("pull: write failed: {note_id}: {e}");
            self.set_sync_state(note_id, &SyncState::Error(e.to_string()));
            self.emit(note_id, SyncStatus::Error(format!("写入失败: {e}")), None);
            if let Ok(store) = self.storage.lock() {
                let _ = store.fail_op(op_id, &e.to_string());
            }
            return;
        }

        let local_hash = hash_content(remote_content.as_bytes());
        let remote_hash = hash_content(remote_content.as_bytes());
        if let Ok(store) = self.storage.lock() {
            let _ = store.set_baselines(note_id, &local_hash, &remote_hash);
            let _ = store.update_sync_status(note_id, &SyncStatus::Synced);
            let _ = store.update_sync_state(note_id, &SyncState::Synced);
            let _ = store.add_sync_history(note_id, "pull", Some(&local_hash));
            let _ = store.save_snapshot(note_id, remote_content, &local_hash);
            let _ = store.complete_op(op_id);
        }
        self.emit(note_id, SyncStatus::Synced, None);
        tracing::info!("pull成功: {note_id}");
    }

    // ─── CreateRemote ───────────────────────────────────

    async fn execute_create_remote(&self, note_id: &str, content: &str, title: &str) {
        let op_id = self.enqueue("create_remote", note_id, None);
        if op_id.is_none() { return; }
        let op_id = op_id.unwrap();

        self.emit(note_id, SyncStatus::Syncing, None);
        self.set_sync_state(note_id, &SyncState::Executing);

        match self.provider.create(title, content).await {
            Ok(created_meta) => {
                // Extract remote_id from provider response
                let remote_id = match created_meta.remote_id.as_deref() {
                    Some(id) if !id.is_empty() => id,
                    _ => {
                        tracing::error!("create_remote: provider returned no remote_id for {note_id}");
                        self.set_sync_state(note_id, &SyncState::Error("no remote_id".into()));
                        if let Ok(store) = self.storage.lock() { let _ = store.fail_op(op_id, "no remote_id"); }
                        return;
                    }
                };
                let local_hash = hash_content(content.as_bytes());

                // Read back remote content for accurate remote_base_hash (P5)
                let remote_hash = match self.provider.read(remote_id).await {
                    Ok(read_output) => hash_content(read_output.content.as_bytes()),
                    Err(_) => local_hash.clone(), // fallback: assume same
                };

                if let Ok(store) = self.storage.lock() {
                    let _ = store.update_remote_id(note_id, remote_id);
                    let _ = store.set_baselines(note_id, &local_hash, &remote_hash);
                    let _ = store.update_url(note_id, &created_meta.url);
                    let _ = store.update_sync_status(note_id, &SyncStatus::Synced);
                    let _ = store.update_sync_state(note_id, &SyncState::Synced);
                    let _ = store.add_sync_history(note_id, "create_remote", Some(&local_hash));
                    let _ = store.save_snapshot(note_id, content, &local_hash);
                    let _ = store.complete_op(op_id);
                }
                self.emit(note_id, SyncStatus::Synced, Some(title.to_string()));
                tracing::info!("create_remote成功: {note_id} → {remote_id}");
            }
            Err(e) => {
                tracing::error!("create_remote failed: {note_id}: {e}");
                self.set_sync_state(note_id, &SyncState::Error(e.to_string()));
                self.emit(note_id, SyncStatus::Error(e.to_string()), None);
                if let Ok(store) = self.storage.lock() {
                    let _ = store.fail_op(op_id, &e.to_string());
                }
            }
        }
    }

    // ─── DeleteRemote ───────────────────────────────────

    async fn execute_delete_remote(&self, note_id: &str, remote_id: &str) {
        let op_id = self.enqueue("delete_remote", note_id, None);
        if op_id.is_none() { return; }
        let op_id = op_id.unwrap();

        match self.provider.delete(remote_id).await {
            Ok(()) => {
                tracing::info!("delete_remote成功: {note_id} (remote={remote_id})");
            }
            Err(e) => {
                if !e.is_not_found() {
                    tracing::error!("delete_remote failed: {note_id}: {e}");
                    self.set_sync_state(note_id, &SyncState::Error(e.to_string()));
                    if let Ok(store) = self.storage.lock() {
                        let _ = store.fail_op(op_id, &e.to_string());
                    }
                    return;
                }
                // Already deleted server-side — proceed
                tracing::debug!("delete_remote: already gone: {remote_id}");
            }
        }

        // Clean up: local file first (under write_guard), then meta, then DB last
        // DB record deleted last so crash recovery can resume from PendingDelete
        let local_path = self.get_local_path(note_id);
        if let Some(ref path) = local_path {
            let _guard = self.write_guard.guard(path);
            let _ = std::fs::remove_file(path);
        }
        let _ = std::fs::remove_file(meta_path(&self.workspace_dir, note_id));
        if let Ok(store) = self.storage.lock() {
            let _ = store.complete_op(op_id);
            let _ = store.delete_doc(note_id);
        }
    }

    // ─── RenameRemote ───────────────────────────────────

    async fn execute_rename_remote(&self, note_id: &str, new_title: &str) {
        let remote_id = match self.get_remote_id(note_id) {
            Some(id) => id,
            None => return,
        };

        let op_id = self.enqueue("rename_remote", note_id, Some(new_title));
        if op_id.is_none() { return; }
        let op_id = op_id.unwrap();

        match self.provider.rename(&remote_id, new_title).await {
            Ok(()) => {
                if let Ok(store) = self.storage.lock() {
                    let _ = store.update_title(note_id, new_title);
                    // Clear desired_title — rename fulfilled
                    let _ = store.clear_desired_title(note_id);
                    let _ = store.update_sync_state(note_id, &SyncState::Synced);
                    let _ = store.complete_op(op_id);
                }
                tracing::info!("rename_remote成功: {note_id} → {new_title}");
            }
            Err(e) => {
                tracing::error!("rename_remote failed: {note_id}: {e}");
                self.set_sync_state(note_id, &SyncState::Error(e.to_string()));
                if let Ok(store) = self.storage.lock() {
                    let _ = store.fail_op(op_id, &e.to_string());
                }
            }
        }
    }

    // ─── MarkConflict ───────────────────────────────────

    async fn execute_mark_conflict(&self, note_id: &str) {
        // Save a conflict copy
        if let Some(local_path) = self.get_local_path(note_id) {
            let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let stem = local_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("content");
            let conflict_path = local_path
                .with_file_name(format!("{stem}.conflict-{timestamp}.md"));
            if let Err(e) = tokio::fs::copy(&local_path, &conflict_path).await {
                tracing::error!("保存冲突文件失败: {e}");
            }
        }

        if let Ok(store) = self.storage.lock() {
            let _ = store.update_sync_status(note_id, &SyncStatus::BothModified);
            let _ = store.update_sync_state(note_id, &SyncState::Conflict);
            let _ = store.add_sync_history(note_id, "conflict", None);
        }
        self.emit(note_id, SyncStatus::BothModified, None);
    }

    // ─── ReclaimOrphan ──────────────────────────────────

    fn execute_reclaim_orphan(&self, note_id: &str, new_path: &Path) {
        let path_str = new_path.to_string_lossy().to_string();
        let folder = folder_of(&self.workspace_dir, new_path);
        if let Ok(store) = self.storage.lock() {
            let _ = store.update_local_path(note_id, &path_str);
            let _ = store.update_folder_path(note_id, &folder);
        }
        tracing::info!("reclaim_orphan: {note_id} → {}", new_path.display());
    }

    // ─── AdoptNewFile ───────────────────────────────────

    async fn execute_adopt_new_file(&self, path: &Path) {
        let raw = match tokio::fs::read(path).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("adopt: cannot read {}: {e}", path.display());
                return;
            }
        };
        let content = crate::util::decode_content(&raw);
        let title = extract_title(&content);
        let hash = hash_content(content.as_bytes());
        let folder = folder_of(&self.workspace_dir, path);
        let path_str = path.to_string_lossy().to_string();
        let note_id = new_note_id();

        // Check for orphan with matching hash
        let orphan = self.storage.lock()
            .ok()
            .and_then(|s| s.find_orphan_by_hash(&hash).ok().flatten());

        if let Some(orphan_doc) = orphan {
            // Reclaim instead of creating duplicate
            self.execute_reclaim_orphan(&orphan_doc.note_id, path);
            self.emit(&orphan_doc.note_id, SyncStatus::Synced, Some(orphan_doc.title));
            return;
        }

        // Create new remote doc
        let op_id = self.enqueue("adopt", &note_id, None);
        if op_id.is_none() { return; }
        let op_id = op_id.unwrap();

        match self.provider.create(&title, &content).await {
            Ok(created) => {
                let new_remote_id = match created.remote_id.as_deref() {
                    Some(id) if !id.is_empty() => id.to_string(),
                    _ => {
                        tracing::error!("adopt: provider returned no remote_id for new file");
                        if let Ok(store) = self.storage.lock() { let _ = store.fail_op(op_id, "no remote_id"); }
                        return;
                    }
                };
                let meta = DocMeta {
                    note_id: note_id.clone(),
                    remote_id: Some(new_remote_id.clone()),
                    doc_id: note_id.clone(),
                    title: title.clone(),
                    doc_type: created.doc_type,
                    url: created.url,
                    owner_name: created.owner_name,
                    created_at: created.created_at,
                    updated_at: created.updated_at,
                    local_path: Some(path_str),
                    content_hash: Some(hash.clone()),
                    sync_status: SyncStatus::Synced,
                    folder_path: folder,
                    file_size: None,
                    word_count: None,
                    sync_state: SyncState::Synced,
                    title_mode: "manual".to_string(),
                    desired_title: None,
                    desired_path: None,
                };
                if let Ok(store) = self.storage.lock() {
                    let _ = store.upsert_doc(&meta);
                    let _ = store.add_sync_history(&note_id, "adopt", Some(&hash));
                    let _ = store.save_snapshot(&note_id, &content, &hash);
                    let _ = store.complete_op(op_id);
                }
                self.emit(&note_id, SyncStatus::Synced, Some(title));
                tracing::info!("adopt成功: {note_id} → {new_remote_id}");
            }
            Err(e) => {
                tracing::warn!("adopt failed: {}: {e}", path.display());
                if let Ok(store) = self.storage.lock() { let _ = store.fail_op(op_id, &e.to_string()); }
            }
        }
    }

    // ─── MarkFileMissing ────────────────────────────────

    fn execute_mark_file_missing(&self, note_id: &str) {
        if let Ok(store) = self.storage.lock() {
            let _ = store.update_sync_state(note_id, &SyncState::FileMissing);
        }
        tracing::info!("file_missing: {note_id}");
    }

    // ─── DeriveTitleRename ──────────────────────────────

    async fn execute_derive_title_rename(&self, note_id: &str, new_title: &str) {
        let local_path = match self.get_local_path(note_id) {
            Some(p) => p,
            None => return,
        };

        // Rename local file under write guard
        let folder = folder_of(&self.workspace_dir, &local_path);
        let new_path = titled_content_path_in(&self.workspace_dir, &folder, new_title);

        if new_path != local_path && !new_path.exists() {
            let _guard = self.write_guard.guard(&local_path);
            let _guard2 = self.write_guard.guard(&new_path);
            if let Err(e) = std::fs::rename(&local_path, &new_path) {
                tracing::warn!("derive_title_rename: local rename failed: {e}");
                return;
            }
            let new_path_str = new_path.to_string_lossy().to_string();
            if let Ok(store) = self.storage.lock() {
                let _ = store.update_local_path(note_id, &new_path_str);
            }
        }

        // Update title + set title_mode to manual after first derivation
        if let Ok(store) = self.storage.lock() {
            let _ = store.update_title(note_id, new_title);
            let _ = store.update_title_mode(note_id, "manual");
        }

        // Rename on remote
        self.execute_rename_remote(note_id, new_title).await;
        tracing::info!("derive_title_rename成功: {note_id} → {new_title}");
    }

    // ─── Recreate ───────────────────────────────────────

    async fn execute_recreate(&self, note_id: &str, title: &str, content: &str, local_hash: &str) {
        match self.provider.create(title, content).await {
            Ok(new_meta) => {
                let new_remote_id = match new_meta.remote_id.as_deref() {
                    Some(id) if !id.is_empty() => id.to_string(),
                    _ => {
                        tracing::error!("recreate: provider returned no remote_id for {note_id}");
                        return;
                    }
                };

                // Read back for remote baseline
                let remote_hash = match self.provider.read(&new_remote_id).await {
                    Ok(read_output) => hash_content(read_output.content.as_bytes()),
                    Err(_) => local_hash.to_string(), // fallback
                };

                if let Ok(store) = self.storage.lock() {
                    let _ = store.update_remote_id(note_id, &new_remote_id);
                    let _ = store.set_baselines(note_id, local_hash, &remote_hash);
                    let _ = store.update_url(note_id, &new_meta.url);
                    let _ = store.update_sync_status(note_id, &SyncStatus::Synced);
                    let _ = store.update_sync_state(note_id, &SyncState::Synced);
                    let _ = store.add_sync_history(note_id, "recreate", Some(local_hash));
                    let _ = store.save_snapshot(note_id, content, local_hash);
                }
                self.emit(note_id, SyncStatus::Synced, Some(title.to_string()));
                tracing::info!("recreate成功: {note_id} → {new_remote_id}");
            }
            Err(e) => {
                tracing::error!("recreate failed: {note_id}: {e}");
                self.set_sync_state(note_id, &SyncState::Error(e.to_string()));
                self.emit(note_id, SyncStatus::Error(e.to_string()), None);
            }
        }
    }

    // ─── Helpers ────────────────────────────────────────

    fn get_remote_id(&self, note_id: &str) -> Option<String> {
        self.storage.lock().ok()
            .and_then(|s| s.get_note(note_id).ok().flatten())
            .and_then(|n| n.remote_id)
    }

    fn get_local_path(&self, note_id: &str) -> Option<PathBuf> {
        self.storage.lock().ok()
            .and_then(|s| s.get_note(note_id).ok().flatten())
            .and_then(|n| n.local_path)
            .map(PathBuf::from)
    }

    fn get_title(&self, note_id: &str) -> Option<String> {
        self.storage.lock().ok()
            .and_then(|s| s.get_note(note_id).ok().flatten())
            .map(|n| n.title)
    }

    fn set_sync_state(&self, note_id: &str, state: &SyncState) {
        if let Ok(store) = self.storage.lock() {
            let _ = store.update_sync_state(note_id, state);
        }
    }

    fn enqueue(&self, op_kind: &str, note_id: &str, payload: Option<&str>) -> Option<i64> {
        self.storage.lock().ok()
            .and_then(|s| s.enqueue_op(note_id, op_kind, payload, None).ok())
    }

    fn emit(&self, note_id: &str, status: SyncStatus, title: Option<String>) {
        let _ = self.status_tx.send(SyncStatusUpdate {
            note_id: note_id.to_string(),
            status,
            title,
            new_remote_id: None,
        });
    }
}
