use crate::state::AppState;
use larknotes_core::*;
use larknotes_editor::EditorLauncher;
use larknotes_sync::{hash_content, SyncEvent};

fn lock_err(e: impl std::fmt::Display) -> String {
    format!("Internal lock error: {e}")
}

#[tauri::command]
pub async fn get_auth_status(
    state: tauri::State<'_, AppState>,
) -> Result<AuthStatus, String> {
    state.provider.auth_status().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn search_docs(
    state: tauri::State<'_, AppState>,
    query: String,
) -> Result<Vec<DocMeta>, String> {
    state
        .provider
        .search_docs(&query)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn create_doc(
    state: tauri::State<'_, AppState>,
    title: String,
) -> Result<DocMeta, String> {
    let title = if title.is_empty() {
        "Untitled".to_string()
    } else {
        title
    };
    let markdown = format!("# {title}");

    // 1. Write local file + open editor IMMEDIATELY (no network wait)
    let workspace_dir = state.config.read().map_err(lock_err)?.workspace_dir.clone();
    std::fs::create_dir_all(docs_dir(&workspace_dir)).map_err(|e| e.to_string())?;

    let content_path = unique_content_path(&workspace_dir, &title);
    std::fs::write(&content_path, &markdown).map_err(|e| e.to_string())?;

    // Open editor right away — user sees it instantly
    {
        let editor = state.editor.read().map_err(lock_err)?;
        if let Err(e) = editor.open_file(&content_path) {
            tracing::warn!("打开编辑器失败: {e}");
        }
    }

    // 2. Create remote doc (this is the slow part, ~1-2s network call)
    let mut meta = state
        .provider
        .create_doc(&title, &markdown)
        .await
        .map_err(|e| e.to_string())?;

    // 3. Store in DB
    let hash = hash_content(markdown.as_bytes());
    meta.local_path = Some(content_path.to_string_lossy().to_string());
    meta.content_hash = Some(hash);
    meta.sync_status = SyncStatus::Synced;

    state
        .storage
        .lock()
        .map_err(lock_err)?
        .upsert_doc(&meta)
        .map_err(|e| e.to_string())?;

    Ok(meta)
}

#[tauri::command]
pub async fn open_doc_in_editor(
    state: tauri::State<'_, AppState>,
    doc_id: String,
) -> Result<(), String> {
    let workspace_dir = state.config.read().map_err(lock_err)?.workspace_dir.clone();

    // Check if we have a local_path in storage
    let existing_path = state
        .storage
        .lock()
        .map_err(lock_err)?
        .get_doc(&doc_id)
        .ok()
        .flatten()
        .and_then(|d| d.local_path)
        .map(std::path::PathBuf::from)
        .filter(|p| p.exists());

    let cp = if let Some(path) = existing_path {
        path
    } else {
        // Fetch from remote and save with title-based filename
        let content = state
            .provider
            .fetch_doc(&doc_id)
            .await
            .map_err(|e| e.to_string())?;

        std::fs::create_dir_all(docs_dir(&workspace_dir)).map_err(|e| e.to_string())?;

        // Use title from storage or extract from content
        let title = state
            .storage
            .lock()
            .map_err(lock_err)?
            .get_doc(&doc_id)
            .ok()
            .flatten()
            .map(|d| d.title)
            .unwrap_or_else(|| extract_title(&content));

        let cp = unique_content_path(&workspace_dir, &title);
        std::fs::write(&cp, &content).map_err(|e| e.to_string())?;

        // Update storage
        let hash = hash_content(content.as_bytes());
        let store = state.storage.lock().map_err(lock_err)?;
        if let Ok(Some(mut meta)) = store.get_doc(&doc_id) {
            meta.content_hash = Some(hash);
            meta.local_path = Some(cp.to_string_lossy().to_string());
            let _ = store.upsert_doc(&meta);
        }
        cp
    };

    state
        .editor
        .read()
        .map_err(lock_err)?
        .open_file(&cp)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_doc_list(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<DocMeta>, String> {
    state
        .storage
        .lock()
        .map_err(lock_err)?
        .list_docs()
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_app_config(
    state: tauri::State<'_, AppState>,
) -> Result<AppConfig, String> {
    Ok(state.config.read().map_err(lock_err)?.clone())
}

#[tauri::command]
pub async fn set_editor(
    state: tauri::State<'_, AppState>,
    editor: String,
) -> Result<(), String> {
    state.config.write().map_err(lock_err)?.editor_command = editor.clone();
    state.editor.write().map_err(lock_err)?.set_command(&editor);
    state
        .storage
        .lock()
        .map_err(lock_err)?
        .set_config("editor_command", &editor)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_workspace(
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<(), String> {
    let p = std::path::PathBuf::from(&path);
    std::fs::create_dir_all(p.join("docs")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(p.join(".meta")).map_err(|e| e.to_string())?;
    state.config.write().map_err(lock_err)?.workspace_dir = p;
    state
        .storage
        .lock()
        .map_err(lock_err)?
        .set_config("workspace_dir", &path)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn detect_editors() -> Result<Vec<(String, String)>, String> {
    let mut found: Vec<(String, String)> = Vec::new();

    // 1. Check PATH-based editors
    let path_candidates: Vec<(&str, &str)> = vec![
        ("VS Code", "code"),
        ("Notepad++", "notepad++"),
        ("Sublime Text", "subl"),
        ("Vim", "vim"),
        ("Neovim", "nvim"),
    ];
    for (label, cmd) in &path_candidates {
        if which::which(cmd).is_ok() {
            found.push((label.to_string(), cmd.to_string()));
        }
    }

    // 2. Check common Windows install locations for editors not on PATH
    #[cfg(target_os = "windows")]
    {
        let home = dirs::home_dir().unwrap_or_default();
        let program_files = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
        let program_files_x86 = std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
        let local_appdata = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| {
            home.join("AppData").join("Local").to_string_lossy().to_string()
        });

        let win_candidates: Vec<(&str, Vec<std::path::PathBuf>)> = vec![
            ("Typora", vec![
                home.join("Applications").join("Typora").join("Typora.exe"),
                std::path::PathBuf::from(&program_files).join("Typora").join("Typora.exe"),
                std::path::PathBuf::from(&program_files_x86).join("Typora").join("Typora.exe"),
                std::path::PathBuf::from(&local_appdata).join("Programs").join("Typora").join("Typora.exe"),
            ]),
            ("Obsidian", vec![
                std::path::PathBuf::from(&local_appdata).join("Obsidian").join("Obsidian.exe"),
                std::path::PathBuf::from(&program_files).join("Obsidian").join("Obsidian.exe"),
            ]),
            ("VS Code", vec![
                std::path::PathBuf::from(&local_appdata).join("Programs").join("Microsoft VS Code").join("Code.exe"),
                std::path::PathBuf::from(&program_files).join("Microsoft VS Code").join("Code.exe"),
            ]),
            ("Notepad++", vec![
                std::path::PathBuf::from(&program_files).join("Notepad++").join("notepad++.exe"),
                std::path::PathBuf::from(&program_files_x86).join("Notepad++").join("notepad++.exe"),
            ]),
            ("Sublime Text", vec![
                std::path::PathBuf::from(&program_files).join("Sublime Text").join("sublime_text.exe"),
                std::path::PathBuf::from(&program_files).join("Sublime Text 3").join("sublime_text.exe"),
            ]),
        ];

        for (label, paths) in win_candidates {
            // Skip if already found via PATH
            if found.iter().any(|(l, _)| l == label) {
                continue;
            }
            for p in paths {
                if p.exists() {
                    found.push((label.to_string(), p.to_string_lossy().to_string()));
                    break;
                }
            }
        }
    }

    // 3. Always include Notepad as fallback on Windows
    #[cfg(target_os = "windows")]
    {
        if !found.iter().any(|(l, _)| l == "记事本") {
            found.push(("记事本".to_string(), "notepad".to_string()));
        }
    }

    if found.is_empty() {
        found.push(("记事本".to_string(), "notepad".to_string()));
    }
    Ok(found)
}

#[tauri::command]
pub async fn pick_folder() -> Result<Option<String>, String> {
    let folder = rfd::FileDialog::new()
        .set_title("选择工作区文件夹")
        .pick_folder();
    Ok(folder.map(|f| f.to_string_lossy().to_string()))
}

#[tauri::command]
pub async fn manual_sync(
    state: tauri::State<'_, AppState>,
    doc_id: String,
) -> Result<(), String> {
    state
        .sync_tx
        .send(SyncEvent::SyncRequested { doc_id })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn import_doc(
    state: tauri::State<'_, AppState>,
    doc_id: String,
) -> Result<DocMeta, String> {
    // 1. Fetch content from remote
    let content = state
        .provider
        .fetch_doc(&doc_id)
        .await
        .map_err(|e| e.to_string())?;

    let workspace_dir = state.config.read().map_err(lock_err)?.workspace_dir.clone();
    std::fs::create_dir_all(docs_dir(&workspace_dir)).map_err(|e| e.to_string())?;

    // 2. Get metadata via search (title, url, owner)
    let title = extract_title(&content);
    let search_results = state
        .provider
        .search_docs(&title)
        .await
        .unwrap_or_default();
    let remote_meta = search_results.iter().find(|d| d.doc_id == doc_id);

    // 3. Write local file
    let content_path = unique_content_path(&workspace_dir, &title);
    std::fs::write(&content_path, &content).map_err(|e| e.to_string())?;

    // 4. Build and store meta
    let hash = hash_content(content.as_bytes());
    let meta = DocMeta {
        doc_id: doc_id.clone(),
        title: remote_meta.map(|m| m.title.clone()).unwrap_or_else(|| title.clone()),
        doc_type: remote_meta.map(|m| m.doc_type.clone()).unwrap_or_else(|| "DOCX".to_string()),
        url: remote_meta.map(|m| m.url.clone()).unwrap_or_default(),
        owner_name: remote_meta.map(|m| m.owner_name.clone()).unwrap_or_default(),
        created_at: remote_meta.map(|m| m.created_at.clone()).unwrap_or_default(),
        updated_at: remote_meta.map(|m| m.updated_at.clone()).unwrap_or_default(),
        local_path: Some(content_path.to_string_lossy().to_string()),
        content_hash: Some(hash),
        sync_status: SyncStatus::Synced,
    };

    state
        .storage
        .lock()
        .map_err(lock_err)?
        .upsert_doc(&meta)
        .map_err(|e| e.to_string())?;

    // 5. Open in editor
    {
        let editor = state.editor.read().map_err(lock_err)?;
        if let Err(e) = editor.open_file(&content_path) {
            tracing::warn!("打开编辑器失败: {e}");
        }
    }

    Ok(meta)
}

#[tauri::command]
pub async fn delete_doc(
    state: tauri::State<'_, AppState>,
    doc_id: String,
) -> Result<(), String> {
    // Get local path before deleting from DB
    let local_path = state
        .storage
        .lock()
        .map_err(lock_err)?
        .get_doc(&doc_id)
        .ok()
        .flatten()
        .and_then(|d| d.local_path);

    // Delete from DB
    state
        .storage
        .lock()
        .map_err(lock_err)?
        .delete_doc(&doc_id)
        .map_err(|e| e.to_string())?;

    // Delete local file (if exists)
    if let Some(path) = local_path {
        let _ = std::fs::remove_file(&path);
    }

    // Delete meta JSON
    let workspace_dir = state.config.read().map_err(lock_err)?.workspace_dir.clone();
    let _ = std::fs::remove_file(meta_path(&workspace_dir, &doc_id));

    Ok(())
}

#[tauri::command]
pub async fn reveal_in_explorer(
    state: tauri::State<'_, AppState>,
    doc_id: String,
) -> Result<(), String> {
    let local_path = state
        .storage
        .lock()
        .map_err(lock_err)?
        .get_doc(&doc_id)
        .ok()
        .flatten()
        .and_then(|d| d.local_path)
        .map(std::path::PathBuf::from);

    match local_path {
        Some(p) if p.exists() => {
            EditorLauncher::open_in_explorer(&p).map_err(|e| e.to_string())
        }
        _ => Err("本地文件不存在".to_string()),
    }
}

#[tauri::command]
pub async fn get_sync_history(
    state: tauri::State<'_, AppState>,
    doc_id: String,
) -> Result<Vec<SyncHistoryEntry>, String> {
    state
        .storage
        .lock()
        .map_err(lock_err)?
        .get_sync_history(&doc_id, 50)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_snapshots(
    state: tauri::State<'_, AppState>,
    doc_id: String,
) -> Result<Vec<VersionSnapshot>, String> {
    state
        .storage
        .lock()
        .map_err(lock_err)?
        .get_snapshots(&doc_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn restore_snapshot(
    state: tauri::State<'_, AppState>,
    doc_id: String,
    snapshot_id: i64,
) -> Result<(), String> {
    // 1. Get snapshot content
    let snapshot = state
        .storage
        .lock()
        .map_err(lock_err)?
        .get_snapshot_by_id(snapshot_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "快照不存在".to_string())?;

    // 2. Get local path
    let local_path = state
        .storage
        .lock()
        .map_err(lock_err)?
        .get_doc(&doc_id)
        .ok()
        .flatten()
        .and_then(|d| d.local_path)
        .ok_or_else(|| "文档本地路径不存在".to_string())?;

    // 3. Write content to local file
    std::fs::write(&local_path, &snapshot.content).map_err(|e| e.to_string())?;

    // 4. Trigger sync
    state
        .sync_tx
        .send(SyncEvent::SyncRequested { doc_id })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn quick_note(
    state: tauri::State<'_, AppState>,
) -> Result<DocMeta, String> {
    let title = chrono::Local::now().format("笔记 %Y-%m-%d %H:%M").to_string();
    let markdown = format!("# {title}\n\n");

    let workspace_dir = state.config.read().map_err(lock_err)?.workspace_dir.clone();
    std::fs::create_dir_all(docs_dir(&workspace_dir)).map_err(|e| e.to_string())?;

    let content_path = unique_content_path(&workspace_dir, &title);
    std::fs::write(&content_path, &markdown).map_err(|e| e.to_string())?;

    // Open editor immediately
    {
        let editor = state.editor.read().map_err(lock_err)?;
        if let Err(e) = editor.open_file(&content_path) {
            tracing::warn!("打开编辑器失败: {e}");
        }
    }

    // Create remote doc (async, may take 1-2s)
    let mut meta = state
        .provider
        .create_doc(&title, &markdown)
        .await
        .map_err(|e| e.to_string())?;

    // Store in DB
    let hash = hash_content(markdown.as_bytes());
    meta.local_path = Some(content_path.to_string_lossy().to_string());
    meta.content_hash = Some(hash);
    meta.sync_status = SyncStatus::Synced;

    state
        .storage
        .lock()
        .map_err(lock_err)?
        .upsert_doc(&meta)
        .map_err(|e| e.to_string())?;

    Ok(meta)
}

#[tauri::command]
pub async fn get_autostart_status(
    app: tauri::AppHandle,
) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch()
        .is_enabled()
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_autostart(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let manager = app.autolaunch();
    if enabled {
        manager.enable().map_err(|e| e.to_string())
    } else {
        manager.disable().map_err(|e| e.to_string())
    }
}
