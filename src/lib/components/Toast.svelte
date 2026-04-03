<script lang="ts">
  interface ToastMessage {
    id: number;
    text: string;
    type: "error" | "success" | "info";
  }

  interface Props {
    messages: ToastMessage[];
    onDismiss: (id: number) => void;
  }

  let { messages, onDismiss }: Props = $props();

  function dotColor(type: string): string {
    switch (type) {
      case "success": return "var(--c-green)";
      case "info": return "var(--c-blue)";
      default: return "var(--c-red)";
    }
  }

  function borderColor(type: string): string {
    switch (type) {
      case "success": return "rgba(92,184,138,0.2)";
      case "info": return "rgba(91,159,212,0.2)";
      default: return "rgba(212,91,91,0.2)";
    }
  }
</script>

{#if messages.length > 0}
  <div class="toast-stack" role="status" aria-live="polite">
    {#each messages as msg (msg.id)}
      <div
        class="toast"
        style="border-color: {borderColor(msg.type)}"
        role={msg.type === "error" ? "alert" : undefined}
      >
        <div class="toast-dot" style="background: {dotColor(msg.type)}"></div>
        <span class="toast-msg">{msg.text}</span>
        <button class="toast-close" title="关闭" onclick={() => onDismiss(msg.id)}>
          <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
            <path d="M3 3l6 6M9 3l-6 6" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>
          </svg>
        </button>
      </div>
    {/each}
  </div>
{/if}

<style>
  .toast-stack {
    position: fixed;
    bottom: 36px;
    right: 16px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    z-index: 1100;
  }
  .toast {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 10px 14px;
    border-radius: var(--radius-lg);
    background: var(--c-bg-elevated);
    box-shadow: 0 8px 24px rgba(0,0,0,0.4);
    max-width: 360px;
    animation: toastIn 250ms ease both;
  }
  @keyframes toastIn {
    from { opacity: 0; transform: translateY(8px) scale(0.96); }
    to { opacity: 1; transform: translateY(0) scale(1); }
  }
  .toast-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    margin-top: 5px;
    flex-shrink: 0;
  }
  .toast-msg {
    flex: 1;
    font-size: 12px;
    color: var(--c-text);
    line-height: 1.5;
    word-wrap: break-word;
    overflow-wrap: break-word;
    min-width: 0;
  }
  .toast-close {
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
  .toast-close:hover {
    background: var(--c-bg-active);
    color: var(--c-text);
  }
</style>
