<script lang="ts">
  import { openDocInEditor } from "../api";
  import { syncStatusLabel, syncStatusColor, highlightSegments, formatRelativeTime, formatShortDate, sortDocs } from "../types";
  import type { DocMeta, SortField, SortDirection } from "../types";
  import ContextMenu from "./ContextMenu.svelte";

  interface Props {
    docs: DocMeta[];
    searchQuery?: string;
    onError: (msg: string) => void;
    onSync: (docId: string) => void;
    onPull?: (docId: string) => void;
    onImport?: (docId: string) => void;
    onDelete?: (docId: string) => void;
    onReveal?: (docId: string) => void;
    onShowHistory?: (docId: string) => void;
    onResolveConflict?: (docId: string) => void;
    onBatchDelete?: (docIds: string[]) => void;
    onBatchSync?: (docIds: string[]) => void;
  }

  let {
    docs, searchQuery = "", onError, onSync, onPull, onImport,
    onDelete, onReveal, onShowHistory, onResolveConflict,
    onBatchDelete, onBatchSync,
  }: Props = $props();

  let openingId = $state<string | null>(null);
  let contextMenu = $state<{ x: number; y: number; doc: DocMeta } | null>(null);
  let sortField = $state<SortField>("updated_at");
  let sortDirection = $state<SortDirection>("desc");
  let batchMode = $state(false);
  let selected = $state<Set<string>>(new Set());

  let sortedDocs = $derived(sortDocs(docs, sortField, sortDirection));
  let selectedCount = $derived(selected.size);
  let allSelected = $derived(sortedDocs.length > 0 && selected.size === sortedDocs.length);

  function toggleSort(field: SortField) {
    if (sortField === field) {
      sortDirection = sortDirection === "desc" ? "asc" : "desc";
    } else {
      sortField = field;
      sortDirection = field === "title" ? "asc" : "desc";
    }
  }

  function toggleBatchMode() {
    batchMode = !batchMode;
    if (!batchMode) selected = new Set();
  }

  function toggleSelect(docId: string) {
    const next = new Set(selected);
    if (next.has(docId)) next.delete(docId); else next.add(docId);
    selected = next;
  }

  function toggleSelectAll() {
    if (allSelected) {
      selected = new Set();
    } else {
      selected = new Set(sortedDocs.map(d => d.doc_id));
    }
  }

  function handleBatchDelete() {
    if (selectedCount === 0) return;
    onBatchDelete?.([...selected]);
  }

  function handleBatchSync() {
    if (selectedCount === 0) return;
    onBatchSync?.([...selected]);
  }

  async function handleOpen(doc: DocMeta) {
    if (batchMode) { toggleSelect(doc.doc_id); return; }
    if (openingId) return;
    openingId = doc.doc_id;
    try {
      await openDocInEditor(doc.doc_id);
    } catch (e) {
      onError(`打开失败: ${e}`);
    } finally {
      openingId = null;
    }
  }

  function handleContextMenu(e: MouseEvent, doc: DocMeta) {
    e.preventDefault();
    contextMenu = { x: e.clientX, y: e.clientY, doc };
  }

  function getContextMenuItems(doc: DocMeta) {
    const items: { label: string; action: () => void; danger?: boolean; separator?: boolean }[] = [
      { label: "在编辑器中打开", action: () => handleOpen(doc) },
    ];
    if (doc.url) {
      items.push({
        label: "在飞书中打开",
        action: () => window.open(doc.url, "_blank"),
      });
    }
    if (doc.local_path) {
      items.push({
        label: "在文件管理器中显示",
        action: () => onReveal?.(doc.doc_id),
      });
    }
    items.push({
      label: "推送到远程",
      action: () => onSync(doc.doc_id),
      separator: true,
    });
    items.push({
      label: "从远程拉取",
      action: () => onPull?.(doc.doc_id),
    });
    if (doc.sync_status.type === "Conflict") {
      items.push({
        label: "解决冲突",
        action: () => onResolveConflict?.(doc.doc_id),
      });
    }
    items.push({
      label: "查看历史",
      action: () => onShowHistory?.(doc.doc_id),
      separator: true,
    });
    items.push({
      label: "删除",
      action: () => onDelete?.(doc.doc_id),
      danger: true,
      separator: true,
    });
    return items;
  }

  function isWarning(type: string): boolean {
    return type === "Conflict" || type === "Error";
  }

  function animDelay(i: number): number {
    return Math.min(i * 25, 250);
  }

  function sortArrow(field: SortField): string {
    if (sortField !== field) return "";
    return sortDirection === "asc" ? " \u2191" : " \u2193";
  }
</script>

<div class="doc-list">
  {#if docs.length === 0 && !searchQuery}
    <div class="empty-state">
      <div class="empty-illustration">
        <svg width="64" height="64" viewBox="0 0 64 64" fill="none">
          <rect x="14" y="8" width="36" height="48" rx="4" stroke="var(--c-text-tertiary)" stroke-width="1.5" opacity="0.25"/>
          <rect x="18" y="12" width="28" height="40" rx="2" stroke="var(--c-text-tertiary)" stroke-width="1" opacity="0.15" stroke-dasharray="2 2"/>
          <path d="M22 22h20M22 28h16M22 34h12" stroke="var(--c-text-tertiary)" stroke-width="1.5" stroke-linecap="round" opacity="0.15"/>
          <circle cx="44" cy="44" r="14" fill="var(--c-bg)" stroke="var(--c-accent)" stroke-width="1.5" opacity="0.6"/>
          <path d="M44 38v12M38 44h12" stroke="var(--c-accent)" stroke-width="1.8" stroke-linecap="round" opacity="0.8"/>
        </svg>
      </div>
      <p class="empty-title">还没有文档</p>
      <p class="empty-hint">点击上方 <kbd class="empty-kbd">快速笔记</kbd> 或按 <kbd class="empty-kbd">Ctrl+N</kbd> 创建第一篇</p>
      <p class="empty-hint">也可以使用搜索功能从飞书导入已有文档</p>
    </div>
  {:else if docs.length === 0 && searchQuery}
    <div class="empty-state">
      <p class="empty-title">没有找到匹配的文档</p>
      <p class="empty-hint">试试其他关键词</p>
    </div>
  {:else}
    <div class="list-header">
      <div class="header-left">
        {#if batchMode}
          <label class="batch-checkbox-label">
            <input type="checkbox" class="batch-checkbox" checked={allSelected} onchange={toggleSelectAll} />
          </label>
        {/if}
        <button class="sort-btn" class:sort-btn--active={sortField === "title"} onclick={() => toggleSort("title")}>
          标题{sortArrow("title")}
        </button>
        {#if searchQuery}
          <span class="search-count">({docs.length} 篇)</span>
        {/if}
      </div>
      <div class="header-right">
        {#if batchMode && selectedCount > 0}
          <div class="batch-actions">
            <button class="batch-btn batch-btn--sync" onclick={handleBatchSync}>
              同步 ({selectedCount})
            </button>
            <button class="batch-btn batch-btn--danger" onclick={handleBatchDelete}>
              删除 ({selectedCount})
            </button>
          </div>
        {/if}
        <button class="sort-btn" class:sort-btn--active={sortField === "updated_at"} onclick={() => toggleSort("updated_at")}>
          时间{sortArrow("updated_at")}
        </button>
        <button class="sort-btn" class:sort-btn--active={sortField === "sync_status"} onclick={() => toggleSort("sync_status")}>
          状态{sortArrow("sync_status")}
        </button>
        <button class="manage-btn" class:manage-btn--active={batchMode} onclick={toggleBatchMode}>
          {batchMode ? "完成" : "管理"}
        </button>
      </div>
    </div>
    <div class="list">
      {#each sortedDocs as doc, i (doc.doc_id)}
        <div
          class="doc-row"
          class:doc-row--warn={isWarning(doc.sync_status.type)}
          class:doc-row--opening={!batchMode && openingId === doc.doc_id}
          class:doc-row--selected={batchMode && selected.has(doc.doc_id)}
          role="button"
          tabindex="0"
          onclick={() => handleOpen(doc)}
          oncontextmenu={(e: MouseEvent) => handleContextMenu(e, doc)}
          onkeydown={(e: KeyboardEvent) => { if (e.key === 'Enter' || e.key === ' ') handleOpen(doc); }}
          style="animation-delay: {animDelay(i)}ms"
        >
          {#if isWarning(doc.sync_status.type)}
            <div class="doc-accent-bar" style="background: {syncStatusColor(doc.sync_status.type)};"></div>
          {/if}

          {#if batchMode}
            <label class="batch-checkbox-label" onclick={(e: MouseEvent) => e.stopPropagation()}>
              <input type="checkbox" class="batch-checkbox" checked={selected.has(doc.doc_id)} onchange={() => toggleSelect(doc.doc_id)} />
            </label>
          {:else}
            <div class="doc-status-dot" style="background: {syncStatusColor(doc.sync_status.type)};"></div>
          {/if}

          <div class="doc-info">
            <span class="doc-title">
              {#if searchQuery}
                {#each highlightSegments(doc.title || "Untitled", searchQuery) as seg}
                  {#if seg.match}<mark class="highlight">{seg.text}</mark>{:else}{seg.text}{/if}
                {/each}
              {:else}
                {doc.title || "Untitled"}
              {/if}
            </span>
            <span class="doc-meta">
              {formatRelativeTime(doc.updated_at)}{#if doc.created_at} · {formatShortDate(doc.created_at)}{/if}
            </span>
          </div>

          <div class="doc-badge">
            <span class="badge" style="color: {syncStatusColor(doc.sync_status.type)};">
              {syncStatusLabel(doc.sync_status)}
            </span>
          </div>

          {#if !batchMode}
            <button
              class="doc-action"
              onclick={(e: MouseEvent) => { e.stopPropagation(); onSync(doc.doc_id); }}
              title="推送同步"
            >
              <svg width="14" height="14" viewBox="0 0 15 15" fill="none">
                <path d="M2.5 7.5a5 5 0 019-3M12.5 7.5a5 5 0 01-9 3" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>
                <path d="M11.5 1.5v3h-3M3.5 13.5v-3h3" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/>
              </svg>
            </button>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

{#if contextMenu}
  <ContextMenu
    x={contextMenu.x}
    y={contextMenu.y}
    items={getContextMenuItems(contextMenu.doc)}
    onClose={() => (contextMenu = null)}
  />
{/if}

<style>
  .doc-list {
    flex: 1;
    overflow-y: auto;
    padding: 4px 6px;
  }
  /* Empty state */
  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    gap: 10px;
    color: var(--c-text-tertiary);
    padding: 40px 20px;
  }
  .empty-illustration { margin-bottom: 8px; }
  .empty-title {
    font-size: 15px;
    font-weight: 500;
    color: var(--c-text-secondary);
    margin: 0;
  }
  .empty-hint {
    font-size: 12px;
    margin: 0;
    line-height: 1.7;
    text-align: center;
  }
  .empty-kbd {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 1px 8px;
    border-radius: var(--radius-sm);
    background: var(--c-accent-bg, rgba(212,165,71,0.1));
    color: var(--c-accent);
    font-weight: 500;
    font-size: 11px;
    font-family: var(--font-sans);
    border: 1px solid var(--c-accent-border, rgba(212,165,71,0.15));
  }

  /* List header — matches doc-row column layout */
  .list-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 4px 10px 2px;
    gap: 10px;
  }
  .header-left {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: 1;
    min-width: 0;
  }
  .header-right {
    display: flex;
    align-items: center;
    gap: 2px;
    flex-shrink: 0;
  }
  .search-count {
    font-size: 11px;
    color: var(--c-text-tertiary);
    letter-spacing: 0.02em;
  }
  .sort-btn {
    padding: 2px 8px;
    border: none;
    outline: none;
    background: transparent;
    color: var(--c-text-tertiary);
    font-size: 11px;
    font-family: var(--font-sans);
    cursor: pointer;
    border-radius: var(--radius-sm);
    transition: all var(--transition);
  }
  .sort-btn:hover {
    color: var(--c-text-secondary);
    background: var(--c-bg-hover);
  }
  .sort-btn--active {
    color: var(--c-accent);
    font-weight: 600;
  }
  .manage-btn {
    padding: 2px 10px;
    border: 1px solid var(--c-border, rgba(0,0,0,0.08));
    outline: none;
    background: transparent;
    color: var(--c-text-tertiary);
    font-size: 11px;
    font-family: var(--font-sans);
    cursor: pointer;
    border-radius: var(--radius-sm);
    transition: all var(--transition);
    margin-left: 4px;
  }
  .manage-btn:hover {
    color: var(--c-text-secondary);
    background: var(--c-bg-hover);
  }
  .manage-btn--active {
    color: var(--c-accent);
    border-color: var(--c-accent);
    font-weight: 500;
  }

  /* Batch actions */
  .batch-actions {
    display: flex;
    gap: 4px;
    margin-right: 8px;
  }
  .batch-btn {
    padding: 2px 10px;
    border: none;
    outline: none;
    border-radius: var(--radius-sm);
    font-size: 11px;
    font-family: var(--font-sans);
    font-weight: 500;
    cursor: pointer;
    transition: all var(--transition);
  }
  .batch-btn--sync {
    background: var(--c-accent-bg, rgba(212,165,71,0.1));
    color: var(--c-accent);
  }
  .batch-btn--sync:hover {
    background: var(--c-accent-bg, rgba(212,165,71,0.2));
  }
  .batch-btn--danger {
    background: rgba(212,91,91,0.08);
    color: #d45b5b;
  }
  .batch-btn--danger:hover {
    background: rgba(212,91,91,0.15);
  }

  /* Batch checkbox */
  .batch-checkbox-label {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    flex-shrink: 0;
  }
  .batch-checkbox {
    width: 14px;
    height: 14px;
    margin: 0;
    cursor: pointer;
    accent-color: var(--c-accent);
  }

  /* List */
  .list { display: flex; flex-direction: column; gap: 1px; }

  .doc-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px 10px;
    border-radius: var(--radius);
    cursor: pointer;
    border: none;
    outline: none;
    background: transparent;
    transition: all var(--transition);
    text-align: left;
    width: 100%;
    font-family: var(--font-sans);
    animation: fadeSlideIn 250ms ease both;
    position: relative;
    overflow: hidden;
  }
  .doc-row:hover {
    background: var(--c-bg-hover);
  }
  .doc-row:active {
    background: var(--c-bg-active);
  }
  .doc-row--opening {
    opacity: 0.6;
    pointer-events: none;
  }
  .doc-row--selected {
    background: var(--c-accent-bg, rgba(212,165,71,0.06));
  }

  /* Warning rows get a left accent bar */
  .doc-row--warn {
    background: rgba(212,91,91,0.03);
  }
  .doc-accent-bar {
    position: absolute;
    left: 0;
    top: 4px;
    bottom: 4px;
    width: 3px;
    border-radius: 2px;
    opacity: 0.6;
  }

  @keyframes fadeSlideIn {
    from { opacity: 0; transform: translateY(4px); }
    to { opacity: 1; transform: translateY(0); }
  }

  .doc-status-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .doc-info {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: 12px;
  }
  .doc-title {
    flex: 1;
    min-width: 0;
    font-size: 13px;
    font-weight: 500;
    color: var(--c-text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    letter-spacing: -0.01em;
  }
  .doc-meta {
    flex-shrink: 0;
    font-size: 11px;
    color: var(--c-text-tertiary);
    letter-spacing: 0.01em;
    white-space: nowrap;
  }
  .doc-badge {
    flex-shrink: 0;
  }
  .badge {
    font-size: 11px;
    font-weight: 500;
    letter-spacing: 0.02em;
  }
  .doc-action {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border-radius: var(--radius);
    border: none;
    outline: none;
    background: transparent;
    color: var(--c-text-tertiary);
    cursor: pointer;
    transition: all var(--transition);
    opacity: 0;
    flex-shrink: 0;
  }
  .doc-row:hover .doc-action { opacity: 1; }
  .doc-action:hover {
    background: var(--c-bg-active);
    color: var(--c-text);
  }

  .highlight {
    background: var(--c-accent-bg, rgba(212,165,71,0.25));
    color: var(--c-accent);
    border-radius: 2px;
    padding: 0 1px;
  }
</style>
