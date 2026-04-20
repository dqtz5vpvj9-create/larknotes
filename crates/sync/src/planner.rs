use crate::decision::{SyncDecision, decide};
use crate::scanner::{ChangeKind, ScanResult};
use larknotes_core::{DocMeta, SyncState};
use std::collections::HashMap;
use std::path::PathBuf;

// ─── SyncAction ─────────────────────────────────────────

/// Actions the Executor should perform, produced by the Planner.
#[derive(Debug, Clone)]
pub enum SyncAction {
    /// Push local content to remote.
    Push {
        note_id: String,
        content: String,
        title: String,
        local_hash: String,
    },
    /// Pull remote content to local file. Carries the observed Lark
    /// `latest_modify_time` / `latest_modify_user` so the executor can
    /// persist the new baseline atomically with the local write.
    Pull {
        note_id: String,
        remote_content: String,
        modify_time: i64,
        modify_user: String,
    },
    /// Create a new remote document for a local-only note.
    CreateRemote {
        note_id: String,
        content: String,
        title: String,
    },
    /// Delete a remote document (note is tombstoned).
    DeleteRemote {
        note_id: String,
        remote_id: String,
    },
    /// Rename a remote document.
    RenameRemote {
        note_id: String,
        new_title: String,
    },
    /// Mark note as having both-modified conflict.
    MarkConflict {
        note_id: String,
    },
    /// Reclaim an orphaned note by pointing it at a new file path.
    ReclaimOrphan {
        note_id: String,
        new_path: PathBuf,
    },
    /// Adopt a genuinely new file (create note + remote doc).
    AdoptNewFile {
        path: PathBuf,
    },
    /// Mark note as file-missing.
    MarkFileMissing {
        note_id: String,
    },
    /// Derive title from first heading and rename (for title_mode=derive_once).
    DeriveTitleRename {
        note_id: String,
        new_title: String,
    },
}

/// Remote observation: what we observed from polling the remote for a note.
///
/// Scheduler emits one of these only when Lark's `latest_modify_time` /
/// `latest_modify_user` for a tracked doc differs from the stored baseline,
/// so presence in the observation list already means "remote changed since
/// our last sync". `modify_time` and `modify_user` are passed through so the
/// executor can persist the new baseline after a successful pull.
#[derive(Debug, Clone)]
pub struct RemoteObservation {
    pub note_id: String,
    pub remote_content: String,
    pub modify_time: i64,
    pub modify_user: String,
}

// ─── Planner ────────────────────────────────────────────

/// Produce a list of SyncActions based on scan results, note state, and remote observations.
///
/// This is a pure function — no I/O, no DB writes. Fully testable.
pub fn plan(
    scan: &ScanResult,
    notes: &[DocMeta],
    remote_observations: &[RemoteObservation],
) -> Vec<SyncAction> {
    let mut actions = Vec::new();

    let note_by_id: HashMap<&str, &DocMeta> = notes
        .iter()
        .map(|n| (n.note_id.as_str(), n))
        .collect();

    let remote_by_note: HashMap<&str, &RemoteObservation> = remote_observations
        .iter()
        .map(|o| (o.note_id.as_str(), o))
        .collect();

    // 1. Handle changed notes (local content changed)
    for (note_id, change_kind) in &scan.changed {
        let note = match note_by_id.get(note_id.as_str()) {
            Some(n) => *n,
            None => continue,
        };

        match &note.sync_state {
            SyncState::PendingCreate => {
                // Already pending create — skip, executor will handle
                continue;
            }
            SyncState::PendingDelete | SyncState::Conflict => {
                // Don't sync tombstoned or conflicted notes automatically
                continue;
            }
            _ => {}
        }

        // Check if remote also changed. Scheduler only emits an observation
        // when (modify_time, modify_user) diverges from the stored baseline,
        // so presence already means "remote changed".
        let remote_obs = remote_by_note.get(note_id.as_str());
        let remote_changed = remote_obs.is_some();

        let has_base = note.content_hash.is_some();
        let local_changed = *change_kind == ChangeKind::ContentChanged;

        if remote_changed {
            let decision = decide(local_changed, true, has_base);
            match decision {
                SyncDecision::PushLocal => {
                    if let Some(path) = note.local_path.as_ref() {
                        let content = std::fs::read_to_string(path).unwrap_or_default();
                        let hash = crate::hasher::hash_content(content.as_bytes());
                        actions.push(SyncAction::Push {
                            note_id: note_id.clone(),
                            content,
                            title: note.title.clone(),
                            local_hash: hash,
                        });
                    }
                }
                SyncDecision::PullRemote => {
                    if let Some(obs) = remote_obs {
                        actions.push(SyncAction::Pull {
                            note_id: note_id.clone(),
                            remote_content: obs.remote_content.clone(),
                            modify_time: obs.modify_time,
                            modify_user: obs.modify_user.clone(),
                        });
                    }
                }
                SyncDecision::BothModified => {
                    actions.push(SyncAction::MarkConflict {
                        note_id: note_id.clone(),
                    });
                }
                SyncDecision::NoChange | SyncDecision::NewFile => {}
            }
        } else if local_changed {
            // Only local changed, no remote observation
            if let Some(path) = note.local_path.as_ref() {
                let content = std::fs::read_to_string(path).unwrap_or_default();
                let hash = crate::hasher::hash_content(content.as_bytes());
                actions.push(SyncAction::Push {
                    note_id: note_id.clone(),
                    content,
                    title: note.title.clone(),
                    local_hash: hash,
                });
            }

            // Check for derive_once title rename
            if note.title_mode == "derive_once" {
                if let Some(path) = note.local_path.as_ref() {
                    let content = std::fs::read_to_string(path).unwrap_or_default();
                    let derived = larknotes_core::extract_title(&content);
                    if derived != note.title && !derived.is_empty() {
                        actions.push(SyncAction::DeriveTitleRename {
                            note_id: note_id.clone(),
                            new_title: derived,
                        });
                    }
                }
            }
        }
    }

    // 2. Handle remote-only changes (poll detected changes, no local change)
    for obs in remote_observations {
        let already_handled = scan.changed.iter().any(|(id, _)| id == &obs.note_id);
        if already_handled {
            continue;
        }

        let note = match note_by_id.get(obs.note_id.as_str()) {
            Some(n) => *n,
            None => continue,
        };

        match &note.sync_state {
            SyncState::PendingDelete | SyncState::Conflict | SyncState::PendingCreate => continue,
            _ => {}
        }

        // Remote changed, local didn't — pull
        actions.push(SyncAction::Pull {
            note_id: obs.note_id.clone(),
            remote_content: obs.remote_content.clone(),
            modify_time: obs.modify_time,
            modify_user: obs.modify_user.clone(),
        });
    }

    // 3. Handle missing files
    for note_id in &scan.missing {
        actions.push(SyncAction::MarkFileMissing {
            note_id: note_id.clone(),
        });
    }

    // 4. Handle renames (orphan note matched to new file by hash)
    for (note_id, new_path) in &scan.renamed {
        actions.push(SyncAction::ReclaimOrphan {
            note_id: note_id.clone(),
            new_path: new_path.clone(),
        });
    }

    // 5. Handle new files (no matching orphan note)
    for path in &scan.new_files {
        actions.push(SyncAction::AdoptNewFile { path: path.clone() });
    }

    // 6. Handle pending state notes (desired-state commands)
    for note in notes {
        match &note.sync_state {
            SyncState::PendingCreate => {
                if note.remote_id.is_none() {
                    if let Some(path) = note.local_path.as_ref() {
                        let content = std::fs::read_to_string(path).unwrap_or_default();
                        actions.push(SyncAction::CreateRemote {
                            note_id: note.note_id.clone(),
                            content,
                            title: note.title.clone(),
                        });
                    }
                }
            }
            SyncState::PendingDelete => {
                if let Some(ref remote_id) = note.remote_id {
                    actions.push(SyncAction::DeleteRemote {
                        note_id: note.note_id.clone(),
                        remote_id: remote_id.clone(),
                    });
                }
            }
            SyncState::PendingRename => {
                // desired_title is stored in the note but we use the title field for display
                // The rename action will use the current title
                actions.push(SyncAction::RenameRemote {
                    note_id: note.note_id.clone(),
                    new_title: note.title.clone(),
                });
            }
            _ => {}
        }
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use larknotes_core::*;

    fn make_note(note_id: &str, title: &str, hash: Option<&str>, state: SyncState) -> DocMeta {
        DocMeta {
            note_id: note_id.to_string(),
            remote_id: Some(format!("remote_{note_id}")),
            doc_id: format!("remote_{note_id}"),
            title: title.to_string(),
            doc_type: "DOCX".to_string(),
            url: String::new(),
            owner_name: "test".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
            local_path: None,
            content_hash: hash.map(|s| s.to_string()),
            sync_status: SyncStatus::Synced,
            folder_path: String::new(),
            file_size: None,
            word_count: None,
            sync_state: state,
            title_mode: "manual".to_string(),
            desired_title: None,
            desired_path: None,
        }
    }

    #[test]
    fn plan_missing_marks_file_missing() {
        let scan = ScanResult {
            missing: vec!["n1".to_string()],
            ..Default::default()
        };
        let notes = vec![make_note("n1", "Note1", Some("abc"), SyncState::Synced)];
        let actions = plan(&scan, &notes, &[]);

        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], SyncAction::MarkFileMissing { note_id } if note_id == "n1"));
    }

    #[test]
    fn plan_new_file_adopts() {
        let path = PathBuf::from("/docs/new.md");
        let scan = ScanResult {
            new_files: vec![path.clone()],
            ..Default::default()
        };
        let actions = plan(&scan, &[], &[]);

        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], SyncAction::AdoptNewFile { path: p } if *p == path));
    }

    #[test]
    fn plan_renamed_reclaims() {
        let new_path = PathBuf::from("/docs/renamed.md");
        let scan = ScanResult {
            renamed: vec![("n1".to_string(), new_path.clone())],
            ..Default::default()
        };
        let notes = vec![make_note("n1", "Note1", Some("abc"), SyncState::Synced)];
        let actions = plan(&scan, &notes, &[]);

        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], SyncAction::ReclaimOrphan { note_id, new_path: p }
            if note_id == "n1" && *p == new_path));
    }

    #[test]
    fn plan_pending_create_emits_create_remote() {
        let scan = ScanResult::default();
        let notes = vec![{
            let mut n = make_note("n1", "New Note", None, SyncState::PendingCreate);
            n.remote_id = None;
            n.local_path = Some("/docs/new.md".to_string());
            n
        }];
        let actions = plan(&scan, &notes, &[]);

        assert!(actions.iter().any(|a| matches!(a, SyncAction::CreateRemote { note_id, .. } if note_id == "n1")));
    }

    #[test]
    fn plan_pending_delete_emits_delete_remote() {
        let scan = ScanResult::default();
        let notes = vec![make_note("n1", "To Delete", Some("abc"), SyncState::PendingDelete)];
        let actions = plan(&scan, &notes, &[]);

        assert!(actions.iter().any(|a| matches!(a, SyncAction::DeleteRemote { note_id, .. } if note_id == "n1")));
    }

    #[test]
    fn plan_remote_only_change_pulls() {
        let scan = ScanResult::default();
        let notes = vec![make_note("n1", "Note1", Some("abc"), SyncState::Synced)];
        let remote = vec![RemoteObservation {
            note_id: "n1".to_string(),
            remote_content: "# Updated remotely".to_string(),
            modify_time: 1_700_000_000,
            modify_user: "ou_other".to_string(),
        }];
        let actions = plan(&scan, &notes, &remote);

        assert!(actions.iter().any(|a| matches!(a, SyncAction::Pull { note_id, .. } if note_id == "n1")));
    }

    #[test]
    fn plan_both_modified_marks_conflict() {
        let scan = ScanResult {
            changed: vec![("n1".to_string(), ChangeKind::ContentChanged)],
            ..Default::default()
        };
        let notes = vec![make_note("n1", "Note1", Some("abc"), SyncState::Synced)];
        let remote = vec![RemoteObservation {
            note_id: "n1".to_string(),
            remote_content: "# Remote content".to_string(),
            modify_time: 1_700_000_000,
            modify_user: "ou_other".to_string(),
        }];
        let actions = plan(&scan, &notes, &remote);

        assert!(actions.iter().any(|a| matches!(a, SyncAction::MarkConflict { note_id } if note_id == "n1")));
    }

    #[test]
    fn plan_skips_tombstoned_notes() {
        let scan = ScanResult {
            changed: vec![("n1".to_string(), ChangeKind::ContentChanged)],
            ..Default::default()
        };
        let notes = vec![make_note("n1", "Note1", Some("abc"), SyncState::PendingDelete)];
        let actions = plan(&scan, &notes, &[]);

        // Should not emit Push for a tombstoned note
        assert!(!actions.iter().any(|a| matches!(a, SyncAction::Push { .. })));
    }
}
