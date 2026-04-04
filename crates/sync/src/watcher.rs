use crate::reconcile::reconcile_paths;
use larknotes_core::{docs_dir, folder_of, LarkNotesError};
use larknotes_storage::Storage;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{CreateKind, ModifyKind, RemoveKind}};
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
        Self::with_notify(workspace_dir, tx, storage, None)
    }

    pub fn with_notify(
        workspace_dir: PathBuf,
        tx: mpsc::UnboundedSender<SyncEvent>,
        storage: Arc<Mutex<Storage>>,
        docs_changed_tx: Option<mpsc::UnboundedSender<()>>,
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
                            Self::handle_rename(&event, &workspace_clone, &storage, &tx_clone, &docs_changed_tx);
                        }
                        EventKind::Create(CreateKind::Folder) => {
                            Self::handle_folder_create(&event, &workspace_clone, &storage, &docs_changed_tx);
                        }
                        EventKind::Remove(RemoveKind::Folder) => {
                            Self::handle_folder_remove(&event, &workspace_clone, &storage, &docs_changed_tx);
                        }
                        EventKind::Modify(_) | EventKind::Create(_) => {
                            Self::handle_modify_create(&event, &workspace_clone, &storage, &tx_clone);
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
            .watch(&docs_dir, RecursiveMode::Recursive)
            .map_err(|e| LarkNotesError::Sync(format!("启动文件监听失败: {e}")))?;

        tracing::info!("文件监听已启动 (recursive): {}", docs_dir.display());

        Ok(Self {
            _watcher: watcher,
        })
    }

    fn handle_modify_create(
        event: &Event,
        workspace: &std::path::Path,
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

                            // Update folder_path if it changed (file moved into subfolder)
                            let current_folder = folder_of(workspace, path);
                            if current_folder != doc.folder_path {
                                let _ = store.update_folder_path(&doc.doc_id, &current_folder);
                            }

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
        docs_changed_tx: &Option<mpsc::UnboundedSender<()>>,
    ) {
        tracing::info!(
            "检测到重命名: {:?}, paths={:?}",
            event.kind,
            event.paths
        );

        // Check if this is a folder rename (both paths are directories)
        let is_folder_rename = event.paths.len() == 2
            && event.paths[1].is_dir();

        if is_folder_rename {
            Self::handle_folder_rename(event, workspace, storage, docs_changed_tx);
            return;
        }

        let matches = reconcile_paths(workspace, storage);

        for m in &matches {
            let new_path = PathBuf::from(&m.new_path);
            tracing::info!(
                "重命名修复: doc_id={}, {} → {}",
                m.doc_id,
                m.old_path.as_deref().unwrap_or("?"),
                m.new_path
            );

            // Update folder_path based on new location
            if let Ok(store) = storage.lock() {
                let folder = folder_of(workspace, &new_path);
                let _ = store.update_folder_path(&m.doc_id, &folder);
            }

            let _ = tx.send(SyncEvent::FileChanged {
                doc_id: m.doc_id.clone(),
                path: new_path,
            });
        }

        if !matches.is_empty() {
            if let Some(tx) = docs_changed_tx {
                let _ = tx.send(());
            }
        }
    }

    fn handle_folder_rename(
        event: &Event,
        workspace: &std::path::Path,
        storage: &Arc<Mutex<Storage>>,
        docs_changed_tx: &Option<mpsc::UnboundedSender<()>>,
    ) {
        if event.paths.len() != 2 {
            return;
        }
        let docs = docs_dir(workspace);
        let old_rel = match event.paths[0].strip_prefix(&docs) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => return,
        };
        let new_rel = match event.paths[1].strip_prefix(&docs) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => return,
        };

        tracing::info!("文件夹重命名: {} → {}", old_rel, new_rel);

        if let Ok(store) = storage.lock() {
            let _ = store.rename_folder(&old_rel, &new_rel);
            // Also update local_path for all affected documents
            if let Ok(all_docs) = store.list_docs() {
                let old_dir = docs.join(&*old_rel);
                let new_dir = docs.join(&*new_rel);
                for doc in &all_docs {
                    if let Some(ref lp) = doc.local_path {
                        let lp_path = std::path::Path::new(lp);
                        if lp_path.starts_with(&old_dir) {
                            if let Ok(suffix) = lp_path.strip_prefix(&old_dir) {
                                let new_lp = new_dir.join(suffix).to_string_lossy().to_string();
                                let _ = store.update_local_path(&doc.doc_id, &new_lp);
                            }
                        }
                    }
                }
            }
        }

        if let Some(tx) = docs_changed_tx {
            let _ = tx.send(());
        }
    }

    fn handle_folder_create(
        event: &Event,
        workspace: &std::path::Path,
        storage: &Arc<Mutex<Storage>>,
        docs_changed_tx: &Option<mpsc::UnboundedSender<()>>,
    ) {
        let docs = docs_dir(workspace);
        for path in &event.paths {
            if let Ok(rel) = path.strip_prefix(&docs) {
                let folder_path = rel.to_string_lossy().replace('\\', "/");
                if folder_path.is_empty() {
                    continue;
                }
                tracing::info!("新建文件夹: {}", folder_path);
                if let Ok(store) = storage.lock() {
                    let _ = store.upsert_folder(&folder_path, None);
                }
                if let Some(tx) = docs_changed_tx {
                    let _ = tx.send(());
                }
            }
        }
    }

    fn handle_folder_remove(
        event: &Event,
        workspace: &std::path::Path,
        storage: &Arc<Mutex<Storage>>,
        docs_changed_tx: &Option<mpsc::UnboundedSender<()>>,
    ) {
        let docs = docs_dir(workspace);
        for path in &event.paths {
            if let Ok(rel) = path.strip_prefix(&docs) {
                let folder_path = rel.to_string_lossy().replace('\\', "/");
                if folder_path.is_empty() {
                    continue;
                }
                tracing::info!("删除文件夹: {}", folder_path);
                if let Ok(store) = storage.lock() {
                    let _ = store.delete_folder(&folder_path);
                }
                if let Some(tx) = docs_changed_tx {
                    let _ = tx.send(());
                }
            }
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
            folder_path: String::new(),
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

    #[tokio::test]
    async fn test_watcher_subfolder_file_triggers_event() {
        let (tmp, storage, mut rx, _watcher) = setup_watcher_test();
        let sub_dir = tmp.path().join("docs").join("project-a");
        std::fs::create_dir_all(&sub_dir).unwrap();

        let file_path = sub_dir.join("note.md");
        let file_path_str = file_path.to_string_lossy().to_string();
        register_doc(&storage, "doc1", &file_path_str);

        // Small delay for watcher to register the new subdir
        tokio::time::sleep(Duration::from_millis(200)).await;

        std::fs::write(&file_path, "# Note").unwrap();

        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
        match event {
            Ok(Some(SyncEvent::FileChanged { doc_id, .. })) => {
                assert_eq!(doc_id, "doc1");
            }
            Ok(Some(other)) => panic!("Expected FileChanged, got {other:?}"),
            Ok(None) => panic!("Channel closed"),
            Err(_) => panic!("Timed out — recursive watcher may not have detected subfolder change"),
        }
    }
}
