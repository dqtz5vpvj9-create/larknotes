<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import {
    getAuthStatus, getDocList, getAppConfig, createDoc, manualSync,
    importDoc, deleteDoc, revealInExplorer, openDocInEditor, quickNote,
    pullDoc, getFolderTree, createFolder as apiCreateFolder, renameDoc,
  } from "./lib/api";
  import type { AuthStatus, DocMeta, AppConfig, SyncStatusUpdate, FolderTreeNode } from "./lib/types";
  import Toolbar from "./lib/components/Toolbar.svelte";
  import DocList from "./lib/components/DocList.svelte";
  import StatusBar from "./lib/components/StatusBar.svelte";
  import Settings from "./lib/components/Settings.svelte";
  import Toast from "./lib/components/Toast.svelte";
  import CreateDocModal from "./lib/components/CreateDocModal.svelte";
  import ConfirmDialog from "./lib/components/ConfirmDialog.svelte";
  import DocHistory from "./lib/components/DocHistory.svelte";
  import CommandPalette from "./lib/components/CommandPalette.svelte";
  import ConflictResolver from "./lib/components/ConflictResolver.svelte";
  import FolderTree from "./lib/components/FolderTree.svelte";

  // ─── Toast system ──────────────────────────────────
  interface ToastMessage {
    id: number;
    text: string;
    type: "error" | "success" | "info";
  }

  let toasts = $state<ToastMessage[]>([]);
  let toastId = 0;

  function addToast(text: string, type: "error" | "success" | "info" = "error") {
    const id = ++toastId;
    toasts = [...toasts, { id, text, type }];
    // Scale error duration with message length so long messages aren't cut off
    const duration = type === "error" ? Math.max(6000, Math.min(text.length * 80, 15000)) : 3000;
    setTimeout(() => {
      toasts = toasts.filter((t) => t.id !== id);
    }, duration);
  }

  function dismissToast(id: number) {
    toasts = toasts.filter((t) => t.id !== id);
  }

  // ─── App state ─────────────────────────────────────
  let loading = $state(true);
  let auth = $state<AuthStatus | null>(null);
  let docs = $state<DocMeta[]>([]);
  let config = $state<AppConfig | null>(null);
  let theme = $state<"dark" | "light">("dark");
  let searchQuery = $state("");
  let filterConflicts = $state(false);
  let currentFolder = $state("");
  let folderTree = $state<FolderTreeNode[]>([]);
  let sidebarCollapsed = $state(false);

  // ─── Panel/modal visibility ────────────────────────
  let showSettings = $state(false);
  let showCreate = $state(false);
  let creating = $state(false);
  let showCommandPalette = $state(false);
  let deleteConfirm = $state<{ docId: string; title: string } | null>(null);
  let historyDocId = $state<string | null>(null);
  let conflictDocId = $state<string | null>(null);
  // restoreConfirm is intentionally kept for future DocHistory integration
  // when snapshot restore is exposed via the history panel
  let copiedLoginCmd = $state(false);
  let copiedRefreshCmd = $state(false);

  // ─── Derived state ─────────────────────────────────
  let conflicts = $derived(
    docs.filter((d) => d.sync_status.type === "Conflict")
  );

  let syncingCount = $derived(
    docs.filter((d) => d.sync_status.type === "Syncing").length
  );

  let displayDocs = $derived.by(() => {
    let filtered = docs;
    if (filterConflicts) {
      filtered = filtered.filter((d) => d.sync_status.type === "Conflict" || d.sync_status.type === "Error");
    }
    // Only apply folder filter when user has explicitly selected a folder.
    // When currentFolder is "" (no folder selected), show all docs.
    if (currentFolder !== "") {
      filtered = filtered.filter((d) => (d.folder_path ?? "") === currentFolder);
    }
    return filtered;
  });

  let rootDocCount = $derived(docs.filter((d) => (d.folder_path ?? "") === "").length);

  // ─── Document actions ──────────────────────────────
  async function handleCreate(title: string) {
    if (creating) return;
    creating = true;
    showCreate = false;
    try {
      await createDoc(title, currentFolder || undefined);
      docs = await getDocList();
      refreshFolderTree();
      addToast(`「${title}」已创建`, "success");
    } catch (e) {
      addToast(`创建失败: ${e}`);
    } finally {
      creating = false;
    }
  }

  async function refreshFolderTree() {
    try { folderTree = await getFolderTree(); } catch {}
  }

  async function handleCreateFolder(parentPath: string) {
    const name = prompt("文件夹名称:");
    if (!name) return;
    const path = parentPath ? `${parentPath}/${name}` : name;
    try {
      await apiCreateFolder(path);
      await refreshFolderTree();
    } catch (e) {
      addToast(`创建文件夹失败: ${e}`);
    }
  }

  async function handleDeleteFolder(path: string) {
    try {
      const { deleteFolder } = await import("./lib/api");
      await deleteFolder(path);
      if (currentFolder === path || currentFolder.startsWith(path + "/")) {
        currentFolder = "";
      }
      await refreshFolderTree();
    } catch (e) {
      addToast(`删除文件夹失败: ${e}`);
    }
  }

  function handleSyncDoc(docId: string) {
    docs = docs.map((d) => {
      if (d.doc_id === docId) {
        return { ...d, sync_status: { type: "Syncing" as const } };
      }
      return d;
    });
    addToast("正在推送…", "info");
    manualSync(docId).catch((e) => {
      addToast(`推送失败: ${e}`);
    });
  }

  async function handlePullDoc(docId: string) {
    try {
      addToast("正在拉取远程内容…", "info");
      const meta = await pullDoc(docId);
      docs = docs.map((d) => d.doc_id === docId ? meta : d);
      addToast(`「${meta.title}」已更新`, "success");
    } catch (e) {
      addToast(`拉取失败: ${e}`);
    }
  }

  async function handleImportDoc(docId: string) {
    try {
      addToast("正在导入…", "info");
      const meta = await importDoc(docId);
      docs = await getDocList();
      addToast(`「${meta.title}」已导入`, "success");
    } catch (e) {
      addToast(`导入失败: ${e}`);
    }
  }

  function handleDeleteDoc(docId: string) {
    const doc = docs.find((d) => d.doc_id === docId);
    deleteConfirm = { docId, title: doc?.title ?? "未知文档" };
  }

  async function handleRenameDoc(docId: string) {
    const doc = docs.find((d) => d.doc_id === docId);
    const newTitle = prompt("请输入新标题", doc?.title ?? "");
    if (!newTitle || newTitle === doc?.title) return;
    try {
      const updated = await renameDoc(docId, newTitle);
      docs = docs.map((d) => (d.doc_id === docId ? updated : d));
      addToast(`已重命名为「${newTitle}」`, "success");
    } catch (e: any) {
      addToast(`重命名失败: ${e}`, "error");
    }
  }

  function handleBatchDelete(docIds: string[]) {
    batchDeleteIds = docIds;
  }

  function handleBatchSync(docIds: string[]) {
    for (const id of docIds) {
      handleSyncDoc(id);
    }
  }

  let batchDeleteIds = $state<string[] | null>(null);

  async function confirmBatchDelete() {
    if (!batchDeleteIds) return;
    const ids = batchDeleteIds;
    batchDeleteIds = null;
    let successCount = 0;
    let failCount = 0;
    for (const docId of ids) {
      try {
        await deleteDoc(docId);
        docs = docs.filter((d) => d.doc_id !== docId);
        successCount++;
      } catch {
        failCount++;
      }
    }
    if (failCount > 0) {
      addToast(`已删除 ${successCount} 篇，${failCount} 篇失败`);
    } else {
      addToast(`已删除 ${successCount} 篇文档`, "success");
    }
  }

  async function confirmDelete() {
    if (!deleteConfirm) return;
    const { docId } = deleteConfirm;
    deleteConfirm = null;
    try {
      await deleteDoc(docId);
      docs = docs.filter((d) => d.doc_id !== docId);
      addToast("已删除", "success");
    } catch (e: any) {
      const msg = String(e);
      if (msg.startsWith("REMOTE_DELETE_FAILED:")) {
        // Remote delete failed — ask user if they want to delete locally only
        const reason = msg.slice("REMOTE_DELETE_FAILED:".length);
        remoteDeleteFail = { docId, reason };
      } else {
        addToast(`删除失败: ${e}`);
      }
    }
  }

  let remoteDeleteFail = $state<{ docId: string; reason: string } | null>(null);

  async function confirmLocalOnlyDelete() {
    if (!remoteDeleteFail) return;
    const { docId } = remoteDeleteFail;
    remoteDeleteFail = null;
    try {
      await deleteDoc(docId, true);
      docs = docs.filter((d) => d.doc_id !== docId);
      addToast("已删除本地文档", "success");
    } catch (e) {
      addToast(`删除失败: ${e}`);
    }
  }

  async function handleRefresh() {
    try {
      docs = await getDocList();
    } catch (e) {
      addToast(`刷新失败: ${e}`);
    }
  }

  async function handleOpenDoc(docId: string) {
    try {
      await openDocInEditor(docId);
    } catch (e) {
      addToast(`打开失败: ${e}`);
    }
  }

  async function handleQuickNote() {
    try {
      const meta = await quickNote();
      docs = await getDocList();
      addToast(`「${meta.title}」已创建`, "success");
    } catch (e) {
      addToast(`创建快速笔记失败: ${e}`);
    }
  }

  async function handleRevealInExplorer(docId: string) {
    try {
      await revealInExplorer(docId);
    } catch (e) {
      addToast(`${e}`);
    }
  }

  function handleConflictResolved(doc: DocMeta) {
    docs = docs.map((d) => d.doc_id === doc.doc_id ? doc : d);
    addToast(`「${doc.title}」冲突已解决`, "success");
  }

  // ─── UI helpers ────────────────────────────────────
  function copyToClipboard(text: string, setter: (v: boolean) => void) {
    navigator.clipboard.writeText(text);
    setter(true);
    setTimeout(() => setter(false), 2000);
  }

  function toggleTheme() {
    theme = theme === "dark" ? "light" : "dark";
    document.documentElement.setAttribute("data-theme", theme);
    localStorage.setItem("larknotes-theme", theme);
  }

  function handleGlobalKeydown(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && e.key === "k") {
      e.preventDefault();
      // Focus the search input in the toolbar
      const searchInput = document.querySelector<HTMLInputElement>('.search-wrapper input');
      searchInput?.focus();
      return;
    }
    if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key === "P") {
      e.preventDefault();
      showCommandPalette = !showCommandPalette;
      return;
    }
    if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key === "N") {
      e.preventDefault();
      showCreate = true;
      return;
    }
    if ((e.ctrlKey || e.metaKey) && e.key === "n") {
      e.preventDefault();
      handleQuickNote();
    }
    if (e.key === "Escape") {
      if (showCommandPalette) { showCommandPalette = false; return; }
      if (conflictDocId) { conflictDocId = null; return; }
      if (historyDocId) { historyDocId = null; return; }
      if (showCreate) { showCreate = false; return; }
      if (showSettings) { showSettings = false; return; }
    }
  }

  // ─── Initialization ────────────────────────────────
  onMount(() => {
    // Restore theme
    const saved = localStorage.getItem("larknotes-theme") as "dark" | "light" | null;
    if (saved) {
      theme = saved;
      document.documentElement.setAttribute("data-theme", saved);
    }

    // Initialize data
    (async () => {
      try {
        const [authResult, docsResult, configResult, folderResult] = await Promise.allSettled([
          getAuthStatus(),
          getDocList(),
          getAppConfig(),
          getFolderTree(),
        ]);

        if (authResult.status === "fulfilled") auth = authResult.value;
        if (docsResult.status === "fulfilled") docs = docsResult.value;
        if (configResult.status === "fulfilled") config = configResult.value;
        if (folderResult.status === "fulfilled") folderTree = folderResult.value;
      } catch (e) {
        addToast(`初始化失败: ${e}`);
      } finally {
        loading = false;
      }
    })();

    // Listen for sync status updates from backend
    listen<SyncStatusUpdate>("sync-status", (event) => {
      const { doc_id, status, title, new_doc_id } = event.payload;
      docs = docs.map((d) => {
        if (d.doc_id === doc_id) {
          return {
            ...d,
            doc_id: new_doc_id ?? d.doc_id,
            sync_status: status,
            title: title ?? d.title,
          };
        }
        return d;
      });
      if (status.type === "Synced") {
        addToast(`「${title ?? doc_id}」同步完成`, "success");
      } else if (status.type === "Conflict") {
        addToast(`「${title ?? doc_id}」同步冲突`, "error");
      }
    });

    // Listen for doc list changes (e.g. after editor-close rename)
    listen("docs-changed", async () => {
      try {
        docs = await getDocList();
        refreshFolderTree();
      } catch (e) {
        console.error("Failed to refresh docs:", e);
      }
    });

    // Periodic auth status check (every 60s)
    const authInterval = setInterval(async () => {
      try {
        auth = await getAuthStatus();
      } catch { /* ignore */ }
    }, 60_000);

    return () => { clearInterval(authInterval); };
  });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<main class="app-shell" style="background: var(--c-bg);" onkeydown={handleGlobalKeydown}>
  {#if loading}
    <div class="loading-screen">
      <div class="loading-content">
        <div class="loading-spinner"></div>
        <span class="loading-text">加载中</span>
      </div>
    </div>
  {:else}
    <Toolbar
      onDocsUpdate={(d) => (docs = d)}
      onError={(msg) => addToast(msg)}
      onShowSettings={() => (showSettings = !showSettings)}
      onShowCreate={() => (showCreate = true)}
      onQuickNote={handleQuickNote}
      settingsOpen={showSettings}
      onToggleTheme={toggleTheme}
      onSearchQueryChange={(q) => (searchQuery = q)}
      {theme}
    />

    {#if !auth?.logged_in}
      <div class="banner banner--error">
        <span class="banner-dot banner-dot--red"></span>
        <span class="banner-text banner-text--red">
          未登录 — 在终端运行
          <button
            class="cmd-tag cmd-tag--red"
            onclick={() => copyToClipboard("lark-cli auth login", (v) => copiedLoginCmd = v)}
            title="点击复制"
          >
            {copiedLoginCmd ? "已复制 ✓" : "lark-cli auth login"}
          </button>
          完成登录后刷新
        </span>
        <button class="banner-action banner-action--red" onclick={handleRefresh}>
          刷新
        </button>
      </div>
    {:else if auth.needs_refresh}
      <div class="banner banner--amber">
        <span class="banner-dot banner-dot--amber"></span>
        <span class="banner-text banner-text--amber">
          登录令牌即将过期 — 在终端运行
          <button
            class="cmd-tag cmd-tag--amber"
            onclick={() => copyToClipboard("lark-cli auth login", (v) => copiedRefreshCmd = v)}
            title="点击复制"
          >
            {copiedRefreshCmd ? "已复制 ✓" : "lark-cli auth login"}
          </button>
          刷新令牌
        </span>
      </div>
    {/if}

    {#if conflicts.length > 0}
      <div class="banner banner--warn">
        <span class="banner-dot banner-dot--red"></span>
        <span class="banner-text banner-text--red">
          {conflicts.length} 篇文档存在冲突
        </span>
        <button class="banner-action banner-action--red" onclick={() => { filterConflicts = !filterConflicts; }}>
          {filterConflicts ? "显示全部" : "查看"}
        </button>
      </div>
    {/if}

    <div class="content-area">
      {#if showSettings}
        <div class="settings-panel">
          <Settings
            editorCommand={config?.editor_command ?? "notepad"}
            workspacePath={config?.workspace_dir ?? ""}
            syncDebounceMs={config?.sync_debounce_ms ?? 2000}
            autoSync={config?.auto_sync ?? true}
            providerCliPath={config?.provider_cli_path ?? "lark-cli"}
            onClose={() => (showSettings = false)}
            onEditorChange={(e) => {
              if (config) config = { ...config, editor_command: e };
            }}
            onWorkspaceChange={(p) => {
              if (config) config = { ...config, workspace_dir: p };
            }}
            onConfigChange={(key, value) => {
              if (config) config = { ...config, [key]: value };
            }}
            onError={(msg) => addToast(msg)}
          />
        </div>
      {:else}
        <div class="main-layout">
          <aside class="sidebar" class:sidebar--collapsed={sidebarCollapsed}>
            <div class="sidebar-header">
              <span class="sidebar-title">文件夹</span>
              <button class="sidebar-toggle" onclick={() => (sidebarCollapsed = !sidebarCollapsed)} title={sidebarCollapsed ? "展开侧边栏" : "收起侧边栏"}>
                <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
                  {#if sidebarCollapsed}
                    <path d="M5 3l4 4-4 4" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>
                  {:else}
                    <path d="M9 3L5 7l4 4" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>
                  {/if}
                </svg>
              </button>
            </div>
            {#if !sidebarCollapsed}
              <FolderTree
                tree={folderTree}
                {currentFolder}
                {rootDocCount}
                onSelectFolder={(path) => (currentFolder = path)}
                onCreateFolder={handleCreateFolder}
                onDeleteFolder={handleDeleteFolder}
              />
            {/if}
          </aside>
          <div class="doc-list-area">
            <DocList
              docs={displayDocs}
              {searchQuery}
              onError={(msg) => addToast(msg)}
              onSync={handleSyncDoc}
              onPull={handlePullDoc}
              onImport={handleImportDoc}
              onDelete={handleDeleteDoc}
              onRename={handleRenameDoc}
              onReveal={handleRevealInExplorer}
              onShowHistory={(id) => (historyDocId = id)}
              onResolveConflict={(id) => (conflictDocId = id)}
              onBatchDelete={handleBatchDelete}
              onBatchSync={handleBatchSync}
            />
          </div>
        </div>
      {/if}
    </div>

    <StatusBar
      {auth}
      docCount={docs.length}
      editorName={config?.editor_command ?? "未知"}
      {syncingCount}
    />
  {/if}

  <!-- Modals & overlays -->
  {#if showCreate}
    <CreateDocModal
      onConfirm={handleCreate}
      onCancel={() => (showCreate = false)}
    />
  {/if}

  {#if deleteConfirm}
    <ConfirmDialog
      title="删除文档"
      message={`确定删除「${deleteConfirm.title}」？将同时删除本地文件和飞书云端文档。`}
      confirmLabel="删除"
      danger={true}
      onConfirm={confirmDelete}
      onCancel={() => (deleteConfirm = null)}
    />
  {/if}

  {#if batchDeleteIds}
    <ConfirmDialog
      title="批量删除"
      message={`确定删除选中的 ${batchDeleteIds.length} 篇文档？将同时删除本地文件和飞书云端文档。`}
      confirmLabel={`删除 ${batchDeleteIds.length} 篇`}
      danger={true}
      onConfirm={confirmBatchDelete}
      onCancel={() => (batchDeleteIds = null)}
    />
  {/if}

  {#if remoteDeleteFail}
    <ConfirmDialog
      title="远程删除失败"
      message={`无法删除飞书云端文档：${remoteDeleteFail.reason}\n\n是否仅删除本地副本？`}
      confirmLabel="仅删除本地"
      danger={true}
      onConfirm={confirmLocalOnlyDelete}
      onCancel={() => (remoteDeleteFail = null)}
    />
  {/if}


  {#if showCommandPalette}
    <CommandPalette
      {docs}
      onClose={() => (showCommandPalette = false)}
      onNewDoc={() => (showCreate = true)}
      onQuickNote={handleQuickNote}
      onSettings={() => (showSettings = true)}
      onToggleTheme={toggleTheme}
      onRefresh={handleRefresh}
      onFilterConflicts={() => { filterConflicts = !filterConflicts; }}
      onOpenDoc={handleOpenDoc}
    />
  {/if}

  {#if historyDocId}
    {@const historyDoc = docs.find((d) => d.doc_id === historyDocId)}
    <DocHistory
      docId={historyDocId}
      docTitle={historyDoc?.title ?? "未知文档"}
      onClose={() => (historyDocId = null)}
    />
  {/if}

  {#if conflictDocId}
    {@const conflictDoc = docs.find((d) => d.doc_id === conflictDocId)}
    <ConflictResolver
      docId={conflictDocId}
      docTitle={conflictDoc?.title ?? "未知文档"}
      onClose={() => (conflictDocId = null)}
      onResolved={handleConflictResolved}
      onError={(msg) => addToast(msg)}
    />
  {/if}

  <Toast messages={toasts} onDismiss={dismissToast} />
</main>

<style>
  .loading-screen {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--c-text-tertiary);
  }
  .loading-content {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
  }
  .loading-spinner {
    width: 24px;
    height: 24px;
    border: 2px solid currentColor;
    border-top-color: transparent;
    border-radius: 50%;
    animation: spin 0.6s linear infinite;
  }
  @keyframes spin {
    to { transform: rotate(360deg); }
  }
  .loading-text {
    font-size: 13px;
    letter-spacing: 0.04em;
  }

  .content-area {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    position: relative;
  }
  .main-layout {
    display: flex;
    flex: 1;
    overflow: hidden;
  }
  .sidebar {
    display: flex;
    flex-direction: column;
    width: 200px;
    min-width: 200px;
    border-right: 1px solid var(--c-border);
    overflow: hidden;
    transition: width 150ms ease, min-width 150ms ease;
  }
  .sidebar--collapsed {
    width: 36px;
    min-width: 36px;
  }
  .sidebar-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 6px 8px;
    font-size: 11px;
    font-weight: 500;
    color: var(--c-text-tertiary);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    flex-shrink: 0;
  }
  .sidebar--collapsed .sidebar-title {
    display: none;
  }
  .sidebar-toggle {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    border: none;
    background: transparent;
    color: var(--c-text-tertiary);
    cursor: pointer;
    border-radius: var(--radius-sm);
    transition: color 60ms ease;
  }
  .sidebar-toggle:hover {
    color: var(--c-text);
  }
  .doc-list-area {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    min-width: 0;
  }
  .settings-panel {
    flex: 1;
    display: flex;
    flex-direction: column;
    animation: slideInRight 200ms ease both;
  }
  @keyframes slideInRight {
    from { opacity: 0; transform: translateX(12px); }
    to { opacity: 1; transform: translateX(0); }
  }

  /* Banners */
  .banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 12px;
    flex-shrink: 0;
    font-size: 12px;
  }
  .banner--warn {
    background: rgba(212,91,91,0.06);
    border-bottom: 1px solid rgba(212,91,91,0.1);
  }
  .banner--error {
    background: rgba(212,91,91,0.06);
    border-bottom: 1px solid rgba(212,91,91,0.1);
  }
  .banner--amber {
    background: rgba(212,194,71,0.06);
    border-bottom: 1px solid rgba(212,194,71,0.1);
  }
  .banner-dot {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .banner-dot--red { background: var(--c-red); }
  .banner-dot--amber { background: var(--c-amber); }
  .banner-text {
    font-size: 12px;
    flex: 1;
  }
  .banner-text--red { color: var(--c-red); }
  .banner-text--amber { color: var(--c-amber); }
  .banner-action {
    padding: 2px 10px;
    border-radius: var(--radius-sm);
    border: 1px solid transparent;
    background: transparent;
    font-size: 11px;
    font-family: var(--font-sans);
    cursor: pointer;
    transition: all var(--transition);
    flex-shrink: 0;
  }
  .banner-action--red {
    border-color: rgba(212,91,91,0.2);
    color: var(--c-red);
  }
  .banner-action--red:hover { background: rgba(212,91,91,0.1); }
  .cmd-tag {
    display: inline;
    font-family: var(--font-mono);
    padding: 1px 8px;
    border-radius: var(--radius-sm);
    font-size: 11px;
    border: none;
    cursor: pointer;
    transition: all var(--transition);
  }
  .cmd-tag--red {
    background: rgba(212,91,91,0.1);
    color: inherit;
  }
  .cmd-tag--red:hover { background: rgba(212,91,91,0.18); }
  .cmd-tag--amber {
    background: rgba(212,194,71,0.1);
    color: inherit;
  }
  .cmd-tag--amber:hover { background: rgba(212,194,71,0.18); }
</style>
