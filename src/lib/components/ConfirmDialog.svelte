<script lang="ts">
  interface Props {
    title: string;
    message: string;
    confirmLabel?: string;
    cancelLabel?: string;
    danger?: boolean;
    onConfirm: () => void;
    onCancel: () => void;
  }

  let { title, message, confirmLabel = "确认", cancelLabel = "取消", danger = false, onConfirm, onCancel }: Props = $props();

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") onCancel();
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="overlay" onkeydown={handleKeydown} onclick={onCancel}>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="dialog" onclick={(e: MouseEvent) => e.stopPropagation()}>
    <h3 class="dialog-title">{title}</h3>
    <p class="dialog-message">{message}</p>
    <div class="dialog-actions">
      <button class="btn-cancel" onclick={onCancel}>{cancelLabel}</button>
      <button
        class="btn-confirm"
        class:btn-confirm--danger={danger}
        onclick={onConfirm}
      >
        {confirmLabel}
      </button>
    </div>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 900;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(0,0,0,0.5);
    animation: fadeIn 120ms ease both;
  }
  @keyframes fadeIn {
    from { opacity: 0; }
    to { opacity: 1; }
  }
  .dialog {
    background: var(--c-bg-elevated);
    border: 1px solid var(--c-border);
    border-radius: var(--radius);
    padding: 20px;
    min-width: 320px;
    max-width: 420px;
    box-shadow: 0 12px 40px rgba(0,0,0,0.4);
    animation: scaleIn 150ms ease both;
  }
  @keyframes scaleIn {
    from { opacity: 0; transform: scale(0.95); }
    to { opacity: 1; transform: scale(1); }
  }
  .dialog-title {
    margin: 0 0 8px;
    font-size: 14px;
    font-weight: 600;
    color: var(--c-text);
  }
  .dialog-message {
    margin: 0 0 16px;
    font-size: 13px;
    color: var(--c-text-secondary);
    line-height: 1.5;
  }
  .dialog-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .btn-cancel {
    padding: 6px 16px;
    border-radius: var(--radius);
    border: 1px solid var(--c-border);
    background: transparent;
    color: var(--c-text-secondary);
    font-size: 13px;
    font-family: var(--font-sans);
    cursor: pointer;
    transition: all var(--transition);
  }
  .btn-cancel:hover {
    background: var(--c-bg-hover);
    color: var(--c-text);
  }
  .btn-confirm {
    padding: 6px 18px;
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
  .btn-confirm:hover { opacity: 0.9; }
  .btn-confirm--danger {
    background: var(--c-red);
    color: white;
  }
</style>
