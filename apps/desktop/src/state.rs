use larknotes_core::{AppConfig, DocProvider};
use larknotes_editor::EditorLauncher;
use larknotes_storage::Storage;
use larknotes_sync::SyncEvent;
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::mpsc;

pub struct AppState {
    pub provider: Arc<dyn DocProvider>,
    pub storage: Arc<Mutex<Storage>>,
    pub sync_tx: mpsc::UnboundedSender<SyncEvent>,
    pub config: Arc<RwLock<AppConfig>>,
    pub editor: Arc<RwLock<EditorLauncher>>,
}
