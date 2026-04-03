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
                                engine.sync_one(&doc_id).await;
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
                            engine.sync_one(&doc_id).await;
                        });
                    }
                }
            }
        }
    }

    /// Sync a single document. Public for testing.
    pub async fn sync_one(&self, doc_id: &str) {
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

        if old_hash.as_deref() == Some(&new_hash) {
            tracing::debug!("内容未变化, 跳过同步: {doc_id}");
            return;
        }

        // 4. Update status to Syncing
        self.emit_status(doc_id, SyncStatus::Syncing, None);
        if let Ok(store) = self.storage.lock() {
            let _ = store.update_sync_status(doc_id, &SyncStatus::Syncing);
        }

        // 5. Extract title
        let title = extract_title(&content);

        // 6. Push to remote with retry (exponential backoff: 5s, 15s, 45s)
        let retry_delays = [
            Duration::from_secs(5),
            Duration::from_secs(15),
            Duration::from_secs(45),
        ];
        // First attempt
        match self.provider.update_doc(doc_id, &content).await {
            Ok(()) => {
                self.mark_synced(doc_id, &new_hash, &title, &content);
                return;
            }
            Err(e) => {
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

            match self.provider.update_doc(doc_id, &content).await {
                Ok(()) => {
                    tracing::info!("重试成功: {doc_id} (第{}次)", i + 1);
                    self.mark_synced(doc_id, &new_hash, &title, &content);
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

    fn mark_synced(&self, doc_id: &str, new_hash: &str, _title: &str, content: &str) {
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
    struct MockProvider {
        update_should_fail: AtomicBool,
        /// If > 0, fail this many times with transient error, then succeed
        transient_fail_count: std::sync::atomic::AtomicI32,
        updated_docs: Mutex<Vec<(String, String)>>,
    }

    impl MockProvider {
        fn new() -> Self {
            Self {
                update_should_fail: AtomicBool::new(false),
                transient_fail_count: std::sync::atomic::AtomicI32::new(0),
                updated_docs: Mutex::new(Vec::new()),
            }
        }

        fn set_fail(&self, fail: bool) {
            self.update_should_fail.store(fail, Ordering::SeqCst);
        }

        /// Set transient failures: fail N times then succeed
        fn set_transient_failures(&self, count: i32) {
            self.transient_fail_count.store(count, Ordering::SeqCst);
        }

        fn get_updates(&self) -> Vec<(String, String)> {
            self.updated_docs.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl DocProvider for MockProvider {
        async fn auth_status(&self) -> Result<AuthStatus, LarkNotesError> {
            Ok(AuthStatus {
                logged_in: true,
                user_name: Some("MockUser".to_string()),
                expires_at: None,
                needs_refresh: false,
            })
        }
        async fn search_docs(&self, _query: &str) -> Result<Vec<DocMeta>, LarkNotesError> {
            Ok(vec![])
        }
        async fn create_doc(&self, _title: &str, _markdown: &str) -> Result<DocMeta, LarkNotesError> {
            unimplemented!()
        }
        async fn fetch_doc(&self, _doc_id: &str) -> Result<String, LarkNotesError> {
            Ok(String::new())
        }
        async fn update_doc(&self, doc_id: &str, markdown: &str) -> Result<(), LarkNotesError> {
            // Permanent failure mode
            if self.update_should_fail.load(Ordering::SeqCst) {
                return Err(LarkNotesError::Auth("404 not found".into()));
            }
            // Transient failure mode: decrement counter, fail if > 0
            let remaining = self.transient_fail_count.fetch_sub(1, Ordering::SeqCst);
            if remaining > 0 {
                return Err(LarkNotesError::Cli("connection timeout".into()));
            }
            self.updated_docs
                .lock()
                .unwrap()
                .push((doc_id.to_string(), markdown.to_string()));
            Ok(())
        }
        async fn delete_doc(&self, _doc_id: &str) -> Result<(), LarkNotesError> {
            Ok(())
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
        engine.sync_one("doc1").await;

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
        engine.sync_one("doc2").await;

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
        engine.sync_one("doc3").await;

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
    async fn test_sync_one_does_not_update_title() {
        let (tmp, storage) = setup_test_workspace();
        let workspace = tmp.path().to_path_buf();
        let provider = Arc::new(MockProvider::new());

        create_test_doc(&workspace, &storage, "doc4", "Old Title", "# My Custom Title\n\nBody", None);

        let (engine, _) = SyncEngine::new(provider, storage.clone(), workspace, Arc::new(AtomicU64::new(2000)));
        let engine = Arc::new(engine);
        engine.sync_one("doc4").await;

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
        engine.sync_one("nonexistent").await;
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
        };
        storage.lock().unwrap().upsert_doc(&meta).unwrap();

        let (engine, _) = SyncEngine::new(
            provider.clone(), storage, workspace, Arc::new(AtomicU64::new(2000)),
        );
        let engine = Arc::new(engine);

        // Should not panic, just fail to read and return
        engine.sync_one("ghost").await;
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
        engine.sync_one("retry_doc").await;

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
        engine.sync_one("hist_doc").await;

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
        engine.sync_one("doc_rename").await;

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
}
