#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod state;

use commands::*;
use larknotes_core::*;
use tauri::{Emitter, Manager};
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use larknotes_editor::{detect_editor, EditorLauncher};
use larknotes_provider_cli::CliProvider;
use larknotes_storage::Storage;
use larknotes_sync::{FileWatcher, Scheduler, SyncEvent, WriteGuard, scan_orphan_files};
use larknotes_sync::executor::SyncStatusUpdate;
use state::AppState;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

#[derive(clap::Parser)]
#[command(name = "LarkNotes")]
struct CliArgs {
    /// Create a quick note and open it in the editor
    #[arg(long)]
    quick_note: bool,
}

/// Load app config from DB without starting the full Tauri runtime.
/// Returns (config, db_path) so callers can reuse the same DB location.
fn load_config() -> (AppConfig, std::path::PathBuf) {
    let mut config = AppConfig::default();
    let db_path = config.workspace_dir.join("app.db");
    if let Ok(storage) = Storage::new(&db_path) {
        if let Ok(Some(editor)) = storage.get_config("editor_command") {
            config.editor_command = editor;
        }
        if let Ok(Some(cli_path)) = storage.get_config("provider_cli_path") {
            config.provider_cli_path = cli_path;
        }
        if let Ok(Some(ws)) = storage.get_config("workspace_dir") {
            let p = std::path::PathBuf::from(&ws);
            if p.exists() {
                config.workspace_dir = p;
            }
        }
        if let Ok(Some(debounce)) = storage.get_config("sync_debounce_ms") {
            if let Ok(ms) = debounce.parse::<u64>() {
                config.sync_debounce_ms = ms;
            }
        }
        if let Ok(Some(auto)) = storage.get_config("auto_sync") {
            config.auto_sync = auto == "true";
        }
    }
    if config.editor_command == "notepad" {
        config.editor_command = detect_editor();
    }
    (config, db_path)
}

/// Fast path: create a quick note and exit without starting the Tauri runtime.
fn fast_quick_note() {
    let (config, db_path) = load_config();
    let storage = Storage::new(&db_path).expect("Failed to init database");
    let editor = EditorLauncher::new(&config.editor_command);

    match commands::execute_quick_note_core(&config, &storage, &editor, None, None) {
        Ok(meta) => tracing::info!("Quick note created: {}", meta.title),
        Err(e) => tracing::error!("Failed to create quick note: {e}"),
    }
}

/// Check if another LarkNotes instance is already running (Windows named mutex).
#[cfg(windows)]
fn is_another_instance_running() -> bool {
    use windows_sys::Win32::System::Threading::OpenMutexW;
    use windows_sys::Win32::Foundation::{CloseHandle, SYNCHRONIZE};
    let name: Vec<u16> = "Global\\LarkNotes\0".encode_utf16().collect();
    unsafe {
        let handle = OpenMutexW(SYNCHRONIZE, 0, name.as_ptr());
        if handle == 0 {
            false
        } else {
            CloseHandle(handle);
            true
        }
    }
}

fn main() {
    // Set up file logging: workspace/logs/larknotes-YYYY-MM-DD.log (7-day rolling)
    let log_dir = AppConfig::default().workspace_dir.join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let file_appender = tracing_appender::rolling::daily(&log_dir, "larknotes");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    // Clean up old log files (keep last 7 days)
    if let Ok(entries) = std::fs::read_dir(&log_dir) {
        let cutoff = std::time::SystemTime::now()
            - std::time::Duration::from_secs(7 * 24 * 3600);
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    if modified < cutoff {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }

    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "larknotes=info".into());

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(tracing_subscriber::fmt::layer().with_ansi(false).with_writer(non_blocking))
        .init();

    tracing::info!(
        "SESSION START — LarkNotes v{} on {} {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    );

    // Parse CLI arguments
    use clap::Parser;
    let cli = CliArgs::parse();

    // Fast path: --quick-note with no running instance → create note and exit
    #[cfg(windows)]
    if cli.quick_note && !is_another_instance_running() {
        fast_quick_note();
        return;
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if args.iter().any(|a| a == "--quick-note") {
                let state = app.state::<AppState>();
                if let (Ok(config), Ok(storage), Ok(editor)) = (
                    state.config.read(),
                    state.storage.lock(),
                    state.editor.read(),
                ) {
                    let _ = commands::execute_quick_note_core(
                        &config, &storage, &editor,
                        Some(&state.window_monitor), Some(&state.sync_tx),
                    );
                }
            } else {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.unminimize();
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
        }))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(move |app| {
            let app_handle = app.handle().clone();

            // Create named mutex so fast_quick_note() can detect a running instance
            #[cfg(windows)]
            unsafe {
                use windows_sys::Win32::System::Threading::CreateMutexW;
                let name: Vec<u16> = "Global\\LarkNotes\0".encode_utf16().collect();
                CreateMutexW(std::ptr::null(), 0, name.as_ptr());
                // Intentionally leaked — held for process lifetime
            }

            // 1. Load config from DB
            let (config, db_path) = load_config();
            let workspace = config.workspace_dir.clone();
            std::fs::create_dir_all(workspace.join("docs"))?;
            std::fs::create_dir_all(workspace.join(".meta"))?;

            // 2. Init storage
            let storage = Arc::new(Mutex::new(
                Storage::new(&db_path).expect("Failed to init database"),
            ));

            // 3. Crash recovery: reset stale "Syncing" docs → "LocalModified"
            {
                let store = storage.lock().expect("Storage lock poisoned at init");
                match store.reset_stale_syncing() {
                    Ok(0) => {}
                    Ok(n) => tracing::warn!("恢复了 {n} 个中断的同步任务"),
                    Err(e) => tracing::error!("重置同步状态失败: {e}"),
                }
            }

            // 3b. Scan folder tree and register subfolders in DB
            {
                let count = larknotes_sync::scan_folder_tree(&workspace, &storage);
                if count > 0 {
                    tracing::info!("启动时注册了 {} 个文件夹", count);
                }
            }

            // 3c. Startup path reconciliation: fix orphaned docs from
            //     external renames while app was not running
            {
                let matches = larknotes_sync::reconcile_paths(&workspace, &storage);
                if !matches.is_empty() {
                    tracing::info!("启动时修复了 {} 个孤儿文档路径", matches.len());
                }
            }

            // 3d. Rename files whose names don't match their title.
            {
                let count = larknotes_sync::rename_stale_paths(&workspace, &storage);
                if count > 0 {
                    tracing::info!("启动时重命名了 {} 个文件以匹配标题", count);
                }
            }

            // 4. Read config values before wrapping in Arc
            let cli_path = config.provider_cli_path.clone();
            let editor_command = config.editor_command.clone();
            let debounce_ms = Arc::new(AtomicU64::new(config.sync_debounce_ms));
            let config = Arc::new(RwLock::new(config));

            // 5. Init provider (validate CLI exists)
            if which::which(&cli_path).is_err() {
                tracing::warn!(
                    "Provider CLI '{}' not found in PATH. Sync will not work until installed.",
                    cli_path
                );
            }
            let cli_provider = Arc::new(CliProvider::new(&cli_path));
            let provider: Arc<dyn DocProvider> = cli_provider.clone();
            let auth: Arc<dyn ProviderAuth> = cli_provider.clone();

            // 6. Init editor
            let editor = Arc::new(RwLock::new(EditorLauncher::new(&editor_command)));

            // 7. Init sync channel + scheduler
            let (sync_tx, sync_rx) = tokio::sync::mpsc::unbounded_channel();
            let write_guard = WriteGuard::new();
            let (status_tx, _status_rx) = tokio::sync::broadcast::channel::<SyncStatusUpdate>(64);
            let scheduler = Arc::new(Scheduler::new(
                provider.clone(),
                storage.clone(),
                workspace.clone(),
                debounce_ms.clone(),
                write_guard.clone(),
                status_tx.clone(),
                config.clone(),
            ));

            // 8. Start file watcher — stored in app managed state to prevent drop
            //    Watcher does NOT hold storage — sends events to engine for serial processing.
            let (docs_changed_tx, mut docs_changed_rx) = tokio::sync::mpsc::unbounded_channel();
            let watcher = FileWatcher::new(
                workspace.clone(), sync_tx.clone(), Some(write_guard.clone()),
            ).expect("Failed to start file watcher");
            app.manage(watcher);

            // 8b. Relay watcher rename notifications → frontend "docs-changed"
            let rename_handle = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                while docs_changed_rx.recv().await.is_some() {
                    let _ = rename_handle.emit("docs-changed", ());
                }
            });

            // 9. Spawn scheduler + status relay
            let sched = scheduler.clone();
            let handle = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                let mut status_rx = status_tx.subscribe();
                let relay_handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        match status_rx.recv().await {
                            Ok(update) => {
                                let _ = relay_handle.emit("sync-status", &update);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            Err(_) => continue,
                        }
                    }
                });

                Scheduler::run(sched, sync_rx, Some(docs_changed_tx)).await;
            });

            // 9b. Adopt orphan .md files created while app was closed.
            // Delegate to the engine's adopt_new_file via NewFileDetected events
            // (avoids duplicating creation logic and the LocalModified→SyncRequested double-push bug).
            {
                let orphan_storage = storage.clone();
                let orphan_workspace = workspace.clone();
                let orphan_tx = sync_tx.clone();
                tauri::async_runtime::spawn(async move {
                    let orphans = scan_orphan_files(&orphan_workspace, &orphan_storage);
                    if orphans.is_empty() {
                        return;
                    }
                    tracing::info!("startup: found {} orphan file(s), sending to engine...", orphans.len());
                    for path in orphans {
                        let _ = orphan_tx.send(SyncEvent::NewFileDetected { path });
                    }
                });
            }

            // 10. System tray
            let show_item = MenuItemBuilder::with_id("show", "显示窗口").build(app)?;
            let quit_item = MenuItemBuilder::with_id("quit", "退出").build(app)?;
            let tray_menu = MenuBuilder::new(app)
                .item(&show_item)
                .separator()
                .item(&quit_item)
                .build()?;

            let quitting = Arc::new(AtomicBool::new(false));
            let quitting_flag = quitting.clone();
            app.manage(QuittingFlag(quitting));

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().cloned().unwrap())
                .tooltip("LarkNotes")
                .menu(&tray_menu)
                .on_menu_event(move |app, event| {
                    match event.id().as_ref() {
                        "show" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.unminimize();
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                        "quit" => {
                            quitting_flag.store(true, Ordering::SeqCst);
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::DoubleClick { .. } = event {
                        if let Some(w) = tray.app_handle().get_webview_window("main") {
                            let _ = w.unminimize();
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                })
                .build(app)?;

            // 11. Window monitor: detects when editor windows close → triggers rename
            let (window_monitor, mut rename_rx) = larknotes_editor::window_monitor::WindowMonitor::new();
            let window_monitor = Arc::new(window_monitor);

            // Polling task: checks window titles every 1s
            {
                let monitor = window_monitor.clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        monitor.check_once();
                    }
                });
            }

            // Rename task: receives close notifications → renames files
            {
                let monitor_storage = storage.clone();
                let monitor_workspace = config.read().unwrap().workspace_dir.clone();
                let monitor_handle = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    while let Some(closed_doc_ids) = rename_rx.recv().await {
                        tracing::info!(
                            "编辑器窗口已关闭 {} 个文档，检查是否需要重命名",
                            closed_doc_ids.len()
                        );
                        let count = larknotes_sync::rename_stale_paths(
                            &monitor_workspace,
                            &monitor_storage,
                        );
                        if count > 0 {
                            tracing::info!("编辑器关闭后重命名了 {} 个文件", count);
                            let _ = monitor_handle.emit("docs-changed", ());
                        }
                    }
                });
            }

            // 12. Set app state
            let shutdown_tx = sync_tx.clone();
            app.manage(AppState {
                provider,
                auth,
                storage,
                sync_tx,
                config,
                editor,
                debounce_ms,
                write_guard,
                window_monitor,
                cli_provider,
            });
            app.manage(ShutdownSender(Mutex::new(Some(shutdown_tx))));

            // If launched with --quick-note, hide the main window
            if cli.quick_note {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }

            tracing::info!("LarkNotes 初始化完成");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_auth_status,
            search_docs,
            search_docs_local,
            create_doc,
            open_doc_in_editor,
            get_doc_list,
            get_app_config,
            set_editor,
            set_workspace,
            detect_editors,
            pick_folder,
            manual_sync,
            import_doc,
            delete_doc,
            rename_doc,
            reveal_in_explorer,
            get_sync_history,
            get_snapshots,
            restore_snapshot,
            quick_note,
            get_autostart_status,
            set_autostart,
            pull_doc,
            set_sync_debounce,
            set_auto_sync,
            set_provider_cli_path,
            open_login_url,
            resolve_conflict,
            get_conflict_diff,
            get_folder_tree,
            create_folder,
            rename_folder,
            delete_folder,
            move_doc_to_folder,
            get_quick_note_shortcut_status,
            set_quick_note_shortcut,
        ])
        .build(tauri::generate_context!())
        .expect("error while building LarkNotes")
        .run(|app, event| {
            match event {
                tauri::RunEvent::WindowEvent {
                    label,
                    event: tauri::WindowEvent::CloseRequested { api, .. },
                    ..
                } if label == "main" => {
                    let is_quitting = app
                        .try_state::<QuittingFlag>()
                        .is_some_and(|f| f.0.load(Ordering::SeqCst));
                    if !is_quitting {
                        // Prevent window destruction — hide to tray instead
                        api.prevent_close();
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.hide();
                        }
                    }
                }
                tauri::RunEvent::Exit => {
                    tracing::info!("LarkNotes 正在关闭...");
                    if let Some(sender) = app.try_state::<ShutdownSender>() {
                        if let Ok(mut tx) = sender.0.lock() {
                            if let Some(tx) = tx.take() {
                                let _ = tx.send(SyncEvent::Shutdown);
                            }
                        }
                    }
                    tracing::info!("SESSION END");
                }
                _ => {}
            }
        });
}

/// Wrapper to hold the sync shutdown sender in Tauri managed state.
struct ShutdownSender(Mutex<Option<tokio::sync::mpsc::UnboundedSender<SyncEvent>>>);

/// Flag to distinguish real quit from window-close-to-tray.
struct QuittingFlag(Arc<AtomicBool>);
