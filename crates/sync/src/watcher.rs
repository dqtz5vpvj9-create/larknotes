use crate::write_guard::WriteGuard;
use larknotes_core::{docs_dir, LarkNotesError};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{CreateKind, ModifyKind, RemoveKind, RenameMode}};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::mpsc;

/// Minimum interval between events on the same path (deduplication window).
const EVENT_DEDUP_MS: u128 = 100;

#[derive(Debug, Clone)]
pub enum SyncEvent {
    /// A tracked .md file was modified (watcher detected content change).
    /// Engine will look up doc_id from DB and decide whether to sync.
    FileModified { path: PathBuf },
    /// Explicitly identified file change with known doc_id (used by engine internally).
    FileChanged { doc_id: String, path: PathBuf },
    /// A new untracked .md file was detected.
    NewFileDetected { path: PathBuf },
    /// Manual sync requested by user.
    SyncRequested { doc_id: String },
    /// Paired file move: old_path → new_path. Engine updates DB local_path + renames remote.
    FileMoved { old_path: PathBuf, new_path: PathBuf },
    /// File deleted from filesystem. Engine should delete from remote + DB.
    FileDeleted { path: PathBuf },
    /// Unpaired rename event — engine should run reconcile_paths as fallback.
    FileRenamed { workspace: PathBuf },
    /// Folder renamed in filesystem.
    FolderRenamed { old_rel: String, new_rel: String },
    /// New folder created in filesystem.
    FolderCreated { folder_path: String },
    /// Folder removed from filesystem.
    FolderRemoved { folder_path: String },
    /// Shutdown the sync engine.
    Shutdown,
}

pub struct FileWatcher {
    _watcher: RecommendedWatcher,
}

impl FileWatcher {
    /// Create a new FileWatcher. The watcher does NOT hold a Storage reference —
    /// it only sends events to the SyncEngine queue for serial processing.
    pub fn new(
        workspace_dir: PathBuf,
        tx: mpsc::UnboundedSender<SyncEvent>,
        write_guard: Option<WriteGuard>,
    ) -> Result<Self, LarkNotesError> {
        let docs_dir = workspace_dir.join("docs");
        std::fs::create_dir_all(&docs_dir)
            .map_err(|e| LarkNotesError::Sync(format!("创建docs目录失败: {e}")))?;

        let tx_clone = tx.clone();
        let workspace_clone = workspace_dir.clone();
        let write_guard_clone = write_guard.clone();
        // Event deduplication: track last event time per path to skip rapid-fire duplicates.
        let recent_events: Arc<Mutex<HashMap<PathBuf, Instant>>> = Arc::new(Mutex::new(HashMap::new()));
        let recent_events_clone = recent_events.clone();
        // Rename pairing: hold a Name(From) event until matching Name(To) arrives.
        let pending_from: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
        let pending_from_clone = pending_from.clone();
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            match res {
                Ok(event) => {
                    match event.kind {
                        EventKind::Modify(ModifyKind::Name(rename_mode)) => {
                            Self::handle_rename(&event, rename_mode, &workspace_clone, &tx_clone, &pending_from_clone);
                        }
                        EventKind::Create(CreateKind::Folder) => {
                            Self::handle_folder_create(&event, &workspace_clone, &tx_clone);
                        }
                        EventKind::Remove(RemoveKind::Folder) => {
                            Self::handle_folder_remove(&event, &workspace_clone, &tx_clone);
                        }
                        EventKind::Remove(RemoveKind::File) | EventKind::Remove(RemoveKind::Any) => {
                            Self::handle_file_remove(&event, &tx_clone);
                        }
                        EventKind::Modify(_) | EventKind::Create(_) => {
                            // Deduplicate rapid events on the same path
                            let dominated = {
                                let mut map = recent_events_clone.lock().unwrap_or_else(|e| e.into_inner());
                                let now = Instant::now();
                                // Prune stale entries (> 1s old) to prevent unbounded growth
                                map.retain(|_, t| now.duration_since(*t).as_millis() < 1000);
                                let dominated = event.paths.iter().all(|p| {
                                    if let Some(last) = map.get(p) {
                                        now.duration_since(*last).as_millis() < EVENT_DEDUP_MS
                                    } else {
                                        false
                                    }
                                });
                                for p in &event.paths {
                                    map.insert(p.clone(), now);
                                }
                                dominated
                            };
                            if !dominated {
                                Self::handle_modify_create(&event, &tx_clone, &write_guard_clone);
                            }
                        }
                        _ => (),
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

    /// Handle file modify/create events. No DB access — just send event to engine.
    fn handle_modify_create(
        event: &Event,
        tx: &mpsc::UnboundedSender<SyncEvent>,
        write_guard: &Option<WriteGuard>,
    ) {
        for path in &event.paths {
            let is_md = path.extension().and_then(|e| e.to_str()) == Some("md");
            let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !is_md || fname.contains(".conflict-") || fname.starts_with(".~") {
                continue;
            }

            // Skip paths currently being written by the executor
            if let Some(ref wg) = write_guard {
                if wg.is_guarded(path) {
                    tracing::debug!("watcher: 跳过被write_guard保护的路径: {}", path.display());
                    continue;
                }
            }

            // Send to engine — engine will look up DB to determine if this is
            // a known doc (FileChanged) or new file (NewFileDetected).
            let _ = tx.send(SyncEvent::FileModified {
                path: path.clone(),
            });
        }
    }

    /// Handle rename events with From/To pairing.
    ///
    /// On Windows, `notify` sends separate events for Name(From) and Name(To).
    /// We pair them: hold From, wait for To, emit a single FileMoved event.
    /// If To arrives without a pending From (or paths are already paired), handle directly.
    fn handle_rename(
        event: &Event,
        rename_mode: RenameMode,
        workspace: &std::path::Path,
        tx: &mpsc::UnboundedSender<SyncEvent>,
        pending_from: &Arc<Mutex<Option<PathBuf>>>,
    ) {
        // Case 1: event already has both paths (some platforms pair them)
        if event.paths.len() == 2 {
            let old = &event.paths[0];
            let new = &event.paths[1];

            // Check if folder rename
            if old.is_dir() || new.is_dir() {
                Self::handle_folder_rename(event, workspace, tx);
                return;
            }

            tracing::info!("检测到文件移动: {} → {}", old.display(), new.display());
            let _ = tx.send(SyncEvent::FileMoved {
                old_path: old.clone(),
                new_path: new.clone(),
            });
            return;
        }

        // Case 2: unpaired events — use state machine
        match rename_mode {
            RenameMode::From => {
                if let Some(path) = event.paths.first() {
                    tracing::debug!("检测到重命名(From): {}", path.display());
                    let mut guard = pending_from.lock().unwrap_or_else(|e| e.into_inner());
                    // If there was already a pending From that never got paired, flush it
                    if let Some(old_pending) = guard.take() {
                        tracing::debug!("刷新未配对的From: {}", old_pending.display());
                        let _ = tx.send(SyncEvent::FileRenamed {
                            workspace: workspace.to_path_buf(),
                        });
                    }
                    *guard = Some(path.clone());
                }
            }
            RenameMode::To => {
                if let Some(new_path) = event.paths.first() {
                    let mut guard = pending_from.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(old_path) = guard.take() {
                        // Paired! Check if folder rename
                        if old_path.is_dir() || new_path.is_dir() {
                            // Reconstruct a 2-path event for folder rename handler
                            let fake_event = Event {
                                kind: event.kind,
                                paths: vec![old_path, new_path.clone()],
                                attrs: event.attrs.clone(),
                            };
                            Self::handle_folder_rename(&fake_event, workspace, tx);
                        } else {
                            tracing::info!("检测到文件移动: {} → {}", old_path.display(), new_path.display());
                            let _ = tx.send(SyncEvent::FileMoved {
                                old_path,
                                new_path: new_path.clone(),
                            });
                        }
                    } else {
                        // To without From — file appeared (could be moved from outside docs/)
                        tracing::debug!("检测到重命名(To without From): {}", new_path.display());
                        let _ = tx.send(SyncEvent::FileModified {
                            path: new_path.clone(),
                        });
                    }
                }
            }
            RenameMode::Both | RenameMode::Any | RenameMode::Other => {
                // Fallback for platforms that don't distinguish From/To
                tracing::info!("检测到重命名(未配对): paths={:?}", event.paths);
                let _ = tx.send(SyncEvent::FileRenamed {
                    workspace: workspace.to_path_buf(),
                });
            }
        }
    }

    fn handle_folder_rename(
        event: &Event,
        workspace: &std::path::Path,
        tx: &mpsc::UnboundedSender<SyncEvent>,
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

        let _ = tx.send(SyncEvent::FolderRenamed { old_rel, new_rel });
    }

    fn handle_file_remove(
        event: &Event,
        tx: &mpsc::UnboundedSender<SyncEvent>,
    ) {
        for path in &event.paths {
            let is_md = path.extension().and_then(|e| e.to_str()) == Some("md");
            let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !is_md || fname.contains(".conflict-") || fname.starts_with(".~") {
                continue;
            }
            tracing::info!("检测到文件删除: {}", path.display());
            let _ = tx.send(SyncEvent::FileDeleted { path: path.clone() });
        }
    }

    fn handle_folder_create(
        event: &Event,
        workspace: &std::path::Path,
        tx: &mpsc::UnboundedSender<SyncEvent>,
    ) {
        let docs = docs_dir(workspace);
        for path in &event.paths {
            if let Ok(rel) = path.strip_prefix(&docs) {
                let folder_path = rel.to_string_lossy().replace('\\', "/");
                if folder_path.is_empty() {
                    continue;
                }
                tracing::info!("新建文件夹: {}", folder_path);
                let _ = tx.send(SyncEvent::FolderCreated { folder_path });
            }
        }
    }

    fn handle_folder_remove(
        event: &Event,
        workspace: &std::path::Path,
        tx: &mpsc::UnboundedSender<SyncEvent>,
    ) {
        let docs = docs_dir(workspace);
        for path in &event.paths {
            if let Ok(rel) = path.strip_prefix(&docs) {
                let folder_path = rel.to_string_lossy().replace('\\', "/");
                if folder_path.is_empty() {
                    continue;
                }
                tracing::info!("删除文件夹: {}", folder_path);
                let _ = tx.send(SyncEvent::FolderRemoved { folder_path });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn setup_watcher_test() -> (
        tempfile::TempDir,
        mpsc::UnboundedReceiver<SyncEvent>,
        FileWatcher,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        let watcher = FileWatcher::new(tmp.path().to_path_buf(), tx, None).unwrap();
        (tmp, rx, watcher)
    }

    #[tokio::test]
    async fn test_watcher_creates_docs_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let docs_dir = tmp.path().join("docs");
        assert!(!docs_dir.exists());

        let (tx, _rx) = mpsc::unbounded_channel();
        let _watcher = FileWatcher::new(tmp.path().to_path_buf(), tx, None).unwrap();

        assert!(docs_dir.exists(), "FileWatcher should create docs/ directory");
    }

    #[tokio::test]
    async fn test_watcher_md_file_triggers_event() {
        let (tmp, mut rx, _watcher) = setup_watcher_test();
        let docs_dir = tmp.path().join("docs");
        let file_path = docs_dir.join("test.md");

        // Write the file
        std::fs::write(&file_path, "# Hello").unwrap();

        // Wait for event (file watchers can be slow)
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
        match event {
            Ok(Some(SyncEvent::FileModified { path })) => {
                assert!(path.ends_with("test.md"));
            }
            Ok(Some(other)) => panic!("Expected FileModified, got {other:?}"),
            Ok(None) => panic!("Channel closed"),
            Err(_) => panic!("Timed out waiting for FileModified event"),
        }
    }

    #[tokio::test]
    async fn test_watcher_ignores_txt_file() {
        let (tmp, mut rx, _watcher) = setup_watcher_test();
        let docs_dir = tmp.path().join("docs");

        // Write a .txt file
        std::fs::write(docs_dir.join("test.txt"), "hello").unwrap();

        // Should NOT receive an event
        let result = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        assert!(result.is_err(), "Should not receive event for .txt file");
    }

    #[tokio::test]
    async fn test_watcher_ignores_conflict_file() {
        let (tmp, mut rx, _watcher) = setup_watcher_test();
        let docs_dir = tmp.path().join("docs");
        let conflict_file = docs_dir.join("test.conflict-20260101-120000.md");

        std::fs::write(&conflict_file, "# Conflict").unwrap();

        let result = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        assert!(result.is_err(), "Should not receive event for conflict file");
    }

    #[tokio::test]
    async fn test_watcher_detects_unregistered_file() {
        let (tmp, mut rx, _watcher) = setup_watcher_test();
        let docs_dir = tmp.path().join("docs");

        // Write .md file — watcher sends FileModified (engine will determine if new)
        std::fs::write(docs_dir.join("unknown.md"), "# Unknown").unwrap();

        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
        match event {
            Ok(Some(SyncEvent::FileModified { path })) => {
                assert!(path.ends_with("unknown.md"));
            }
            Ok(Some(other)) => panic!("Expected FileModified, got {other:?}"),
            Ok(None) => panic!("Channel closed"),
            Err(_) => panic!("Timed out waiting for FileModified event"),
        }
    }

    #[tokio::test]
    async fn test_watcher_subfolder_file_triggers_event() {
        let (tmp, mut rx, _watcher) = setup_watcher_test();
        let sub_dir = tmp.path().join("docs").join("project-a");
        std::fs::create_dir_all(&sub_dir).unwrap();

        // Small delay for watcher to register the new subdir
        tokio::time::sleep(Duration::from_millis(200)).await;

        let file_path = sub_dir.join("note.md");
        std::fs::write(&file_path, "# Note").unwrap();

        // May receive FolderCreated first, then FileModified
        let mut got_file_event = false;
        for _ in 0..5 {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
                Ok(Some(SyncEvent::FileModified { path })) if path.ends_with("note.md") => {
                    got_file_event = true;
                    break;
                }
                Ok(Some(_)) => continue, // skip folder events
                _ => break,
            }
        }
        assert!(got_file_event, "Should receive FileModified for subfolder file");
    }
}
