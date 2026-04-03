<script lang="ts">
  import type { AuthStatus } from "../types";

  interface Props {
    auth: AuthStatus | null;
    docCount: number;
    editorName: string;
    syncingCount?: number;
  }

  let { auth, docCount, editorName, syncingCount = 0 }: Props = $props();
</script>

<div class="status-bar">
  <div class="status-section">
    {#if auth?.logged_in}
      <span class="status-dot status-dot--ok"></span>
      <span class="status-label">{auth.user_name ?? "已登录"}</span>
    {:else}
      <span class="status-dot status-dot--err"></span>
      <span class="status-label" style="color: var(--c-text-tertiary);">未登录</span>
    {/if}
  </div>

  <div class="status-section status-section--center">
    <span>{docCount} 篇文档</span>
    {#if syncingCount > 0}
      <span class="status-syncing">· 正在同步 {syncingCount} 篇</span>
    {/if}
  </div>

  <div class="status-section">
    <span class="editor-label">{editorName}</span>
  </div>
</div>

<style>
  .status-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 4px 16px;
    border-top: 1px solid var(--c-border);
    background: var(--c-bg);
    font-size: 11px;
    color: var(--c-text-tertiary);
    flex-shrink: 0;
    letter-spacing: 0.02em;
    min-height: 28px;
  }
  .status-section {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: 1;
  }
  .status-section--center {
    justify-content: center;
    gap: 4px;
  }
  .status-section:last-child {
    justify-content: flex-end;
  }
  .status-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .status-dot--ok { background: var(--c-green); }
  .status-dot--err { background: var(--c-red); }
  .status-label {
    color: var(--c-text-secondary);
  }
  .status-syncing {
    color: var(--c-blue);
  }
  .editor-label {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--c-text-tertiary);
    padding: 1px 6px;
    border-radius: var(--radius-sm);
    background: var(--c-bg-elevated);
  }
</style>
