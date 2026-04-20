use crate::executor::{Executor, SyncStatusUpdate};
use crate::hasher::hash_content;
use crate::planner::{self, SyncAction};
use crate::scanner;
use crate::watcher::SyncEvent;
use crate::write_guard::WriteGuard;
use larknotes_core::*;
use larknotes_storage::Storage;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, Semaphore};

/// Maximum number of concurrent sync operations.
const MAX_CONCURRENT_SYNCS: usize = 5;

/// Scheduler: wires Scanner → Planner → Executor.
///
/// Receives events from watcher + commands, debounces per-note,
/// runs scan→plan→execute pipelines, respects auto_sync config.
pub struct Scheduler {
    executor: Arc<Executor>,
    provider: Arc<dyn DocProvider>,
    storage: Arc<Mutex<Storage>>,
    workspace_dir: PathBuf,
    debounce_ms: Arc<AtomicU64>,
    semaphore: Arc<Semaphore>,
    per_note_locks: Arc<Mutex<HashSet<String>>>,
    config: Arc<std::sync::RwLock<AppConfig>>,
}

impl Scheduler {
    pub fn new(
        provider: Arc<dyn DocProvider>,
        storage: Arc<Mutex<Storage>>,
        workspace_dir: PathBuf,
        debounce_ms: Arc<AtomicU64>,
        write_guard: WriteGuard,
        status_tx: broadcast::Sender<SyncStatusUpdate>,
        config: Arc<std::sync::RwLock<AppConfig>>,
    ) -> Self {
        let executor = Arc::new(Executor::new(
            provider.clone(),
            storage.clone(),
            workspace_dir.clone(),
            write_guard,
            status_tx,
        ));
        Self {
            executor,
            provider,
            storage,
            workspace_dir,
            debounce_ms,
            semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_SYNCS)),
            per_note_locks: Arc::new(Mutex::new(HashSet::new())),
            config,
        }
    }

    pub fn status_receiver(&self) -> broadcast::Receiver<SyncStatusUpdate> {
        // Access via executor's broadcast channel
        // The executor owns the sender; callers should hold onto a receiver
        // For now, we return a dummy — the real receiver is created at construction
        unimplemented!("use the broadcast::Receiver returned from broadcast::channel")
    }

    /// Main event loop. Consumes the receiver and runs until Shutdown.
    pub async fn run(
        scheduler: Arc<Self>,
        mut rx: mpsc::UnboundedReceiver<SyncEvent>,
        docs_changed_tx: Option<mpsc::UnboundedSender<()>>,
    ) {
        let mut debounce_timers: HashMap<String, Instant> = HashMap::new();
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        let mut poll_interval = tokio::time::interval(Duration::from_secs(300));
        // Skip the first immediate tick
        poll_interval.tick().await;

        loop {
            tokio::select! {
                Some(event) = rx.recv() => {
                    match event {
                        SyncEvent::FileModified { path } => {
                            // Look up note by path
                            let note_id = scheduler.storage.lock().ok()
                                .and_then(|s| s.get_doc_by_path(&path.to_string_lossy()).ok().flatten())
                                .map(|d| d.note_id);

                            if let Some(note_id) = note_id {
                                let debounce = scheduler.debounce_ms.load(Ordering::Relaxed);
                                let deadline = Instant::now() + Duration::from_millis(debounce);
                                debounce_timers.insert(note_id, deadline);
                            } else {
                                // Unknown file — could be new
                                let not_known = scheduler.storage.lock().ok()
                                    .map(|s| s.get_doc_by_path(&path.to_string_lossy()).ok().flatten().is_none())
                                    .unwrap_or(true);
                                if not_known {
                                    let sched = scheduler.clone();
                                    let sem = scheduler.semaphore.clone();
                                    tokio::spawn(async move {
                                        let _permit = sem.acquire().await.unwrap();
                                        sched.executor.execute(SyncAction::AdoptNewFile { path }).await;
                                    });
                                }
                            }
                            Self::notify_changed(&docs_changed_tx);
                        }
                        SyncEvent::FileChanged { doc_id, path: _ } => {
                            let debounce = scheduler.debounce_ms.load(Ordering::Relaxed);
                            let deadline = Instant::now() + Duration::from_millis(debounce);
                            // doc_id is actually note_id in the new architecture
                            debounce_timers.insert(doc_id, deadline);
                        }
                        SyncEvent::NewFileDetected { path } => {
                            let not_known = scheduler.storage.lock().ok()
                                .map(|s| s.get_doc_by_path(&path.to_string_lossy()).ok().flatten().is_none())
                                .unwrap_or(true);
                            if not_known {
                                let sched = scheduler.clone();
                                let sem = scheduler.semaphore.clone();
                                tokio::spawn(async move {
                                    let _permit = sem.acquire().await.unwrap();
                                    sched.executor.execute(SyncAction::AdoptNewFile { path }).await;
                                });
                            }
                            Self::notify_changed(&docs_changed_tx);
                        }
                        SyncEvent::FileMoved { old_path, new_path } => {
                            // Update local_path in DB
                            let old_str = old_path.to_string_lossy().to_string();
                            let new_str = new_path.to_string_lossy().to_string();
                            if let Ok(store) = scheduler.storage.lock() {
                                if let Ok(Some(doc)) = store.get_doc_by_path(&old_str) {
                                    let _ = store.update_local_path(&doc.note_id, &new_str);
                                    let folder = folder_of(&scheduler.workspace_dir, &new_path);
                                    let _ = store.update_folder_path(&doc.note_id, &folder);
                                }
                            }
                            Self::notify_changed(&docs_changed_tx);
                        }
                        SyncEvent::FileDeleted { path } => {
                            let path_str = path.to_string_lossy().to_string();
                            let doc = scheduler.storage.lock().ok()
                                .and_then(|s| s.get_doc_by_path(&path_str).ok().flatten());
                            if let Some(doc) = doc {
                                // Set desired state, then dispatch through sync_note
                                if let Ok(store) = scheduler.storage.lock() {
                                    let _ = store.update_sync_state(&doc.note_id, &SyncState::PendingDelete);
                                }
                                let sched = scheduler.clone();
                                let note_id = doc.note_id.clone();
                                tokio::spawn(async move {
                                    sched.sync_note(&note_id, false).await;
                                });
                            }
                            Self::notify_changed(&docs_changed_tx);
                        }
                        SyncEvent::FileRenamed { workspace } => {
                            // Full reconciliation scan
                            let sched = scheduler.clone();
                            tokio::spawn(async move {
                                let result = scanner::scan(&workspace, &sched.storage);
                                for (note_id, new_path) in &result.renamed {
                                    sched.executor.execute(SyncAction::ReclaimOrphan {
                                        note_id: note_id.clone(),
                                        new_path: new_path.clone(),
                                    }).await;
                                }
                            });
                            Self::notify_changed(&docs_changed_tx);
                        }
                        SyncEvent::FolderRenamed { old_rel, new_rel } => {
                            if let Ok(store) = scheduler.storage.lock() {
                                let _ = store.rename_folder(&old_rel, &new_rel);
                                let docs = docs_dir(&scheduler.workspace_dir);
                                let old_dir = docs.join(&old_rel);
                                let new_dir = docs.join(&new_rel);
                                if let Ok(all_docs) = store.list_docs() {
                                    for doc in &all_docs {
                                        if let Some(ref lp) = doc.local_path {
                                            let lp_path = std::path::Path::new(lp);
                                            if lp_path.starts_with(&old_dir) {
                                                if let Ok(suffix) = lp_path.strip_prefix(&old_dir) {
                                                    let new_lp = new_dir.join(suffix).to_string_lossy().to_string();
                                                    let _ = store.update_local_path(&doc.note_id, &new_lp);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            Self::notify_changed(&docs_changed_tx);
                        }
                        SyncEvent::FolderCreated { folder_path } => {
                            if let Ok(s) = scheduler.storage.lock() {
                                let _ = s.upsert_folder(&folder_path, None);
                            }
                            Self::notify_changed(&docs_changed_tx);
                        }
                        SyncEvent::FolderRemoved { folder_path } => {
                            if let Ok(s) = scheduler.storage.lock() {
                                let _ = s.delete_folder(&folder_path);
                            }
                            Self::notify_changed(&docs_changed_tx);
                        }
                        SyncEvent::SyncRequested { doc_id } => {
                            // Immediate sync, bypass debounce
                            let sched = scheduler.clone();
                            let sem = scheduler.semaphore.clone();
                            tokio::spawn(async move {
                                let _permit = sem.acquire().await.unwrap();
                                sched.sync_note(&doc_id, true).await;
                            });
                        }
                        SyncEvent::Shutdown => {
                            tracing::info!("Scheduler关闭");
                            break;
                        }
                    }
                }
                _ = interval.tick() => {
                    // Process debounced timers
                    let now = Instant::now();
                    let ready: Vec<String> = debounce_timers
                        .iter()
                        .filter(|(_, deadline)| now >= **deadline)
                        .map(|(id, _)| id.clone())
                        .collect();

                    for note_id in ready {
                        debounce_timers.remove(&note_id);

                        // Respect auto_sync config
                        let auto_sync = scheduler.config.read()
                            .map(|c| c.auto_sync)
                            .unwrap_or(true);
                        if !auto_sync {
                            continue;
                        }

                        let sched = scheduler.clone();
                        let sem = scheduler.semaphore.clone();
                        tokio::spawn(async move {
                            let _permit = sem.acquire().await.unwrap();
                            sched.sync_note(&note_id, false).await;
                        });
                    }
                }
                _ = poll_interval.tick() => {
                    // Respect auto_sync config
                    let auto_sync = scheduler.config.read()
                        .map(|c| c.auto_sync)
                        .unwrap_or(true);
                    if !auto_sync {
                        continue;
                    }

                    let sched = scheduler.clone();
                    tokio::spawn(async move {
                        sched.poll_and_sync().await;
                    });
                }
            }
        }
    }

    /// Sync a single note: scan → plan → execute.
    async fn sync_note(&self, note_id: &str, force: bool) {
        // Per-note mutex: skip if already running
        {
            let mut locks = match self.per_note_locks.lock() {
                Ok(l) => l,
                Err(_) => return,
            };
            if locks.contains(note_id) {
                tracing::debug!("sync_note: {note_id} already in progress, skipping");
                return;
            }
            locks.insert(note_id.to_string());
        }

        // Get note from DB
        let note = self.storage.lock().ok()
            .and_then(|s| s.get_note(note_id).ok().flatten());

        if let Some(note) = note {
            // Dispatch based on sync_state — desired-state commands set these
            match &note.sync_state {
                SyncState::PendingCreate => {
                    // New local doc needs remote creation
                    if let Some(ref path) = note.local_path {
                        let path = std::path::Path::new(path);
                        if path.exists() {
                            let raw = tokio::fs::read(path).await.unwrap_or_default();
                            let content = crate::util::decode_content(&raw);
                            let title = extract_title(&content);
                            self.executor.execute(SyncAction::CreateRemote {
                                note_id: note_id.to_string(),
                                content,
                                title,
                            }).await;
                        }
                    }
                }
                SyncState::PendingDelete => {
                    if let Some(ref remote_id) = note.remote_id {
                        self.executor.execute(SyncAction::DeleteRemote {
                            note_id: note_id.to_string(),
                            remote_id: remote_id.clone(),
                        }).await;
                    } else {
                        // No remote_id — just clean up locally
                        if let Ok(store) = self.storage.lock() {
                            let _ = store.delete_doc(note_id);
                        }
                        if let Some(ref path) = note.local_path {
                            let _ = std::fs::remove_file(path);
                        }
                    }
                }
                SyncState::PendingRename => {
                    if let Some(ref new_title) = note.desired_title {
                        self.executor.execute(SyncAction::RenameRemote {
                            note_id: note_id.to_string(),
                            new_title: new_title.clone(),
                        }).await;
                    }
                }
                _ => {
                    // Normal sync: check local content for changes, push if needed
                    if let Some(ref path) = note.local_path {
                        let path = std::path::Path::new(path);
                        if path.exists() {
                            let raw = tokio::fs::read(path).await.unwrap_or_default();
                            let content = crate::util::decode_content(&raw);
                            let hash = hash_content(content.as_bytes());
                            let local_changed = note.content_hash.as_deref() != Some(&hash);

                            if local_changed || force {
                                let title = extract_title(&content);
                                self.executor.execute(SyncAction::Push {
                                    note_id: note_id.to_string(),
                                    content,
                                    title,
                                    local_hash: hash,
                                }).await;
                            }
                        }
                    }
                }
            }
        }

        // Release per-note lock
        if let Ok(mut locks) = self.per_note_locks.lock() {
            locks.remove(note_id);
        }
    }

    /// Full poll: check all synced docs for remote changes, then plan+execute.
    async fn poll_and_sync(&self) {
        let docs = match self.storage.lock() {
            Ok(s) => s.list_synced_docs().unwrap_or_default(),
            Err(_) => return,
        };

        if docs.is_empty() {
            return;
        }

        // Scan filesystem
        let scan_result = scanner::scan(&self.workspace_dir, &self.storage);

        // Filter to docs eligible for remote polling.
        let eligible: Vec<&DocMeta> = docs
            .iter()
            .filter(|d| {
                !matches!(
                    d.sync_state,
                    SyncState::Executing
                        | SyncState::Conflict
                        | SyncState::PendingDelete
                        | SyncState::PendingCreate
                )
            })
            .filter(|d| d.remote_id.as_deref().is_some_and(|s| !s.is_empty()))
            .collect();

        let remote_ids: Vec<String> = eligible
            .iter()
            .map(|d| d.remote_id.clone().unwrap())
            .collect();

        if remote_ids.is_empty() {
            return;
        }

        tracing::debug!("poll: querying metas for {} docs", remote_ids.len());

        // Single batch metas query — Lark's authoritative change indicator.
        // Far cheaper than one content fetch per doc, and not affected by
        // server-side markdown normalisation.
        let metas = match self.provider.query_metas(&remote_ids).await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("poll: query_metas failed: {e}");
                return;
            }
        };
        let meta_by_id: HashMap<String, &RemoteMeta> = metas
            .found
            .iter()
            .map(|m| (m.remote_id.clone(), m))
            .collect();
        let gone: HashSet<&str> = metas.gone.iter().map(String::as_str).collect();

        // Mark notes whose remote doc is gone (deleted or permission revoked).
        // The local file is preserved; the user decides via UI whether to
        // recreate on remote, delete locally, or restore from snapshot.
        if !gone.is_empty() {
            let to_mark: Vec<String> = eligible
                .iter()
                .filter(|d| {
                    d.remote_id
                        .as_deref()
                        .is_some_and(|id| gone.contains(id))
                })
                .map(|d| d.note_id.clone())
                .collect();
            if !to_mark.is_empty() {
                if let Ok(store) = self.storage.lock() {
                    for note_id in &to_mark {
                        let _ = store.update_sync_status(note_id, &SyncStatus::RemoteDeleted);
                    }
                }
                tracing::warn!("poll: {} doc(s) gone from remote: {:?}", to_mark.len(), to_mark);
            }
        }

        // Decide per-doc: skip, adopt-baseline-only, or fetch + observe.
        let mut to_fetch: Vec<(String, String, i64, String)> = Vec::new();
        for doc in &eligible {
            let remote_id = doc.remote_id.clone().unwrap();
            if gone.contains(remote_id.as_str()) {
                // Already handled above.
                continue;
            }
            let Some(meta) = meta_by_id.get(&remote_id) else {
                // Token requested but neither in `found` nor `gone` — Lark
                // returned no info at all. Likely transient; try again next
                // poll cycle.
                tracing::debug!("poll: no meta for {remote_id}, retrying next cycle");
                continue;
            };

            // Recovery: a doc previously flagged RemoteDeleted that now
            // appears in `found` is back (permission restored, undeleted).
            // Clear the status so the user no longer sees the alert.
            if doc.sync_status == SyncStatus::RemoteDeleted {
                if let Ok(store) = self.storage.lock() {
                    let _ = store.update_sync_status(&doc.note_id, &SyncStatus::Synced);
                }
            }

            let stored = self
                .storage
                .lock()
                .ok()
                .and_then(|s| s.get_remote_modify_baseline(&doc.note_id).ok().flatten());

            match stored {
                Some((t, ref u))
                    if t == meta.modify_time && u == &meta.modify_user =>
                {
                    // Unchanged since last baseline — common case, no I/O.
                    continue;
                }
                None => {
                    // No baseline yet (fresh DB / migrated row). Adopt the
                    // current state as baseline without pulling — the user's
                    // local file is the truth they just opened the app with.
                    if let Ok(store) = self.storage.lock() {
                        let _ = store.set_remote_modify_baseline(
                            &doc.note_id,
                            meta.modify_time,
                            &meta.modify_user,
                        );
                    }
                    continue;
                }
                Some(_) => {
                    // Baseline exists and differs — genuine remote change.
                    to_fetch.push((
                        doc.note_id.clone(),
                        remote_id,
                        meta.modify_time,
                        meta.modify_user.clone(),
                    ));
                }
            }
        }

        // Fetch content only for docs that actually changed.
        let mut remote_observations = Vec::new();
        for (note_id, remote_id, modify_time, modify_user) in to_fetch {
            let _permit = match self.semaphore.acquire().await {
                Ok(p) => p,
                Err(_) => return,
            };
            match self.provider.read(&remote_id).await {
                Ok(read_output) => {
                    remote_observations.push(planner::RemoteObservation {
                        note_id,
                        remote_content: read_output.content,
                        modify_time,
                        modify_user,
                    });
                }
                Err(e) => {
                    if e.is_not_found() {
                        // Lark's `metas` doesn't surface soft-deletes; `read`
                        // is the only signal we get. Flag the local note so
                        // the user can decide (recreate vs. delete locally).
                        tracing::warn!("poll: remote doc deleted: {note_id} (remote={remote_id})");
                        if let Ok(store) = self.storage.lock() {
                            let _ = store.update_sync_status(&note_id, &SyncStatus::RemoteDeleted);
                        }
                    } else {
                        tracing::debug!("poll: failed to read remote {remote_id}: {e}");
                    }
                }
            }
        }

        // Plan based on scan + remote observations
        let notes = match self.storage.lock() {
            Ok(s) => s.list_docs().unwrap_or_default(),
            Err(_) => return,
        };

        let actions = planner::plan(&scan_result, &notes, &remote_observations);

        // Execute actions
        for action in actions {
            let executor = self.executor.clone();
            let sem = self.semaphore.clone();
            tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                executor.execute(action).await;
            });
        }
    }

    fn notify_changed(tx: &Option<mpsc::UnboundedSender<()>>) {
        if let Some(ref tx) = tx {
            let _ = tx.send(());
        }
    }
}
