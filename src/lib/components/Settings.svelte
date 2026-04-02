<script lang="ts">
  import { onMount } from "svelte";
  import { setEditor, setWorkspace, detectEditors, pickFolder, getAutostartStatus, setAutostart } from "../api";

  interface Props {
    editorCommand: string;
    workspacePath: string;
    onClose: () => void;
    onEditorChange: (editor: string) => void;
    onWorkspaceChange: (path: string) => void;
    onError: (msg: string) => void;
  }

  let { editorCommand, workspacePath, onClose, onEditorChange, onWorkspaceChange, onError }: Props = $props();

  let editorInput = $state("");
  let workspaceInput = $state("");
  let detectedEditors = $state<[string, string][]>([]);
  let saving = $state(false);
  let autostartEnabled = $state(false);
  let autostartLoading = $state(false);

  $effect(() => { editorInput = editorCommand; });
  $effect(() => { workspaceInput = workspacePath; });

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
    editorInput.toLowerCase() !== editorCommand.toLowerCase() || workspaceInput !== workspacePath
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
      <p class="field-hint">文档将保存在此目录的 docs/ 子文件夹中</p>
    </div>

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
    padding: 20px;
    display: flex;
    flex-direction: column;
    gap: 20px;
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
    border-color: rgba(212,165,71,0.4);
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
    background: rgba(212,165,71,0.25);
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

  .editor-chip--selected {
    background: rgba(212,165,71,0.15);
    border-color: var(--c-accent);
    color: var(--c-accent);
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
