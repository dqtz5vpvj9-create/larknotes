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
use larknotes_sync::{FileWatcher, SyncEngine, SyncEvent};
use state::AppState;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, RwLock};

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

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            let app_handle = app.handle().clone();

            // 1. Default config
            let mut config = AppConfig::default();
            let workspace_dir = config.workspace_dir.clone();
            std::fs::create_dir_all(workspace_dir.join("docs"))?;
            std::fs::create_dir_all(workspace_dir.join(".meta"))?;

            // 2. Init storage
            let db_path = workspace_dir.join("app.db");
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

            // 3b. Startup path reconciliation: fix orphaned docs from
            //     external renames while app was not running
            {
                let matches = larknotes_sync::reconcile_paths(&workspace_dir, &storage);
                if !matches.is_empty() {
                    tracing::info!("启动时修复了 {} 个孤儿文档路径", matches.len());
                }
            }

            // 3c. Rename files whose names don't match their title.
            //     This is deferred from editing sessions to avoid confusing editors.
            {
                let count = larknotes_sync::rename_stale_paths(&workspace_dir, &storage);
                if count > 0 {
                    tracing::info!("启动时重命名了 {} 个文件以匹配标题", count);
                }
            }

            // 4. Load config from DB
            {
                let store = storage.lock().expect("Storage lock poisoned at init");
                if let Ok(Some(editor)) = store.get_config("editor_command") {
                    config.editor_command = editor;
                }
                if let Ok(Some(cli_path)) = store.get_config("lark_cli_path") {
                    config.lark_cli_path = cli_path;
                }
                if let Ok(Some(ws)) = store.get_config("workspace_dir") {
                    let p = std::path::PathBuf::from(&ws);
                    if p.exists() {
                        config.workspace_dir = p;
                    }
                }
                if let Ok(Some(debounce)) = store.get_config("sync_debounce_ms") {
                    if let Ok(ms) = debounce.parse::<u64>() {
                        config.sync_debounce_ms = ms;
                    }
                }
                if let Ok(Some(auto)) = store.get_config("auto_sync") {
                    config.auto_sync = auto == "true";
                }
            }
            if config.editor_command == "notepad" {
                config.editor_command = detect_editor();
            }

            let workspace = config.workspace_dir.clone();
            std::fs::create_dir_all(workspace.join("docs"))?;
            std::fs::create_dir_all(workspace.join(".meta"))?;

            // 4. Read config values before wrapping in Arc
            let cli_path = config.lark_cli_path.clone();
            let editor_command = config.editor_command.clone();
            let debounce_ms = Arc::new(AtomicU64::new(config.sync_debounce_ms));
            let config = Arc::new(RwLock::new(config));

            // 5. Init provider
            let provider = Arc::new(CliProvider::new(&cli_path));

            // 6. Init editor
            let editor = Arc::new(RwLock::new(EditorLauncher::new(&editor_command)));

            // 7. Init sync channel + engine
            let (sync_tx, sync_rx) = tokio::sync::mpsc::unbounded_channel();
            let (sync_engine, _status_rx) = SyncEngine::new(
                provider.clone() as Arc<dyn DocProvider>,
                storage.clone(),
                workspace.clone(),
                debounce_ms.clone(),
            );
            let sync_engine = Arc::new(sync_engine);

            // 8. Start file watcher — stored in app managed state to prevent drop
            let watcher = FileWatcher::new(workspace, sync_tx.clone(), storage.clone())
                .expect("Failed to start file watcher");
            app.manage(watcher);

            // 9. Spawn sync engine + status relay
            let engine = sync_engine.clone();
            let handle = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                let mut status_rx = engine.status_receiver();
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

                SyncEngine::run(engine, sync_rx).await;
            });

            // 10. System tray
            let show_item = MenuItemBuilder::with_id("show", "显示窗口").build(app)?;
            let quit_item = MenuItemBuilder::with_id("quit", "退出").build(app)?;
            let tray_menu = MenuBuilder::new(app)
                .item(&show_item)
                .separator()
                .item(&quit_item)
                .build()?;

            let _tray = TrayIconBuilder::new()
                .tooltip("LarkNotes")
                .menu(&tray_menu)
                .on_menu_event(move |app, event| {
                    match event.id().as_ref() {
                        "show" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                        "quit" => {
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::DoubleClick { .. } = event {
                        if let Some(w) = tray.app_handle().get_webview_window("main") {
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
                storage,
                sync_tx,
                config,
                editor,
                debounce_ms,
                window_monitor,
            });
            app.manage(ShutdownSender(Mutex::new(Some(shutdown_tx))));

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
            set_lark_cli_path,
            open_login_url,
            resolve_conflict,
            get_conflict_diff,
        ])
        .build(tauri::generate_context!())
        .expect("error while building LarkNotes")
        .run(|app, event| {
            match event {
                tauri::RunEvent::ExitRequested { api, .. } => {
                    // Prevent exit — minimize to tray instead
                    api.prevent_exit();
                    if let Some(w) = app.get_webview_window("main") {
                        let _ = w.hide();
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
