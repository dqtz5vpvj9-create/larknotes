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

    let store = match storage.lock() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("reconcile_paths: storage lock poisoned: {e}");
            return Vec::new();
        }
    };

    let all_docs = match store.list_docs() {
        Ok(docs) => docs,
        Err(e) => {
            tracing::error!("reconcile_paths: list_docs failed: {e}");
            return Vec::new();
        }
    };

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

    // Find orphan files: .md files in docs/ that are NOT in any doc's local_path
    // Also skip conflict files
    let orphan_files: Vec<PathBuf> = match std::fs::read_dir(&docs_path) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                let is_md = p.extension().and_then(|e| e.to_str()) == Some("md");
                let fname = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let is_conflict = fname.contains(".conflict-");
                let is_known = known_paths.contains(&p.to_string_lossy().to_string());
                is_md && !is_conflict && !is_known
            })
            .collect(),
        Err(e) => {
            tracing::error!("reconcile_paths: read_dir failed: {e}");
            return Vec::new();
        }
    };

    if orphan_files.is_empty() {
        tracing::info!(
            "reconcile_paths: {} orphan doc(s) but no orphan files to match",
            orphan_docs.len()
        );
        return Vec::new();
    }

    let mut matches = Vec::new();
    let mut used_files: HashSet<PathBuf> = HashSet::new();

    // Pass 1: Match by content hash (most reliable)
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
                        if let Err(e) = store.update_local_path(&doc.doc_id, &new_path) {
                            tracing::error!(
                                "reconcile_paths: update_local_path failed for {}: {e}",
                                doc.doc_id
                            );
                            continue;
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
            // Compare the file's title (from content) with doc.title
            if let Ok(content) = std::fs::read_to_string(file_path) {
                let file_title = extract_title(&content);
                if file_title == doc.title {
                    let new_path = file_path.to_string_lossy().to_string();
                    // Also update the content hash while we're at it
                    let new_hash = hash_content(content.as_bytes());
                    if let Err(e) = store.update_local_path(&doc.doc_id, &new_path) {
                        tracing::error!(
                            "reconcile_paths: update_local_path failed for {}: {e}",
                            doc.doc_id
                        );
                        continue;
                    }
                    let _ = store.update_content_hash(&doc.doc_id, &new_hash);
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
}
