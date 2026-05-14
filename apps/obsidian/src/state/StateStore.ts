import { App, normalizePath } from "obsidian";

/**
 * Per-file sync state. Drives the two-axis change classification (see
 * `src/sync/decision.ts`):
 *
 *   localChanged  = hash(current vault file)  ≠ localBaseHash
 *   remoteChanged = node.obj_edit_time        ≠ remoteEditTime
 *
 * The two axes use different signals on purpose. Local is a content hash —
 * we have the vault file, hashing it is free. Remote is the wiki node's
 * `obj_edit_time`, which `wiki nodes list` returns for free: a sync can tell
 * which docs changed remotely *without fetching their content*. This
 * replaced the v3 `remoteBaseHash` content-hash, which forced a full fetch
 * of every doc on every sync just to detect change.
 *
 * Keyed by Lark `nodeToken` so the state survives changes in local path
 * mapping (e.g. renaming `localRoot`, switching to per-space subfolders).
 * `localPath` is recorded but only used for cleanup / logging.
 */
export interface FileSyncState {
  localPath: string;
  nodeToken: string;
  docToken: string;
  /** Hash of the vault file content (Obsidian-form) at last successful sync. */
  localBaseHash: string;
  /** The wiki node's `obj_edit_time` (Unix seconds, as a string) at last
   *  successful sync — the remote-change signal. */
  remoteEditTime: string;
  /** Legacy (schema ≤ v3) content-hash remote baseline. Present only on
   *  entries written before the v4 migration; the first post-upgrade sync
   *  uses it once to re-baseline onto `remoteEditTime`, then drops it. */
  remoteBaseHash?: string;
  lastSyncedAt: string;
}

export interface StateShape {
  /** Keyed by `nodeToken`. */
  files: Record<string, FileSyncState>;
  /** Schema version — bumped each time the key/shape changes. */
  schemaVersion: number;
}

const SCHEMA_VERSION = 4;
const DEFAULT_STATE: StateShape = { files: {}, schemaVersion: SCHEMA_VERSION };

export class StateStore {
  private state: StateShape = structuredClone(DEFAULT_STATE);

  constructor(private app: App, private pluginId: string) {}

  private get path(): string {
    return normalizePath(`${this.app.vault.configDir}/plugins/${this.pluginId}/sync-state.json`);
  }

  async load(): Promise<void> {
    try {
      const adapter = this.app.vault.adapter;
      if (!(await adapter.exists(this.path))) {
        this.state = structuredClone(DEFAULT_STATE);
        return;
      }
      const raw = await adapter.read(this.path);
      const parsed = JSON.parse(raw) as Partial<StateShape> & {
        files?: Record<string, FileSyncState>;
      };
      this.state = this.migrate(parsed);
    } catch (err) {
      console.warn("LarkWikiSync: failed to load sync state, starting fresh.", err);
      this.state = structuredClone(DEFAULT_STATE);
    }
  }

  /**
   * Migrate older on-disk state to the current schema.
   *
   * - v1 (pre-0.0.11) was keyed by `localPath`; v2 re-keyed by `nodeToken`.
   * - v3 split the single `lastSyncedHash` into `localBaseHash` +
   *   `remoteBaseHash` (both content hashes).
   * - v4 replaced `remoteBaseHash` with `remoteEditTime` (the node's
   *   `obj_edit_time`). We can't derive an edit time from a hash, so we keep
   *   the old `remoteBaseHash` as a one-shot legacy fallback: the first
   *   post-upgrade sync compares it against freshly-fetched content to decide
   *   if the remote actually changed, then stamps `remoteEditTime` and drops
   *   `remoteBaseHash`. This avoids a mass spurious pull/conflict on upgrade.
   */
  private migrate(raw: {
    files?: Record<string, FileSyncState & { lastSyncedHash?: string }>;
    schemaVersion?: number;
  }): StateShape {
    if (raw?.schemaVersion === SCHEMA_VERSION && raw.files) {
      return { files: raw.files, schemaVersion: SCHEMA_VERSION };
    }
    const files: Record<string, FileSyncState> = {};
    for (const entry of Object.values(raw?.files ?? {})) {
      if (!entry?.nodeToken) continue; // unsalvageable
      const legacyHash = entry.lastSyncedHash; // v1/v2 single hash
      files[entry.nodeToken] = {
        localPath: entry.localPath,
        nodeToken: entry.nodeToken,
        docToken: entry.docToken,
        localBaseHash: entry.localBaseHash ?? legacyHash ?? "",
        remoteEditTime: entry.remoteEditTime ?? "",
        // Kept so the first post-v4 sync can re-baseline without a spurious
        // pull/conflict. Omitted entirely once the entry is next synced.
        remoteBaseHash: entry.remoteBaseHash ?? legacyHash,
        lastSyncedAt: entry.lastSyncedAt,
      };
    }
    const migratedCount = Object.keys(files).length;
    const droppedCount = Object.keys(raw?.files ?? {}).length - migratedCount;
    if (droppedCount > 0) {
      console.warn(
        `LarkWikiSync: state migration dropped ${droppedCount} unrecognised entries.`,
      );
    }
    console.info(`LarkWikiSync: state migrated to schema v${SCHEMA_VERSION} (${migratedCount} entries).`);
    return { files, schemaVersion: SCHEMA_VERSION };
  }

  async save(): Promise<void> {
    const adapter = this.app.vault.adapter;
    await adapter.write(this.path, JSON.stringify(this.state, null, 2));
  }

  get(nodeToken: string): FileSyncState | undefined {
    return this.state.files[nodeToken];
  }

  upsert(entry: FileSyncState): void {
    this.state.files[entry.nodeToken] = entry;
  }

  remove(nodeToken: string): void {
    delete this.state.files[nodeToken];
  }

  all(): FileSyncState[] {
    return Object.values(this.state.files);
  }

  async reset(): Promise<void> {
    this.state = structuredClone(DEFAULT_STATE);
    await this.save();
  }
}
