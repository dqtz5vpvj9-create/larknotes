<script lang="ts">
  import { onDestroy } from "svelte";
  import { searchDocsLocal, searchDocs, getDocList } from "../api";
  import type { DocMeta } from "../types";

  interface Props {
    onDocsUpdate: (docs: DocMeta[]) => void;
    onError: (msg: string) => void;
    onShowSettings: () => void;
    onShowCreate: () => void;
    onQuickNote: () => void;
    onToggleTheme: () => void;
    onSearchQueryChange?: (query: string) => void;
    settingsOpen: boolean;
    theme: "dark" | "light";
  }

  let { onDocsUpdate, onError, onShowSettings, onShowCreate, onQuickNote, onToggleTheme, onSearchQueryChange, settingsOpen, theme }: Props = $props();

  let searchQuery = $state("");
  let searchFocused = $state(false);
  let searchTimeout: ReturnType<typeof setTimeout> | null = null;
  let searchMode = $state<"local" | "remote">("local");

  onDestroy(() => {
    if (searchTimeout) clearTimeout(searchTimeout);
  });

  function handleSearch() {
    if (searchTimeout) clearTimeout(searchTimeout);
    onSearchQueryChange?.(searchQuery.trim());
    searchTimeout = setTimeout(async () => {
      try {
        const q = searchQuery.trim();
        if (!q) {
          const docs = await getDocList();
          onDocsUpdate(docs);
        } else if (searchMode === "local") {
          const results = await searchDocsLocal(q);
          onDocsUpdate(results);
        } else {
          const results = await searchDocs(q);
          onDocsUpdate(results);
        }
      } catch (e) {
        onError(`搜索失败: ${e}`);
      }
    }, 300);
  }

  function clearSearch() {
    searchQuery = "";
    onSearchQueryChange?.("");
    handleSearch();
  }

  function toggleSearchMode() {
    searchMode = searchMode === "local" ? "remote" : "local";
    if (searchQuery.trim()) {
      handleSearch();
    }
  }

  async function handleRefresh() {
    try {
      searchQuery = "";
      onSearchQueryChange?.("");
      const docs = await getDocList();
      onDocsUpdate(docs);
    } catch (e) {
      onError(`刷新失败: ${e}`);
    }
  }
</script>

<div class="toolbar" data-tauri-drag-region>
  <div class="toolbar-left">
    <button
      class="create-btn"
      onclick={onQuickNote}
      title="快速笔记 (Ctrl+N)"
    >
      <svg width="15" height="15" viewBox="0 0 16 16" fill="none">
        <path d="M4 2h8a1 1 0 011 1v10a1 1 0 01-1 1H4a1 1 0 01-1-1V3a1 1 0 011-1z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/>
        <path d="M5.5 5.5h5M5.5 8h3" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
      </svg>
      <span>快速笔记</span>
    </button>
    <button
      class="tool-btn"
      onclick={onShowCreate}
      title="新建文档 (Ctrl+Shift+N)"
    >
      <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
        <path d="M7 1.5v11M1.5 7h11" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>
      </svg>
    </button>
  </div>

  <div class="search-wrapper" class:focused={searchFocused}>
    <svg class="search-icon" width="14" height="14" viewBox="0 0 14 14" fill="none">
      <circle cx="6.2" cy="6.2" r="4.5" stroke="currentColor" stroke-width="1.4"/>
      <path d="M9.5 9.5l3 3" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>
    </svg>
    <input
      type="text"
      placeholder={searchFocused ? `搜索${searchMode === "local" ? "本地" : "飞书"}文档...` : "搜索  Ctrl+K"}
      bind:value={searchQuery}
      oninput={handleSearch}
      onfocus={() => searchFocused = true}
      onblur={() => searchFocused = false}
    />
    <button
      class="search-mode-btn"
      class:search-mode-btn--remote={searchMode === "remote"}
      onclick={toggleSearchMode}
      title={searchMode === "local" ? "切换到飞书搜索" : "切换到本地搜索"}
    >
      {searchMode === "local" ? "本地" : "飞书"}
    </button>
    {#if searchQuery}
      <button class="search-clear" onclick={clearSearch} title="清除">
        <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
          <path d="M3 3l6 6M9 3l-6 6" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>
        </svg>
      </button>
    {/if}
  </div>

  <div class="toolbar-right">
    <button class="tool-btn" onclick={handleRefresh} title="刷新列表">
      <svg width="15" height="15" viewBox="0 0 15 15" fill="none">
        <path d="M2.5 7.5a5 5 0 019-3M12.5 7.5a5 5 0 01-9 3" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>
        <path d="M11.5 1.5v3h-3M3.5 13.5v-3h3" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/>
      </svg>
    </button>
    <button class="tool-btn" onclick={onToggleTheme} title={theme === "dark" ? "切换亮色" : "切换暗色"}>
      {#if theme === "dark"}
        <svg width="15" height="15" viewBox="0 0 15 15" fill="none">
          <circle cx="7.5" cy="7.5" r="3.5" stroke="currentColor" stroke-width="1.3"/>
          <path d="M7.5 1v1.5M7.5 12.5V14M1 7.5h1.5M12.5 7.5H14M3.05 3.05l1.06 1.06M10.89 10.89l1.06 1.06M3.05 11.95l1.06-1.06M10.89 4.11l1.06-1.06" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
        </svg>
      {:else}
        <svg width="15" height="15" viewBox="0 0 15 15" fill="none">
          <path d="M13 8.5a5.5 5.5 0 01-7-7 6 6 0 107 7z" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/>
        </svg>
      {/if}
    </button>
    <button
      class="tool-btn"
      class:tool-btn--active={settingsOpen}
      onclick={onShowSettings}
      title="设置"
    >
      <svg width="15" height="15" viewBox="0 0 16 16" fill="none">
        <path d="M6.5 1.5h3l.4 1.8.8.3 1.6-.9 2.1 2.1-.9 1.6.3.8 1.8.4v3l-1.8.4-.3.8.9 1.6-2.1 2.1-1.6-.9-.8.3-.4 1.8h-3l-.4-1.8-.8-.3-1.6.9-2.1-2.1.9-1.6-.3-.8-1.8-.4v-3l1.8-.4.3-.8-.9-1.6 2.1-2.1 1.6.9.8-.3z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/>
        <circle cx="8" cy="8" r="2" stroke="currentColor" stroke-width="1.2"/>
      </svg>
    </button>
  </div>
</div>

<style>
  .toolbar {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 12px;
    border-bottom: 1px solid var(--c-border);
    background: var(--c-bg);
    flex-shrink: 0;
  }
  .toolbar-left { flex-shrink: 0; }
  .toolbar-right { display: flex; gap: 2px; flex-shrink: 0; }

  .create-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 14px;
    border-radius: var(--radius);
    font-size: 13px;
    font-weight: 500;
    letter-spacing: -0.01em;
    cursor: pointer;
    border: none;
    outline: none;
    transition: all var(--transition);
    background: var(--c-accent);
    color: #1a1a1e;
    font-family: var(--font-sans);
  }
  .create-btn:hover { background: var(--c-accent-hover); }
  .create-btn:active { transform: scale(0.97); }

  .search-wrapper {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 12px;
    height: 34px;
    border-radius: var(--radius);
    background: var(--c-bg-elevated);
    border: 1px solid transparent;
    transition: all var(--transition);
  }
  .search-wrapper.focused {
    border-color: var(--c-accent-border, rgba(212,165,71,0.3));
    background: var(--c-bg-hover);
  }
  .search-icon {
    color: var(--c-text-tertiary);
    flex-shrink: 0;
  }
  .search-wrapper input {
    flex: 1;
    border: none;
    outline: none;
    background: transparent;
    font-size: 13px;
    color: var(--c-text);
    font-family: var(--font-sans);
  }
  .search-mode-btn {
    flex-shrink: 0;
    padding: 2px 8px;
    border-radius: var(--radius-sm);
    border: 1px solid var(--c-border);
    background: transparent;
    color: var(--c-text-tertiary);
    font-size: 10px;
    font-family: var(--font-sans);
    cursor: pointer;
    transition: all var(--transition);
  }
  .search-mode-btn:hover {
    color: var(--c-text-secondary);
    border-color: var(--c-text-tertiary);
  }
  .search-mode-btn--remote {
    color: var(--c-blue);
    border-color: var(--c-blue);
    background: rgba(91,159,212,0.1);
  }
  .search-clear {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    border: none;
    outline: none;
    background: transparent;
    color: var(--c-text-tertiary);
    cursor: pointer;
    border-radius: var(--radius-sm);
    flex-shrink: 0;
    transition: all var(--transition);
  }
  .search-clear:hover {
    background: var(--c-bg-active);
    color: var(--c-text);
  }

  .tool-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 32px;
    height: 32px;
    border-radius: var(--radius);
    color: var(--c-text-secondary);
    cursor: pointer;
    transition: all var(--transition);
    border: none;
    outline: none;
    background: transparent;
  }
  .tool-btn:hover {
    color: var(--c-text);
    background: var(--c-bg-hover);
  }
  .tool-btn:active {
    background: var(--c-bg-active);
  }
  .tool-btn--active {
    color: var(--c-accent) !important;
    background: var(--c-accent-bg, rgba(212,165,71,0.1)) !important;
  }
</style>
