use larknotes_core::LarkNotesError;
use larknotes_storage::Storage;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
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
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            match res {
                Ok(event) => {
                    // Accept all Modify and Create events.
                    match event.kind {
                        EventKind::Modify(_) | EventKind::Create(_) => {}
                        _ => return,
                    }

                    for path in &event.paths {
                        // Only process .md files, skip conflict files
                        let is_md = path.extension().and_then(|e| e.to_str()) == Some("md");
                        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        if !is_md || fname.contains(".conflict-") {
                            continue;
                        }

                        // Look up doc_id from storage by local_path
                        let path_str = path.to_string_lossy().to_string();
                        if let Ok(store) = storage.lock() {
                            if let Ok(docs) = store.list_docs() {
                                for doc in &docs {
                                    if doc.local_path.as_deref() == Some(&path_str) {
                                        tracing::info!(
                                            "检测到文件变更: doc_id={}, file={}",
                                            doc.doc_id, fname
                                        );
                                        let _ = tx_clone.send(SyncEvent::FileChanged {
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
}
