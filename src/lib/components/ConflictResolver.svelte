<script lang="ts">
  import { onMount } from "svelte";
  import { getConflictDiff, resolveConflict } from "../api";
  import type { DocMeta } from "../types";

  interface Props {
    docId: string;
    docTitle: string;
    onClose: () => void;
    onResolved: (doc: DocMeta) => void;
    onError: (msg: string) => void;
  }

  let { docId, docTitle, onClose, onResolved, onError }: Props = $props();

  let localContent = $state("");
  let remoteContent = $state("");
  let loading = $state(true);
  let resolving = $state(false);

  onMount(async () => {
    try {
      const [local, remote] = await getConflictDiff(docId);
      localContent = local;
      remoteContent = remote;
    } catch (e) {
      onError(`加载冲突内容失败: ${e}`);
      onClose();
    } finally {
      loading = false;
    }
  });

  async function handleResolve(resolution: "keep_local" | "keep_remote") {
    if (resolving) return;
    resolving = true;
    try {
      const doc = await resolveConflict(docId, resolution);
      onResolved(doc);
      onClose();
    } catch (e) {
      onError(`解决冲突失败: ${e}`);
    } finally {
      resolving = false;
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") onClose();
  }

  function handleOverlayClick(e: MouseEvent) {
    if ((e.target as HTMLElement).classList.contains("conflict-overlay")) {
      onClose();
    }
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="conflict-overlay" onclick={handleOverlayClick} onkeydown={handleKeydown}>
  <div class="conflict-panel">
    <header class="conflict-header">
      <div class="header-top">
        <h3 class="header-title">解决冲突</h3>
        <button class="close-btn" onclick={onClose} title="关闭 (Esc)">
          <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
            <path d="M3 3l8 8M11 3l-8 8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
          </svg>
        </button>
      </div>
      <p class="header-doc">{docTitle}</p>
    </header>

    {#if loading}
      <div class="loading-state">
        <div class="spinner"></div>
        <span>加载中</span>
      </div>
    {:else}
      <div class="diff-container">
        <div class="diff-pane">
          <div class="diff-label">
            <span class="diff-label-dot" style="background: var(--c-amber);"></span>
            本地版本
          </div>
          <pre class="diff-content">{localContent || "(空)"}</pre>
        </div>
        <div class="diff-pane">
          <div class="diff-label">
            <span class="diff-label-dot" style="background: var(--c-blue);"></span>
            远程版本
          </div>
          <pre class="diff-content">{remoteContent || "(空)"}</pre>
        </div>
      </div>

      <div class="conflict-actions">
        <button
          class="resolve-btn resolve-btn--local"
          onclick={() => handleResolve("keep_local")}
          disabled={resolving}
        >
          <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
            <path d="M7 2v10M2 7l5 5 5-5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" transform="rotate(180 7 7)"/>
          </svg>
          保留本地版本
        </button>
        <button
          class="resolve-btn resolve-btn--remote"
          onclick={() => handleResolve("keep_remote")}
          disabled={resolving}
        >
          <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
            <path d="M7 2v10M2 7l5 5 5-5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
          </svg>
          保留远程版本
        </button>
      </div>
    {/if}
  </div>
</div>

<style>
  .conflict-overlay {
    position: fixed;
    inset: 0;
    z-index: 900;
    background: rgba(0,0,0,0.5);
    display: flex;
    align-items: center;
    justify-content: center;
    animation: fadeIn 150ms ease both;
  }
  @keyframes fadeIn {
    from { opacity: 0; }
    to { opacity: 1; }
  }

  .conflict-panel {
    width: 720px;
    max-width: 90vw;
    max-height: 80vh;
    background: var(--c-bg);
    border: 1px solid var(--c-border);
    border-radius: var(--radius-lg);
    display: flex;
    flex-direction: column;
    overflow: hidden;
    box-shadow: 0 16px 48px rgba(0,0,0,0.4);
    animation: scaleIn 200ms ease both;
  }
  @keyframes scaleIn {
    from { opacity: 0; transform: scale(0.96); }
    to { opacity: 1; transform: scale(1); }
  }

  .conflict-header {
    padding: 16px 20px 12px;
    border-bottom: 1px solid var(--c-border);
    flex-shrink: 0;
  }
  .header-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .header-title {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
    color: var(--c-text);
  }
  .header-doc {
    margin: 4px 0 0;
    font-size: 11px;
    color: var(--c-text-tertiary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .close-btn {
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
  }
  .close-btn:hover {
    background: var(--c-bg-hover);
    color: var(--c-text);
  }

  .loading-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    padding: 40px;
    color: var(--c-text-tertiary);
    font-size: 12px;
  }
  .spinner {
    width: 18px;
    height: 18px;
    border: 2px solid var(--c-text-tertiary);
    border-top-color: transparent;
    border-radius: 50%;
    animation: spin 0.6s linear infinite;
  }
  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  .diff-container {
    display: flex;
    flex: 1;
    overflow: hidden;
    gap: 1px;
    background: var(--c-border);
  }
  .diff-pane {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: var(--c-bg);
  }
  .diff-label {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 12px;
    font-size: 11px;
    font-weight: 600;
    color: var(--c-text-secondary);
    border-bottom: 1px solid var(--c-border);
    flex-shrink: 0;
  }
  .diff-label-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
  }
  .diff-content {
    flex: 1;
    overflow-y: auto;
    padding: 12px;
    margin: 0;
    font-size: 12px;
    font-family: var(--font-mono);
    color: var(--c-text);
    white-space: pre-wrap;
    word-break: break-all;
    line-height: 1.6;
  }

  .conflict-actions {
    display: flex;
    gap: 8px;
    padding: 12px 20px;
    border-top: 1px solid var(--c-border);
    justify-content: center;
    flex-shrink: 0;
  }
  .resolve-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 8px 20px;
    border-radius: var(--radius);
    border: 1px solid var(--c-border);
    background: transparent;
    font-size: 13px;
    font-weight: 500;
    font-family: var(--font-sans);
    cursor: pointer;
    transition: all var(--transition);
  }
  .resolve-btn:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .resolve-btn--local {
    color: var(--c-amber);
    border-color: var(--c-amber);
  }
  .resolve-btn--local:hover:not(:disabled) {
    background: rgba(212,165,71,0.1);
  }
  .resolve-btn--remote {
    color: var(--c-blue);
    border-color: var(--c-blue);
  }
  .resolve-btn--remote:hover:not(:disabled) {
    background: rgba(91,159,212,0.1);
  }
</style>
