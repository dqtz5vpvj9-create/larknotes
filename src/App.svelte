<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { getAuthStatus, getDocList, getAppConfig, createDoc, manualSync, importDoc, deleteDoc, revealInExplorer, openDocInEditor, quickNote } from "./lib/api";
  import type { AuthStatus, DocMeta, AppConfig, SyncStatusUpdate } from "./lib/types";
  import Toolbar from "./lib/components/Toolbar.svelte";
  import DocList from "./lib/components/DocList.svelte";
  import StatusBar from "./lib/components/StatusBar.svelte";
  import Settings from "./lib/components/Settings.svelte";
  import Toast from "./lib/components/Toast.svelte";
  import CreateDocModal from "./lib/components/CreateDocModal.svelte";
  import ConfirmDialog from "./lib/components/ConfirmDialog.svelte";
  import DocHistory from "./lib/components/DocHistory.svelte";
  import CommandPalette from "./lib/components/CommandPalette.svelte";

  interface ToastMessage {
    id: number;
    text: string;
    type: "error" | "success" | "info";
  }

  let loading = $state(true);
  let auth = $state<AuthStatus | null>(null);
  let docs = $state<DocMeta[]>([]);
  let config = $state<AppConfig | null>(null);
  let showSettings = $state(false);
  let showCreate = $state(false);
  let creating = $state(false);
  let toasts = $state<ToastMessage[]>([]);
  let toastId = 0;
  let copiedLoginCmd = $state(false);
  let copiedRefreshCmd = $state(false);
  let theme = $state<"dark" | "light">("dark");
  let filterConflicts = $state(false);
  let searchQuery = $state("");
  let deleteConfirm = $state<{ docId: string; title: string } | null>(null);
  let historyDocId = $state<string | null>(null);
  let showCommandPalette = $state(false);

  function addToast(text: string, type: "error" | "success" | "info" = "error") {
    const id = ++toastId;
    toasts = [...toasts, { id, text, type }];
    setTimeout(() => {
      toasts = toasts.filter((t) => t.id !== id);
    }, type === "error" ? 6000 : 3000);
  }

  function dismissToast(id: number) {
    toasts = toasts.filter((t) => t.id !== id);
  }

  async function handleCreate(title: string) {
    if (creating) return;
    creating = true;
    showCreate = false;
    try {
      await createDoc(title);
      docs = await getDocList();
      addToast(`「${title}」已创建`, "success");
    } catch (e) {
      addToast(`创建失败: ${e}`);
    } finally {
      creating = false;
    }
  }

  function handleSyncDoc(docId: string) {
    // Optimistic: set status to syncing immediately
    docs = docs.map((d) => {
      if (d.doc_id === docId) {
        return { ...d, sync_status: { type: "Syncing" as const } };
      }
      return d;
    });
    addToast("正在同步…", "info");
    manualSync(docId).catch((e) => {
      addToast(`同步失败: ${e}`);
    });
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

  async function confirmDelete() {
    if (!deleteConfirm) return;
    const { docId } = deleteConfirm;
    deleteConfirm = null;
    try {
      await deleteDoc(docId);
      docs = docs.filter((d) => d.doc_id !== docId);
      addToast("已删除", "success");
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
    if ((e.ctrlKey || e.metaKey) && e.key === "n") {
      e.preventDefault();
      showCreate = true;
    }
    if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key === "P") {
      e.preventDefault();
      showCommandPalette = !showCommandPalette;
    }
    if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key === "N") {
      e.preventDefault();
      handleQuickNote();
    }
    if (e.key === "Escape") {
      if (showCommandPalette) { showCommandPalette = false; return; }
      if (historyDocId) { historyDocId = null; return; }
      if (showCreate) { showCreate = false; return; }
      if (showSettings) { showSettings = false; return; }
    }
  }

  onMount(async () => {
    // Restore theme
    const saved = localStorage.getItem("larknotes-theme") as "dark" | "light" | null;
    if (saved) {
      theme = saved;
      document.documentElement.setAttribute("data-theme", saved);
    }

    try {
      const [authResult, docsResult, configResult] = await Promise.allSettled([
        getAuthStatus(),
        getDocList(),
        getAppConfig(),
      ]);

      if (authResult.status === "fulfilled") auth = authResult.value;
      if (docsResult.status === "fulfilled") docs = docsResult.value;
      if (configResult.status === "fulfilled") config = configResult.value;
    } catch (e) {
      addToast(`初始化失败: ${e}`);
    } finally {
      loading = false;
    }

    listen<SyncStatusUpdate>("sync-status", (event) => {
      const { doc_id, status, title } = event.payload;
      docs = docs.map((d) => {
        if (d.doc_id === doc_id) {
          return { ...d, sync_status: status, title: title ?? d.title };
        }
        return d;
      });
      // Show toast for sync completion
      if (status.type === "Synced") {
        addToast(`「${title ?? doc_id}」同步完成`, "success");
      } else if (status.type === "Conflict") {
        addToast(`「${title ?? doc_id}」同步冲突`, "error");
      }
    });
  });

  let conflicts = $derived(
    docs.filter((d) => d.sync_status.type === "Conflict")
  );

  let syncingCount = $derived(
    docs.filter((d) => d.sync_status.type === "Syncing").length
  );

  let displayDocs = $derived(
    filterConflicts
      ? docs.filter((d) => d.sync_status.type === "Conflict" || d.sync_status.type === "Error")
      : docs
  );
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<main class="app-shell" style="background: var(--c-bg);" onkeydown={handleGlobalKeydown}>
  {#if loading}
    <div class="flex-1 flex items-center justify-center" style="color: var(--c-text-tertiary);">
      <div class="flex flex-col items-center gap-3">
        <div class="w-6 h-6 border-2 border-current border-t-transparent rounded-full animate-spin"></div>
        <span class="text-sm tracking-wide">加载中</span>
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

    {#if auth?.logged_in && auth.needs_refresh}
      <div class="banner banner--amber">
        <span class="banner-dot banner-dot--amber"></span>
        <span class="banner-text banner-text--amber">
          登录令牌即将过期，同步可能失败 — 在终端运行
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
        </span>
      </div>
    {/if}

    <div class="content-area">
      {#if showSettings}
        <div class="settings-panel">
          <Settings
            editorCommand={config?.editor_command ?? "notepad"}
            workspacePath={config?.workspace_dir ?? ""}
            onClose={() => (showSettings = false)}
            onEditorChange={(e) => {
              if (config) config = { ...config, editor_command: e };
            }}
            onWorkspaceChange={(p) => {
              if (config) config = { ...config, workspace_dir: p };
            }}
            onError={(msg) => addToast(msg)}
          />
        </div>
      {:else}
        <DocList
          docs={displayDocs}
          {searchQuery}
          onError={(msg) => addToast(msg)}
          onSync={handleSyncDoc}
          onImport={handleImportDoc}
          onDelete={handleDeleteDoc}
          onReveal={handleRevealInExplorer}
          onShowHistory={(id) => (historyDocId = id)}
        />
      {/if}
    </div>

    <StatusBar
      {auth}
      docCount={docs.length}
      editorName={config?.editor_command ?? "未知"}
      {syncingCount}
    />
  {/if}

  {#if showCreate}
    <CreateDocModal
      onConfirm={handleCreate}
      onCancel={() => (showCreate = false)}
    />
  {/if}

  {#if deleteConfirm}
    <ConfirmDialog
      title="删除文档"
      message={`确定删除「${deleteConfirm.title}」？此操作仅删除本地副本，不影响飞书文档。`}
      confirmLabel="删除"
      danger={true}
      onConfirm={confirmDelete}
      onCancel={() => (deleteConfirm = null)}
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

  <Toast messages={toasts} onDismiss={dismissToast} />
</main>

<style>
  .content-area {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    position: relative;
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
