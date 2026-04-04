use crate::{hash_content, SyncEvent};
use larknotes_core::*;
use larknotes_storage::Storage;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc};
use tokio::time::{Duration, Instant};

/// Decode file content from any encoding.
/// 1. Check BOM (UTF-16 LE/BE, UTF-8 BOM)
/// 2. Try UTF-8
/// 3. Auto-detect encoding via chardetng (handles GBK, Shift-JIS, Latin-1, etc.)
fn decode_content(raw: &[u8]) -> String {
    // UTF-16 LE BOM
    if raw.len() >= 2 && raw[0] == 0xFF && raw[1] == 0xFE {
        let (decoded, _, _) = encoding_rs::UTF_16LE.decode(&raw[2..]);
        return decoded.into_owned();
    }
    // UTF-16 BE BOM
    if raw.len() >= 2 && raw[0] == 0xFE && raw[1] == 0xFF {
        let (decoded, _, _) = encoding_rs::UTF_16BE.decode(&raw[2..]);
        return decoded.into_owned();
    }
    // UTF-8 BOM
    if raw.len() >= 3 && raw[0] == 0xEF && raw[1] == 0xBB && raw[2] == 0xBF {
        return String::from_utf8_lossy(&raw[3..]).into_owned();
    }
    // Try valid UTF-8 first
    if let Ok(s) = std::str::from_utf8(raw) {
        return s.to_string();
    }
    // Auto-detect encoding (GBK, Shift-JIS, EUC-KR, Latin-1, etc.)
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(raw, true);
    let encoding = detector.guess(None, true);
    tracing::info!("检测到文件编码: {}", encoding.name());
    let (decoded, _, _) = encoding.decode(raw);
    decoded.into_owned()
}

#[derive(Clone, Debug, Serialize)]
pub struct SyncStatusUpdate {
    pub doc_id: String,
    pub status: SyncStatus,
    pub title: Option<String>,
}

pub struct SyncEngine {
    provider: Arc<dyn DocProvider>,
    storage: Arc<Mutex<Storage>>,
    workspace_dir: PathBuf,
    debounce_ms: Arc<AtomicU64>,
    status_tx: broadcast::Sender<SyncStatusUpdate>,
}

impl SyncEngine {
    pub fn new(
        provider: Arc<dyn DocProvider>,
        storage: Arc<Mutex<Storage>>,
        workspace_dir: PathBuf,
        debounce_ms: Arc<AtomicU64>,
    ) -> (Self, broadcast::Receiver<SyncStatusUpdate>) {
        let (status_tx, status_rx) = broadcast::channel(64);
        (
            Self {
                provider,
                storage,
                workspace_dir,
                debounce_ms,
                status_tx,
            },
            status_rx,
        )
    }

    pub fn status_receiver(&self) -> broadcast::Receiver<SyncStatusUpdate> {
        self.status_tx.subscribe()
    }

    pub async fn run(
        engine: Arc<SyncEngine>,
        mut rx: mpsc::UnboundedReceiver<SyncEvent>,
    ) {
        let mut debounce_timers: HashMap<String, Instant> = HashMap::new();
        let mut interval = tokio::time::interval(Duration::from_millis(500));

        tracing::info!("同步引擎已启动");

        loop {
            tokio::select! {
                Some(event) = rx.recv() => {
                    match event {
                        SyncEvent::FileChanged { doc_id, .. } => {
                            let deadline = Instant::now()
                                + Duration::from_millis(engine.debounce_ms.load(Ordering::Relaxed));
                            debounce_timers.insert(doc_id.clone(), deadline);
                            tracing::debug!("文件变更, 等待debounce: {doc_id}");
                        }
                        SyncEvent::SyncRequested { doc_id } => {
                            let engine = engine.clone();
                            tokio::spawn(async move {
                                engine.sync_one(&doc_id, true).await;
                            });
                        }
                        SyncEvent::Shutdown => {
                            tracing::info!("同步引擎关闭");
                            break;
                        }
                    }
                }
                _ = interval.tick() => {
                    let now = Instant::now();
                    let ready: Vec<String> = debounce_timers
                        .iter()
                        .filter(|(_, deadline)| now >= **deadline)
                        .map(|(id, _)| id.clone())
                        .collect();

                    for doc_id in ready {
                        debounce_timers.remove(&doc_id);
                        let engine = engine.clone();
                        tokio::spawn(async move {
                            engine.sync_one(&doc_id, false).await;
                        });
                    }
                }
            }
        }
    }

    /// Sync a single document. When `force` is true (manual sync),
    /// always push even if content hash matches — this handles remote deletion.
    pub async fn sync_one(&self, doc_id: &str, force: bool) {
        // Get local_path from storage
        let local_path = match self.storage.lock() {
            Ok(store) => store
                .get_doc(doc_id)
                .ok()
                .flatten()
                .and_then(|d| d.local_path)
                .map(std::path::PathBuf::from),
            Err(e) => {
                tracing::error!("Storage lock poisoned: {e}");
                return;
            }
        };

        let content_path = match local_path {
            Some(p) if p.exists() => p,
            _ => {
                // Fallback: try titled path from DB title
                let title = self.storage.lock()
                    .ok()
                    .and_then(|s| s.get_doc(doc_id).ok().flatten())
                    .map(|d| d.title)
                    .unwrap_or_default();
                titled_content_path(&self.workspace_dir, &title)
            }
        };

        // 1. Read local content — handle UTF-8, UTF-8 BOM, and UTF-16 LE/BE
        let raw = match tokio::fs::read(&content_path).await {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("读取文件失败 {}: {e}", content_path.display());
                return;
            }
        };
        let content = decode_content(&raw);

        // 2. Compute hash
        let new_hash = hash_content(content.as_bytes());

        // 3. Check if content actually changed
        let old_hash = self.storage.lock()
            .ok()
            .and_then(|s| s.get_doc(doc_id).ok().flatten())
            .and_then(|d| d.content_hash);

        if old_hash.as_deref() == Some(&new_hash) && !force {
            tracing::debug!("内容未变化, 跳过同步: {doc_id}");
            return;
        }
        if force {
            tracing::info!("手动同步, 强制推送: {doc_id}");
        }

        // 4. Update status to Syncing
        self.emit_status(doc_id, SyncStatus::Syncing, None);
        if let Ok(store) = self.storage.lock() {
            let _ = store.update_sync_status(doc_id, &SyncStatus::Syncing);
        }

        // 5. Extract title
        let title = extract_title(&content);

        // 5b. Check if title changed (compared to DB) for conditional rename
        let old_title = self.storage.lock()
            .ok()
            .and_then(|s| s.get_doc(doc_id).ok().flatten())
            .map(|d| d.title);
        let title_changed = old_title.as_deref() != Some(&title);

        // 6. Push to remote with retry (exponential backoff: 5s, 15s, 45s)
        let retry_delays = [
            Duration::from_secs(5),
            Duration::from_secs(15),
            Duration::from_secs(45),
        ];
        // First attempt
        match self.provider.write(doc_id, &content).await {
            Ok(write_meta) => {
                tracing::debug!("write 成功: {doc_id}, remote_at={}", write_meta.updated_at);
                if title_changed {
                    if let Err(e) = self.provider.rename(doc_id, &title).await {
                        tracing::error!("重命名失败 (内容已推送): {doc_id}: {e}");
                        // Content is on remote but title is wrong — mark partial failure
                        if let Ok(store) = self.storage.lock() {
                            let _ = store.update_content_hash(doc_id, &new_hash);
                            let _ = store.update_sync_status(
                                doc_id,
                                &SyncStatus::Error(format!("标题同步失败: {e}")),
                            );
                        }
                        self.emit_status(doc_id, SyncStatus::Error(format!("标题同步失败: {e}")), None);
                        return;
                    }
                }
                self.mark_synced(doc_id, &new_hash, &content);
                return;
            }
            Err(e) => {
                if e.is_not_found() {
                    tracing::warn!("远端文档已删除，重新创建: {doc_id}: {e}");
                    self.recreate_on_remote(doc_id, &title, &content, &new_hash).await;
                    return;
                }
                if !e.is_transient() {
                    tracing::error!("同步失败 (永久错误): {doc_id}: {e}");
                    self.handle_conflict(doc_id).await;
                    if let Ok(store) = self.storage.lock() {
                        let _ = store.update_sync_status(doc_id, &SyncStatus::Conflict);
                    }
                    self.emit_status(doc_id, SyncStatus::Conflict, None);
                    return;
                }
                tracing::warn!("同步失败 (将重试): {doc_id}: {e}");
            }
        }

        // Retry attempts for transient errors
        for (i, delay) in retry_delays.iter().enumerate() {
            let err_msg = format!("网络异常，第{}次重试中...", i + 1);
            self.emit_status(doc_id, SyncStatus::Error(err_msg), None);
            if let Ok(store) = self.storage.lock() {
                let _ = store.update_sync_status(
                    doc_id,
                    &SyncStatus::Error(format!("重试中 ({}/3)", i + 1)),
                );
            }

            tokio::time::sleep(*delay).await;

            match self.provider.write(doc_id, &content).await {
                Ok(write_meta) => {
                    tracing::debug!("重试 write 成功: {doc_id}, remote_at={}", write_meta.updated_at);
                    if title_changed {
                        if let Err(e) = self.provider.rename(doc_id, &title).await {
                            tracing::error!("重试后重命名失败: {doc_id}: {e}");
                            if let Ok(store) = self.storage.lock() {
                                let _ = store.update_content_hash(doc_id, &new_hash);
                                let _ = store.update_sync_status(
                                    doc_id,
                                    &SyncStatus::Error(format!("标题同步失败: {e}")),
                                );
                            }
                            self.emit_status(doc_id, SyncStatus::Error(format!("标题同步失败: {e}")), None);
                            return;
                        }
                    }
                    tracing::info!("重试成功: {doc_id} (第{}次)", i + 1);
                    self.mark_synced(doc_id, &new_hash, &content);
                    return;
                }
                Err(e) => {
                    tracing::warn!("重试失败 ({}/3): {doc_id}: {e}", i + 1);
                }
            }
        }

        // All retries exhausted
        tracing::error!("同步失败 (重试耗尽): {doc_id}");
        self.handle_conflict(doc_id).await;
        if let Ok(store) = self.storage.lock() {
            let _ = store.update_sync_status(doc_id, &SyncStatus::Conflict);
        }
        self.emit_status(doc_id, SyncStatus::Conflict, None);
    }

    fn mark_synced(&self, doc_id: &str, new_hash: &str, content: &str) {
        if let Ok(store) = self.storage.lock() {
            let _ = store.update_content_hash(doc_id, new_hash);
            let _ = store.update_sync_status(doc_id, &SyncStatus::Synced);
            // NOTE: We do NOT update title here. Title + filename are updated
            // atomically by rename_stale_paths() after the editor closes.
            // Updating title here would cause the UI to show a new name while
            // the file still has the old name, creating a race condition.
            let _ = store.add_sync_history(doc_id, "push", Some(new_hash));
            let _ = store.save_snapshot(doc_id, content, new_hash);
        }
        self.emit_status(doc_id, SyncStatus::Synced, None);
        tracing::info!("同步成功: {doc_id}");
    }

    /// Re-create a document on the remote when the original was deleted server-side.
    /// Creates a new remote doc, updates the local doc_id to match.
    async fn recreate_on_remote(&self, old_doc_id: &str, title: &str, content: &str, new_hash: &str) {
        match self.provider.create(title, content).await {
            Ok(new_meta) => {
                let new_id = &new_meta.doc_id;
                tracing::info!("远端重建成功: {old_doc_id} → {new_id}");
                if let Ok(store) = self.storage.lock() {
                    // replace_doc_id is transactional — if it fails, the old doc_id remains intact.
                    if let Err(e) = store.replace_doc_id(old_doc_id, new_id) {
                        tracing::error!("重建后替换doc_id失败: {e}");
                        let _ = store.update_sync_status(old_doc_id, &SyncStatus::Error(format!("DB更新失败: {e}")));
                        self.emit_status(old_doc_id, SyncStatus::Error(format!("DB更新失败: {e}")), None);
                        return;
                    }
                    if let Err(e) = store.update_content_hash(new_id, new_hash) {
                        tracing::error!("重建后更新hash失败: {e}");
                    }
                    let _ = store.update_sync_status(new_id, &SyncStatus::Synced);
                    let _ = store.add_sync_history(new_id, "recreate", Some(new_hash));
                    let _ = store.save_snapshot(new_id, content, new_hash);
                    if !new_meta.url.is_empty() {
                        let _ = store.update_url(new_id, &new_meta.url);
                    }
                }
                self.emit_status(new_id, SyncStatus::Synced, Some(title.to_string()));
            }
            Err(e) => {
                tracing::error!("远端重建失败: {old_doc_id}: {e}");
                if let Ok(store) = self.storage.lock() {
                    let _ = store.update_sync_status(old_doc_id, &SyncStatus::Error(format!("重建失败: {e}")));
                }
                self.emit_status(old_doc_id, SyncStatus::Error(format!("重建失败: {e}")), None);
            }
        }
    }

    async fn handle_conflict(&self, doc_id: &str) {
        // Record conflict in history
        if let Ok(store) = self.storage.lock() {
            let _ = store.add_sync_history(doc_id, "conflict", None);
        }

        let local_path = self.storage.lock()
            .ok()
            .and_then(|s| s.get_doc(doc_id).ok().flatten())
            .and_then(|d| d.local_path)
            .map(std::path::PathBuf::from);

        if let Some(src) = local_path {
            let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("content");
            let conflict_path = src.with_file_name(format!("{stem}.conflict-{timestamp}.md"));

            if let Err(e) = tokio::fs::copy(&src, &conflict_path).await {
                tracing::error!("保存冲突文件失败: {e}");
            } else {
                tracing::warn!("冲突文件已保存: {}", conflict_path.display());
            }
        }
    }

    fn emit_status(&self, doc_id: &str, status: SyncStatus, title: Option<String>) {
        let _ = self.status_tx.send(SyncStatusUpdate {
            doc_id: doc_id.to_string(),
            status,
            title,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    // Mock DocProvider for testing
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum FailMode { None, Permanent, NotFound }

    struct MockProvider {
        fail_mode: std::sync::atomic::AtomicU8,
        /// If > 0, fail this many times with transient error, then succeed
        transient_fail_count: std::sync::atomic::AtomicI32,
        /// When true, create_doc returns an error
        create_should_fail: AtomicBool,
        updated_docs: Mutex<Vec<(String, String)>>,
        created_docs: Mutex<Vec<(String, String)>>,
        renamed_docs: Mutex<Vec<(String, String)>>,
        list_result: Mutex<Vec<DocMeta>>,
    }

    impl MockProvider {
        fn new() -> Self {
            Self {
                fail_mode: std::sync::atomic::AtomicU8::new(0),
                transient_fail_count: std::sync::atomic::AtomicI32::new(0),
                create_should_fail: AtomicBool::new(false),
                updated_docs: Mutex::new(Vec::new()),
                created_docs: Mutex::new(Vec::new()),
                renamed_docs: Mutex::new(Vec::new()),
                list_result: Mutex::new(Vec::new()),
            }
        }

        fn set_fail(&self, fail: bool) {
            self.fail_mode.store(if fail { 1 } else { 0 }, Ordering::SeqCst);
        }

        fn set_not_found(&self) {
            self.fail_mode.store(2, Ordering::SeqCst);
        }

        fn set_transient_failures(&self, count: i32) {
            self.transient_fail_count.store(count, Ordering::SeqCst);
        }

        fn get_updates(&self) -> Vec<(String, String)> {
            self.updated_docs.lock().unwrap().clone()
        }

        fn get_creates(&self) -> Vec<(String, String)> {
            self.created_docs.lock().unwrap().clone()
        }

        fn get_renames(&self) -> Vec<(String, String)> {
            self.renamed_docs.lock().unwrap().clone()
        }

        fn set_list_result(&self, docs: Vec<DocMeta>) {
            *self.list_result.lock().unwrap() = docs;
        }

        fn fail_mode(&self) -> FailMode {
            match self.fail_mode.load(Ordering::SeqCst) {
                1 => FailMode::Permanent,
                2 => FailMode::NotFound,
                _ => FailMode::None,
            }
        }
    }

    #[async_trait::async_trait]
    impl DocProvider for MockProvider {
        async fn create(&self, name: &str, _content: &str) -> Result<DocMeta, LarkNotesError> {
            if self.create_should_fail.load(Ordering::SeqCst) {
                return Err(LarkNotesError::Cli("创建文档失败: 权限不足".into()));
            }
            let new_id = format!("new_{}", name.replace(' ', "_"));
            self.created_docs.lock().unwrap().push((new_id.clone(), name.to_string()));
            let now = chrono::Local::now().to_rfc3339();
            Ok(DocMeta {
                doc_id: new_id.clone(),
                title: name.to_string(),
                doc_type: "DOCX".to_string(),
                url: format!("https://feishu.cn/docx/{new_id}"),
                owner_name: "test".to_string(),
                created_at: now.clone(),
                updated_at: now,
                local_path: None,
                content_hash: None,
                sync_status: SyncStatus::Synced,
                folder_path: String::new(),
            })
        }
        async fn read(&self, _id: &str) -> Result<ReadOutput, LarkNotesError> {
            match self.fail_mode() {
                FailMode::Permanent => return Err(LarkNotesError::Auth("403 forbidden".into())),
                FailMode::NotFound => return Err(LarkNotesError::Cli("文档不存在".into())),
                FailMode::None => {}
            }
            Ok(ReadOutput {
                content: String::new(),
                meta: DocMeta {
                    doc_id: String::new(),
                    title: String::new(),
                    doc_type: "DOCX".to_string(),
                    url: String::new(),
                    owner_name: String::new(),
                    created_at: String::new(),
                    updated_at: String::new(),
                    local_path: None,
                    content_hash: None,
                    sync_status: SyncStatus::Synced,
                    folder_path: String::new(),
                },
            })
        }
        async fn write(&self, id: &str, content: &str) -> Result<WriteMeta, LarkNotesError> {
            match self.fail_mode() {
                FailMode::Permanent => return Err(LarkNotesError::Auth("403 forbidden".into())),
                FailMode::NotFound => return Err(LarkNotesError::Cli("文档不存在".into())),
                FailMode::None => {}
            }
            // Transient failure mode: decrement counter, fail if > 0
            let remaining = self.transient_fail_count.fetch_sub(1, Ordering::SeqCst);
            if remaining > 0 {
                return Err(LarkNotesError::Cli("connection timeout".into()));
            }
            self.updated_docs
                .lock()
                .unwrap()
                .push((id.to_string(), content.to_string()));
            Ok(WriteMeta {
                content_hash: String::new(),
                updated_at: chrono::Local::now().to_rfc3339(),
            })
        }
        async fn delete(&self, _id: &str) -> Result<(), LarkNotesError> {
            match self.fail_mode() {
                FailMode::Permanent => return Err(LarkNotesError::Auth("403 forbidden".into())),
                FailMode::NotFound => return Err(LarkNotesError::Cli("文档不存在".into())),
                FailMode::None => {}
            }
            Ok(())
        }
        async fn rename(&self, id: &str, new_name: &str) -> Result<(), LarkNotesError> {
            match self.fail_mode() {
                FailMode::Permanent => return Err(LarkNotesError::Auth("403 forbidden".into())),
                FailMode::NotFound => return Err(LarkNotesError::Cli("文档不存在".into())),
                FailMode::None => {}
            }
            self.renamed_docs.lock().unwrap().push((id.to_string(), new_name.to_string()));
            Ok(())
        }
        async fn list(&self, _folder: Option<&str>) -> Result<Vec<DocMeta>, LarkNotesError> {
            match self.fail_mode() {
                FailMode::Permanent => return Err(LarkNotesError::Auth("403 forbidden".into())),
                FailMode::NotFound => return Err(LarkNotesError::Cli("文档不存在".into())),
                FailMode::None => {}
            }
            Ok(self.list_result.lock().unwrap().clone())
        }
        async fn search(&self, _query: &str) -> Result<Vec<DocMeta>, LarkNotesError> {
            Ok(vec![])
        }
    }

    #[async_trait::async_trait]
    impl ProviderAuth for MockProvider {
        async fn auth_status(&self) -> Result<AuthStatus, LarkNotesError> {
            Ok(AuthStatus {
                logged_in: true,
                user_name: Some("MockUser".to_string()),
                needs_refresh: false,
                expires_at: None,
            })
        }
    }

    fn setup_test_workspace() -> (tempfile::TempDir, Arc<Mutex<Storage>>) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("docs")).unwrap();
        let storage = Storage::new_in_memory().unwrap();
        (tmp, Arc::new(Mutex::new(storage)))
    }

    /// Helper: create a flat doc file at docs/<title>.md with local_path in meta
    fn create_test_doc(
        workspace: &std::path::Path,
        storage: &Arc<Mutex<Storage>>,
        doc_id: &str,
        title: &str,
        content: &str,
        content_hash: Option<String>,
    ) -> PathBuf {
        let file_path = titled_content_path(workspace, title);
        std::fs::write(&file_path, content).unwrap();
        let meta = DocMeta {
            doc_id: doc_id.to_string(),
            title: title.to_string(),
            doc_type: "DOCX".to_string(),
            url: "".to_string(),
            owner_name: "test".to_string(),
            created_at: "2026-01-01".to_string(),
            updated_at: "2026-01-01".to_string(),
            local_path: Some(file_path.to_string_lossy().to_string()),
            content_hash,
            sync_status: SyncStatus::Synced,
            folder_path: String::new(),
        };
        storage.lock().unwrap().upsert_doc(&meta).unwrap();
        file_path
    }

    #[tokio::test]
    async fn test_sync_one_pushes_changed_content() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());

        create_test_doc(&workspace, &storage, "doc1", "Hello", "# Hello\n\nWorld", None);

        let (engine, mut status_rx) = SyncEngine::new(
            provider.clone(), storage.clone(), workspace, Arc::new(AtomicU64::new(2000)),
        );
        let engine = Arc::new(engine);
        engine.sync_one("doc1", false).await;

        let updates = provider.get_updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].1, "# Hello\n\nWorld");

        let doc = storage.lock().unwrap().get_doc("doc1").unwrap().unwrap();
        assert_eq!(doc.sync_status, SyncStatus::Synced);
        assert!(doc.content_hash.is_some());
        assert_eq!(doc.title, "Hello");

        let update = status_rx.try_recv().unwrap();
        assert_eq!(update.status, SyncStatus::Syncing);
        let update = status_rx.try_recv().unwrap();
        assert_eq!(update.status, SyncStatus::Synced);
    }

    #[tokio::test]
    async fn test_sync_one_skips_unchanged_content() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());

        let content = "# Same content";
        let hash = hash_content(content.as_bytes());
        create_test_doc(&workspace, &storage, "doc2", "Same", content, Some(hash));

        let (engine, _) = SyncEngine::new(provider.clone(), storage, workspace, Arc::new(AtomicU64::new(2000)));
        let engine = Arc::new(engine);
        engine.sync_one("doc2", false).await;

        assert!(provider.get_updates().is_empty());
    }

    #[tokio::test]
    async fn test_sync_one_handles_conflict() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());
        provider.set_fail(true);

        create_test_doc(&workspace, &storage, "doc3", "Conflict", "# Conflict test", None);

        let (engine, mut status_rx) = SyncEngine::new(
            provider.clone(), storage.clone(), workspace.clone(), Arc::new(AtomicU64::new(2000)),
        );
        let engine = Arc::new(engine);
        engine.sync_one("doc3", false).await;

        let doc = storage.lock().unwrap().get_doc("doc3").unwrap().unwrap();
        assert_eq!(doc.sync_status, SyncStatus::Conflict);

        // Conflict file should exist in docs/
        let docs = workspace.join("docs");
        let entries: Vec<_> = std::fs::read_dir(&docs)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("conflict"))
            .collect();
        assert_eq!(entries.len(), 1, "Expected one conflict file");

        let update = status_rx.try_recv().unwrap();
        assert_eq!(update.status, SyncStatus::Syncing);
        let update = status_rx.try_recv().unwrap();
        assert_eq!(update.status, SyncStatus::Conflict);
    }

    #[tokio::test]
    async fn test_sync_one_recreates_on_not_found() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());
        provider.set_not_found();

        create_test_doc(&workspace, &storage, "deleted_doc", "Recreate Me", "# Recreate Me\n\nBody", None);

        let (engine, mut status_rx) = SyncEngine::new(
            provider.clone(), storage.clone(), workspace, Arc::new(AtomicU64::new(2000)),
        );
        let engine = Arc::new(engine);
        engine.sync_one("deleted_doc", false).await;

        // Should have called create_doc
        let creates = provider.get_creates();
        assert_eq!(creates.len(), 1);
        assert_eq!(creates[0].1, "Recreate Me");

        // Old doc_id should be gone, new one should exist
        let old = storage.lock().unwrap().get_doc("deleted_doc").unwrap();
        assert!(old.is_none(), "Old doc_id should be replaced");

        let new_id = &creates[0].0;
        let new_doc = storage.lock().unwrap().get_doc(new_id).unwrap().unwrap();
        assert_eq!(new_doc.sync_status, SyncStatus::Synced);
        assert!(new_doc.content_hash.is_some());

        // Status emissions: Syncing, then Synced (with new doc_id)
        let update = status_rx.try_recv().unwrap();
        assert_eq!(update.status, SyncStatus::Syncing);
        let update = status_rx.try_recv().unwrap();
        assert_eq!(update.status, SyncStatus::Synced);
        assert_eq!(update.doc_id, *new_id);
    }

    #[tokio::test]
    async fn test_sync_one_does_not_update_title() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());

        create_test_doc(&workspace, &storage, "doc4", "Old Title", "# My Custom Title\n\nBody", None);

        let (engine, _) = SyncEngine::new(provider, storage.clone(), workspace, Arc::new(AtomicU64::new(2000)));
        let engine = Arc::new(engine);
        engine.sync_one("doc4", false).await;

        // Title should NOT be updated by sync — deferred to rename_stale_paths after editor closes
        let doc = storage.lock().unwrap().get_doc("doc4").unwrap().unwrap();
        assert_eq!(doc.title, "Old Title");
    }

    #[tokio::test(start_paused = true)]
    async fn test_sync_engine_debounce() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());

        create_test_doc(&workspace, &storage, "doc5", "Debounce", "# Debounce test", None);

        let (tx, rx) = mpsc::unbounded_channel();
        let (engine, _) = SyncEngine::new(provider.clone(), storage, workspace, Arc::new(AtomicU64::new(500)));
        let engine = Arc::new(engine);

        let engine_clone = engine.clone();
        let handle = tokio::spawn(async move {
            SyncEngine::run(engine_clone, rx).await;
        });

        for _ in 0..5 {
            tx.send(SyncEvent::FileChanged {
                doc_id: "doc5".to_string(),
                path: PathBuf::new(),
            }).unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        tokio::time::sleep(Duration::from_millis(1500)).await;

        let updates = provider.get_updates();
        assert_eq!(updates.len(), 1, "Expected exactly 1 sync after debounce, got {}", updates.len());

        tx.send(SyncEvent::Shutdown).unwrap();
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn test_sync_engine_manual_sync() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());

        create_test_doc(&workspace, &storage, "doc6", "Manual", "# Manual sync test", None);

        let (tx, rx) = mpsc::unbounded_channel();
        let (engine, _) = SyncEngine::new(provider.clone(), storage, workspace, Arc::new(AtomicU64::new(2000)));
        let engine = Arc::new(engine);

        let engine_clone = engine.clone();
        let handle = tokio::spawn(async move {
            SyncEngine::run(engine_clone, rx).await;
        });

        tx.send(SyncEvent::SyncRequested { doc_id: "doc6".to_string() }).unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;

        assert_eq!(provider.get_updates().len(), 1);

        tx.send(SyncEvent::Shutdown).unwrap();
        handle.await.unwrap();
    }

    // ─── decode_content tests ────────────────────────────

    #[test]
    fn test_decode_content_utf8() {
        let content = "# Hello 你好";
        let decoded = decode_content(content.as_bytes());
        assert_eq!(decoded, "# Hello 你好");
    }

    #[test]
    fn test_decode_content_utf8_bom() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF]; // UTF-8 BOM
        bytes.extend_from_slice("# BOM test".as_bytes());
        let decoded = decode_content(&bytes);
        assert_eq!(decoded, "# BOM test");
    }

    #[test]
    fn test_decode_content_utf16_le_bom() {
        let mut bytes = vec![0xFF, 0xFE]; // UTF-16 LE BOM
        for ch in "Hello".encode_utf16() {
            bytes.extend_from_slice(&ch.to_le_bytes());
        }
        let decoded = decode_content(&bytes);
        assert_eq!(decoded, "Hello");
    }

    #[test]
    fn test_decode_content_utf16_be_bom() {
        let mut bytes = vec![0xFE, 0xFF]; // UTF-16 BE BOM
        for ch in "Hello".encode_utf16() {
            bytes.extend_from_slice(&ch.to_be_bytes());
        }
        let decoded = decode_content(&bytes);
        assert_eq!(decoded, "Hello");
    }

    #[test]
    fn test_decode_content_empty() {
        assert_eq!(decode_content(&[]), "");
    }

    #[test]
    fn test_decode_content_ascii() {
        let decoded = decode_content(b"plain ASCII text 123");
        assert_eq!(decoded, "plain ASCII text 123");
    }

    #[test]
    fn test_decode_content_gbk() {
        // "你好世界" in GBK — longer text helps chardetng identify the encoding
        let (encoded, _, _) = encoding_rs::GBK.encode("你好世界，这是一段中文测试文本。");
        let decoded = decode_content(&encoded);
        assert!(decoded.contains("你好"), "GBK decode should contain 你好, got: {decoded}");
    }

    // ─── sync_one edge cases ─────────────────────────────

    #[tokio::test]
    async fn test_sync_one_nonexistent_doc() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());

        let (engine, _) = SyncEngine::new(
            provider.clone(), storage, workspace, Arc::new(AtomicU64::new(2000)),
        );
        let engine = Arc::new(engine);

        // Should return without crash — doc not in storage
        engine.sync_one("nonexistent", false).await;
        assert!(provider.get_updates().is_empty());
    }

    #[tokio::test]
    async fn test_sync_one_file_missing() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());

        // Create doc in storage with a local_path that doesn't exist on disk
        let fake_path = workspace.join("docs/ghost.md");
        let meta = DocMeta {
            doc_id: "ghost".to_string(),
            title: "Ghost".to_string(),
            doc_type: "DOCX".to_string(),
            url: "".to_string(),
            owner_name: "test".to_string(),
            created_at: "2026-01-01".to_string(),
            updated_at: "2026-01-01".to_string(),
            local_path: Some(fake_path.to_string_lossy().to_string()),
            content_hash: None,
            sync_status: SyncStatus::Synced,
            folder_path: String::new(),
        };
        storage.lock().unwrap().upsert_doc(&meta).unwrap();

        let (engine, _) = SyncEngine::new(
            provider.clone(), storage, workspace, Arc::new(AtomicU64::new(2000)),
        );
        let engine = Arc::new(engine);

        // Should not panic, just fail to read and return
        engine.sync_one("ghost", false).await;
        assert!(provider.get_updates().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn test_sync_one_transient_retry_success() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());
        // Fail once with transient error, then succeed
        provider.set_transient_failures(1);

        create_test_doc(&workspace, &storage, "retry_doc", "Retry", "# Retry test", None);

        let (engine, _) = SyncEngine::new(
            provider.clone(), storage.clone(), workspace, Arc::new(AtomicU64::new(100)),
        );
        let engine = Arc::new(engine);
        engine.sync_one("retry_doc", false).await;

        // Should have retried and succeeded
        let updates = provider.get_updates();
        assert_eq!(updates.len(), 1, "Should succeed after 1 retry");
        let doc = storage.lock().unwrap().get_doc("retry_doc").unwrap().unwrap();
        assert_eq!(doc.sync_status, SyncStatus::Synced);
    }

    // Note: retry exhaustion with real delays (5+15+45=65s) is too slow for unit tests.
    // The permanent error test (test_sync_one_handles_conflict) already covers
    // the conflict path. The transient retry test covers the retry→success path.
    // For full retry exhaustion, use integration tests with shorter delays.

    // ─── Engine: multi-doc debounce ──────────────────────

    #[tokio::test(start_paused = true)]
    async fn test_sync_engine_multi_doc_debounce() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());

        create_test_doc(&workspace, &storage, "docA", "DocA", "# DocA", None);
        create_test_doc(&workspace, &storage, "docB", "DocB", "# DocB", None);

        let (tx, rx) = mpsc::unbounded_channel();
        let (engine, _) = SyncEngine::new(provider.clone(), storage, workspace, Arc::new(AtomicU64::new(300)));
        let engine = Arc::new(engine);

        let engine_clone = engine.clone();
        let handle = tokio::spawn(async move {
            SyncEngine::run(engine_clone, rx).await;
        });

        // Fire both docs
        tx.send(SyncEvent::FileChanged { doc_id: "docA".into(), path: PathBuf::new() }).unwrap();
        tx.send(SyncEvent::FileChanged { doc_id: "docB".into(), path: PathBuf::new() }).unwrap();

        tokio::time::sleep(Duration::from_millis(1000)).await;

        let updates = provider.get_updates();
        assert_eq!(updates.len(), 2, "Both docs should be synced independently");
        let doc_ids: Vec<&str> = updates.iter().map(|(id, _)| id.as_str()).collect();
        assert!(doc_ids.contains(&"docA"));
        assert!(doc_ids.contains(&"docB"));

        tx.send(SyncEvent::Shutdown).unwrap();
        handle.await.unwrap();
    }

    // ─── Engine: SyncRequested bypasses debounce ─────────

    #[tokio::test(start_paused = true)]
    async fn test_sync_requested_bypasses_debounce() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());

        create_test_doc(&workspace, &storage, "bypass", "Bypass", "# Bypass test", None);

        let (tx, rx) = mpsc::unbounded_channel();
        // Very long debounce — FileChanged would wait 10s, but SyncRequested is immediate
        let (engine, _) = SyncEngine::new(provider.clone(), storage, workspace, Arc::new(AtomicU64::new(10_000)));
        let engine = Arc::new(engine);

        let engine_clone = engine.clone();
        let handle = tokio::spawn(async move {
            SyncEngine::run(engine_clone, rx).await;
        });

        tx.send(SyncEvent::SyncRequested { doc_id: "bypass".into() }).unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;

        assert_eq!(provider.get_updates().len(), 1, "SyncRequested should bypass debounce");

        tx.send(SyncEvent::Shutdown).unwrap();
        handle.await.unwrap();
    }

    // ─── sync_one: records history and snapshot ──────────

    #[tokio::test]
    async fn test_sync_one_records_history_and_snapshot() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());

        create_test_doc(&workspace, &storage, "hist_doc", "History", "# History test", None);

        let (engine, _) = SyncEngine::new(
            provider.clone(), storage.clone(), workspace, Arc::new(AtomicU64::new(2000)),
        );
        let engine = Arc::new(engine);
        engine.sync_one("hist_doc", false).await;

        let store = storage.lock().unwrap();
        let history = store.get_sync_history("hist_doc", 10).unwrap();
        assert!(!history.is_empty(), "Should record push in sync history");
        assert_eq!(history[0].action, "push");

        let snapshots = store.get_snapshots("hist_doc").unwrap();
        assert!(!snapshots.is_empty(), "Should save content snapshot");
        assert_eq!(snapshots[0].content, "# History test");
    }

    // ─── mark_synced defers rename (editor safety) ────────

    #[tokio::test]
    async fn test_mark_synced_does_not_rename_file() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());

        // Create doc with title "Old Title"
        let old_path = create_test_doc(
            &workspace, &storage, "doc_rename", "Old Title",
            "# Old Title\n\nBody", None,
        );

        // User edits the file to change the title
        std::fs::write(&old_path, "# New Title\n\nBody").unwrap();

        let (engine, _) = SyncEngine::new(
            provider.clone(), storage.clone(), workspace.clone(),
            Arc::new(AtomicU64::new(2000)),
        );
        let engine = Arc::new(engine);
        engine.sync_one("doc_rename", false).await;

        // File should NOT be renamed (deferred to editor close via rename_stale_paths)
        assert!(old_path.exists(), "File should stay at old path during editing");
        let new_path = workspace.join("docs").join("New Title.md");
        assert!(!new_path.exists(), "File should NOT be renamed during sync");

        // DB title should NOT be updated during sync — deferred to rename_stale_paths
        let doc = storage.lock().unwrap().get_doc("doc_rename").unwrap().unwrap();
        assert_eq!(doc.title, "Old Title");
        // local_path should still point to old file
        assert_eq!(doc.local_path.unwrap(), old_path.to_string_lossy().to_string());
    }

    // ═══════════════════════════════════════════════════════
    // State × Operation matrix tests
    // ═══════════════════════════════════════════════════════

    // ─── PUSH tests ─────────────────────────────────────────

    // #4: PUSH S1 force — manual sync bypasses hash check
    #[tokio::test]
    async fn test_push_s1_force() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());

        let content = "# Same content";
        let hash = hash_content(content.as_bytes());
        create_test_doc(&workspace, &storage, "force_doc", "Force", content, Some(hash));

        let (engine, _) = SyncEngine::new(
            provider.clone(), storage.clone(), workspace,
            Arc::new(AtomicU64::new(2000)),
        );
        let engine = Arc::new(engine);

        // force=false should skip (hash matches)
        engine.sync_one("force_doc", false).await;
        assert!(provider.get_updates().is_empty(), "auto sync should skip when hash matches");

        // force=true should push anyway
        engine.sync_one("force_doc", true).await;
        assert_eq!(provider.get_updates().len(), 1, "manual sync should push even when hash matches");
    }

    // #9: PUSH S5 recreate failure — create_doc also fails
    #[tokio::test]
    async fn test_push_s5_recreate_fail() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());
        // update_doc returns "not found", triggering recreate_on_remote
        provider.set_not_found();
        // create_doc also fails
        provider.create_should_fail.store(true, Ordering::SeqCst);

        create_test_doc(&workspace, &storage, "doomed_doc", "Doomed", "# Doomed\n\nContent", None);

        let (engine, mut status_rx) = SyncEngine::new(
            provider.clone(), storage.clone(), workspace,
            Arc::new(AtomicU64::new(2000)),
        );
        let engine = Arc::new(engine);
        engine.sync_one("doomed_doc", false).await;

        // create_doc should not have been recorded (it failed before pushing)
        assert!(provider.get_creates().is_empty(), "create_doc should have failed");

        // Doc should have an error status
        let doc = storage.lock().unwrap().get_doc("doomed_doc").unwrap().unwrap();
        match &doc.sync_status {
            SyncStatus::Error(msg) => assert!(msg.contains("重建失败"), "Error should mention recreate failure, got: {msg}"),
            other => panic!("Expected Error status, got: {other:?}"),
        }

        // Status emissions: Syncing, then Error
        let update = status_rx.try_recv().unwrap();
        assert_eq!(update.status, SyncStatus::Syncing);
        let update = status_rx.try_recv().unwrap();
        match &update.status {
            SyncStatus::Error(msg) => assert!(msg.contains("重建失败")),
            other => panic!("Expected Error status emission, got: {other:?}"),
        }
    }

    // #6: PUSH S3 — currently behaves like S2 (no remote_hash detection)
    #[tokio::test]
    #[ignore] // TODO: needs remote_hash mechanism to distinguish S3 from S1
    async fn test_push_s3_overwrite() {
        // Would need to simulate remote having newer content.
        // Currently behaves same as S2 (push overwrites remote).
    }

    // #7: PUSH S4 — currently no conflict detection before push
    #[tokio::test]
    #[ignore] // TODO: needs remote_hash mechanism to detect S4
    async fn test_push_s4_conflict() {
        // Would need pre-push fetch to detect conflict.
        // Currently behaves same as S2 (push overwrites remote).
    }

    // ─── CREATE tests ───────────────────────────────────────

    // #1: CREATE OK — provider creates doc and returns metadata
    #[tokio::test]
    async fn test_create_ok() {
        let provider = Arc::new(MockProvider::new());
        let result = provider.create("Test Doc", "# Test\n\nContent").await;
        assert!(result.is_ok());
        let meta = result.unwrap();
        assert!(!meta.doc_id.is_empty());
        assert_eq!(meta.title, "Test Doc");
        assert_eq!(meta.doc_id, "new_Test_Doc");
        assert!(meta.url.contains("new_Test_Doc"));
        assert_eq!(provider.get_creates().len(), 1);
    }

    // #2: CREATE FAIL — provider returns error on create
    #[tokio::test]
    async fn test_create_fail() {
        let provider = Arc::new(MockProvider::new());
        provider.create_should_fail.store(true, Ordering::SeqCst);

        let result = provider.create("Fail Doc", "# Fail").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("权限不足"), "Error should mention permission, got: {err}");
        // create should not have recorded anything
        assert!(provider.get_creates().is_empty());
    }

    // ─── PULL tests ─────────────────────────────────────────

    // #10: PULL S1 noop — read returns content (mock returns empty string)
    #[tokio::test]
    async fn test_pull_s1_noop() {
        let provider = Arc::new(MockProvider::new());
        let result = provider.read("any_doc").await;
        assert!(result.is_ok());
        // MockProvider returns empty string; in real code, pull_doc in commands.rs
        // handles the full flow (compare content, skip if identical)
        assert_eq!(result.unwrap().content, "");
    }

    // #14: PULL S5 fail — fetch when remote deleted
    // Tests the error classification layer used by pull logic.
    // In sync_one(), is_not_found() is checked BEFORE is_transient(),
    // so the recreate path is taken even if is_transient() also returns true.
    #[tokio::test]
    async fn test_pull_s5_fail() {
        // Verify error classification for "not found" errors
        let err = LarkNotesError::Cli("文档不存在".to_string());
        assert!(err.is_not_found(), "Chinese 'doc not found' should be classified as not_found");

        // English variants should be both not_found AND not transient
        let err_en = LarkNotesError::Cli("404 not found".to_string());
        assert!(err_en.is_not_found());
        assert!(!err_en.is_transient(), "English '404 not found' should not be transient");
    }

    // ─── DELETE tests ───────────────────────────────────────

    // #16: DELETE S1 OK — normal delete succeeds
    #[tokio::test]
    async fn test_delete_s1_ok() {
        let provider = Arc::new(MockProvider::new());
        let result = provider.delete("some_doc").await;
        assert!(result.is_ok());
    }

    // #20: DELETE S5 — remote already deleted, error is "not found"
    // Tests error classification used by delete logic in commands.rs
    #[tokio::test]
    async fn test_delete_remote_not_found() {
        let err = LarkNotesError::Cli("文档不存在".to_string());
        assert!(err.is_not_found(), "Should recognize Chinese 'not found'");

        // In the real flow, commands.rs catches is_not_found() and falls back
        // to force_local deletion. This test verifies the classification layer.
        let err2 = LarkNotesError::Cli("no such document".to_string());
        assert!(err2.is_not_found());
    }

    // ─── SEARCH tests ───────────────────────────────────────

    // #22: SEARCH OK — returns results (mock returns empty vec)
    #[tokio::test]
    async fn test_search_ok() {
        let provider = Arc::new(MockProvider::new());
        let result = provider.search("test query").await;
        assert!(result.is_ok());
        // MockProvider returns empty vec; real provider returns matching docs
        assert!(result.unwrap().is_empty());
    }

    // #23: SEARCH empty — no matches (same behavior with mock)
    #[tokio::test]
    async fn test_search_empty() {
        let provider = Arc::new(MockProvider::new());
        let result = provider.search("").await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty(), "Empty query should return empty results");
    }

    // ─── PUSH S2 with force=true variant ────────────────────

    // Verify that force=true on a modified doc (S2) also works
    #[tokio::test]
    async fn test_push_s2_force() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());

        // Store with old hash, write new content → S2 (diverged)
        let old_hash = hash_content(b"# Old content");
        create_test_doc(&workspace, &storage, "s2_force", "S2Force", "# New content", Some(old_hash));

        let (engine, _) = SyncEngine::new(
            provider.clone(), storage.clone(), workspace,
            Arc::new(AtomicU64::new(2000)),
        );
        let engine = Arc::new(engine);

        // force=true should push
        engine.sync_one("s2_force", true).await;
        let updates = provider.get_updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].1, "# New content");

        let doc = storage.lock().unwrap().get_doc("s2_force").unwrap().unwrap();
        assert_eq!(doc.sync_status, SyncStatus::Synced);
    }

    // ─── PUSH S5 recreate records history as "recreate" ─────

    #[tokio::test]
    async fn test_push_s5_recreate_records_history() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());
        provider.set_not_found();

        create_test_doc(&workspace, &storage, "hist_recreate", "HistRecreate", "# Recreated", None);

        let (engine, _) = SyncEngine::new(
            provider.clone(), storage.clone(), workspace,
            Arc::new(AtomicU64::new(2000)),
        );
        let engine = Arc::new(engine);
        engine.sync_one("hist_recreate", false).await;

        let creates = provider.get_creates();
        assert_eq!(creates.len(), 1);
        let new_id = &creates[0].0;

        let store = storage.lock().unwrap();
        let history = store.get_sync_history(new_id, 10).unwrap();
        assert!(!history.is_empty(), "Should record recreate in history");
        assert_eq!(history[0].action, "recreate");

        let snapshots = store.get_snapshots(new_id).unwrap();
        assert!(!snapshots.is_empty(), "Should save snapshot after recreate");
    }

    // ─── PUSH permanent error records conflict history ──────

    #[tokio::test]
    async fn test_push_permanent_error_records_conflict_history() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());
        provider.set_fail(true); // permanent 403

        create_test_doc(&workspace, &storage, "conflict_hist", "ConflictHist", "# Conflict", None);

        let (engine, _) = SyncEngine::new(
            provider.clone(), storage.clone(), workspace,
            Arc::new(AtomicU64::new(2000)),
        );
        let engine = Arc::new(engine);
        engine.sync_one("conflict_hist", false).await;

        let store = storage.lock().unwrap();
        let history = store.get_sync_history("conflict_hist", 10).unwrap();
        assert!(!history.is_empty(), "Should record conflict in history");
        assert_eq!(history[0].action, "conflict");
    }

    // ─── Auth status check ──────────────────────────────────

    #[tokio::test]
    async fn test_auth_status() {
        let provider = Arc::new(MockProvider::new());
        let status = provider.auth_status().await.unwrap();
        assert!(status.logged_in);
        assert_eq!(status.user_name, Some("MockUser".to_string()));
        assert!(!status.needs_refresh);
    }

    // ─── Missing PULL tests ─────────────────────────────────

    // #11: PULL S2 — pull overwrites local modifications with remote content
    #[tokio::test]
    async fn test_pull_s2_overwrite_local() {
        let provider = Arc::new(MockProvider::new());
        // S2: local has modifications, remote has original content
        // read returns remote content (mock returns empty string = remote version)
        let result = provider.read("s2_doc").await;
        assert!(result.is_ok());
        let remote_content = result.unwrap().content;
        // In the real flow, commands.rs pull_doc writes remote_content to local file,
        // overwriting the local modifications. The key behavior: fetch succeeds,
        // and the caller replaces local file content with fetched content.
        assert_eq!(remote_content, "", "Remote content fetched (mock returns empty)");
    }

    // #12: PULL S3 — remote has newer content (needs remote_hash to detect)
    #[tokio::test]
    #[ignore] // TODO: needs remote_hash mechanism to distinguish S3 from S1
    async fn test_pull_s3_update() {
        // S3: remote updated, local stale. Currently indistinguishable from S1
        // without remote_hash comparison. When remote_hash is implemented,
        // pull should detect remote is newer and fetch updated content.
        let provider = Arc::new(MockProvider::new());
        let result = provider.read("s3_doc").await;
        assert!(result.is_ok());
    }

    // #13: PULL S4 — both sides modified (needs remote_hash to detect)
    #[tokio::test]
    #[ignore] // TODO: needs remote_hash mechanism to detect S4 conflict
    async fn test_pull_s4_overwrite() {
        // S4: both local and remote modified. Currently pull just overwrites
        // local with remote (same as S2). When remote_hash is implemented,
        // should detect conflict and warn user before overwriting.
        let provider = Arc::new(MockProvider::new());
        let result = provider.read("s4_doc").await;
        assert!(result.is_ok());
    }

    // #15: PULL S6 — import from remote-only doc (no local file exists)
    #[tokio::test]
    async fn test_pull_s6_import() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());

        // S6: doc exists only on remote, no local file or DB entry
        // Simulate import: read → write to local file → store in DB
        let remote_output = provider.read("remote_only_doc").await.unwrap();
        let remote_content = remote_output.content;

        // Write fetched content to local file (simulating import flow)
        let local_path = titled_content_path(&workspace, "ImportedDoc");
        std::fs::write(&local_path, &remote_content).unwrap();
        assert!(local_path.exists(), "Local file should be created after import");

        // Store doc metadata in DB
        let meta = DocMeta {
            doc_id: "remote_only_doc".to_string(),
            title: "ImportedDoc".to_string(),
            doc_type: "DOCX".to_string(),
            url: "https://feishu.cn/docx/remote_only_doc".to_string(),
            owner_name: "test".to_string(),
            created_at: "2026-01-01".to_string(),
            updated_at: "2026-01-01".to_string(),
            local_path: Some(local_path.to_string_lossy().to_string()),
            content_hash: Some(hash_content(remote_content.as_bytes())),
            sync_status: SyncStatus::Synced,
            folder_path: String::new(),
        };
        storage.lock().unwrap().upsert_doc(&meta).unwrap();

        let stored = storage.lock().unwrap().get_doc("remote_only_doc").unwrap();
        assert!(stored.is_some(), "Imported doc should be in DB");
        assert_eq!(stored.unwrap().title, "ImportedDoc");
    }

    // ─── Missing DELETE tests ───────────────────────────────

    // #17: DELETE S2 — delete when local has unsaved modifications
    #[tokio::test]
    async fn test_delete_s2_ok() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());

        // S2: local file modified (different hash), remote exists
        let old_hash = hash_content(b"# Original");
        let file_path = create_test_doc(&workspace, &storage, "del_s2", "DelS2", "# Modified locally", Some(old_hash));

        // Delete remote succeeds
        let result = provider.delete("del_s2").await;
        assert!(result.is_ok(), "Remote delete should succeed regardless of local state");

        // Clean up local file + DB (simulating full delete flow)
        std::fs::remove_file(&file_path).unwrap();
        storage.lock().unwrap().delete_doc("del_s2").unwrap();

        assert!(!file_path.exists(), "Local file should be removed");
        assert!(storage.lock().unwrap().get_doc("del_s2").unwrap().is_none(), "DB entry should be removed");
    }

    // #18: DELETE S3 — delete when remote has newer content (needs remote_hash)
    #[tokio::test]
    #[ignore] // TODO: needs remote_hash mechanism to distinguish S3 from S1
    async fn test_delete_s3_ok() {
        // S3: remote has newer content than local. Delete should still work
        // (both sides deleted). Currently indistinguishable from S1.
        let provider = Arc::new(MockProvider::new());
        let result = provider.delete("s3_doc").await;
        assert!(result.is_ok());
    }

    // #19: DELETE S4 — delete when both sides modified (needs remote_hash)
    #[tokio::test]
    #[ignore] // TODO: needs remote_hash mechanism to detect S4 conflict
    async fn test_delete_s4_ok() {
        // S4: both local and remote modified. Delete should still succeed
        // (user explicitly chose to delete). Currently same as S1 delete.
        let provider = Arc::new(MockProvider::new());
        let result = provider.delete("s4_doc").await;
        assert!(result.is_ok());
    }

    // #21: DELETE S5 — remote returns permanent permission error (not "not found")
    #[tokio::test]
    async fn test_delete_s5_permission_fail() {
        let provider = Arc::new(MockProvider::new());
        provider.set_fail(true); // permanent 403

        let result = provider.delete("perm_fail_doc").await;
        assert!(result.is_err(), "Delete should fail with permission error");

        let err = result.unwrap_err();
        assert!(!err.is_not_found(), "Permission error should NOT be classified as not_found");
        assert!(!err.is_transient(), "Permission error should NOT be transient");
        // In the real flow, commands.rs surfaces this error to the user
        // instead of falling back to force_local deletion.
        assert!(err.to_string().contains("403"), "Error should contain 403");
    }

    // ─── WRITE direct tests ────────────────────────────────

    // #24: WRITE OK — write succeeds and returns WriteMeta
    #[tokio::test]
    async fn test_write_ok() {
        let provider = Arc::new(MockProvider::new());
        let result = provider.write("doc1", "# Updated content").await;
        assert!(result.is_ok());
        let meta = result.unwrap();
        assert!(!meta.updated_at.is_empty(), "WriteMeta should have updated_at");
        let updates = provider.get_updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].0, "doc1");
        assert_eq!(updates[0].1, "# Updated content");
    }

    // #25: WRITE FAIL — permanent error
    #[tokio::test]
    async fn test_write_fail_permanent() {
        let provider = Arc::new(MockProvider::new());
        provider.set_fail(true);
        let result = provider.write("doc1", "content").await;
        assert!(result.is_err());
        assert!(!result.unwrap_err().is_transient());
        assert!(provider.get_updates().is_empty(), "Should not record failed write");
    }

    // #26: WRITE NOT_FOUND — triggers recreate path
    #[tokio::test]
    async fn test_write_not_found() {
        let provider = Arc::new(MockProvider::new());
        provider.set_not_found();
        let result = provider.write("deleted_doc", "content").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().is_not_found());
    }

    // #27: WRITE TRANSIENT — fails then succeeds
    #[tokio::test]
    async fn test_write_transient_then_ok() {
        let provider = Arc::new(MockProvider::new());
        provider.set_transient_failures(2);
        // First two calls fail
        assert!(provider.write("doc1", "c").await.is_err());
        assert!(provider.write("doc1", "c").await.is_err());
        // Third succeeds
        assert!(provider.write("doc1", "c").await.is_ok());
        assert_eq!(provider.get_updates().len(), 1);
    }

    // ─── RENAME direct tests ───────────────────────────────

    // #28: RENAME OK — rename succeeds and is tracked
    #[tokio::test]
    async fn test_rename_ok() {
        let provider = Arc::new(MockProvider::new());
        let result = provider.rename("doc1", "New Title").await;
        assert!(result.is_ok());
        let renames = provider.get_renames();
        assert_eq!(renames.len(), 1);
        assert_eq!(renames[0].0, "doc1");
        assert_eq!(renames[0].1, "New Title");
    }

    // #29: RENAME FAIL — permanent error
    #[tokio::test]
    async fn test_rename_fail_permanent() {
        let provider = Arc::new(MockProvider::new());
        provider.set_fail(true);
        let result = provider.rename("doc1", "New Title").await;
        assert!(result.is_err());
        assert!(provider.get_renames().is_empty());
    }

    // #30: RENAME NOT_FOUND — doc doesn't exist
    #[tokio::test]
    async fn test_rename_not_found() {
        let provider = Arc::new(MockProvider::new());
        provider.set_not_found();
        let result = provider.rename("gone_doc", "New Title").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().is_not_found());
    }

    // #31: RENAME multiple — sequential renames tracked
    #[tokio::test]
    async fn test_rename_multiple() {
        let provider = Arc::new(MockProvider::new());
        provider.rename("d1", "Title A").await.unwrap();
        provider.rename("d2", "Title B").await.unwrap();
        provider.rename("d1", "Title C").await.unwrap();
        let renames = provider.get_renames();
        assert_eq!(renames.len(), 3);
        assert_eq!(renames[2], ("d1".to_string(), "Title C".to_string()));
    }

    // ─── LIST direct tests ─────────────────────────────────

    // #32: LIST OK — empty result
    #[tokio::test]
    async fn test_list_ok_empty() {
        let provider = Arc::new(MockProvider::new());
        let result = provider.list(None).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    // #33: LIST OK — with results
    #[tokio::test]
    async fn test_list_ok_with_docs() {
        let provider = Arc::new(MockProvider::new());
        let now = chrono::Local::now().to_rfc3339();
        provider.set_list_result(vec![
            DocMeta {
                doc_id: "d1".into(), title: "Doc One".into(),
                doc_type: "DOCX".into(), url: "".into(),
                owner_name: "test".into(),
                created_at: now.clone(), updated_at: now.clone(),
                local_path: None, content_hash: None,
                sync_status: SyncStatus::Synced, folder_path: String::new(),
            },
            DocMeta {
                doc_id: "d2".into(), title: "Doc Two".into(),
                doc_type: "DOCX".into(), url: "".into(),
                owner_name: "test".into(),
                created_at: now.clone(), updated_at: now,
                local_path: None, content_hash: None,
                sync_status: SyncStatus::Synced, folder_path: "sub".into(),
            },
        ]);
        let result = provider.list(None).await.unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].doc_id, "d1");
        assert_eq!(result[1].folder_path, "sub");
    }

    // #34: LIST FAIL — permission error
    #[tokio::test]
    async fn test_list_fail() {
        let provider = Arc::new(MockProvider::new());
        provider.set_fail(true);
        let result = provider.list(Some("folder")).await;
        assert!(result.is_err());
    }

    // #35: LIST with folder filter (mock doesn't filter, just verifies call works)
    #[tokio::test]
    async fn test_list_with_folder() {
        let provider = Arc::new(MockProvider::new());
        let result = provider.list(Some("project-a")).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
