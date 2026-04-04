use crate::state::AppState;
use larknotes_core::*;
use larknotes_editor::EditorLauncher;
use larknotes_sync::{hash_content, SyncEvent};

fn lock_err(e: impl std::fmt::Display) -> String {
    format!("Internal lock error: {e}")
}

/// Shared logic: pull remote content into local DB + file.
/// Used by `pull_doc` and `resolve_conflict("keep_remote")`.
fn sync_from_remote(
    state: &AppState,
    doc_id: &str,
    content: &str,
    meta: &mut DocMeta,
    action: &str,
) -> Result<(), String> {
    let workspace_dir = state.config.read().map_err(lock_err)?.workspace_dir.clone();
    let old_local_path = meta.local_path.as_ref().map(std::path::PathBuf::from);

    std::fs::create_dir_all(docs_dir(&workspace_dir)).map_err(|e| e.to_string())?;

    let new_title = extract_title(content);
    let ideal_path = unique_content_path(&workspace_dir, &new_title);

    // Rename file if title changed
    let local_path = match &old_local_path {
        Some(old) if old.exists() && *old != ideal_path => {
            match std::fs::rename(old, &ideal_path) {
                Ok(()) => {
                    tracing::info!("{action}: 文件已重命名 {} → {}", old.display(), ideal_path.display());
                    ideal_path
                }
                Err(e) => {
                    tracing::warn!("{action}: 重命名文件失败: {e}");
                    old.clone()
                }
            }
        }
        Some(old) if old.exists() => old.clone(),
        _ => ideal_path,
    };

    // Update DB BEFORE writing file to prevent watcher push-back
    let hash = hash_content(content.as_bytes());
    meta.local_path = Some(local_path.to_string_lossy().to_string());
    meta.content_hash = Some(hash.clone());
    meta.sync_status = SyncStatus::Synced;
    meta.title = new_title;

    {
        let store = state.storage.lock().map_err(lock_err)?;
        store.upsert_doc(meta).map_err(|e| e.to_string())?;
        store.add_sync_history(doc_id, action, Some(&hash)).map_err(|e| e.to_string())?;
        store.save_snapshot(doc_id, content, &hash).map_err(|e| e.to_string())?;
    }

    // Write file AFTER DB so watcher sees matching hash and skips
    std::fs::write(&local_path, content).map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
pub async fn get_auth_status(
    state: tauri::State<'_, AppState>,
) -> Result<AuthStatus, String> {
    state.auth.auth_status().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn search_docs(
    state: tauri::State<'_, AppState>,
    query: String,
) -> Result<Vec<DocMeta>, String> {
    state
        .provider
        .search(&query)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn create_doc(
    state: tauri::State<'_, AppState>,
    title: String,
    #[allow(unused_variables)]
    folder_path: Option<String>,
) -> Result<DocMeta, String> {
    let title = if title.is_empty() {
        "Untitled".to_string()
    } else {
        title
    };
    let folder = folder_path.unwrap_or_default();
    let markdown = format!("# {title}");

    let workspace_dir = state.config.read().map_err(lock_err)?.workspace_dir.clone();
    let target_dir = if folder.is_empty() {
        docs_dir(&workspace_dir)
    } else {
        docs_dir(&workspace_dir).join(&folder)
    };
    std::fs::create_dir_all(&target_dir).map_err(|e| e.to_string())?;

    // 1. Create remote doc first (the slow part, ~1-2s)
    let mut meta = state
        .provider
        .create(&title, &markdown)
        .await
        .map_err(|e| e.to_string())?;

    // 2. Store in DB with the correct hash BEFORE writing the local file.
    //    This prevents the file watcher from seeing a new file without a
    //    matching doc_id/hash in the DB and triggering an unwanted sync.
    let content_path = unique_content_path_in(&workspace_dir, &folder, &title);
    let hash = hash_content(markdown.as_bytes());
    meta.local_path = Some(content_path.to_string_lossy().to_string());
    meta.content_hash = Some(hash);
    meta.sync_status = SyncStatus::Synced;
    meta.folder_path = folder;

    state
        .storage
        .lock()
        .map_err(lock_err)?
        .upsert_doc(&meta)
        .map_err(|e| e.to_string())?;

    // 3. Write local file + open editor
    std::fs::write(&content_path, &markdown).map_err(|e| e.to_string())?;

    {
        let editor = state.editor.read().map_err(lock_err)?;
        match editor.open_file(&content_path) {
            Ok(child) => {
                let filename = content_path.file_name().unwrap().to_string_lossy().to_string();
                state.window_monitor.track_with_child(&meta.doc_id, &filename, Some(child));
            }
            Err(e) => tracing::warn!("打开编辑器失败: {e}"),
        }
    }

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
        let read_output = state
            .provider
            .read(&doc_id)
            .await
            .map_err(|e| e.to_string())?;
        let content = read_output.content;

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

    let editor = state.editor.read().map_err(lock_err)?;
    match editor.open_file(&cp) {
        Ok(child) => {
            let filename = cp.file_name().unwrap().to_string_lossy().to_string();
            state.window_monitor.track_with_child(&doc_id, &filename, Some(child));
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
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
                std::path::PathBuf::from(&program_files).join("Typora").join("Typora.exe"),
                std::path::PathBuf::from(&program_files_x86).join("Typora").join("Typora.exe"),
                std::path::PathBuf::from(&local_appdata).join("Programs").join("Typora").join("Typora.exe"),
                home.join("Applications").join("Typora").join("Typora.exe"),
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

    // 3. Check common macOS install locations
    #[cfg(target_os = "macos")]
    {
        let mac_candidates: Vec<(&str, &str)> = vec![
            ("Typora", "/Applications/Typora.app/Contents/MacOS/Typora"),
            ("Obsidian", "/Applications/Obsidian.app/Contents/MacOS/Obsidian"),
            ("VS Code", "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"),
            ("Sublime Text", "/Applications/Sublime Text.app/Contents/SharedSupport/bin/subl"),
        ];
        for (label, path) in mac_candidates {
            if found.iter().any(|(l, _)| l == label) {
                continue;
            }
            if std::path::Path::new(path).exists() {
                found.push((label.to_string(), path.to_string()));
            }
        }
    }

    if found.is_empty() {
        #[cfg(target_os = "windows")]
        found.push(("记事本".to_string(), "notepad".to_string()));
        #[cfg(target_os = "macos")]
        found.push(("TextEdit".to_string(), "open -a TextEdit".to_string()));
        #[cfg(target_os = "linux")]
        found.push(("gedit".to_string(), "gedit".to_string()));
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
    let read_output = state
        .provider
        .read(&doc_id)
        .await
        .map_err(|e| e.to_string())?;
    let content = read_output.content;

    let workspace_dir = state.config.read().map_err(lock_err)?.workspace_dir.clone();
    std::fs::create_dir_all(docs_dir(&workspace_dir)).map_err(|e| e.to_string())?;

    // 2. Get metadata via search (title, url, owner)
    let title = extract_title(&content);
    let search_results = state
        .provider
        .search(&title)
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
        folder_path: String::new(),
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
        match editor.open_file(&content_path) {
            Ok(child) => {
                let filename = content_path.file_name().unwrap().to_string_lossy().to_string();
                state.window_monitor.track_with_child(&doc_id, &filename, Some(child));
            }
            Err(e) => tracing::warn!("打开编辑器失败: {e}"),
        }
    }

    Ok(meta)
}

#[tauri::command]
pub async fn delete_doc(
    state: tauri::State<'_, AppState>,
    doc_id: String,
    force_local: Option<bool>,
) -> Result<(), String> {
    // Try remote delete first (unless user already confirmed local-only)
    if !force_local.unwrap_or(false) {
        if let Err(e) = state.provider.delete(&doc_id).await {
            // Return error with a prefix so frontend can distinguish and show confirm dialog
            return Err(format!("REMOTE_DELETE_FAILED:{e}"));
        }
    }

    // Delete from DB and local file
    let local_path = {
        let store = state.storage.lock().map_err(lock_err)?;
        let path = store.get_doc(&doc_id).ok().flatten().and_then(|d| d.local_path);
        store.delete_doc(&doc_id).map_err(|e| e.to_string())?;
        path
    };

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
    let title = chrono::Local::now().format("笔记 %Y-%m-%d %H:%M:%S").to_string();
    let markdown = format!("# {title}\n\n");

    let workspace_dir = state.config.read().map_err(lock_err)?.workspace_dir.clone();
    std::fs::create_dir_all(docs_dir(&workspace_dir)).map_err(|e| e.to_string())?;

    // 1. Create remote doc first
    let mut meta = state
        .provider
        .create(&title, &markdown)
        .await
        .map_err(|e| e.to_string())?;

    // 2. DB insert BEFORE file write (prevents watcher race)
    let content_path = unique_content_path(&workspace_dir, &title);
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

    // 3. Write file + open editor
    std::fs::write(&content_path, &markdown).map_err(|e| e.to_string())?;

    {
        let editor = state.editor.read().map_err(lock_err)?;
        match editor.open_file(&content_path) {
            Ok(child) => {
                let filename = content_path.file_name().unwrap().to_string_lossy().to_string();
                state.window_monitor.track_with_child(&meta.doc_id, &filename, Some(child));
            }
            Err(e) => tracing::warn!("打开编辑器失败: {e}"),
        }
    }

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

#[tauri::command]
pub async fn pull_doc(
    state: tauri::State<'_, AppState>,
    doc_id: String,
) -> Result<DocMeta, String> {
    let read_output = state
        .provider
        .read(&doc_id)
        .await
        .map_err(|e| e.to_string())?;

    let mut meta = state
        .storage
        .lock()
        .map_err(lock_err)?
        .get_doc(&doc_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "文档不存在".to_string())?;

    sync_from_remote(&state, &doc_id, &read_output.content, &mut meta, "pull")?;

    Ok(meta)
}

#[tauri::command]
pub async fn search_docs_local(
    state: tauri::State<'_, AppState>,
    query: String,
) -> Result<Vec<DocMeta>, String> {
    state
        .storage
        .lock()
        .map_err(lock_err)?
        .search_docs_local(&query)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_sync_debounce(
    state: tauri::State<'_, AppState>,
    ms: u64,
) -> Result<(), String> {
    state.config.write().map_err(lock_err)?.sync_debounce_ms = ms;
    // Update the running sync engine's debounce immediately
    state.debounce_ms.store(ms, std::sync::atomic::Ordering::Relaxed);
    state
        .storage
        .lock()
        .map_err(lock_err)?
        .set_config("sync_debounce_ms", &ms.to_string())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_auto_sync(
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    state.config.write().map_err(lock_err)?.auto_sync = enabled;
    state
        .storage
        .lock()
        .map_err(lock_err)?
        .set_config("auto_sync", if enabled { "true" } else { "false" })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_provider_cli_path(
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<(), String> {
    state.config.write().map_err(lock_err)?.provider_cli_path = path.clone();
    // Update the running CLI provider immediately
    state.cli_provider.set_cli_path(&path);
    state
        .storage
        .lock()
        .map_err(lock_err)?
        .set_config("provider_cli_path", &path)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn open_login_url(
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let cli_path = state.config.read().map_err(lock_err)?.provider_cli_path.clone();
    // Return the command the user needs to run
    Ok(format!("{} auth login", cli_path))
}

#[tauri::command]
pub async fn resolve_conflict(
    state: tauri::State<'_, AppState>,
    doc_id: String,
    resolution: String,  // "keep_local" or "keep_remote"
) -> Result<DocMeta, String> {
    if resolution == "keep_remote" {
        // Pull remote content and overwrite local
        let read_output = state
            .provider
            .read(&doc_id)
            .await
            .map_err(|e| e.to_string())?;

        let mut meta = state
            .storage
            .lock()
            .map_err(lock_err)?
            .get_doc(&doc_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "文档不存在".to_string())?;

        sync_from_remote(&state, &doc_id, &read_output.content, &mut meta, "pull")?;

        Ok(meta)
    } else {
        // keep_local: push local content to remote
        let meta = state
            .storage
            .lock()
            .map_err(lock_err)?
            .get_doc(&doc_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "文档不存在".to_string())?;

        let local_path = meta.local_path
            .as_ref()
            .ok_or_else(|| "本地文件不存在".to_string())?;

        let content = std::fs::read_to_string(local_path).map_err(|e| e.to_string())?;

        state
            .provider
            .write(&doc_id, &content)
            .await
            .map_err(|e| e.to_string())?;

        let hash = hash_content(content.as_bytes());
        let mut meta = meta;
        meta.content_hash = Some(hash.clone());
        meta.sync_status = SyncStatus::Synced;

        let store = state.storage.lock().map_err(lock_err)?;
        store.upsert_doc(&meta).map_err(|e| e.to_string())?;
        store.add_sync_history(&doc_id, "push", Some(&hash)).map_err(|e| e.to_string())?;
        store.save_snapshot(&doc_id, &content, &hash).map_err(|e| e.to_string())?;

        Ok(meta)
    }
}

#[tauri::command]
pub async fn get_conflict_diff(
    state: tauri::State<'_, AppState>,
    doc_id: String,
) -> Result<(String, String), String> {
    // Get local content
    let meta = state
        .storage
        .lock()
        .map_err(lock_err)?
        .get_doc(&doc_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "文档不存在".to_string())?;

    let local_content = match &meta.local_path {
        Some(path) => std::fs::read_to_string(path).unwrap_or_default(),
        None => String::new(),
    };

    // Get remote content
    let remote_content = match state.provider.read(&doc_id).await {
        Ok(output) => output.content,
        Err(_) => String::new(),
    };

    Ok((local_content, remote_content))
}

// ─── Folder commands ──────────────────────────────────

#[derive(serde::Serialize)]
pub struct FolderTreeNode {
    pub name: String,
    pub path: String,
    pub children: Vec<FolderTreeNode>,
    pub doc_count: usize,
}

#[tauri::command]
pub fn get_folder_tree(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<FolderTreeNode>, String> {
    let store = state.storage.lock().map_err(lock_err)?;
    let folders = store.list_folders().map_err(|e| e.to_string())?;
    let all_docs = store.list_docs().map_err(|e| e.to_string())?;

    // Count docs per folder
    let mut doc_counts = std::collections::HashMap::new();
    for doc in &all_docs {
        *doc_counts.entry(doc.folder_path.clone()).or_insert(0usize) += 1;
    }

    // Build tree from flat list of folder paths
    let mut root_children: Vec<FolderTreeNode> = Vec::new();

    for folder in &folders {
        let parts: Vec<&str> = folder.folder_path.split('/').collect();
        insert_into_tree(&mut root_children, &parts, 0, &folder.folder_path, &doc_counts);
    }

    Ok(root_children)
}

fn insert_into_tree(
    nodes: &mut Vec<FolderTreeNode>,
    parts: &[&str],
    depth: usize,
    full_path: &str,
    doc_counts: &std::collections::HashMap<String, usize>,
) {
    if depth >= parts.len() {
        return;
    }
    let name = parts[depth];
    let partial_path: String = parts[..=depth].join("/");

    let idx = nodes.iter().position(|n| n.name == name);
    let node = if let Some(i) = idx {
        &mut nodes[i]
    } else {
        let count = doc_counts.get(&partial_path).copied().unwrap_or(0);
        nodes.push(FolderTreeNode {
            name: name.to_string(),
            path: partial_path.clone(),
            children: Vec::new(),
            doc_count: count,
        });
        nodes.last_mut().unwrap()
    };

    if depth + 1 < parts.len() {
        insert_into_tree(&mut node.children, parts, depth + 1, full_path, doc_counts);
    }
}

#[tauri::command]
pub fn create_folder(
    state: tauri::State<'_, AppState>,
    folder_path: String,
) -> Result<(), String> {
    let workspace_dir = state.config.read().map_err(lock_err)?.workspace_dir.clone();
    let abs_path = docs_dir(&workspace_dir).join(&folder_path);
    std::fs::create_dir_all(&abs_path).map_err(|e| e.to_string())?;

    state
        .storage
        .lock()
        .map_err(lock_err)?
        .upsert_folder(&folder_path, None)
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
pub fn rename_folder(
    state: tauri::State<'_, AppState>,
    old_path: String,
    new_path: String,
) -> Result<(), String> {
    let workspace_dir = state.config.read().map_err(lock_err)?.workspace_dir.clone();
    let docs = docs_dir(&workspace_dir);
    let abs_old = docs.join(&old_path);
    let abs_new = docs.join(&new_path);

    std::fs::rename(&abs_old, &abs_new).map_err(|e| e.to_string())?;

    // DB updates (folder + child folder paths + document folder_path + local_path)
    let store = state.storage.lock().map_err(lock_err)?;
    store.rename_folder(&old_path, &new_path).map_err(|e| e.to_string())?;

    // Update local_path for affected docs
    if let Ok(all_docs) = store.list_docs() {
        for doc in &all_docs {
            if let Some(ref lp) = doc.local_path {
                let lp_path = std::path::Path::new(lp);
                if lp_path.starts_with(&abs_old) {
                    if let Ok(suffix) = lp_path.strip_prefix(&abs_old) {
                        let new_lp = abs_new.join(suffix).to_string_lossy().to_string();
                        let _ = store.update_local_path(&doc.doc_id, &new_lp);
                    }
                }
            }
        }
    }

    Ok(())
}

#[tauri::command]
pub fn delete_folder(
    state: tauri::State<'_, AppState>,
    folder_path: String,
) -> Result<(), String> {
    let workspace_dir = state.config.read().map_err(lock_err)?.workspace_dir.clone();
    let abs_path = docs_dir(&workspace_dir).join(&folder_path);

    // Only delete if empty
    if abs_path.exists() {
        let is_empty = std::fs::read_dir(&abs_path)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false);
        if !is_empty {
            return Err("文件夹不为空，无法删除".to_string());
        }
        std::fs::remove_dir(&abs_path).map_err(|e| e.to_string())?;
    }

    state
        .storage
        .lock()
        .map_err(lock_err)?
        .delete_folder(&folder_path)
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
pub fn move_doc_to_folder(
    state: tauri::State<'_, AppState>,
    doc_id: String,
    target_folder: String,
) -> Result<(), String> {
    let workspace_dir = state.config.read().map_err(lock_err)?.workspace_dir.clone();
    let store = state.storage.lock().map_err(lock_err)?;

    let doc = store
        .get_doc(&doc_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("文档不存在: {doc_id}"))?;

    let old_path = doc
        .local_path
        .as_ref()
        .map(std::path::PathBuf::from)
        .ok_or("文档没有本地文件")?;

    if !old_path.exists() {
        return Err("本地文件不存在".to_string());
    }

    // Target directory
    let target_dir = if target_folder.is_empty() {
        docs_dir(&workspace_dir)
    } else {
        docs_dir(&workspace_dir).join(&target_folder)
    };
    std::fs::create_dir_all(&target_dir).map_err(|e| e.to_string())?;

    let filename = old_path.file_name().ok_or("无法获取文件名")?;
    let new_path = target_dir.join(filename);

    if new_path == old_path {
        return Ok(()); // Already in target folder
    }

    std::fs::rename(&old_path, &new_path).map_err(|e| e.to_string())?;

    let new_path_str = new_path.to_string_lossy().to_string();
    store
        .update_local_path(&doc_id, &new_path_str)
        .map_err(|e| e.to_string())?;
    store
        .update_folder_path(&doc_id, &target_folder)
        .map_err(|e| e.to_string())?;

    Ok(())
}
