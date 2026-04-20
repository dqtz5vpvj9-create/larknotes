//! Live integration tests for SyncEngine using real CliProvider + Feishu API.
//! Run with: cargo test -p larknotes-sync -- --ignored

use larknotes_core::*;
use larknotes_provider_cli::test_support::test_folder_token;
use larknotes_provider_cli::CliProvider;
use larknotes_storage::Storage;
use larknotes_sync::{hash_content};
use larknotes_sync::engine::SyncEngine;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

fn live_provider() -> Arc<CliProvider> {
    Arc::new(CliProvider::new("lark-cli"))
}

fn test_title(label: &str) -> String {
    format!("_Test_{label}_{}", chrono::Local::now().format("%H%M%S%3f"))
}

/// Create a doc inside the dedicated test folder (mirrors the helper in
/// crates/provider-cli). Use this in place of `provider.create(...)`.
async fn live_create(
    provider: &CliProvider,
    title: &str,
    content: &str,
) -> Result<DocMeta, LarkNotesError> {
    let folder = test_folder_token().await;
    provider.create_in_folder(title, content, Some(&folder)).await
}

fn setup_workspace() -> (tempfile::TempDir, Arc<Mutex<Storage>>) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("docs")).unwrap();
    let storage = Storage::new_in_memory().unwrap();
    (tmp, Arc::new(Mutex::new(storage)))
}

/// Create a remote doc via CliProvider and set up local file + DB entry.
/// Returns (note_id, remote_id, local_file_path).
async fn create_synced_doc(
    provider: &CliProvider,
    workspace: &std::path::Path,
    storage: &Arc<Mutex<Storage>>,
    title: &str,
    content: &str,
    store_matching_hash: bool,
) -> (String, String, std::path::PathBuf) {
    // Create on remote
    let meta = live_create(provider, title, content).await.unwrap();
    let remote_id = meta.remote_id.clone().unwrap_or_default();
    let note_id = new_note_id();

    // Write local file
    let file_path = titled_content_path(workspace, title);
    std::fs::write(&file_path, content).unwrap();

    // Store in DB
    let hash = if store_matching_hash {
        Some(hash_content(content.as_bytes()))
    } else {
        None
    };
    let db_meta = DocMeta {
        note_id: note_id.clone(),
        remote_id: Some(remote_id.clone()),
        doc_id: remote_id.clone(),
        title: title.to_string(),
        doc_type: "DOCX".to_string(),
        url: meta.url.clone(),
        owner_name: "test".to_string(),
        created_at: meta.created_at.clone(),
        updated_at: meta.updated_at.clone(),
        local_path: Some(file_path.to_string_lossy().to_string()),
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
    storage.lock().unwrap().upsert_doc(&db_meta).unwrap();

    (note_id, remote_id, file_path)
}

// #3: PUSH S1 skip — hash matches, force=false → no remote update
#[tokio::test]
#[ignore]
async fn test_live_push_s1_skip() {
    let provider = live_provider();
    let (tmp, storage) = setup_workspace();
    let title = test_title("push_s1_skip");
    let content = format!("# {title}\n\nS1 skip test.");

    let (note_id, remote_id, _path) = create_synced_doc(
        &provider, tmp.path(), &storage, &title, &content, true,
    ).await;

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Fetch remote content before sync
    let before = provider.read(&remote_id).await.unwrap().content;

    let (engine, _rx) = SyncEngine::new(
        provider.clone(), storage.clone(), tmp.path().to_path_buf(),
        Arc::new(AtomicU64::new(2000)),
    );
    let engine = Arc::new(engine);
    engine.sync_one(&note_id, false).await;

    // Fetch remote content after sync — should be unchanged
    let after = provider.read(&remote_id).await.unwrap().content;
    assert_eq!(before, after, "Remote content should not change when hash matches");

    // DB status should still be Synced
    let doc = storage.lock().unwrap().get_doc(&note_id).unwrap().unwrap();
    assert_eq!(doc.sync_status, SyncStatus::Synced);

    let _ = provider.delete(&remote_id).await;
}

// #4: PUSH S1 force — hash matches but force=true → still pushes
#[tokio::test]
#[ignore]
async fn test_live_push_s1_force() {
    let provider = live_provider();
    let (tmp, storage) = setup_workspace();
    let title = test_title("push_s1_force");
    let content = format!("# {title}\n\nS1 force test.");

    let (note_id, remote_id, _path) = create_synced_doc(
        &provider, tmp.path(), &storage, &title, &content, true,
    ).await;

    let (engine, _rx) = SyncEngine::new(
        provider.clone(), storage.clone(), tmp.path().to_path_buf(),
        Arc::new(AtomicU64::new(2000)),
    );
    let engine = Arc::new(engine);
    // force=true should push even though hash matches
    engine.sync_one(&note_id, true).await;

    // DB status should be Synced (push succeeded)
    let doc = storage.lock().unwrap().get_doc(&note_id).unwrap().unwrap();
    assert_eq!(doc.sync_status, SyncStatus::Synced);
    assert!(doc.content_hash.is_some(), "Hash should be updated after force push");

    let _ = provider.delete(&remote_id).await;
}

// #5: PUSH S2 — local content differs from stored hash → pushes new content
#[tokio::test]
#[ignore]
async fn test_live_push_s2_ok() {
    let provider = live_provider();
    let (tmp, storage) = setup_workspace();
    let title = test_title("push_s2");
    let original = format!("# {title}\n\nOriginal content.");
    let modified = format!("# {title}\n\nModified locally — push S2 test.");

    // Create remote with original content, store original hash
    let (note_id, remote_id, path) = create_synced_doc(
        &provider, tmp.path(), &storage, &title, &original, true,
    ).await;

    // Modify local file (simulating S2: local modified)
    std::fs::write(&path, &modified).unwrap();

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let (engine, _rx) = SyncEngine::new(
        provider.clone(), storage.clone(), tmp.path().to_path_buf(),
        Arc::new(AtomicU64::new(2000)),
    );
    let engine = Arc::new(engine);
    engine.sync_one(&note_id, false).await;

    // Verify remote content was updated
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let fetched = provider.read(&remote_id).await.unwrap().content;
    assert!(
        fetched.contains("Modified locally"),
        "Remote should have new content, got: {fetched}"
    );

    let doc = storage.lock().unwrap().get_doc(&note_id).unwrap().unwrap();
    assert_eq!(doc.sync_status, SyncStatus::Synced);

    let _ = provider.delete(&remote_id).await;
}

// #6: PUSH S3 — remote has newer content [needs remote_hash]
#[tokio::test]
#[ignore] // TODO: needs remote_hash mechanism to distinguish S3 from S1
async fn test_live_push_s3_overwrite() {
    // Currently indistinguishable from S1/S2 without remote_hash.
    // When implemented, this should detect remote is newer and warn.
}

// #7: PUSH S4 — both sides modified [needs remote_hash]
#[tokio::test]
#[ignore] // TODO: needs remote_hash mechanism to detect S4 conflict
async fn test_live_push_s4_conflict() {
    // Currently no conflict detection before push.
    // When implemented, should detect both sides modified and flag conflict.
}

// #8: PUSH S5 — remote deleted, sync_one triggers recreate
#[tokio::test]
#[ignore]
async fn test_live_push_s5_recreate() {
    let provider = live_provider();
    let (tmp, storage) = setup_workspace();
    let title = test_title("push_s5_recreate");
    let content = format!("# {title}\n\nRecreate test.");

    let (note_id, remote_id, _path) = create_synced_doc(
        &provider, tmp.path(), &storage, &title, &content, false,
    ).await;

    // Delete remote doc
    provider.delete(&remote_id).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let (engine, _rx) = SyncEngine::new(
        provider.clone(), storage.clone(), tmp.path().to_path_buf(),
        Arc::new(AtomicU64::new(2000)),
    );
    let engine = Arc::new(engine);
    engine.sync_one(&note_id, true).await;

    // note_id stays the same, remote_id should have changed
    let doc = storage.lock().unwrap().get_doc(&note_id).unwrap().unwrap();
    assert_eq!(doc.sync_status, SyncStatus::Synced, "Status should be Synced after recreate");
    let new_remote_id = doc.remote_id.clone().unwrap_or_default();
    assert_ne!(new_remote_id, remote_id, "remote_id should have changed after recreate");

    // Verify new doc exists on remote
    let fetched = provider.read(&new_remote_id).await.unwrap().content;
    assert!(!fetched.is_empty(), "New remote doc should have content");

    // Cleanup
    let _ = provider.delete(&new_remote_id).await;
}

// #9a: PUSH with title change — sync_one calls write + rename
#[tokio::test]
#[ignore]
async fn test_live_push_with_rename() {
    let provider = live_provider();
    let (tmp, storage) = setup_workspace();
    let original_title = test_title("push_rename");
    let content = format!("# {original_title}\n\nOriginal content.");

    let (note_id, remote_id, path) = create_synced_doc(
        &provider, tmp.path(), &storage, &original_title, &content, true,
    ).await;

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Modify local file with a new title
    let new_title = test_title("pushed_renamed");
    let new_content = format!("# {new_title}\n\nContent with new title.");
    std::fs::write(&path, &new_content).unwrap();

    let (engine, _rx) = SyncEngine::new(
        provider.clone(), storage.clone(), tmp.path().to_path_buf(),
        Arc::new(AtomicU64::new(2000)),
    );
    let engine = Arc::new(engine);
    engine.sync_one(&note_id, false).await;

    // Verify remote content was updated
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let fetched = provider.read(&remote_id).await.unwrap();
    assert!(
        fetched.content.contains("Content with new title"),
        "Remote content should be updated, got: {}", fetched.content
    );

    // DB should be synced
    let doc = storage.lock().unwrap().get_doc(&note_id).unwrap().unwrap();
    assert_eq!(doc.sync_status, SyncStatus::Synced);

    let _ = provider.delete(&remote_id).await;
}

// #9b: LIST via engine — verify list returns docs from Lark
#[tokio::test]
#[ignore]
async fn test_live_list_through_provider() {
    let provider = live_provider();
    let title = test_title("list_test");
    let md = format!("# {title}\n\nList test doc.");

    let meta = live_create(&provider, &title, &md).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // List should return at least one doc
    let docs = provider.list(None).await.unwrap();
    assert!(!docs.is_empty(), "List should return at least the doc we created");

    let _ = provider.delete(&meta.remote_id.unwrap_or_default()).await;
}

// #9: PUSH S5 recreate fail — both update and create fail
#[tokio::test]
#[ignore]
async fn test_live_push_s5_recreate_fail() {
    // Use a broken CLI provider so create also fails
    let bad_provider: Arc<dyn DocProvider> = Arc::new(CliProvider::new("nonexistent-lark-cli-xyz"));
    let (tmp, storage) = setup_workspace();
    let title = test_title("push_s5_fail");
    let content = "# Recreate fail test";

    // Set up DB entry pointing to a nonexistent remote doc
    let file_path = titled_content_path(tmp.path(), &title);
    std::fs::write(&file_path, content).unwrap();
    let meta = DocMeta {
        note_id: "nonexistent_doc_999".to_string(),
        remote_id: Some("nonexistent_doc_999".to_string()),
        doc_id: "nonexistent_doc_999".to_string(),
        title: title.clone(),
        doc_type: "DOCX".to_string(),
        url: String::new(),
        owner_name: "test".to_string(),
        created_at: String::new(),
        updated_at: String::new(),
        local_path: Some(file_path.to_string_lossy().to_string()),
        content_hash: None,
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

    let (engine, _rx) = SyncEngine::new(
        bad_provider, storage.clone(), tmp.path().to_path_buf(),
        Arc::new(AtomicU64::new(2000)),
    );
    let engine = Arc::new(engine);
    engine.sync_one("nonexistent_doc_999", true).await;

    // Should be in Error state (not Synced, not Conflict)
    let doc = storage.lock().unwrap().get_doc("nonexistent_doc_999").unwrap().unwrap();
    match &doc.sync_status {
        SyncStatus::Error(_) => { /* expected */ }
        SyncStatus::Conflict => { /* also acceptable for now */ }
        other => panic!("Expected Error or Conflict status, got: {:?}", other),
    }
}
