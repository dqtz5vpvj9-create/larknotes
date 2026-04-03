use crate::reconcile::reconcile_paths;
use larknotes_core::LarkNotesError;
use larknotes_storage::Storage;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::ModifyKind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum SyncEvent {
    FileChanged { doc_id: String, path: PathBuf },
    SyncRequested { doc_id: String },
    Shutdown,
}

pub struct FileWatcher {
    _watcher: RecommendedWatcher,
}

impl FileWatcher {
    pub fn new(
        workspace_dir: PathBuf,
        tx: mpsc::UnboundedSender<SyncEvent>,
        storage: Arc<Mutex<Storage>>,
    ) -> Result<Self, LarkNotesError> {
        let docs_dir = workspace_dir.join("docs");
        std::fs::create_dir_all(&docs_dir)
            .map_err(|e| LarkNotesError::Sync(format!("创建docs目录失败: {e}")))?;

        let tx_clone = tx.clone();
        let workspace_clone = workspace_dir.clone();
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            match res {
                Ok(event) => {
                    match event.kind {
                        EventKind::Modify(ModifyKind::Name(_)) => {
                            // File renamed: reconcile orphan docs with new paths
                            Self::handle_rename(&event, &workspace_clone, &storage, &tx_clone);
                        }
                        EventKind::Modify(_) | EventKind::Create(_) => {
                            // Handle file modifications and creations
                            Self::handle_modify_create(&event, &storage, &tx_clone);
                        }
                        _ => return,
                    }
                }
                Err(e) => {
                    tracing::error!("文件监听错误: {e}");
                }
            }
        })
        .map_err(|e| LarkNotesError::Sync(format!("创建文件监听失败: {e}")))?;

        watcher
            .watch(&docs_dir, RecursiveMode::NonRecursive)
            .map_err(|e| LarkNotesError::Sync(format!("启动文件监听失败: {e}")))?;

        tracing::info!("文件监听已启动: {}", docs_dir.display());

        Ok(Self {
            _watcher: watcher,
        })
    }

    fn handle_modify_create(
        event: &Event,
        storage: &Arc<Mutex<Storage>>,
        tx: &mpsc::UnboundedSender<SyncEvent>,
    ) {
        for path in &event.paths {
            let is_md = path.extension().and_then(|e| e.to_str()) == Some("md");
            let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !is_md || fname.contains(".conflict-") {
                continue;
            }

            let path_str = path.to_string_lossy().to_string();
            if let Ok(store) = storage.lock() {
                if let Ok(docs) = store.list_docs() {
                    for doc in &docs {
                        if doc.local_path.as_deref() == Some(&path_str) {
                            tracing::info!(
                                "检测到文件变更: doc_id={}, file={}",
                                doc.doc_id, fname
                            );
                            let _ = tx.send(SyncEvent::FileChanged {
                                doc_id: doc.doc_id.clone(),
                                path: path.clone(),
                            });
                            break;
                        }
                    }
                }
            }
        }
    }

    fn handle_rename(
        event: &Event,
        workspace: &std::path::Path,
        storage: &Arc<Mutex<Storage>>,
        tx: &mpsc::UnboundedSender<SyncEvent>,
    ) {
        // For Rename(Both): event.paths = [old_path, new_path]
        // For Rename(From)/Rename(To): event.paths = [path]
        //
        // Strategy: run reconcile_paths to match orphan docs with orphan files.
        // This handles all rename variants uniformly without needing to pair
        // From/To events across separate callbacks.

        tracing::info!(
            "检测到文件重命名: {:?}, paths={:?}",
            event.kind,
            event.paths
        );

        let matches = reconcile_paths(workspace, storage);

        // For each reconciled doc, send a FileChanged event so the sync engine
        // picks up any content changes that happened alongside the rename.
        for m in &matches {
            let new_path = PathBuf::from(&m.new_path);
            tracing::info!(
                "重命名修复: doc_id={}, {} → {}",
                m.doc_id,
                m.old_path.as_deref().unwrap_or("?"),
                m.new_path
            );
            let _ = tx.send(SyncEvent::FileChanged {
                doc_id: m.doc_id.clone(),
                path: new_path,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use larknotes_core::*;
    use larknotes_storage::Storage;
    use std::time::Duration;

    fn setup_watcher_test() -> (
        tempfile::TempDir,
        Arc<Mutex<Storage>>,
        mpsc::UnboundedReceiver<SyncEvent>,
        FileWatcher,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(Mutex::new(Storage::new_in_memory().unwrap()));
        let (tx, rx) = mpsc::unbounded_channel();
        let watcher = FileWatcher::new(tmp.path().to_path_buf(), tx, storage.clone()).unwrap();
        (tmp, storage, rx, watcher)
    }

    fn register_doc(storage: &Arc<Mutex<Storage>>, doc_id: &str, local_path: &str) {
        let meta = DocMeta {
            doc_id: doc_id.to_string(),
            title: "Test".to_string(),
            doc_type: "DOCX".to_string(),
            url: "".to_string(),
            owner_name: "test".to_string(),
            created_at: "2026-01-01".to_string(),
            updated_at: "2026-01-01".to_string(),
            local_path: Some(local_path.to_string()),
            content_hash: None,
            sync_status: SyncStatus::Synced,
        };
        storage.lock().unwrap().upsert_doc(&meta).unwrap();
    }

    #[tokio::test]
    async fn test_watcher_creates_docs_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let docs_dir = tmp.path().join("docs");
        assert!(!docs_dir.exists());

        let storage = Arc::new(Mutex::new(Storage::new_in_memory().unwrap()));
        let (tx, _rx) = mpsc::unbounded_channel();
        let _watcher = FileWatcher::new(tmp.path().to_path_buf(), tx, storage).unwrap();

        assert!(docs_dir.exists(), "FileWatcher should create docs/ directory");
    }

    #[tokio::test]
    async fn test_watcher_md_file_triggers_event() {
        let (tmp, storage, mut rx, _watcher) = setup_watcher_test();
        let docs_dir = tmp.path().join("docs");
        let file_path = docs_dir.join("test.md");
        let file_path_str = file_path.to_string_lossy().to_string();

        register_doc(&storage, "doc1", &file_path_str);

        // Write the file
        std::fs::write(&file_path, "# Hello").unwrap();

        // Wait for event (file watchers can be slow)
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
        match event {
            Ok(Some(SyncEvent::FileChanged { doc_id, .. })) => {
                assert_eq!(doc_id, "doc1");
            }
            Ok(Some(other)) => panic!("Expected FileChanged, got {other:?}"),
            Ok(None) => panic!("Channel closed"),
            Err(_) => panic!("Timed out waiting for FileChanged event"),
        }
    }

    #[tokio::test]
    async fn test_watcher_ignores_txt_file() {
        let (tmp, _storage, mut rx, _watcher) = setup_watcher_test();
        let docs_dir = tmp.path().join("docs");

        // Write a .txt file
        std::fs::write(docs_dir.join("test.txt"), "hello").unwrap();

        // Should NOT receive an event
        let result = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        assert!(result.is_err(), "Should not receive event for .txt file");
    }

    #[tokio::test]
    async fn test_watcher_ignores_conflict_file() {
        let (tmp, storage, mut rx, _watcher) = setup_watcher_test();
        let docs_dir = tmp.path().join("docs");
        let conflict_file = docs_dir.join("test.conflict-20260101-120000.md");

        register_doc(&storage, "doc1", &conflict_file.to_string_lossy().to_string());

        std::fs::write(&conflict_file, "# Conflict").unwrap();

        let result = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        assert!(result.is_err(), "Should not receive event for conflict file");
    }

    #[tokio::test]
    async fn test_watcher_ignores_unregistered_file() {
        let (tmp, _storage, mut rx, _watcher) = setup_watcher_test();
        let docs_dir = tmp.path().join("docs");

        // Write .md file not registered in storage
        std::fs::write(docs_dir.join("unknown.md"), "# Unknown").unwrap();

        let result = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        assert!(result.is_err(), "Should not receive event for unregistered file");
    }
}
