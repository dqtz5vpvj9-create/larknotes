<script lang="ts">
  import type { FolderTreeNode } from "../types";

  interface Props {
    tree: FolderTreeNode[];
    currentFolder: string;
    rootDocCount: number;
    onSelectFolder: (path: string) => void;
    onCreateFolder: (parentPath: string) => void;
    onDeleteFolder: (path: string) => void;
  }

  let { tree, currentFolder, rootDocCount, onSelectFolder, onCreateFolder, onDeleteFolder }: Props = $props();

  let expanded: Record<string, boolean> = $state({});

  function toggle(path: string) {
    expanded[path] = !expanded[path];
  }

  function isExpanded(path: string): boolean {
    return expanded[path] ?? false;
  }

  function isActive(path: string): boolean {
    return currentFolder === path;
  }

  function isAncestor(folderPath: string): boolean {
    return currentFolder.startsWith(folderPath + "/");
  }

  // Auto-expand ancestors of current folder
  $effect(() => {
    if (currentFolder) {
      const parts = currentFolder.split("/");
      for (let i = 1; i <= parts.length; i++) {
        const ancestor = parts.slice(0, i).join("/");
        expanded[ancestor] = true;
      }
    }
  });
</script>

<nav class="folder-tree">
  <button
    class="tree-item"
    class:tree-item--active={currentFolder === ""}
    onclick={() => onSelectFolder("")}
  >
    <svg class="tree-icon" width="14" height="14" viewBox="0 0 16 16" fill="none">
      <path d="M1.5 2.5h5l1.5 1.5H14.5v9H1.5z" stroke="currentColor" stroke-width="1.2" fill="none"/>
    </svg>
    <span class="tree-label">All Documents</span>
    <span class="tree-count">{rootDocCount}</span>
  </button>

  {#each tree as node (node.path)}
    {@render folderNode(node, 0)}
  {/each}

  <button class="tree-add" onclick={() => onCreateFolder("")} title="New folder">
    <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
      <path d="M6 1v10M1 6h10" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>
    </svg>
    <span>New Folder</span>
  </button>
</nav>

{#snippet folderNode(node: FolderTreeNode, depth: number)}
  <div class="tree-group" style="--depth: {depth}">
    <button
      class="tree-item"
      class:tree-item--active={isActive(node.path)}
      class:tree-item--ancestor={isAncestor(node.path)}
      onclick={() => { onSelectFolder(node.path); if (node.children.length) toggle(node.path); }}
    >
      {#if node.children.length > 0}
        <svg
          class="tree-chevron"
          class:tree-chevron--open={isExpanded(node.path)}
          width="10" height="10" viewBox="0 0 10 10"
        >
          <path d="M3 2l4 3-4 3" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
      {:else}
        <span class="tree-chevron-spacer"></span>
      {/if}

      <svg class="tree-icon" width="14" height="14" viewBox="0 0 16 16" fill="none">
        <path d="M1.5 2.5h5l1.5 1.5H14.5v9H1.5z" stroke="currentColor" stroke-width="1.2" fill={isActive(node.path) ? "var(--c-accent-bg)" : "none"}/>
      </svg>
      <span class="tree-label">{node.name}</span>
      {#if node.doc_count > 0}
        <span class="tree-count">{node.doc_count}</span>
      {/if}
    </button>

    {#if isExpanded(node.path) && node.children.length > 0}
      {#each node.children as child (child.path)}
        {@render folderNode(child, depth + 1)}
      {/each}
    {/if}
  </div>
{/snippet}

<style>
  .folder-tree {
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: 8px 0;
    overflow-y: auto;
    font-family: var(--font-sans);
    font-size: 12px;
    user-select: none;
  }

  .tree-group {
    padding-left: calc(var(--depth, 0) * 14px);
  }

  .tree-item {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 5px 10px;
    border: none;
    background: transparent;
    color: var(--c-text-secondary);
    font: inherit;
    cursor: pointer;
    border-radius: var(--radius-sm);
    transition: background 60ms ease, color 60ms ease;
    text-align: left;
    white-space: nowrap;
    overflow: hidden;
  }
  .tree-item:hover {
    background: var(--c-bg-hover);
    color: var(--c-text);
  }
  .tree-item--active {
    background: var(--c-accent-bg);
    color: var(--c-accent);
    font-weight: 500;
  }
  .tree-item--ancestor {
    color: var(--c-text);
  }

  .tree-icon {
    flex-shrink: 0;
    opacity: 0.7;
  }
  .tree-item--active .tree-icon {
    opacity: 1;
  }

  .tree-chevron {
    flex-shrink: 0;
    transition: transform 120ms ease;
    opacity: 0.5;
  }
  .tree-chevron--open {
    transform: rotate(90deg);
  }
  .tree-chevron-spacer {
    width: 10px;
    flex-shrink: 0;
  }

  .tree-label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .tree-count {
    flex-shrink: 0;
    font-size: 10px;
    color: var(--c-text-tertiary);
    background: var(--c-bg);
    padding: 0 5px;
    border-radius: 8px;
    line-height: 16px;
  }

  .tree-add {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 10px;
    margin-top: 4px;
    border: none;
    background: transparent;
    color: var(--c-text-tertiary);
    font: inherit;
    font-size: 11px;
    cursor: pointer;
    border-radius: var(--radius-sm);
    transition: color 60ms ease;
  }
  .tree-add:hover {
    color: var(--c-accent);
  }
</style>
