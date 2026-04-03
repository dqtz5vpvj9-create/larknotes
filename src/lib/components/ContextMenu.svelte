<script lang="ts">
  import { onMount } from "svelte";

  interface MenuItem {
    label: string;
    icon?: string;
    action: () => void;
    danger?: boolean;
    separator?: boolean;
  }

  interface Props {
    x: number;
    y: number;
    items: MenuItem[];
    onClose: () => void;
  }

  let { x, y, items, onClose }: Props = $props();
  let menuEl = $state<HTMLDivElement | null>(null);
  let selectedIndex = $state(-1);

  onMount(() => {
    // Adjust position if menu would go off-screen
    if (menuEl) {
      const rect = menuEl.getBoundingClientRect();
      if (rect.right > window.innerWidth) {
        menuEl.style.left = `${window.innerWidth - rect.width - 8}px`;
      }
      if (rect.bottom > window.innerHeight) {
        menuEl.style.top = `${window.innerHeight - rect.height - 8}px`;
      }
      menuEl.focus();
    }

    function handleClick(e: MouseEvent) {
      if (menuEl && !menuEl.contains(e.target as Node)) {
        onClose();
      }
    }
    window.addEventListener("click", handleClick);
    return () => {
      window.removeEventListener("click", handleClick);
    };
  });

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      selectedIndex = Math.min(selectedIndex + 1, items.length - 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      selectedIndex = Math.max(selectedIndex - 1, 0);
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      if (selectedIndex >= 0 && selectedIndex < items.length) {
        items[selectedIndex].action();
        onClose();
      }
    }
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="context-menu"
  bind:this={menuEl}
  style="left: {x}px; top: {y}px;"
  role="menu"
  tabindex="-1"
  onkeydown={handleKeydown}
>
  {#each items as item, i}
    {#if item.separator}
      <div class="separator"></div>
    {/if}
    <button
      class="menu-item"
      class:menu-item--danger={item.danger}
      class:menu-item--selected={i === selectedIndex}
      role="menuitem"
      onclick={() => { item.action(); onClose(); }}
      onmouseenter={() => (selectedIndex = i)}
    >
      {item.label}
    </button>
  {/each}
</div>

<style>
  .context-menu {
    position: fixed;
    z-index: 1000;
    min-width: 160px;
    padding: 4px;
    border-radius: var(--radius);
    background: var(--c-bg-elevated);
    border: 1px solid var(--c-border);
    box-shadow: 0 8px 30px rgba(0,0,0,0.3), 0 2px 8px rgba(0,0,0,0.2);
    animation: menuFadeIn 120ms ease both;
    outline: none;
  }
  @keyframes menuFadeIn {
    from { opacity: 0; transform: scale(0.95) translateY(-4px); }
    to { opacity: 1; transform: scale(1) translateY(0); }
  }
  .menu-item {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 6px 12px;
    border-radius: 4px;
    border: none;
    outline: none;
    background: transparent;
    color: var(--c-text-secondary);
    font-size: 12px;
    font-family: var(--font-sans);
    cursor: pointer;
    transition: all 80ms;
    text-align: left;
  }
  .menu-item:hover, .menu-item--selected {
    background: var(--c-bg-hover);
    color: var(--c-text);
  }
  .menu-item--danger { color: var(--c-red); }
  .menu-item--danger:hover, .menu-item--danger.menu-item--selected {
    background: rgba(212,91,91,0.1);
    color: var(--c-red);
  }
  .separator {
    height: 1px;
    background: var(--c-border);
    margin: 4px 8px;
  }
</style>
