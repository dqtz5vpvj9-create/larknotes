<script lang="ts">
  import { onMount } from "svelte";
  import {
    setEditor, setWorkspace, detectEditors, pickFolder,
    getAutostartStatus, setAutostart, setSyncDebounce, setAutoSync, setProviderCliPath,
  } from "../api";

  interface Props {
    editorCommand: string;
    workspacePath: string;
    syncDebounceMs: number;
    autoSync: boolean;
    providerCliPath: string;
    onClose: () => void;
    onEditorChange: (editor: string) => void;
    onWorkspaceChange: (path: string) => void;
    onConfigChange: (key: string, value: unknown) => void;
    onError: (msg: string) => void;
  }

  let {
    editorCommand, workspacePath, syncDebounceMs, autoSync, providerCliPath,
    onClose, onEditorChange, onWorkspaceChange, onConfigChange, onError,
  }: Props = $props();

  let editorInput = $state("");
  let workspaceInput = $state("");
  let debounceInput = $state(2000);
  let autoSyncInput = $state(true);
  let cliPathInput = $state("lark-cli");
  let detectedEditors = $state<[string, string][]>([]);
  let saving = $state(false);
  let autostartEnabled = $state(false);
  let autostartLoading = $state(false);

  $effect(() => { editorInput = editorCommand; });
  $effect(() => { workspaceInput = workspacePath; });
  $effect(() => { debounceInput = syncDebounceMs; });
  $effect(() => { autoSyncInput = autoSync; });
  $effect(() => { cliPathInput = providerCliPath; });

  onMount(async () => {
    try {
      detectedEditors = await detectEditors();
    } catch {
      // ignore
    }
    try {
      autostartEnabled = await getAutostartStatus();
    } catch {
      // ignore — may not be available in dev mode
    }
  });

  async function toggleAutostart() {
    autostartLoading = true;
    try {
      await setAutostart(!autostartEnabled);
      autostartEnabled = !autostartEnabled;
    } catch (e) {
      onError(`设置开机启动失败: ${e}`);
    } finally {
      autostartLoading = false;
    }
  }

  async function toggleAutoSync() {
    autoSyncInput = !autoSyncInput;
  }

  async function handleBrowse() {
    try {
      const path = await pickFolder();
      if (path) workspaceInput = path;
    } catch (e) {
      onError(`选择文件夹失败: ${e}`);
    }
  }

  async function handleSave() {
    if (saving) return;
    saving = true;
    try {
      if (editorInput !== editorCommand) {
        await setEditor(editorInput);
        onEditorChange(editorInput);
      }
      if (workspaceInput !== workspacePath) {
        await setWorkspace(workspaceInput);
        onWorkspaceChange(workspaceInput);
      }
      if (debounceInput !== syncDebounceMs) {
        const clamped = Math.max(500, Math.min(30000, Number(debounceInput) || 2000));
        debounceInput = clamped;
        await setSyncDebounce(clamped);
        onConfigChange("sync_debounce_ms", clamped);
      }
      if (autoSyncInput !== autoSync) {
        await setAutoSync(autoSyncInput);
        onConfigChange("auto_sync", autoSyncInput);
      }
      if (cliPathInput !== providerCliPath) {
        await setProviderCliPath(cliPathInput);
        onConfigChange("provider_cli_path", cliPathInput);
      }
      onClose();
    } catch (e) {
      onError(`保存设置失败: ${e}`);
    } finally {
      saving = false;
    }
  }

  function selectEditor(cmd: string) {
    editorInput = cmd;
  }

  let hasChanges = $derived(
    editorInput.toLowerCase() !== editorCommand.toLowerCase()
    || workspaceInput !== workspacePath
    || debounceInput !== syncDebounceMs
    || autoSyncInput !== autoSync
    || cliPathInput !== providerCliPath
  );
</script>

<div class="settings">
  <div class="settings-header">
    <h2>设置</h2>
    <button class="close-btn" title="关闭 (Esc)" onclick={onClose}>
      <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
        <path d="M3 3l8 8M11 3l-8 8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
      </svg>
    </button>
  </div>

  <div class="settings-body">
    <div class="section-label">存储</div>

    <div class="field">
      <label class="field-label" for="workspace-input">本地文件路径</label>
      <div class="field-row">
        <input
          id="workspace-input"
          type="text"
          class="field-input field-input--grow"
          bind:value={workspaceInput}
          placeholder="C:\Users\...\LarkNotes"
        />
        <button class="browse-btn" onclick={handleBrowse} title="选择文件夹">
          <svg width="14" height="14" viewBox="0 0 14 14" fill="none">
            <path d="M1.5 3.5a1 1 0 011-1h3l1 1.5h5a1 1 0 011 1v5.5a1 1 0 01-1 1h-9a1 1 0 01-1-1v-7z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/>
          </svg>
          浏览
        </button>
      </div>
      <p class="field-hint">文档将保存在此目录的 docs/ 子文件夹中（修改后需重启应用）</p>
    </div>

    <div class="section-label">编辑器</div>

    <div class="field">
      <label class="field-label" for="editor-input">编辑器命令</label>
      <input
        id="editor-input"
        type="text"
        class="field-input"
        bind:value={editorInput}
        placeholder="typora / code / notepad"
      />
      {#if detectedEditors.length > 0}
        <div class="editor-chips">
          {#each detectedEditors as [label, cmd]}
            <button
              class="editor-chip"
              class:editor-chip--selected={editorInput.toLowerCase() === cmd.toLowerCase()}
              onclick={() => selectEditor(cmd)}
            >
              {label}
            </button>
          {/each}
        </div>
      {/if}
      <p class="field-hint">选择已安装的编辑器，或手动输入命令</p>
    </div>

    <div class="section-label">同步</div>

    <div class="field">
      <div class="toggle-row">
        <label class="field-label" for="autosync-toggle">自动同步</label>
        <button
          id="autosync-toggle"
          class="toggle"
          class:toggle--on={autoSyncInput}
          onclick={toggleAutoSync}
          role="switch"
          aria-checked={autoSyncInput}
          aria-label="自动同步"
        >
          <span class="toggle-knob"></span>
        </button>
      </div>
      <p class="field-hint">文件修改后自动推送到飞书</p>
    </div>

    <div class="field">
      <label class="field-label" for="debounce-input">同步延迟 (毫秒)</label>
      <input
        id="debounce-input"
        type="number"
        class="field-input"
        bind:value={debounceInput}
        min="500"
        max="30000"
        step="500"
      />
      <p class="field-hint">文件修改后等待多久再同步，避免频繁操作 (推荐 2000)</p>
    </div>

    <div class="section-label">高级</div>

    <div class="field">
      <label class="field-label" for="cli-path-input">Lark CLI 路径</label>
      <input
        id="cli-path-input"
        type="text"
        class="field-input"
        bind:value={cliPathInput}
        placeholder="lark-cli"
      />
      <p class="field-hint">lark-cli 命令路径，通常保持默认即可</p>
    </div>

    <div class="field">
      <div class="toggle-row">
        <label class="field-label" for="autostart-toggle">开机时自动启动</label>
        <button
          id="autostart-toggle"
          class="toggle"
          class:toggle--on={autostartEnabled}
          onclick={toggleAutostart}
          disabled={autostartLoading}
          role="switch"
          aria-checked={autostartEnabled}
          aria-label="开机自动启动"
        >
          <span class="toggle-knob"></span>
        </button>
      </div>
      <p class="field-hint">系统启动时自动在后台运行 LarkNotes</p>
    </div>
  </div>

  <div class="settings-footer">
    <button class="btn-secondary" onclick={onClose}>取消</button>
    <button
      class="btn-primary"
      onclick={handleSave}
      disabled={!hasChanges || saving}
    >
      {saving ? "保存中..." : "保存"}
    </button>
  </div>
</div>

<style>
  .settings {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
  }
  .settings-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 16px 20px 12px;
    border-bottom: 1px solid var(--c-border);
    flex-shrink: 0;
  }
  .settings-header h2 {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
    color: var(--c-text);
    letter-spacing: -0.01em;
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

  .settings-body {
    flex: 1;
    padding: 16px 20px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .section-label {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--c-text-tertiary);
    padding-top: 4px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .field-label {
    font-size: 12px;
    font-weight: 500;
    color: var(--c-text-secondary);
    letter-spacing: 0.02em;
  }
  .field-row {
    display: flex;
    gap: 6px;
    align-items: stretch;
  }
  .field-input {
    padding: 8px 12px;
    border-radius: var(--radius);
    border: 1px solid var(--c-border);
    background: var(--c-bg-elevated);
    color: var(--c-text);
    font-size: 13px;
    font-family: var(--font-mono);
    outline: none;
    transition: all var(--transition);
  }
  .field-input--grow { flex: 1; min-width: 0; }
  .field-input:focus {
    border-color: var(--c-accent-border, rgba(212,165,71,0.4));
    background: var(--c-bg-hover);
  }
  .field-hint {
    font-size: 11px;
    color: var(--c-text-tertiary);
    margin: 0;
  }

  .browse-btn {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 0 12px;
    border-radius: var(--radius);
    border: 1px solid var(--c-border);
    background: var(--c-bg-elevated);
    color: var(--c-text-secondary);
    font-size: 12px;
    font-family: var(--font-sans);
    cursor: pointer;
    transition: all var(--transition);
    white-space: nowrap;
    flex-shrink: 0;
  }
  .browse-btn:hover {
    background: var(--c-bg-hover);
    color: var(--c-text);
    border-color: var(--c-text-tertiary);
  }

  .editor-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 2px;
  }
  .editor-chip {
    padding: 4px 12px;
    border-radius: var(--radius-pill);
    border: 1px solid var(--c-border);
    background: transparent;
    color: var(--c-text-secondary);
    font-size: 12px;
    font-family: var(--font-sans);
    cursor: pointer;
    transition: all var(--transition);
  }
  .editor-chip:hover {
    background: var(--c-bg-hover);
    color: var(--c-text);
    border-color: var(--c-text-tertiary);
  }
  .editor-chip--selected {
    background: var(--c-accent-bg, rgba(212,165,71,0.15));
    border-color: var(--c-accent);
    color: var(--c-accent);
  }

  .toggle-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .toggle {
    position: relative;
    width: 36px;
    height: 20px;
    border-radius: 10px;
    border: 1px solid var(--c-border);
    background: var(--c-bg-elevated);
    cursor: pointer;
    transition: all var(--transition);
    padding: 0;
    flex-shrink: 0;
  }
  .toggle:hover { border-color: var(--c-text-tertiary); }
  .toggle--on {
    background: var(--c-accent-bg, rgba(212,165,71,0.25));
    border-color: var(--c-accent);
  }
  .toggle-knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: var(--c-text-tertiary);
    transition: all var(--transition);
  }
  .toggle--on .toggle-knob {
    left: 18px;
    background: var(--c-accent);
  }
  .toggle:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .settings-footer {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
    padding: 12px 20px;
    border-top: 1px solid var(--c-border);
    flex-shrink: 0;
    position: sticky;
    bottom: 0;
    background: var(--c-bg);
  }
  .btn-primary {
    padding: 6px 18px;
    border-radius: var(--radius);
    border: none;
    outline: none;
    background: var(--c-accent);
    color: #1a1a1e;
    font-size: 13px;
    font-weight: 500;
    font-family: var(--font-sans);
    cursor: pointer;
    transition: all var(--transition);
  }
  .btn-primary:hover:not(:disabled) { background: var(--c-accent-hover); }
  .btn-primary:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .btn-secondary {
    padding: 6px 16px;
    border-radius: var(--radius);
    border: 1px solid var(--c-border);
    outline: none;
    background: transparent;
    color: var(--c-text-secondary);
    font-size: 13px;
    font-family: var(--font-sans);
    cursor: pointer;
    transition: all var(--transition);
  }
  .btn-secondary:hover {
    background: var(--c-bg-hover);
    color: var(--c-text);
  }
</style>
