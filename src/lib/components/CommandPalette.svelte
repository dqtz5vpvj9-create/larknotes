<script lang="ts">
  import type { DocMeta } from "../types";

  interface Command {
    id: string;
    label: string;
    hint?: string;
    action: () => void;
  }

  interface Props {
    docs: DocMeta[];
    onClose: () => void;
    onNewDoc: () => void;
    onQuickNote: () => void;
    onSettings: () => void;
    onToggleTheme: () => void;
    onRefresh: () => void;
    onFilterConflicts: () => void;
    onOpenDoc: (docId: string) => void;
  }

  let {
    docs, onClose, onNewDoc, onQuickNote, onSettings,
    onToggleTheme, onRefresh, onFilterConflicts, onOpenDoc,
  }: Props = $props();

  let query = $state("");
  let selectedIndex = $state(0);
  let inputEl: HTMLInputElement | undefined = $state();

  const staticCommands: Command[] = [
    { id: "new-doc", label: "新建文档", hint: "Ctrl+N", action: () => { onClose(); onNewDoc(); } },
    { id: "quick-note", label: "快速笔记", hint: "Ctrl+Shift+N", action: () => { onClose(); onQuickNote(); } },
    { id: "settings", label: "打开设置", action: () => { onClose(); onSettings(); } },
    { id: "theme", label: "切换主题", action: () => { onClose(); onToggleTheme(); } },
    { id: "refresh", label: "刷新文档列表", action: () => { onClose(); onRefresh(); } },
    { id: "conflicts", label: "查看冲突文档", action: () => { onClose(); onFilterConflicts(); } },
  ];

  let filteredCommands = $derived.by(() => {
    const q = query.trim().toLowerCase();
    const results: Command[] = [];

    // Static commands
    for (const cmd of staticCommands) {
      if (!q || cmd.label.toLowerCase().includes(q)) {
        results.push(cmd);
      }
    }

    // Document commands
    for (const doc of docs) {
      const label = `打开: ${doc.title || "Untitled"}`;
      if (!q || label.toLowerCase().includes(q) || (doc.title || "").toLowerCase().includes(q)) {
        results.push({
          id: `doc-${doc.doc_id}`,
          label,
          action: () => { onClose(); onOpenDoc(doc.doc_id); },
        });
      }
    }

    return results;
  });

  $effect(() => {
    // Reset selection when results change
    selectedIndex = 0;
  });

  $effect(() => {
    inputEl?.focus();
  });

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      selectedIndex = Math.min(selectedIndex + 1, filteredCommands.length - 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      selectedIndex = Math.max(selectedIndex - 1, 0);
    } else if (e.key === "Enter") {
      e.preventDefault();
      const cmd = filteredCommands[selectedIndex];
      if (cmd) cmd.action();
    }
  }

  function handleOverlayClick(e: MouseEvent) {
    if ((e.target as HTMLElement).classList.contains("palette-overlay")) {
      onClose();
    }
  }

  /** Highlight matching segments in text */
  function highlightSegments(text: string, q: string): { text: string; match: boolean }[] {
    if (!q) return [{ text, match: false }];
    const lower = text.toLowerCase();
    const qLower = q.toLowerCase();
    const segments: { text: string; match: boolean }[] = [];
    let lastIndex = 0;
    let idx = lower.indexOf(qLower);
    while (idx !== -1) {
      if (idx > lastIndex) segments.push({ text: text.slice(lastIndex, idx), match: false });
      segments.push({ text: text.slice(idx, idx + q.length), match: true });
      lastIndex = idx + q.length;
      idx = lower.indexOf(qLower, lastIndex);
    }
    if (lastIndex < text.length) segments.push({ text: text.slice(lastIndex), match: false });
    return segments.length ? segments : [{ text, match: false }];
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="palette-overlay" onclick={handleOverlayClick} onkeydown={handleKeydown}>
  <div class="palette">
    <div class="palette-input-row">
      <svg class="palette-search-icon" width="15" height="15" viewBox="0 0 14 14" fill="none">
        <circle cx="6.2" cy="6.2" r="4.5" stroke="currentColor" stroke-width="1.4"/>
        <path d="M9.5 9.5l3 3" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>
      </svg>
      <input
        bind:this={inputEl}
        bind:value={query}
        type="text"
        class="palette-input"
        placeholder="输入命令或文档名称..."
      />
    </div>

    <div class="palette-results">
      {#if filteredCommands.length === 0}
        <div class="palette-empty">无匹配结果</div>
      {:else}
        {#each filteredCommands as cmd, i (cmd.id)}
          <button
            class="palette-item"
            class:palette-item--selected={i === selectedIndex}
            onclick={() => cmd.action()}
            onmouseenter={() => (selectedIndex = i)}
          >
            <span class="palette-label">
              {#each highlightSegments(cmd.label, query.trim()) as seg}
                {#if seg.match}<mark class="palette-highlight">{seg.text}</mark>{:else}{seg.text}{/if}
              {/each}
            </span>
            {#if cmd.hint}
              <kbd class="palette-hint">{cmd.hint}</kbd>
            {/if}
          </button>
        {/each}
      {/if}
    </div>
  </div>
</div>

<style>
  .palette-overlay {
    position: fixed;
    inset: 0;
    z-index: 950;
    background: rgba(0,0,0,0.4);
    display: flex;
    justify-content: center;
    padding-top: 20vh;
    animation: overlayFadeIn 100ms ease both;
  }
  @keyframes overlayFadeIn {
    from { opacity: 0; }
    to { opacity: 1; }
  }

  .palette {
    width: 480px;
    max-width: 90vw;
    max-height: 400px;
    background: var(--c-bg-elevated);
    border: 1px solid var(--c-border);
    border-radius: 8px;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    box-shadow: 0 16px 48px rgba(0,0,0,0.4);
    animation: paletteIn 120ms cubic-bezier(0.16, 1, 0.3, 1) both;
    align-self: flex-start;
  }
  @keyframes paletteIn {
    from { opacity: 0; transform: scale(0.96) translateY(-4px); }
    to { opacity: 1; transform: scale(1) translateY(0); }
  }

  .palette-input-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px 16px;
    border-bottom: 1px solid var(--c-border);
  }
  .palette-search-icon {
    color: var(--c-text-tertiary);
    flex-shrink: 0;
  }
  .palette-input {
    flex: 1;
    border: none;
    outline: none;
    background: transparent;
    color: var(--c-text);
    font-size: 14px;
    font-family: var(--font-sans);
  }
  .palette-input::placeholder {
    color: var(--c-text-tertiary);
  }

  .palette-results {
    flex: 1;
    overflow-y: auto;
    padding: 4px;
  }

  .palette-empty {
    padding: 24px 16px;
    text-align: center;
    color: var(--c-text-tertiary);
    font-size: 12px;
    font-family: var(--font-sans);
  }

  .palette-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    width: 100%;
    padding: 8px 12px;
    border: none;
    outline: none;
    background: transparent;
    color: var(--c-text);
    font-size: 13px;
    font-family: var(--font-sans);
    cursor: pointer;
    border-radius: var(--radius);
    transition: background 60ms ease;
    text-align: left;
  }
  .palette-item--selected {
    background: var(--c-bg-hover);
  }

  .palette-label {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .palette-highlight {
    background: rgba(212,165,71,0.25);
    color: var(--c-accent);
    border-radius: 2px;
    padding: 0 1px;
  }

  .palette-hint {
    flex-shrink: 0;
    margin-left: 12px;
    padding: 2px 6px;
    border-radius: var(--radius-sm);
    background: var(--c-bg);
    border: 1px solid var(--c-border);
    color: var(--c-text-tertiary);
    font-size: 11px;
    font-family: var(--font-mono);
  }
</style>
