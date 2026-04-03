<script lang="ts">
  interface Props {
    onConfirm: (title: string) => void;
    onCancel: () => void;
  }

  let { onConfirm, onCancel }: Props = $props();

  let title = $state("未命名");
  let inputEl: HTMLInputElement | undefined = $state();

  $effect(() => {
    inputEl?.select();
  });

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      onConfirm(title.trim() || "未命名");
    } else if (e.key === "Escape") {
      onCancel();
    }
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="modal-overlay" onkeydown={handleKeydown} onclick={onCancel}>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div class="modal-card" onclick={(e) => e.stopPropagation()}>
    <h3 class="modal-title">新建文档</h3>
    <div class="modal-body">
      <label class="field-label" for="doc-title">文档标题</label>
      <input
        id="doc-title"
        type="text"
        class="field-input"
        bind:value={title}
        bind:this={inputEl}
        onkeydown={handleKeydown}
        placeholder="输入标题..."
      />
    </div>
    <div class="modal-actions">
      <button class="btn-secondary" onclick={onCancel}>
        取消
      </button>
      <button class="btn-primary" onclick={() => onConfirm(title.trim() || "未命名")}>
        创建
      </button>
    </div>
  </div>
</div>

<style>
  .modal-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 200;
    animation: fadeIn 150ms ease;
  }
  @keyframes fadeIn {
    from { opacity: 0; }
    to { opacity: 1; }
  }
  .modal-card {
    background: var(--c-bg-elevated);
    border: 1px solid var(--c-border);
    border-radius: var(--radius-lg);
    padding: 20px 24px;
    width: 360px;
    box-shadow: 0 16px 48px rgba(0, 0, 0, 0.5);
    animation: slideUp 200ms ease;
  }
  @keyframes slideUp {
    from { opacity: 0; transform: translateY(8px) scale(0.98); }
    to { opacity: 1; transform: translateY(0) scale(1); }
  }
  .modal-title {
    margin: 0 0 16px;
    font-size: 15px;
    font-weight: 600;
    color: var(--c-text);
    letter-spacing: -0.01em;
  }
  .modal-body {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 20px;
  }
  .field-label {
    font-size: 12px;
    font-weight: 500;
    color: var(--c-text-secondary);
    letter-spacing: 0.02em;
  }
  .field-input {
    padding: 8px 12px;
    border-radius: var(--radius);
    border: 1px solid var(--c-border);
    background: var(--c-bg);
    color: var(--c-text);
    font-size: 14px;
    font-family: var(--font-sans);
    outline: none;
    transition: all var(--transition);
  }
  .field-input:focus {
    border-color: var(--c-accent-border);
    background: var(--c-bg-hover);
  }
  .modal-actions {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
  }
  .btn-primary {
    padding: 7px 20px;
    border-radius: var(--radius);
    border: none;
    background: var(--c-accent);
    color: #1a1a1e;
    font-size: 13px;
    font-weight: 500;
    font-family: var(--font-sans);
    cursor: pointer;
    transition: all var(--transition);
  }
  .btn-primary:hover { background: var(--c-accent-hover); }
  .btn-secondary {
    padding: 7px 16px;
    border-radius: var(--radius);
    border: 1px solid var(--c-border);
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
