use crate::hasher::hash_content;
use larknotes_core::docs_dir;
use larknotes_storage::Storage;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// ─── Types ──────────────────────────────────────────────

/// What changed for a known note.
#[derive(Debug, Clone, PartialEq)]
pub enum ChangeKind {
    ContentChanged,
    MetadataOnly,
}

/// Result of a filesystem scan.
#[derive(Debug, Clone, Default)]
pub struct ScanResult {
    /// Notes whose local file content changed relative to the local baseline.
    pub changed: Vec<(String, ChangeKind)>,
    /// New .md files not associated with any note.
    pub new_files: Vec<PathBuf>,
    /// Notes whose file is missing from disk.
    pub missing: Vec<String>,
    /// Notes whose file moved (matched by content hash).
    pub renamed: Vec<(String, PathBuf)>,
}

// ─── Scanner ────────────────────────────────────────────

/// Increment scan generation and return the new value.
fn next_scan_gen(storage: &Storage) -> i64 {
    // Use app_config to store/increment scan_gen counter
    let current: i64 = storage
        .get_config("scan_gen")
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let next = current + 1;
    let _ = storage.set_config("scan_gen", &next.to_string());
    next
}

/// Perform a full filesystem scan of docs/ and return a ScanResult.
///
/// This reads the filesystem and updates worktree_snapshot in the DB.
/// It does NOT mutate note records or perform any provider I/O.
pub fn scan(workspace: &Path, storage: &Arc<Mutex<Storage>>) -> ScanResult {
    let docs_path = docs_dir(workspace);
    if !docs_path.exists() {
        return ScanResult::default();
    }

    let gen = {
        let store = match storage.lock() {
            Ok(s) => s,
            Err(_) => return ScanResult::default(),
        };
        next_scan_gen(&store)
    };

    // 1. Collect all .md files on disk (skip conflict/temp files)
    let disk_files: Vec<PathBuf> = walkdir::WalkDir::new(&docs_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            let is_md = p.extension().and_then(|e| e.to_str()) == Some("md");
            let fname = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            is_md && !fname.contains(".conflict-") && !fname.starts_with(".~")
        })
        .collect();

    // 2. Build lookup: path → (mtime_ns, size) from disk
    let mut disk_map: HashMap<String, (PathBuf, Option<i64>, Option<i64>)> = HashMap::new();
    for path in &disk_files {
        let path_str = path.to_string_lossy().to_string();
        let (mtime, size) = file_metadata(path);
        disk_map.insert(path_str, (path.clone(), mtime, size));
    }

    // 3. Load existing worktree_snapshot + notes for comparison
    let (snapshots, notes) = {
        let store = match storage.lock() {
            Ok(s) => s,
            Err(_) => return ScanResult::default(),
        };
        let snaps = store.list_worktree_snapshots().unwrap_or_default();
        let notes = store.list_docs().unwrap_or_default();
        (snaps, notes)
    };

    // Build note lookups
    let note_by_path: HashMap<String, &larknotes_core::DocMeta> = notes
        .iter()
        .filter_map(|n| n.local_path.as_ref().map(|p| (p.clone(), n)))
        .collect();

    let _note_by_id: HashMap<&str, &larknotes_core::DocMeta> = notes
        .iter()
        .map(|n| (n.note_id.as_str(), n))
        .collect();

    let snapshot_by_path: HashMap<&str, &larknotes_storage::WorktreeEntry> = snapshots
        .iter()
        .map(|s| (s.observed_path.as_str(), s))
        .collect();

    let snapshot_by_note: HashMap<&str, &larknotes_storage::WorktreeEntry> = snapshots
        .iter()
        .map(|s| (s.note_id.as_str(), s))
        .collect();

    let mut result = ScanResult::default();
    let mut seen_note_ids: HashSet<String> = HashSet::new();

    // 4. Process each file on disk
    for (path_str, (path, mtime, size)) in &disk_map {
        // Is this a known note?
        if let Some(note) = note_by_path.get(path_str.as_str()) {
            seen_note_ids.insert(note.note_id.clone());

            // Check worktree_snapshot for quick skip (mtime+size)
            let snap = snapshot_by_note.get(note.note_id.as_str());
            let mtime_match = snap.map_or(false, |s| s.mtime_ns == *mtime && s.size == *size);

            if mtime_match {
                // File hasn't changed — just update scan_gen
                if let Ok(store) = storage.lock() {
                    let _ = store.upsert_worktree_snapshot(
                        &note.note_id,
                        path_str,
                        *mtime,
                        *size,
                        snap.and_then(|s| s.content_hash.as_deref()),
                        gen,
                    );
                }
                continue;
            }

            // Hash the file
            let content = read_and_decode(path);
            let hash = hash_content(content.as_bytes());

            // Update worktree_snapshot
            if let Ok(store) = storage.lock() {
                let _ = store.upsert_worktree_snapshot(
                    &note.note_id,
                    path_str,
                    *mtime,
                    *size,
                    Some(&hash),
                    gen,
                );
            }

            // Compare against local_base_hash (content_hash in notes table)
            let local_changed = note.content_hash.as_deref() != Some(&hash);
            if local_changed {
                result.changed.push((note.note_id.clone(), ChangeKind::ContentChanged));
            }
        } else {
            // Unknown file — could be renamed or genuinely new
            // Check if path was known under a different note's snapshot
            if let Some(snap) = snapshot_by_path.get(path_str.as_str()) {
                // Path is already in snapshot under some note_id — update
                seen_note_ids.insert(snap.note_id.clone());
                if let Ok(store) = storage.lock() {
                    let _ = store.upsert_worktree_snapshot(
                        &snap.note_id,
                        path_str,
                        *mtime,
                        *size,
                        snap.content_hash.as_deref(),
                        gen,
                    );
                }
            } else {
                // Truly new file
                result.new_files.push(path.clone());
            }
        }
    }

    // 5. Check for missing files (notes with local_path not found on disk)
    // Also check for renames by hash-matching orphaned notes to new files.
    let mut orphan_notes: Vec<(&str, Option<&str>)> = Vec::new();
    for note in &notes {
        if seen_note_ids.contains(&note.note_id) {
            continue;
        }
        // Note wasn't seen on disk
        if note.local_path.is_some() {
            orphan_notes.push((
                &note.note_id,
                note.content_hash.as_deref(),
            ));
        }
    }

    // Try to match orphan notes to new files by content hash
    if !orphan_notes.is_empty() && !result.new_files.is_empty() {
        let mut remaining_new: Vec<PathBuf> = Vec::new();
        let new_file_hashes: Vec<(PathBuf, String)> = result
            .new_files
            .iter()
            .map(|p| {
                let content = read_and_decode(p);
                let hash = hash_content(content.as_bytes());
                (p.clone(), hash)
            })
            .collect();

        let mut matched_orphans: HashSet<String> = HashSet::new();
        let mut matched_files: HashSet<PathBuf> = HashSet::new();

        for (note_id, maybe_hash) in &orphan_notes {
            if let Some(hash) = maybe_hash {
                if let Some(pos) = new_file_hashes.iter().position(|(_, h)| h == *hash && !matched_files.contains(&new_file_hashes[new_file_hashes.iter().position(|(_, hh)| hh == h).unwrap()].0)) {
                    let (path, _) = &new_file_hashes[pos];
                    result.renamed.push((note_id.to_string(), path.clone()));
                    matched_orphans.insert(note_id.to_string());
                    matched_files.insert(path.clone());
                }
            }
        }

        // Remaining new files that didn't match any orphan
        for (path, _) in &new_file_hashes {
            if !matched_files.contains(path) {
                remaining_new.push(path.clone());
            }
        }
        result.new_files = remaining_new;

        // Remaining orphan notes = truly missing
        for (note_id, _) in &orphan_notes {
            if !matched_orphans.contains(*note_id) {
                result.missing.push(note_id.to_string());
            }
        }
    } else {
        // No new files to match against — all orphans are missing
        for (note_id, _) in &orphan_notes {
            result.missing.push(note_id.to_string());
        }
    }

    // 6. Mark stale worktree entries
    if let Ok(store) = storage.lock() {
        let _ = store.mark_stale_worktree(gen);
    }

    result
}

/// Scan a single file path (incremental scan, e.g. after watcher event).
/// Returns Some((note_id, ChangeKind)) if the file belongs to a known note that changed,
/// or None if the file is not known / unchanged.
pub fn scan_single(
    path: &Path,
    _workspace: &Path,
    storage: &Arc<Mutex<Storage>>,
) -> Option<(String, ChangeKind)> {
    let path_str = path.to_string_lossy().to_string();
    let (mtime, size) = file_metadata(path);

    let store = storage.lock().ok()?;
    let note = store.get_doc_by_path(&path_str).ok()??;

    // Quick skip via worktree_snapshot
    if let Ok(Some(snap)) = store.get_worktree_snapshot(&note.note_id) {
        if snap.mtime_ns == mtime && snap.size == size {
            return None;
        }
    }

    drop(store); // Release lock before I/O

    let content = read_and_decode(path);
    let hash = hash_content(content.as_bytes());

    // Update snapshot
    let _gen = {
        let store = storage.lock().ok()?;
        let gen: i64 = store
            .get_config("scan_gen")
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let _ = store.upsert_worktree_snapshot(
            &note.note_id,
            &path_str,
            mtime,
            size,
            Some(&hash),
            gen,
        );
        gen
    };

    let local_changed = note.content_hash.as_deref() != Some(&hash);
    if local_changed {
        Some((note.note_id, ChangeKind::ContentChanged))
    } else {
        None
    }
}

// ─── Helpers ────────────────────────────────────────────

fn file_metadata(path: &Path) -> (Option<i64>, Option<i64>) {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64);
            let size = Some(meta.len() as i64);
            (mtime, size)
        }
        Err(_) => (None, None),
    }
}

fn read_and_decode(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(raw) => crate::util::decode_content(&raw),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use larknotes_core::*;

    fn test_setup() -> (tempfile::TempDir, Arc<Mutex<Storage>>) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("docs")).unwrap();
        let storage = Storage::new_in_memory().unwrap();
        (tmp, Arc::new(Mutex::new(storage)))
    }

    fn create_note(
        workspace: &Path,
        storage: &Arc<Mutex<Storage>>,
        note_id: &str,
        title: &str,
        content: &str,
        set_hash: bool,
    ) -> PathBuf {
        let path = workspace.join("docs").join(format!("{title}.md"));
        std::fs::write(&path, content).unwrap();
        let hash = if set_hash {
            Some(hash_content(content.as_bytes()))
        } else {
            None
        };
        let meta = DocMeta {
            note_id: note_id.to_string(),
            remote_id: Some(format!("remote_{note_id}")),
            doc_id: format!("remote_{note_id}"),
            title: title.to_string(),
            doc_type: "DOCX".to_string(),
            url: String::new(),
            owner_name: "test".to_string(),
            created_at: "2026-01-01".to_string(),
            updated_at: "2026-01-01".to_string(),
            local_path: Some(path.to_string_lossy().to_string()),
            content_hash: hash,
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
        path
    }

    #[test]
    fn scan_detects_new_file() {
        let (tmp, storage) = test_setup();
        let docs = tmp.path().join("docs");
        std::fs::write(docs.join("new.md"), "# New").unwrap();

        let result = scan(tmp.path(), &storage);
        assert_eq!(result.new_files.len(), 1);
        assert!(result.changed.is_empty());
        assert!(result.missing.is_empty());
    }

    #[test]
    fn scan_detects_content_change() {
        let (tmp, storage) = test_setup();
        let path = create_note(tmp.path(), &storage, "n1", "Note1", "# Original", true);

        // Modify the file
        std::fs::write(&path, "# Modified").unwrap();

        let result = scan(tmp.path(), &storage);
        assert_eq!(result.changed.len(), 1);
        assert_eq!(result.changed[0].0, "n1");
        assert_eq!(result.changed[0].1, ChangeKind::ContentChanged);
    }

    #[test]
    fn scan_detects_missing_file() {
        let (tmp, storage) = test_setup();
        let path = create_note(tmp.path(), &storage, "n2", "Note2", "# Will Delete", true);

        // Delete the file
        std::fs::remove_file(&path).unwrap();

        let result = scan(tmp.path(), &storage);
        assert!(result.missing.contains(&"n2".to_string()));
    }

    #[test]
    fn scan_detects_rename_by_hash() {
        let (tmp, storage) = test_setup();
        let content = "# Unique Content For Rename Test";
        let old_path = create_note(tmp.path(), &storage, "n3", "OldName", content, true);

        // Simulate rename: delete old, create new with same content
        std::fs::remove_file(&old_path).unwrap();
        let new_path = tmp.path().join("docs").join("NewName.md");
        std::fs::write(&new_path, content).unwrap();

        let result = scan(tmp.path(), &storage);
        assert_eq!(result.renamed.len(), 1);
        assert_eq!(result.renamed[0].0, "n3");
        assert_eq!(result.renamed[0].1, new_path);
        assert!(result.new_files.is_empty());
        assert!(result.missing.is_empty());
    }

    #[test]
    fn scan_skips_conflict_and_temp_files() {
        let (tmp, storage) = test_setup();
        let docs = tmp.path().join("docs");
        std::fs::write(docs.join("note.conflict-20260101.md"), "conflict").unwrap();
        std::fs::write(docs.join(".~lock.md"), "temp").unwrap();
        std::fs::write(docs.join("normal.md"), "# Normal").unwrap();

        let result = scan(tmp.path(), &storage);
        assert_eq!(result.new_files.len(), 1);
        assert!(result.new_files[0].to_string_lossy().contains("normal"));
    }

    #[test]
    fn scan_no_change_when_synced() {
        let (tmp, storage) = test_setup();
        create_note(tmp.path(), &storage, "n4", "Synced", "# Synced Content", true);

        let result = scan(tmp.path(), &storage);
        assert!(result.changed.is_empty());
        assert!(result.new_files.is_empty());
        assert!(result.missing.is_empty());
    }

    #[test]
    fn scan_new_file_no_base_hash() {
        let (tmp, storage) = test_setup();
        // Note with no content_hash (newly created, never synced)
        create_note(tmp.path(), &storage, "n5", "NoHash", "# No Hash Yet", false);

        let result = scan(tmp.path(), &storage);
        // Should detect as changed since content_hash is None != hash(content)
        assert_eq!(result.changed.len(), 1);
        assert_eq!(result.changed[0].0, "n5");
    }
}
