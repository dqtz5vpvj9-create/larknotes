/**
 * Two-axis sync change detection.
 *
 * Ported from larknotes `crates/sync/src/decision.rs` — the surviving sync
 * engine of the abandoned standalone desktop app. The lesson it encodes:
 *
 * Local and remote changes are detected independently, each with its OWN
 * signal:
 *   - `localChanged`:  hash(current vault file) ≠ `localBaseHash`
 *                      (content hash — the vault file is local, hashing is free)
 *   - `remoteChanged`: wiki node `obj_edit_time` ≠ `remoteEditTime`
 *                      (timestamp from `wiki nodes list` — no content fetch)
 *
 * The two axes intentionally use different signals. They are not comparable
 * to each other and were never meant to be: `lark-cli`'s markdown roundtrip
 * is canonicalising, so a local hash can't predict a remote hash anyway.
 *
 * History: the pre-increment-1 engine conflated both into one `lastSyncedHash`
 * and recorded `hash(localMd)` after a push — so any Lark normalisation made
 * the next sync see a phantom change. Increment 1 split it into two content
 * hashes; increment 3 replaced the *remote* hash with `obj_edit_time`, which
 * lets a sync detect which docs changed without fetching any of them.
 */
export enum SyncDecision {
  /** Neither side changed — nothing to do. */
  NoChange = "no-change",
  /** Only local changed. Safe to push. */
  PushLocal = "push-local",
  /** Only remote changed. Safe to auto-pull. */
  PullRemote = "pull-remote",
  /** Both sides changed. User must decide. */
  BothModified = "both-modified",
  /** No baseline exists — first time we've seen this node. */
  NewFile = "new-file",
}

/**
 * Pure decision function. No I/O, no side effects.
 *
 * - `localChanged`:  true if hash(local file) ≠ `localBaseHash` in state
 * - `remoteChanged`: true if the node's `obj_edit_time` ≠ `remoteEditTime`
 * - `hasBase`:       true if a `FileSyncState` exists for this node
 *
 * When there is no baseline the caller branches on whether a local file
 * already exists (brand-new pull vs. untracked collision); `decide` only
 * reports `NewFile` so that branch is explicit at the call site.
 */
export function decide(
  localChanged: boolean,
  remoteChanged: boolean,
  hasBase: boolean,
): SyncDecision {
  if (!hasBase) return SyncDecision.NewFile;
  if (!localChanged && !remoteChanged) return SyncDecision.NoChange;
  if (localChanged && !remoteChanged) return SyncDecision.PushLocal;
  if (!localChanged && remoteChanged) return SyncDecision.PullRemote;
  return SyncDecision.BothModified;
}
