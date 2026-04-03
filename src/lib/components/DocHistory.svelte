<script lang="ts">
  import { onMount } from "svelte";
  import { getSyncHistory } from "../api";
  import { formatRelativeTime } from "../types";
  import type { SyncHistoryEntry } from "../types";

  interface Props {
    docId: string;
    docTitle: string;
    onClose: () => void;
  }

  let { docId, docTitle, onClose }: Props = $props();

  let entries = $state<SyncHistoryEntry[]>([]);
  let loading = $state(true);
  let error = $state("");

  $effect(() => {
    loading = true;
    error = "";
    getSyncHistory(docId)
      .then((data) => { entries = data; })
      .catch((e) => { error = `${e}`; })
      .finally(() => { loading = false; });
  });

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") onClose();
  }

  function handleOverlayClick(e: MouseEvent) {
    if ((e.target as HTMLElement).classList.contains("history-overlay")) {
      onClose();
    }
  }

  function actionLabel(action: string): string {
    switch (action) {
      case "push": return "推送";
      case "pull": return "拉取";
      case "conflict": return "冲突";
      default: return action;
    }
  }

  function actionColor(action: string): string {
    switch (action) {
      case "push": return "var(--c-green)";
      case "pull": return "var(--c-blue)";
      case "conflict": return "var(--c-red)";
      default: return "var(--c-text-tertiary)";
    }
  }

  function actionIcon(action: string): string {
    switch (action) {
      case "push": return "↑";
      case "pull": return "↓";
      case "conflict": return "!";
      default: return "·";
    }
  }

  function shortHash(hash: string | null): string {
    if (!hash) return "";
    return hash.slice(0, 8);
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="history-overlay" onclick={handleOverlayClick} onkeydown={handleKeydown}>
  <div class="history-panel">
    <header class="history-header">
      <div class="header-top">
        <h3 class="header-title">同步历史</h3>
        <button class="close-btn" onclick={onClose} title="关闭 (Esc)">
          <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
            <path d="M3 3l8 8M11 3l-8 8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
          </svg>
        </button>
      </div>
      <p class="header-doc">{docTitle}</p>
    </header>

    <div class="history-body">
      {#if loading}
        <div class="state-box">
          <div class="spinner"></div>
          <span>加载中</span>
        </div>
      {:else if error}
        <div class="state-box state-box--error">
          <span>{error}</span>
        </div>
      {:else if entries.length === 0}
        <div class="state-box">
          <span class="empty-icon">∅</span>
          <span>暂无同步记录</span>
        </div>
      {:else}
        <div class="timeline">
          {#each entries as entry, i (entry.id)}
            <div
              class="timeline-item"
              style="animation-delay: {Math.min(i * 30, 300)}ms"
            >
              <div class="tl-rail">
                <div class="tl-node" style="background: {actionColor(entry.action)}; box-shadow: 0 0 0 3px color-mix(in srgb, {actionColor(entry.action)} 15%, transparent);">
                  <span class="tl-node-icon">{actionIcon(entry.action)}</span>
                </div>
                {#if i < entries.length - 1}
                  <div class="tl-line"></div>
                {/if}
              </div>

              <div class="tl-content">
                <div class="tl-row-main">
                  <span class="tl-action" style="color: {actionColor(entry.action)};">
                    {actionLabel(entry.action)}
                  </span>
                  <span class="tl-time">{formatRelativeTime(entry.created_at)}</span>
                </div>
                {#if entry.content_hash}
                  <code class="tl-hash">{shortHash(entry.content_hash)}</code>
                {/if}
              </div>
            </div>
          {/each}
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  .history-overlay {
    position: fixed;
    inset: 0;
    z-index: 900;
    background: rgba(0,0,0,0.35);
    display: flex;
    justify-content: flex-end;
    animation: overlayIn 150ms ease both;
  }
  @keyframes overlayIn {
    from { opacity: 0; }
    to { opacity: 1; }
  }

  .history-panel {
    width: 320px;
    max-width: 80vw;
    height: 100%;
    background: var(--c-bg);
    border-left: 1px solid var(--c-border);
    display: flex;
    flex-direction: column;
    animation: panelSlideIn 200ms cubic-bezier(0.16, 1, 0.3, 1) both;
  }
  @keyframes panelSlideIn {
    from { transform: translateX(100%); }
    to { transform: translateX(0); }
  }

  .history-header {
    padding: 16px 16px 12px;
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
    font-size: 13px;
    font-weight: 600;
    color: var(--c-text);
    letter-spacing: -0.01em;
    font-family: var(--font-sans);
  }
  .header-doc {
    margin: 4px 0 0;
    font-size: 11px;
    color: var(--c-text-tertiary);
    font-family: var(--font-sans);
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

  .history-body {
    flex: 1;
    overflow-y: auto;
    padding: 12px 16px;
  }

  /* States */
  .state-box {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    padding: 40px 16px;
    color: var(--c-text-tertiary);
    font-size: 12px;
    font-family: var(--font-sans);
  }
  .state-box--error { color: var(--c-red); }
  .empty-icon {
    font-size: 24px;
    opacity: 0.4;
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

  /* Timeline */
  .timeline {
    display: flex;
    flex-direction: column;
  }

  .timeline-item {
    display: flex;
    gap: 12px;
    animation: itemIn 200ms ease both;
  }
  @keyframes itemIn {
    from { opacity: 0; transform: translateX(6px); }
    to { opacity: 1; transform: translateX(0); }
  }

  .tl-rail {
    display: flex;
    flex-direction: column;
    align-items: center;
    flex-shrink: 0;
    width: 20px;
  }
  .tl-node {
    width: 20px;
    height: 20px;
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
  }
  .tl-node-icon {
    font-size: 10px;
    font-weight: 700;
    color: var(--c-bg);
    font-family: var(--font-mono);
    line-height: 1;
  }
  .tl-line {
    width: 1px;
    flex: 1;
    min-height: 12px;
    background: var(--c-border);
  }

  .tl-content {
    flex: 1;
    min-width: 0;
    padding-bottom: 16px;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .tl-row-main {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .tl-action {
    font-size: 12px;
    font-weight: 600;
    font-family: var(--font-sans);
    letter-spacing: 0.01em;
  }
  .tl-time {
    font-size: 11px;
    color: var(--c-text-tertiary);
    font-family: var(--font-sans);
  }
  .tl-hash {
    font-size: 10px;
    color: var(--c-text-tertiary);
    font-family: var(--font-mono);
    opacity: 0.7;
    letter-spacing: 0.03em;
  }
</style>
