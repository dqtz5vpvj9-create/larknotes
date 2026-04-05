/// The outcome of two-axis change detection.
///
/// Local and remote changes are detected independently in their own hash spaces:
/// - `local_changed`:  hash(current file) ≠ content_hash (both in local/markdown format)
/// - `remote_changed`: hash(provider.read()) ≠ remote_hash (both in remote/API format)
///
/// The two hash spaces are NOT comparable because lark-cli's markdown↔richtext
/// conversion is not byte-identical on roundtrip.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncDecision {
    /// Neither side changed — nothing to do.
    NoChange,
    /// Only local changed. Safe to push.
    PushLocal,
    /// Only remote changed. Safe to auto-pull.
    PullRemote,
    /// Both sides changed. User must decide.
    BothModified,
    /// No base hash exists — new file that needs initial push.
    NewFile,
}

/// Pure decision function. No I/O, no side effects.
///
/// - `local_changed`:  true if hash(local file) ≠ content_hash in DB
/// - `remote_changed`: true if hash(provider.read()) ≠ remote_hash in DB
/// - `has_base`:       true if content_hash exists in DB (doc has been synced before)
///
/// When `remote_hash` is unknown (NULL in DB), the caller should pass `remote_changed = false`
/// (conservative: assume remote hasn't changed). This matches the old "optimistic push" behavior.
pub fn decide(local_changed: bool, remote_changed: bool, has_base: bool) -> SyncDecision {
    if !has_base {
        return SyncDecision::NewFile;
    }
    match (local_changed, remote_changed) {
        (false, false) => SyncDecision::NoChange,
        (true, false) => SyncDecision::PushLocal,
        (false, true) => SyncDecision::PullRemote,
        (true, true) => SyncDecision::BothModified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_change() {
        assert_eq!(decide(false, false, true), SyncDecision::NoChange);
    }

    #[test]
    fn test_push_local() {
        assert_eq!(decide(true, false, true), SyncDecision::PushLocal);
    }

    #[test]
    fn test_pull_remote() {
        assert_eq!(decide(false, true, true), SyncDecision::PullRemote);
    }

    #[test]
    fn test_both_modified() {
        assert_eq!(decide(true, true, true), SyncDecision::BothModified);
    }

    #[test]
    fn test_new_file() {
        assert_eq!(decide(true, false, false), SyncDecision::NewFile);
        assert_eq!(decide(false, false, false), SyncDecision::NewFile);
        assert_eq!(decide(true, true, false), SyncDecision::NewFile);
    }

    #[test]
    fn test_remote_unknown_treated_as_unchanged() {
        // When remote_hash is NULL, caller passes remote_changed=false
        // This should result in PushLocal (not BothModified)
        assert_eq!(decide(true, false, true), SyncDecision::PushLocal);
    }
}
