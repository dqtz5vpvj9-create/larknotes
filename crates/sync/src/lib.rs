pub mod decision;
pub mod hasher;
pub mod reconcile;
pub mod watcher;
pub mod engine;   // deprecated — kept for test compatibility
pub mod util;
pub mod write_guard;
pub mod scanner;
pub mod planner;
pub mod executor;
pub mod scheduler;

pub use decision::*;
pub use hasher::*;
pub use reconcile::{reconcile_paths, scan_folder_tree, rename_stale_paths, scan_orphan_files};
pub use watcher::*;
// engine::* no longer glob-exported — use engine::SyncEngine explicitly if needed
pub use util::decode_content;
pub use write_guard::WriteGuard;
pub use scanner::{ScanResult, ChangeKind};
pub use planner::{SyncAction, RemoteObservation};
pub use executor::SyncStatusUpdate;
pub use scheduler::Scheduler;
