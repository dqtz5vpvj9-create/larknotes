use crate::hash_content;
use larknotes_core::*;
use larknotes_storage::Storage;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Result of a single path reconciliation match.
#[derive(Debug, Clone)]
pub struct ReconcileMatch {
    pub doc_id: String,
    pub old_path: Option<String>,
    pub new_path: String,
    pub method: ReconcileMethod,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReconcileMethod {
    ContentHash,
    TitleMatch,
}

/// Extract a display title from a filename by stripping the `.md` extension.
fn title_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    Some(stem.to_string())
}

/// Reconcile orphan docs (DB local_path doesn't exist on disk) with orphan files
/// (untracked .md files in docs/ dir). This handles:
/// - External renames while app was not running (startup reconciliation)
/// - External renames while app is running (triggered by watcher)
///
/// Returns a list of matches that were applied (DB updated).
pub fn reconcile_paths(
    workspace: &Path,
    storage: &Arc<Mutex<Storage>>,
) -> Vec<ReconcileMatch> {
    let docs_path = docs_dir(workspace);
    if !docs_path.exists() {
        return Vec::new();
    }

    // Clone doc list under brief lock, then release — file I/O must not hold the lock.
    let all_docs = match storage.lock() {
        Ok(store) => match store.list_docs() {
            Ok(docs) => docs,
            Err(e) => {
                tracing::error!("reconcile_paths: list_docs failed: {e}");
                return Vec::new();
            }
        },
        Err(e) => {
            tracing::error!("reconcile_paths: storage lock poisoned: {e}");
            return Vec::new();
        }
    };
    // Lock is now dropped.

    // Build set of known local_paths for quick lookup
    let known_paths: HashSet<String> = all_docs
        .iter()
        .filter_map(|d| d.local_path.as_ref())
        .cloned()
        .collect();

    // Find orphan docs: local_path is set but file doesn't exist on disk
    let orphan_docs: Vec<&DocMeta> = all_docs
        .iter()
        .filter(|d| {
            if let Some(ref path) = d.local_path {
                !Path::new(path).exists()
            } else {
                false
            }
        })
        .collect();

    if orphan_docs.is_empty() {
        return Vec::new();
    }

    // Find orphan files: .md files in docs/ (recursively) that are NOT in any doc's local_path
    // Also skip conflict files and editor temp/lock files (.~ prefix)
    let orphan_files: Vec<PathBuf> = walkdir::WalkDir::new(&docs_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            let is_md = p.extension().and_then(|e| e.to_str()) == Some("md");
            let fname = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let is_conflict = fname.contains(".conflict-");
            let is_temp = fname.starts_with(".~");
            let is_known = known_paths.contains(&p.to_string_lossy().to_string());
            is_md && !is_conflict && !is_temp && !is_known
        })
        .collect();

    if orphan_files.is_empty() {
        tracing::info!(
            "reconcile_paths: {} orphan doc(s) but no orphan files to match",
            orphan_docs.len()
        );
        return Vec::new();
    }

    let mut matches = Vec::new();
    let mut used_files: HashSet<PathBuf> = HashSet::new();

    // Pass 1: Match by content hash (most reliable) — file I/O without lock
    for doc in &orphan_docs {
        if let Some(ref doc_hash) = doc.content_hash {
            for file_path in &orphan_files {
                if used_files.contains(file_path) {
                    continue;
                }
                if let Ok(content) = std::fs::read(file_path) {
                    let file_hash = hash_content(&content);
                    if &file_hash == doc_hash {
                        let new_path = file_path.to_string_lossy().to_string();
                        // Brief lock for DB update
                        if let Ok(store) = storage.lock() {
                            if let Err(e) = store.update_local_path(&doc.doc_id, &new_path) {
                                tracing::error!(
                                    "reconcile_paths: update_local_path failed for {}: {e}",
                                    doc.doc_id
                                );
                                continue;
                            }
                            let new_title = title_from_filename(file_path);
                            if let Some(ref t) = new_title {
                                if t != &doc.title {
                                    let _ = store.update_title(&doc.doc_id, t);
                                }
                            }
                        }
                        tracing::info!(
                            "reconcile_paths: matched doc {} by content hash → {}",
                            doc.doc_id,
                            file_path.display()
                        );
                        matches.push(ReconcileMatch {
                            doc_id: doc.doc_id.clone(),
                            old_path: doc.local_path.clone(),
                            new_path,
                            method: ReconcileMethod::ContentHash,
                        });
                        used_files.insert(file_path.clone());
                        break;
                    }
                }
            }
        }
    }

    // Collect already-matched doc_ids (owned to avoid borrow conflict)
    let matched_ids: HashSet<String> = matches.iter().map(|m| m.doc_id.clone()).collect();

    // Pass 2: Match by title (fallback for docs not matched by hash)
    for doc in &orphan_docs {
        if matched_ids.contains(&doc.doc_id) {
            continue;
        }
        for file_path in &orphan_files {
            if used_files.contains(file_path) {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(file_path) {
                let file_title = extract_title(&content);
                if file_title == doc.title {
                    let new_path = file_path.to_string_lossy().to_string();
                    let new_hash = hash_content(content.as_bytes());
                    // Brief lock for DB update
                    if let Ok(store) = storage.lock() {
                        if let Err(e) = store.update_local_path(&doc.doc_id, &new_path) {
                            tracing::error!(
                                "reconcile_paths: update_local_path failed for {}: {e}",
                                doc.doc_id
                            );
                            continue;
                        }
                        let _ = store.update_content_hash(&doc.doc_id, &new_hash);
                        let new_title = title_from_filename(file_path);
                        if let Some(ref t) = new_title {
                            if t != &doc.title {
                                let _ = store.update_title(&doc.doc_id, t);
                            }
                        }
                    }
                    tracing::info!(
                        "reconcile_paths: matched doc {} by title '{}' → {}",
                        doc.doc_id,
                        doc.title,
                        file_path.display()
                    );
                    matches.push(ReconcileMatch {
                        doc_id: doc.doc_id.clone(),
                        old_path: doc.local_path.clone(),
                        new_path,
                        method: ReconcileMethod::TitleMatch,
                    });
                    used_files.insert(file_path.clone());
                    break;
                }
            }
        }
    }

    if !matches.is_empty() {
        tracing::info!(
            "reconcile_paths: resolved {} orphan doc(s) ({} remaining)",
            matches.len(),
            orphan_docs.len() - matches.len()
        );
    }

    matches
}

/// Scan the docs/ directory tree and register all subfolders in the DB.
/// Also updates folder_path for any documents whose local_path is in a subfolder.
/// Called at app startup.
pub fn scan_folder_tree(
    workspace: &Path,
    storage: &Arc<Mutex<Storage>>,
) -> usize {
    let docs_path = docs_dir(workspace);
    if !docs_path.exists() {
        return 0;
    }

    let store = match storage.lock() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("scan_folder_tree: storage lock poisoned: {e}");
            return 0;
        }
    };

    let mut count = 0;
    for entry in walkdir::WalkDir::new(&docs_path)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_dir() && entry.path() != docs_path {
            if let Ok(rel) = entry.path().strip_prefix(&docs_path) {
                let folder_path = rel.to_string_lossy().replace('\\', "/");
                let _ = store.upsert_folder(&folder_path, None);
                count += 1;
            }
        }
    }

    // Update folder_path for documents based on their local_path
    if let Ok(all_docs) = store.list_docs() {
        for doc in &all_docs {
            if let Some(ref lp) = doc.local_path {
                let lp_path = Path::new(lp);
                if lp_path.exists() {
                    let folder = folder_of(workspace, lp_path);
                    if folder != doc.folder_path {
                        let _ = store.update_folder_path(&doc.doc_id, &folder);
                    }
                }
            }
        }
    }

    if count > 0 {
        tracing::info!("scan_folder_tree: registered {count} folder(s)");
    }
    count
}

/// Rename local files whose filename doesn't match their content title.
///
/// **Only processes documents with `pending_rename = true`** — set by `quick_note()`
/// and `create_doc()` to defer title-based rename until the editor closes.
/// Normal documents are never renamed by this function.
///
/// Called after editor process exits and at app startup.
///
/// Returns the number of files renamed.
pub fn rename_stale_paths(
    workspace: &Path,
    storage: &Arc<Mutex<Storage>>,
) -> usize {
    // Only process docs marked for rename — not all docs.
    let pending_docs = match storage.lock() {
        Ok(store) => match store.list_pending_rename_docs() {
            Ok(docs) => docs,
            Err(e) => {
                tracing::error!("rename_stale_paths: list_pending_rename_docs failed: {e}");
                return 0;
            }
        },
        Err(e) => {
            tracing::error!("rename_stale_paths: storage lock poisoned: {e}");
            return 0;
        }
    };
    // Lock is now dropped — safe to do file I/O.

    let mut count = 0;

    for doc in &pending_docs {
        let Some(ref local_path_str) = doc.local_path else {
            continue;
        };
        let old_path = PathBuf::from(local_path_str);
        if !old_path.exists() {
            continue; // Orphan — handled by reconcile_paths
        }

        // Read the file and extract the actual title from content (no lock held)
        let content = match std::fs::read_to_string(&old_path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("rename_stale_paths: read failed {}: {e}", old_path.display());
                continue;
            }
        };

        // Auto-delete unchanged quick notes: if content matches the initial
        // template ("# {title}\n\n"), the user never wrote anything.
        let initial_content = format!("# {}\n\n", doc.title);
        if content == initial_content {
            let _ = std::fs::remove_file(&old_path);
            let _ = std::fs::remove_file(meta_path(workspace, &doc.doc_id));
            if let Ok(store) = storage.lock() {
                if doc.remote_id.is_some() {
                    // Has remote copy — mark for remote deletion
                    let _ = store.update_sync_state(&doc.doc_id, &SyncState::PendingDelete);
                    let _ = store.set_pending_rename(&doc.doc_id, false);
                } else {
                    // No remote copy — full local cleanup
                    let _ = store.delete_doc(&doc.doc_id);
                }
            }
            tracing::info!(
                "rename_stale_paths: auto-deleted unchanged quick note {} (title='{}')",
                doc.doc_id, doc.title,
            );
            count += 1;
            continue;
        }

        let content_title = extract_title(&content);

        // Check if filename already matches the content title
        let folder = &doc.folder_path;
        let expected_path = titled_content_path_in(workspace, folder, &content_title);
        if old_path == expected_path {
            // Filename already matches title — clear the flag, update title if stale.
            if let Ok(store) = storage.lock() {
                let _ = store.set_pending_rename(&doc.doc_id, false);
                if doc.title != content_title {
                    let _ = store.update_title(&doc.doc_id, &content_title);
                }
            }
            continue;
        }

        // Re-acquire lock briefly for DB updates
        let store = match storage.lock() {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("rename_stale_paths: storage lock poisoned during update: {e}");
                continue;
            }
        };

        // Verify doc still has expected local_path (race protection)
        if let Ok(Some(current)) = store.get_doc(&doc.doc_id) {
            if current.local_path.as_deref() != Some(local_path_str) {
                continue; // Another thread updated the path, skip
            }
        }

        // Ensure unique title within folder (DB-level dedup)
        let unique_title = store.unique_title(&content_title, folder, Some(&doc.doc_id))
            .unwrap_or_else(|_| content_title.clone());

        // Compute a safe target path (filesystem-level dedup)
        let new_path = unique_content_path_in(workspace, folder, &unique_title);

        if let Err(e) = store.update_title(&doc.doc_id, &unique_title) {
            tracing::error!(
                "rename_stale_paths: update_title failed for {}: {e}",
                doc.doc_id
            );
            continue;
        }
        let new_path_str = new_path.to_string_lossy().to_string();
        if let Err(e) = store.update_local_path(&doc.doc_id, &new_path_str) {
            tracing::error!(
                "rename_stale_paths: update_local_path failed for {}: {e}",
                doc.doc_id
            );
            let _ = store.update_title(&doc.doc_id, &doc.title);
            continue;
        }

        // Drop lock before file rename
        drop(store);

        match std::fs::rename(&old_path, &new_path) {
            Ok(()) => {
                // Clear pending_rename flag after successful rename
                if let Ok(store) = storage.lock() {
                    let _ = store.set_pending_rename(&doc.doc_id, false);
                }
                tracing::info!(
                    "rename_stale_paths: {} → {} (doc={}, title='{}')",
                    old_path.display(),
                    new_path.display(),
                    doc.doc_id,
                    content_title,
                );
                count += 1;
            }
            Err(e) => {
                tracing::warn!(
                    "rename_stale_paths: rename failed {} → {}: {e}",
                    old_path.display(),
                    new_path.display()
                );
                // Revert DB
                if let Ok(store) = storage.lock() {
                    let _ = store.update_local_path(&doc.doc_id, local_path_str);
                    let _ = store.update_title(&doc.doc_id, &doc.title);
                }
            }
        }
    }

    count
}

/// Find `.md` files in `docs/` that are not tracked by any document in the DB.
/// Skips conflict files. Used at startup and by the watcher to discover
/// externally-created files that should be adopted into the system.
pub fn scan_orphan_files(
    workspace: &Path,
    storage: &Arc<Mutex<Storage>>,
) -> Vec<PathBuf> {
    let docs_path = docs_dir(workspace);
    if !docs_path.exists() {
        return Vec::new();
    }

    let store = match storage.lock() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("scan_orphan_files: storage lock poisoned: {e}");
            return Vec::new();
        }
    };

    let known_paths: HashSet<String> = match store.list_docs() {
        Ok(docs) => docs
            .iter()
            .filter_map(|d| d.local_path.as_ref())
            .cloned()
            .collect(),
        Err(e) => {
            tracing::error!("scan_orphan_files: list_docs failed: {e}");
            return Vec::new();
        }
    };

    walkdir::WalkDir::new(&docs_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            let is_md = p.extension().and_then(|e| e.to_str()) == Some("md");
            let fname = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let is_conflict = fname.contains(".conflict-");
            let is_temp = fname.starts_with(".~");
            let is_known = known_paths.contains(&p.to_string_lossy().to_string());
            is_md && !is_conflict && !is_temp && !is_known
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_reconcile_test() -> (tempfile::TempDir, Arc<Mutex<Storage>>) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("docs")).unwrap();
        let storage = Arc::new(Mutex::new(Storage::new_in_memory().unwrap()));
        (tmp, storage)
    }

    fn insert_doc(
        storage: &Arc<Mutex<Storage>>,
        doc_id: &str,
        title: &str,
        local_path: Option<&str>,
        content_hash: Option<&str>,
    ) {
        let meta = DocMeta {
            note_id: doc_id.to_string(),
            remote_id: Some(doc_id.to_string()),
            doc_id: doc_id.to_string(),
            title: title.to_string(),
            doc_type: "DOCX".to_string(),
            url: "".to_string(),
            owner_name: "test".to_string(),
            created_at: "2026-01-01".to_string(),
            updated_at: "2026-01-01".to_string(),
            local_path: local_path.map(|s| s.to_string()),
            content_hash: content_hash.map(|s| s.to_string()),
            sync_status: SyncStatus::Synced,
            folder_path: String::new(),
        file_size: None,
        word_count: None,
            sync_state: SyncState::Synced,
            title_mode: "manual".to_string(),
            desired_title: None,
            desired_path: None,
        };
        storage.lock().unwrap().upsert_doc(&meta).unwrap();
    }

    #[test]
    fn test_reconcile_no_orphans() {
        let (tmp, storage) = setup_reconcile_test();
        let matches = reconcile_paths(tmp.path(), &storage);
        assert!(matches.is_empty());
    }

    #[test]
    fn test_reconcile_by_content_hash() {
        let (tmp, storage) = setup_reconcile_test();
        let content = "# My Document\n\nHello world";
        let hash = hash_content(content.as_bytes());

        // Doc points to old path that doesn't exist
        let old_path = tmp.path().join("docs").join("OldName.md");
        insert_doc(
            &storage,
            "doc1",
            "My Document",
            Some(&old_path.to_string_lossy()),
            Some(&hash),
        );

        // File exists at new path with same content
        let new_path = tmp.path().join("docs").join("NewName.md");
        std::fs::write(&new_path, content).unwrap();

        let matches = reconcile_paths(tmp.path(), &storage);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].doc_id, "doc1");
        assert_eq!(matches[0].method, ReconcileMethod::ContentHash);
        assert_eq!(matches[0].new_path, new_path.to_string_lossy().to_string());

        // Verify DB was updated
        let doc = storage.lock().unwrap().get_doc("doc1").unwrap().unwrap();
        assert_eq!(doc.local_path.unwrap(), new_path.to_string_lossy().to_string());
    }

    #[test]
    fn test_reconcile_by_title_match() {
        let (tmp, storage) = setup_reconcile_test();
        let content = "# My Document\n\nNew content after edit";

        // Doc points to old path, hash doesn't match (content was edited after rename)
        let old_path = tmp.path().join("docs").join("OldName.md");
        insert_doc(
            &storage,
            "doc1",
            "My Document",
            Some(&old_path.to_string_lossy()),
            Some("stale_hash"),
        );

        // File exists at new path with same title but different hash
        let new_path = tmp.path().join("docs").join("My Document.md");
        std::fs::write(&new_path, content).unwrap();

        let matches = reconcile_paths(tmp.path(), &storage);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].doc_id, "doc1");
        assert_eq!(matches[0].method, ReconcileMethod::TitleMatch);
    }

    #[test]
    fn test_reconcile_hash_takes_priority_over_title() {
        let (tmp, storage) = setup_reconcile_test();
        let content = "# My Document\n\nOriginal content";
        let hash = hash_content(content.as_bytes());

        let old_path = tmp.path().join("docs").join("OldName.md");
        insert_doc(
            &storage,
            "doc1",
            "My Document",
            Some(&old_path.to_string_lossy()),
            Some(&hash),
        );

        // Two orphan files: one matches by hash, another matches by title
        let hash_match = tmp.path().join("docs").join("RandomName.md");
        std::fs::write(&hash_match, content).unwrap();

        let title_match = tmp.path().join("docs").join("My Document.md");
        std::fs::write(&title_match, "# My Document\n\nDifferent content").unwrap();

        let matches = reconcile_paths(tmp.path(), &storage);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].method, ReconcileMethod::ContentHash);
        assert_eq!(
            matches[0].new_path,
            hash_match.to_string_lossy().to_string()
        );
    }

    #[test]
    fn test_reconcile_ignores_conflict_files() {
        let (tmp, storage) = setup_reconcile_test();
        let content = "# My Document\n\nContent";
        let hash = hash_content(content.as_bytes());

        let old_path = tmp.path().join("docs").join("OldName.md");
        insert_doc(
            &storage,
            "doc1",
            "My Document",
            Some(&old_path.to_string_lossy()),
            Some(&hash),
        );

        // Only conflict file exists — should NOT match
        let conflict = tmp.path().join("docs").join("My Document.conflict-20260101-120000.md");
        std::fs::write(&conflict, content).unwrap();

        let matches = reconcile_paths(tmp.path(), &storage);
        assert!(matches.is_empty());
    }

    #[test]
    fn test_reconcile_ignores_non_md_files() {
        let (tmp, storage) = setup_reconcile_test();
        let content = "# My Document\n\nContent";
        let hash = hash_content(content.as_bytes());

        let old_path = tmp.path().join("docs").join("OldName.md");
        insert_doc(
            &storage,
            "doc1",
            "My Document",
            Some(&old_path.to_string_lossy()),
            Some(&hash),
        );

        // Only .txt file — should NOT match
        std::fs::write(tmp.path().join("docs").join("My Document.txt"), content).unwrap();

        let matches = reconcile_paths(tmp.path(), &storage);
        assert!(matches.is_empty());
    }

    #[test]
    fn test_reconcile_multiple_orphans() {
        let (tmp, storage) = setup_reconcile_test();

        let content1 = "# Doc One\n\nFirst";
        let hash1 = hash_content(content1.as_bytes());
        let content2 = "# Doc Two\n\nSecond";
        let hash2 = hash_content(content2.as_bytes());

        insert_doc(
            &storage,
            "d1",
            "Doc One",
            Some(&tmp.path().join("docs").join("old1.md").to_string_lossy()),
            Some(&hash1),
        );
        insert_doc(
            &storage,
            "d2",
            "Doc Two",
            Some(&tmp.path().join("docs").join("old2.md").to_string_lossy()),
            Some(&hash2),
        );

        // Both files renamed
        std::fs::write(tmp.path().join("docs").join("new1.md"), content1).unwrap();
        std::fs::write(tmp.path().join("docs").join("new2.md"), content2).unwrap();

        let matches = reconcile_paths(tmp.path(), &storage);
        assert_eq!(matches.len(), 2);
        let ids: Vec<&str> = matches.iter().map(|m| m.doc_id.as_str()).collect();
        assert!(ids.contains(&"d1"));
        assert!(ids.contains(&"d2"));
    }

    #[test]
    fn test_reconcile_existing_path_still_valid() {
        let (tmp, storage) = setup_reconcile_test();
        let content = "# Existing\n\nStill here";
        let path = tmp.path().join("docs").join("Existing.md");
        std::fs::write(&path, content).unwrap();

        // Doc path is valid — not an orphan
        insert_doc(
            &storage,
            "doc1",
            "Existing",
            Some(&path.to_string_lossy()),
            Some(&hash_content(content.as_bytes())),
        );

        let matches = reconcile_paths(tmp.path(), &storage);
        assert!(matches.is_empty(), "Should not reconcile docs with valid paths");
    }

    #[test]
    fn test_reconcile_no_local_path() {
        let (tmp, storage) = setup_reconcile_test();
        // Doc with no local_path — not an orphan (never synced)
        insert_doc(&storage, "doc1", "New Doc", None, None);

        std::fs::write(tmp.path().join("docs").join("New Doc.md"), "# New Doc").unwrap();

        let matches = reconcile_paths(tmp.path(), &storage);
        assert!(matches.is_empty(), "Docs without local_path are not orphans");
    }

    // ─── rename_stale_paths tests ───────────────────────

    #[test]
    fn test_rename_stale_paths_renames_mismatched_file() {
        let (tmp, storage) = setup_reconcile_test();
        let content = "# New Title\n\nBody";

        // File exists at old path, but DB title has been updated to "New Title"
        let old_path = tmp.path().join("docs").join("Old Title.md");
        std::fs::write(&old_path, content).unwrap();

        insert_doc(
            &storage, "doc1", "New Title",
            Some(&old_path.to_string_lossy()),
            Some(&hash_content(content.as_bytes())),
        );
        // Mark for rename (as quick_note/create_doc would)
        storage.lock().unwrap().set_pending_rename("doc1", true).unwrap();

        let count = rename_stale_paths(tmp.path(), &storage);
        assert_eq!(count, 1);

        // File should now be at "New Title.md"
        let new_path = tmp.path().join("docs").join("New Title.md");
        assert!(new_path.exists(), "File should be renamed to match title");
        assert!(!old_path.exists(), "Old file should be gone");

        // DB should point to new path
        let doc = storage.lock().unwrap().get_doc("doc1").unwrap().unwrap();
        assert_eq!(doc.local_path.unwrap(), new_path.to_string_lossy().to_string());
    }

    #[test]
    fn test_rename_stale_paths_skips_matching_file() {
        let (tmp, storage) = setup_reconcile_test();
        let path = tmp.path().join("docs").join("My Doc.md");
        std::fs::write(&path, "# My Doc\n\nBody").unwrap();

        insert_doc(
            &storage, "doc1", "My Doc",
            Some(&path.to_string_lossy()),
            None,
        );

        let count = rename_stale_paths(tmp.path(), &storage);
        assert_eq!(count, 0, "Should not rename when filename already matches title");
        assert!(path.exists());
    }

    #[test]
    fn test_rename_stale_paths_handles_collision() {
        let (tmp, storage) = setup_reconcile_test();

        // Existing file at the target name
        let existing = tmp.path().join("docs").join("Target.md");
        std::fs::write(&existing, "# Target\n\nExisting").unwrap();
        insert_doc(
            &storage, "existing", "Target",
            Some(&existing.to_string_lossy()),
            None,
        );

        // Stale file that needs rename to "Target" but it's taken
        let stale = tmp.path().join("docs").join("Old Name.md");
        std::fs::write(&stale, "# Target\n\nNew doc").unwrap();
        insert_doc(
            &storage, "stale", "Target",
            Some(&stale.to_string_lossy()),
            None,
        );
        // Mark for rename
        storage.lock().unwrap().set_pending_rename("stale", true).unwrap();

        let count = rename_stale_paths(tmp.path(), &storage);
        assert_eq!(count, 1);

        // Should use unique path "Target (2).md"
        let unique = tmp.path().join("docs").join("Target (2).md");
        assert!(unique.exists(), "Should rename to Target (2).md");
        assert!(existing.exists(), "Original Target.md should be untouched");
        assert!(!stale.exists(), "Old Name.md should be gone");
    }

    #[test]
    fn test_rename_stale_paths_skips_orphan_docs() {
        let (tmp, storage) = setup_reconcile_test();

        // Doc with local_path that doesn't exist on disk — orphan, skip it
        let missing = tmp.path().join("docs").join("Missing.md");
        insert_doc(
            &storage, "doc1", "Different Title",
            Some(&missing.to_string_lossy()),
            None,
        );

        let count = rename_stale_paths(tmp.path(), &storage);
        assert_eq!(count, 0, "Should skip orphan docs (file doesn't exist)");
    }

    // ─── auto-delete unchanged quick notes ────────────────

    fn insert_quick_note(
        storage: &Arc<Mutex<Storage>>,
        note_id: &str,
        title: &str,
        local_path: &str,
        remote_id: Option<&str>,
    ) {
        let content = format!("# {title}\n\n");
        let meta = DocMeta {
            note_id: note_id.to_string(),
            remote_id: remote_id.map(|s| s.to_string()),
            doc_id: note_id.to_string(),
            title: title.to_string(),
            doc_type: "DOCX".to_string(),
            url: String::new(),
            owner_name: String::new(),
            created_at: "2026-01-01".to_string(),
            updated_at: "2026-01-01".to_string(),
            local_path: Some(local_path.to_string()),
            content_hash: Some(hash_content(content.as_bytes())),
            sync_status: SyncStatus::New,
            folder_path: String::new(),
            file_size: None,
            word_count: None,
            sync_state: SyncState::PendingCreate,
            title_mode: "derive_once".to_string(),
            desired_title: None,
            desired_path: None,
        };
        storage.lock().unwrap().upsert_doc(&meta).unwrap();
    }

    #[test]
    fn test_auto_delete_unchanged_quick_note() {
        let (tmp, storage) = setup_reconcile_test();
        let title = "笔记 2026-04-13 14_30_45";
        let content = format!("# {title}\n\n");

        let path = tmp.path().join("docs").join(format!("{title}.md"));
        std::fs::write(&path, &content).unwrap();

        insert_quick_note(
            &storage, "qn1", title,
            &path.to_string_lossy(), None,
        );

        let count = rename_stale_paths(tmp.path(), &storage);
        assert_eq!(count, 1);
        assert!(!path.exists(), "Unchanged quick note file should be deleted");
        assert!(
            storage.lock().unwrap().get_doc("qn1").unwrap().is_none(),
            "DB entry should be deleted for unchanged quick note without remote_id",
        );
    }

    #[test]
    fn test_auto_delete_unchanged_quick_note_with_remote_id() {
        let (tmp, storage) = setup_reconcile_test();
        let title = "笔记 2026-04-13 15_00_00";
        let content = format!("# {title}\n\n");

        let path = tmp.path().join("docs").join(format!("{title}.md"));
        std::fs::write(&path, &content).unwrap();

        insert_quick_note(
            &storage, "qn2", title,
            &path.to_string_lossy(), Some("remote_abc"),
        );

        let count = rename_stale_paths(tmp.path(), &storage);
        assert_eq!(count, 1);
        assert!(!path.exists(), "Unchanged quick note file should be deleted");

        // DB entry should remain but be marked PendingDelete for remote cleanup
        let doc = storage.lock().unwrap().get_doc("qn2").unwrap()
            .expect("DB entry should still exist for note with remote_id");
        assert_eq!(doc.sync_state, SyncState::PendingDelete);
        assert_eq!(doc.title_mode, "manual", "title_mode should be cleared");
    }

    #[test]
    fn test_no_delete_modified_quick_note() {
        let (tmp, storage) = setup_reconcile_test();
        let title = "笔记 2026-04-13 16:00:00";
        let content = format!("# {title}\n\nUser wrote some content here.");

        // Use titled_content_path to match production path (sanitizes colons)
        let path = titled_content_path(tmp.path(), title);
        std::fs::write(&path, &content).unwrap();

        insert_quick_note(
            &storage, "qn3", title,
            &path.to_string_lossy(), None,
        );

        let count = rename_stale_paths(tmp.path(), &storage);
        // Filename already matches title, so no rename needed — but NOT deleted
        assert_eq!(count, 0);
        assert!(path.exists(), "Modified quick note should NOT be deleted");
        assert!(
            storage.lock().unwrap().get_doc("qn3").unwrap().is_some(),
            "DB entry should still exist for modified quick note",
        );
    }

    #[test]
    fn test_no_delete_quick_note_with_new_title() {
        let (tmp, storage) = setup_reconcile_test();
        let title = "笔记 2026-04-13 17_00_00";
        // User changed the heading to a new title
        let content = "# Meeting Notes\n\nSome meeting content";

        let path = tmp.path().join("docs").join(format!("{title}.md"));
        std::fs::write(&path, content).unwrap();

        insert_quick_note(
            &storage, "qn4", title,
            &path.to_string_lossy(), None,
        );

        let count = rename_stale_paths(tmp.path(), &storage);
        // Should rename to "Meeting Notes.md", not delete
        assert_eq!(count, 1);
        assert!(!path.exists(), "Old file should be renamed away");
        let new_path = tmp.path().join("docs").join("Meeting Notes.md");
        assert!(new_path.exists(), "File should be renamed to match new title");
        assert!(
            storage.lock().unwrap().get_doc("qn4").unwrap().is_some(),
            "DB entry should still exist after rename",
        );
    }
}
