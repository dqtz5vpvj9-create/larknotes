import { App, FileSystemAdapter, normalizePath, TFile } from "obsidian";
import type { LarkWikiSyncSettings, WikiSpaceConfig } from "../settings";
import type { LarkCli } from "../lark/LarkCli";
import type { StateStore, FileSyncState } from "../state/StateStore";
import { hashString } from "../util/hash";
import { extractImageTokens, larkXmlToObsidianMarkdown } from "../util/larkXml";
import { obsidianToLarkMarkdown } from "../util/obsidianToLarkMd";
import { hasLarkSyncFalse, matchAnyGlob } from "../util/skipRules";
import { decide, SyncDecision } from "./decision";

const ATTACHMENTS_SUBFOLDER = "_attachments";

export interface SyncError {
  phase: "pull" | "push" | "conflict" | "fetch" | "plan";
  file: string;
  message: string;
}

export interface SyncResult {
  pulled: number;
  pushed: number;
  conflicts: number;
  skipped: number;
  reconciled: number;
  errors: SyncError[];
}

/** The wiki-node fields the engine threads through a plan. `obj_edit_time`
 *  is the remote-change signal recorded into `FileSyncState`. */
export interface NodeRef {
  node_token: string;
  obj_token: string;
  title: string;
  obj_edit_time: string;
}

/** Items intended for execution after planning. */
export interface PendingPull {
  space: WikiSpaceConfig;
  node: NodeRef;
  localPath: string;
  remoteMd: string;
  /** hash(remoteMd) — what the vault file will hash to after the pull. */
  remoteHash: string;
}

export interface PendingPush {
  space: WikiSpaceConfig;
  node: NodeRef;
  localPath: string;
  /** Markdown as it lives in the vault (Obsidian-form). Hashed for state. */
  localMd: string;
  /** Markdown prepared for lark-cli. With the v2 API this equals `localMd`. */
  pushMd: string;
  localHash: string;
}

export type ConflictResolution = "keep-local" | "keep-remote" | "sidecar";

export interface PendingConflict {
  space: WikiSpaceConfig;
  node: NodeRef;
  localPath: string;
  localMd: string;
  remoteMd: string;
  prev: FileSyncState;
  /** Per-conflict override set by the resolve modal. Falls back to the
   * global `conflictPolicy` setting when absent. */
  resolution?: ConflictResolution;
}

/** Reconcile = "we have a local file matching remote exactly but no state — adopt it." */
export interface PendingReconcile {
  space: WikiSpaceConfig;
  node: NodeRef;
  localPath: string;
  hash: string;
}

/** Rebaseline = "a legacy (pre-v4) entry is unchanged on both sides; just
 *  stamp its `remoteEditTime` so it stops re-fetching every sync." No content
 *  is written anywhere. */
export interface PendingRebaseline {
  space: WikiSpaceConfig;
  node: NodeRef;
  localPath: string;
  localBaseHash: string;
}

export interface SyncPlan {
  pulls: PendingPull[];
  pushes: PendingPush[];
  conflicts: PendingConflict[];
  reconciles: PendingReconcile[];
  rebaselines: PendingRebaseline[];
  skipped: number;
}

export type PlanDecision = "applyAll" | "pullsOnly" | "cancel";

export interface ProgressEvent {
  phase: "list" | "classify" | "pull" | "push" | "conflict";
  spaceName: string;
  /** Items processed in the current phase. */
  current?: number;
  /** Total items expected in the current phase, if known. */
  total?: number;
  /** Optional file path or label for the item being processed. */
  label?: string;
}

export interface RunOptions {
  dryRun?: boolean;
  /**
   * Called once after `plan()` returns so the UI can show a preview of what
   * will be done. The callback returns one of three actions:
   *   - "applyAll":   apply every pull, push, conflict, and reconcile.
   *   - "pullsOnly":  apply pulls + reconciles + conflicts; skip pushes.
   *   - "cancel":     do nothing.
   * If undefined, behaviour is "applyAll".
   */
  confirmPlan?: (plan: SyncPlan) => Promise<PlanDecision>;
  /**
   * Called once between the plan modal and apply, only if `conflictPolicy`
   * is "ask" and there is at least one conflict. Returns per-conflict
   * resolutions keyed by `nodeToken`. Conflicts not present in the returned
   * map fall back to the sidecar default.
   */
  resolveConflicts?: (
    conflicts: PendingConflict[],
  ) => Promise<Record<string, ConflictResolution>>;
  /** Called as the engine progresses through phases. Best-effort, not exact. */
  onProgress?: (e: ProgressEvent) => void;
  /** Limit sync to a single space (matched by spaceId). */
  onlySpaceId?: string;
}

export class SyncEngine {
  constructor(
    private app: App,
    private settings: LarkWikiSyncSettings,
    private lark: LarkCli,
    private state: StateStore,
  ) {}

  // ---------------------------------------------------------------------------
  // Top-level entry
  // ---------------------------------------------------------------------------

  async run(opts: RunOptions = {}): Promise<SyncResult> {
    const plan = await this.plan(opts);

    if (opts.confirmPlan) {
      const decision = await opts.confirmPlan(plan);
      if (decision === "cancel") {
        return emptyResult();
      }
      if (decision === "pullsOnly") {
        plan.pushes = [];
      }
    }

    if (
      opts.resolveConflicts &&
      this.settings.conflictPolicy === "ask" &&
      plan.conflicts.length > 0
    ) {
      const resolutions = await opts.resolveConflicts(plan.conflicts);
      for (const c of plan.conflicts) {
        const r = resolutions[c.node.node_token];
        if (r) c.resolution = r;
      }
    }

    if (opts.dryRun) {
      return {
        pulled: plan.pulls.length,
        pushed: plan.pushes.length,
        conflicts: plan.conflicts.length,
        // Rebaselines are invisible housekeeping — count them as skips.
        skipped: plan.skipped + plan.rebaselines.length,
        reconciled: plan.reconciles.length,
        errors: [],
      };
    }

    return this.apply(plan, opts);
  }

  /**
   * Walk every configured space, classify every node, and return a plan
   * without writing anything anywhere. Network reads (list + fetch +
   * download attachments) DO happen here — only mutations are deferred.
   */
  async plan(opts: RunOptions = {}): Promise<SyncPlan> {
    const plan: SyncPlan = {
      pulls: [],
      pushes: [],
      conflicts: [],
      reconciles: [],
      rebaselines: [],
      skipped: 0,
    };

    let spaces = this.settings.spaces ?? [];
    if (opts.onlySpaceId) {
      spaces = spaces.filter((s) => s.spaceId === opts.onlySpaceId);
    }
    if (spaces.length === 0) {
      throw new Error("No wiki spaces configured. Open settings to add one.");
    }

    for (const space of spaces) {
      try {
        await this.planOneSpace(space, plan, opts);
      } catch (err) {
        console.error(
          `LarkWikiSync: planning failed for "${space.spaceName || space.spaceId}":`,
          err,
        );
        throw err; // bubble up so the user sees a Notice
      }
    }
    return plan;
  }

  /** Execute a (possibly user-edited) plan. */
  async apply(plan: SyncPlan, opts: RunOptions = {}): Promise<SyncResult> {
    const emit = (e: ProgressEvent) => opts.onProgress?.(e);
    const result: SyncResult = {
      pulled: 0,
      pushed: 0,
      conflicts: 0,
      skipped: plan.skipped + plan.rebaselines.length,
      reconciled: 0,
      errors: [],
    };

    for (const r of plan.reconciles) {
      // Reconcile only fires when localHash === remoteHash.
      this.recordSync(r.localPath, r.node.node_token, r.node.obj_token, {
        localBaseHash: r.hash,
        remoteEditTime: r.node.obj_edit_time,
      });
      result.reconciled++;
    }

    // Rebaselines: legacy (pre-v4) entries unchanged on both sides — just
    // stamp the new remoteEditTime so they stop re-fetching every sync.
    for (const rb of plan.rebaselines) {
      this.recordSync(rb.localPath, rb.node.node_token, rb.node.obj_token, {
        localBaseHash: rb.localBaseHash,
        remoteEditTime: rb.node.obj_edit_time,
      });
    }

    for (let i = 0; i < plan.pulls.length; i++) {
      const p = plan.pulls[i];
      emit({
        phase: "pull",
        spaceName: p.space.spaceName || p.space.spaceId,
        current: i + 1,
        total: plan.pulls.length,
        label: p.node.title,
      });
      try {
        await this.writeLocal(p.localPath, p.remoteMd);
        // The vault file now holds remoteMd; the remote is at obj_edit_time.
        this.recordSync(p.localPath, p.node.node_token, p.node.obj_token, {
          localBaseHash: p.remoteHash,
          remoteEditTime: p.node.obj_edit_time,
        });
        result.pulled++;
      } catch (err) {
        const message = (err as Error).message ?? String(err);
        console.error(`LarkWikiSync: pull failed for ${p.localPath}:`, err);
        result.errors.push({ phase: "pull", file: p.localPath, message });
      }
    }

    for (let i = 0; i < plan.pushes.length; i++) {
      const p = plan.pushes[i];
      emit({
        phase: "push",
        spaceName: p.space.spaceName || p.space.spaceId,
        current: i + 1,
        total: plan.pushes.length,
        label: p.node.title,
      });
      try {
        await this.lark.updateDoc(p.node.obj_token, p.pushMd, "overwrite");
        // Capture the node's new obj_edit_time as the remote baseline so the
        // next sync doesn't see our own write as a remote change. If this
        // throws, state is left untouched; the next sync re-pushes (overwrite
        // is idempotent).
        const verified = await this.captureRemoteBaseline(
          p.node.node_token,
          p.localHash,
        );
        this.recordSync(p.localPath, p.node.node_token, p.node.obj_token, verified);
        result.pushed++;
      } catch (err) {
        const message = (err as Error).message ?? String(err);
        console.error(`LarkWikiSync: push failed for ${p.localPath}:`, err);
        result.errors.push({ phase: "push", file: p.localPath, message });
      }
    }

    for (let i = 0; i < plan.conflicts.length; i++) {
      const c = plan.conflicts[i];
      emit({
        phase: "conflict",
        spaceName: c.space.spaceName || c.space.spaceId,
        current: i + 1,
        total: plan.conflicts.length,
        label: c.node.title,
      });
      try {
        await this.handleConflict(c);
        result.conflicts++;
      } catch (err) {
        const message = (err as Error).message ?? String(err);
        console.error(`LarkWikiSync: conflict handling failed for ${c.localPath}:`, err);
        result.errors.push({ phase: "conflict", file: c.localPath, message });
      }
    }

    const now = new Date().toISOString();
    this.settings.lastSyncedAt = now;

    // Stamp per-space lastSyncedAt for any space that had any executed action.
    const touched = new Set<string>();
    for (const p of plan.pulls) touched.add(p.space.spaceId);
    for (const p of plan.pushes) touched.add(p.space.spaceId);
    for (const c of plan.conflicts) touched.add(c.space.spaceId);
    for (const r of plan.reconciles) touched.add(r.space.spaceId);
    for (const space of this.settings.spaces) {
      if (touched.has(space.spaceId)) space.lastSyncedAt = now;
    }

    await this.state.save();

    return result;
  }

  // ---------------------------------------------------------------------------
  // Planning
  // ---------------------------------------------------------------------------

  private async planOneSpace(
    space: WikiSpaceConfig,
    plan: SyncPlan,
    opts: RunOptions = {},
  ): Promise<void> {
    const emit = (e: ProgressEvent) => opts.onProgress?.(e);
    const spaceLabel = space.spaceName || space.spaceId;

    emit({ phase: "list", spaceName: spaceLabel });
    const nodes = await this.lark.listAllDescendants(
      space.spaceId,
      space.rootNode || undefined,
    );

    const effectiveRoot = this.effectiveRoot(space);

    // node_token → doc title, used to rewrite intra-wiki links into Obsidian
    // wikilinks during the Lark→Obsidian transform. Seed from state (already
    // synced docs from any space) and overlay the current walk so freshly-
    // discovered links resolve too.
    const nodeTitleMap: Record<string, string> = {};
    for (const entry of this.state.all()) {
      const filename = entry.localPath.split("/").pop() ?? "";
      const title = filename.replace(/\.md$/, "");
      if (title) nodeTitleMap[entry.nodeToken] = title;
    }
    for (const n of nodes) {
      if (n.obj_type === "docx" && n.title) nodeTitleMap[n.node_token] = n.title;
    }

    // Attachment plumbing is only touched when a doc is actually fetched, so
    // resolve it lazily — a no-change sync does zero attachment I/O.
    let attachmentsRel: string | undefined;
    let attachmentsAbs: string | undefined;
    let existingAttachments: Record<string, string> | undefined;
    const ensureAttachments = async () => {
      if (existingAttachments) return;
      attachmentsRel = `${effectiveRoot}/${ATTACHMENTS_SUBFOLDER}`;
      attachmentsAbs = this.resolveAttachmentsAbsolutePath(space);
      existingAttachments = await this.scanAttachmentsCache(attachmentsRel);
    };

    let classified = 0;
    const totalDocx = nodes.filter((n) => n.obj_type === "docx").length;

    for (const node of nodes) {
      if (node.obj_type !== "docx") continue;

      classified++;
      emit({
        phase: "classify",
        spaceName: spaceLabel,
        current: classified,
        total: totalDocx,
        label: node.title,
      });

      const localPath = this.mapNodeToLocalPath(space, node);

      if (matchAnyGlob(localPath, this.settings.ignorePatterns ?? [])) {
        plan.skipped++;
        continue;
      }

      const existing = this.state.get(node.node_token);
      const editTime = String(node.obj_edit_time ?? "");
      const nodeRef: NodeRef = {
        node_token: node.node_token,
        obj_token: node.obj_token,
        title: node.title,
        obj_edit_time: editTime,
      };

      try {
        // Local side — always cheap, no network.
        const localFile = this.app.vault.getAbstractFileByPath(localPath);
        const localMd =
          localFile instanceof TFile ? await this.app.vault.read(localFile) : null;
        const localHash = localMd ? hashString(localMd) : null;

        // Per-file opt-out via `lark_sync: false` frontmatter. Honoured even
        // for files that are tracked in state — flipping the flag pauses sync
        // for that file without removing it from either side.
        if (localMd && hasLarkSyncFalse(localMd)) {
          plan.skipped++;
          continue;
        }

        // Remote-change detection via obj_edit_time — no fetch needed. A doc
        // is fetched only when we genuinely need its content: first sight of
        // the node, the edit time moved, or a legacy (pre-v4) entry that must
        // still be re-baselined off its old content hash.
        const isLegacy =
          !!existing && !existing.remoteEditTime && existing.remoteBaseHash !== undefined;
        const editTimeChanged =
          !!existing && !!existing.remoteEditTime && editTime !== existing.remoteEditTime;
        const mustFetch = !existing || editTimeChanged || isLegacy;

        let remoteMd: string | null = null;
        let remoteHash: string | null = null;
        if (mustFetch) {
          await ensureAttachments();
          const rawXml = await this.lark.fetchDoc(node.obj_token);
          const imageMap = await this.resolveImageMap(
            rawXml,
            attachmentsRel!,
            attachmentsAbs!,
            existingAttachments!,
          );
          remoteMd = larkXmlToObsidianMarkdown(rawXml, { imageMap, nodeTitleMap });
          remoteHash = hashString(remoteMd);
        }

        // No prior state — first-sync discovery for this node.
        if (!existing) {
          if (!localFile) {
            plan.pulls.push({
              space, node: nodeRef, localPath,
              remoteMd: remoteMd!, remoteHash: remoteHash!,
            });
            continue;
          }
          if (localHash === remoteHash) {
            // Local file matches remote exactly: silently adopt it.
            plan.reconciles.push({ space, node: nodeRef, localPath, hash: remoteHash! });
            continue;
          }
          // Local file exists with different content — genuine collision.
          plan.conflicts.push({
            space,
            node: nodeRef,
            localPath,
            localMd: localMd!,
            remoteMd: remoteMd!,
            prev: {
              localPath,
              nodeToken: node.node_token,
              docToken: node.obj_token,
              localBaseHash: "",
              remoteEditTime: "",
              lastSyncedAt: "",
            },
          });
          continue;
        }

        // Existing entry — two-axis classification. `localChanged` is a
        // content-hash compare (we have the file); `remoteChanged` is the
        // obj_edit_time compare, except for legacy entries which get one
        // content-hash compare to re-baseline cleanly.
        const localChanged = localHash !== null && localHash !== existing.localBaseHash;
        const remoteChanged = !mustFetch
          ? false
          : isLegacy
            ? remoteHash !== existing.remoteBaseHash
            : true;

        switch (decide(localChanged, remoteChanged, true)) {
          case SyncDecision.NoChange:
            if (isLegacy) {
              // Unchanged on both sides, but the legacy entry still needs its
              // remoteEditTime stamped so it stops re-fetching every sync.
              plan.rebaselines.push({
                space,
                node: nodeRef,
                localPath,
                localBaseHash: existing.localBaseHash,
              });
            } else {
              plan.skipped++;
            }
            break;

          case SyncDecision.PullRemote:
            plan.pulls.push({
              space, node: nodeRef, localPath,
              remoteMd: remoteMd!, remoteHash: remoteHash!,
            });
            break;

          case SyncDecision.PushLocal: {
            if (this.settings.direction === "pull") {
              plan.skipped++;
              break;
            }
            plan.pushes.push({
              space,
              node: nodeRef,
              localPath,
              localMd: localMd!,
              pushMd: obsidianToLarkMarkdown(localMd!),
              localHash: localHash!,
            });
            break;
          }

          case SyncDecision.BothModified:
            plan.conflicts.push({
              space,
              node: nodeRef,
              localPath,
              localMd: localMd!,
              remoteMd: remoteMd!,
              prev: existing,
            });
            break;

          case SyncDecision.NewFile:
            // Unreachable: hasBase is true in this branch. Listed so the
            // switch stays exhaustive if SyncDecision grows.
            break;
        }
      } catch (err) {
        console.error(`LarkWikiSync: classify failed on ${localPath}`, err);
      }
    }
  }

  // ---------------------------------------------------------------------------
  // Per-space path helpers
  // ---------------------------------------------------------------------------

  private effectiveRoot(space: WikiSpaceConfig): string {
    const sanitize = (s: string) => s.replace(/[\\/:*?"<>|]/g, "_");
    const parts = [this.settings.localRoot];
    if (space.spaceName) parts.push(sanitize(space.spaceName));
    return parts.join("/");
  }

  private resolveAttachmentsAbsolutePath(space: WikiSpaceConfig): string {
    const adapter = this.app.vault.adapter;
    if (!(adapter instanceof FileSystemAdapter)) {
      throw new Error("Lark Wiki Sync requires Obsidian desktop (FileSystemAdapter).");
    }
    return `${adapter.getBasePath()}/${this.effectiveRoot(space)}/${ATTACHMENTS_SUBFOLDER}`;
  }

  private mapNodeToLocalPath(
    space: WikiSpaceConfig,
    node: {
      title: string;
      node_token: string;
      obj_type: string;
      parentPath?: string[];
    },
  ): string {
    const sanitize = (s: string) => s.replace(/[\\/:*?"<>|]/g, "_");
    const segments = [
      this.effectiveRoot(space),
      ...(node.parentPath ?? []).map(sanitize),
      `${sanitize(node.title)}.md`,
    ];
    return normalizePath(segments.join("/"));
  }

  // ---------------------------------------------------------------------------
  // Attachments
  // ---------------------------------------------------------------------------

  private async scanAttachmentsCache(relFolder: string): Promise<Record<string, string>> {
    const cache: Record<string, string> = {};
    const adapter = this.app.vault.adapter;
    if (!(await adapter.exists(relFolder))) return cache;
    const listing = await adapter.list(relFolder);
    for (const filePath of listing.files) {
      const filename = filePath.split("/").pop();
      if (!filename) continue;
      const dot = filename.indexOf(".");
      const token = dot > 0 ? filename.slice(0, dot) : filename;
      cache[token] = filename;
    }
    return cache;
  }

  private async resolveImageMap(
    rawXml: string,
    relFolder: string,
    absFolder: string,
    cache: Record<string, string>,
  ): Promise<Record<string, string>> {
    const tokens = extractImageTokens(rawXml);
    if (tokens.length === 0) return {};

    const map: Record<string, string> = {};
    const toDownload: string[] = [];
    for (const token of tokens) {
      if (cache[token]) {
        map[token] = cache[token];
      } else {
        toDownload.push(token);
      }
    }
    if (toDownload.length === 0) return map;

    await this.ensureFolder(relFolder);

    for (const token of toDownload) {
      try {
        const filename = await this.lark.downloadMedia(token, absFolder);
        if (filename) {
          cache[token] = filename;
          map[token] = filename;
        }
      } catch (err) {
        console.warn(`LarkWikiSync: failed to download image ${token}`, err);
      }
    }
    return map;
  }

  // ---------------------------------------------------------------------------
  // Vault I/O
  // ---------------------------------------------------------------------------

  private async writeLocal(path: string, content: string): Promise<void> {
    const folder = path.substring(0, path.lastIndexOf("/"));
    if (folder) await this.ensureFolder(folder);
    const existing = this.app.vault.getAbstractFileByPath(path);
    if (existing instanceof TFile) {
      await this.app.vault.modify(existing, content);
    } else {
      await this.app.vault.create(path, content);
    }
  }

  private async ensureFolder(folder: string): Promise<void> {
    const parts = folder.split("/").filter(Boolean);
    let cur = "";
    for (const p of parts) {
      cur = cur ? `${cur}/${p}` : p;
      if (!(await this.app.vault.adapter.exists(cur))) {
        try {
          await this.app.vault.createFolder(cur);
        } catch (err) {
          if (!(await this.app.vault.adapter.exists(cur))) throw err;
        }
      }
    }
  }

  private recordSync(
    localPath: string,
    nodeToken: string,
    docToken: string,
    baseline: { localBaseHash: string; remoteEditTime: string },
  ): void {
    const entry: FileSyncState = {
      localPath,
      nodeToken,
      docToken,
      localBaseHash: baseline.localBaseHash,
      remoteEditTime: baseline.remoteEditTime,
      // `remoteBaseHash` deliberately omitted — recording an entry drops the
      // legacy field, completing the v3→v4 migration for this node.
      lastSyncedAt: new Date().toISOString(),
    };
    this.state.upsert(entry);
  }

  /**
   * After a push, capture the node's new `obj_edit_time` as the remote
   * baseline so the next sync doesn't see our own write as a remote change.
   * `getNode` succeeding also confirms the node still exists. Replaces the
   * increment-1 content-hash readback — with remote change now keyed on
   * `obj_edit_time`, the fresh edit time is all the post-push state we need.
   * Ported in spirit from larknotes `Executor::refresh_modify_baseline`.
   *
   * If `obj_edit_time` came back stale (Lark eventual consistency right after
   * a write), the worst case is one spurious pull next sync that re-writes
   * content-equivalent markdown — non-destructive, and it self-heals.
   */
  private async captureRemoteBaseline(
    nodeToken: string,
    localHash: string,
  ): Promise<{ localBaseHash: string; remoteEditTime: string }> {
    const node = await this.lark.getNode(nodeToken);
    const editTime = node?.obj_edit_time;
    if (!editTime) {
      throw new Error("push verification failed: getNode returned no obj_edit_time");
    }
    return { localBaseHash: localHash, remoteEditTime: String(editTime) };
  }

  private async handleConflict(c: PendingConflict): Promise<void> {
    const action: ConflictResolution = c.resolution ?? this.policyToResolution();
    switch (action) {
      case "keep-local": {
        const pushMd = obsidianToLarkMarkdown(c.localMd);
        await this.lark.updateDoc(c.node.obj_token, pushMd, "overwrite");
        // Same post-push baseline capture as a normal push.
        const verified = await this.captureRemoteBaseline(
          c.node.node_token,
          hashString(c.localMd),
        );
        this.recordSync(c.localPath, c.node.node_token, c.node.obj_token, verified);
        return;
      }
      case "keep-remote": {
        await this.writeLocal(c.localPath, c.remoteMd);
        // remoteMd is now on disk; the remote is at the node's obj_edit_time.
        this.recordSync(c.localPath, c.node.node_token, c.node.obj_token, {
          localBaseHash: hashString(c.remoteMd),
          remoteEditTime: c.node.obj_edit_time,
        });
        return;
      }
      case "sidecar":
      default: {
        const conflictPath = `${c.localPath}.remote.conflict.md`;
        await this.writeLocal(conflictPath, c.remoteMd);
        console.warn(
          `LarkWikiSync: conflict on ${c.localPath}. Remote saved to ${conflictPath}; manual merge required.`,
        );
        return;
      }
    }
  }

  private policyToResolution(): ConflictResolution {
    switch (this.settings.conflictPolicy) {
      case "prefer-local":
        return "keep-local";
      case "prefer-remote":
        return "keep-remote";
      default:
        return "sidecar";
    }
  }
}

function emptyResult(): SyncResult {
  return { pulled: 0, pushed: 0, conflicts: 0, skipped: 0, reconciled: 0, errors: [] };
}
