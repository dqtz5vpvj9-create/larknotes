use larknotes_core::AppConfig;
use larknotes_editor::window_monitor::WindowMonitor;
use larknotes_editor::EditorLauncher;
use larknotes_provider_cli::CliProvider;
use larknotes_storage::Storage;
use larknotes_sync::SyncEvent;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::mpsc;

pub struct AppState {
    pub provider: Arc<CliProvider>,
    pub storage: Arc<Mutex<Storage>>,
    pub sync_tx: mpsc::UnboundedSender<SyncEvent>,
    pub config: Arc<RwLock<AppConfig>>,
    pub editor: Arc<RwLock<EditorLauncher>>,
    pub debounce_ms: Arc<AtomicU64>,
    /// Monitors editor windows. When a window closes (filename disappears
    /// from all window titles), the background task renames the file.
    pub window_monitor: Arc<WindowMonitor>,
}
