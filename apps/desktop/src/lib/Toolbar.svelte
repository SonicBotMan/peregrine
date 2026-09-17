<script lang="ts">
  /**
   * V5 toolbar: tool-grade affordance — colored filled icons with
   * small caps labels (IDM-style), filter dropdown (absorbs the old
   * sidebar's status/category filters), live task search.
   */
  import { DropdownMenu } from 'bits-ui';
  import { Search, ChevronDown } from '@lucide/svelte';
  import { CATEGORIES, type Category } from './categorize';
  import type { StatusFilter } from './types';

  let {
    query = $bindable(''),
    statusFilter = $bindable<StatusFilter>('all'),
    categoryFilter = $bindable<Category | null>(null),
    counts,
    hasSelection,
    onAdd,
    onPauseAll,
    onResumeAll,
    onClearDone,
    onOpenFolder,
  }: {
    query?: string;
    statusFilter?: StatusFilter;
    categoryFilter?: Category | null;
    counts: { active: number; failed: number; completed: number };
    hasSelection: boolean;
    onAdd: () => void;
    onPauseAll: () => void;
    onResumeAll: () => void;
    onClearDone: () => void;
    onOpenFolder: () => void;
  } = $props();

  const STATUS: { id: StatusFilter; label: string }[] = [
    { id: 'all', label: 'All' },
    { id: 'active', label: 'Active' },
    { id: 'completed', label: 'Completed' },
    { id: 'failed', label: 'Failed' },
  ];

  let searchEl: HTMLInputElement | undefined = $state();
  export function focusSearch() {
    searchEl?.focus();
  }

  const filterLabel = $derived(
    categoryFilter
      ? (CATEGORIES.find((c) => c.id === categoryFilter)?.label ?? 'All')
      : (STATUS.find((s) => s.id === statusFilter)?.label ?? 'All'),
  );
</script>

<div class="toolbar">
  <button class="tool" onclick={onAdd} title="Add URL (⌘N)">
    <svg viewBox="0 0 24 24" fill="none"><circle cx="12" cy="12" r="9.2" fill="oklch(62% 0.14 262)"/><path d="M12 7.5v9M7.5 12h9" stroke="white" stroke-width="2.2" stroke-linecap="round"/></svg>
    <span class="lbl">Add URL</span>
  </button>

  <div class="tdiv"></div>

  <button class="tool" onclick={onPauseAll} disabled={counts.active === 0} title="Pause all">
    <svg viewBox="0 0 24 24" fill="none"><rect x="6" y="4.5" width="4" height="15" rx="1" fill="oklch(72% 0.13 75)"/><rect x="14" y="4.5" width="4" height="15" rx="1" fill="oklch(72% 0.13 75)"/></svg>
    <span class="lbl">Pause All</span>
  </button>
  <button class="tool" onclick={onResumeAll} title="Resume all">
    <svg viewBox="0 0 24 24" fill="none"><path d="M7 4.8l11 7.2-11 7.2z" fill="oklch(63% 0.13 150)" stroke="oklch(50% 0.12 150)" stroke-width="1" stroke-linejoin="round"/></svg>
    <span class="lbl">Resume All</span>
  </button>

  <div class="tdiv"></div>

  <button class="tool" onclick={onClearDone} disabled={counts.completed === 0} title="Remove finished tasks from the list">
    <svg viewBox="0 0 24 24" fill="none"><path d="M4 7h16l-1.3 12.2a1.6 1.6 0 0 1-1.6 1.4H6.9a1.6 1.6 0 0 1-1.6-1.4z" fill="oklch(66% 0.09 260)" opacity=".85"/><path d="M8.5 9.5V6.8a3.5 3.5 0 0 1 7 0v2.7" stroke="oklch(66% 0.09 260)" stroke-width="1.8" stroke-linecap="round"/></svg>
    <span class="lbl">Clear Done</span>
  </button>
  <button class="tool" onclick={onOpenFolder} disabled={!hasSelection} title="Reveal the selected task's file">
    <svg viewBox="0 0 24 24" fill="none"><path d="M3 7.5a1.8 1.8 0 0 1 1.8-1.8h4l2 2.2h8.4A1.8 1.8 0 0 1 21 9.7v7.5a1.8 1.8 0 0 1-1.8 1.8H4.8A1.8 1.8 0 0 1 3 17.2z" fill="oklch(70% 0.11 85)"/><path d="M3 11.5h18" stroke="oklch(58% 0.10 85)" stroke-width="1.4"/></svg>
    <span class="lbl">Open Folder</span>
  </button>

  <div class="spacer"></div>

  <DropdownMenu.Root>
    <DropdownMenu.Trigger class="filter-btn" title="Filter tasks">
      {filterLabel}
      <ChevronDown size={12} />
    </DropdownMenu.Trigger>
    <DropdownMenu.Content class="fmenu" align="end" sideOffset={6}>
      {#each STATUS as s (s.id)}
        <DropdownMenu.Item
          class="fitem"
          onclick={() => {
            statusFilter = s.id;
            categoryFilter = null;
          }}
        >
          {s.label}
          {#if statusFilter === s.id && !categoryFilter}<span class="on">✓</span>{/if}
        </DropdownMenu.Item>
      {/each}
      <DropdownMenu.Separator class="fsep" />
      {#each CATEGORIES as c (c.id)}
        <DropdownMenu.Item
          class="fitem"
          onclick={() => {
            categoryFilter = categoryFilter === c.id ? null : c.id;
            statusFilter = 'all';
          }}
        >
          {c.label}
          {#if categoryFilter === c.id}<span class="on">✓</span>{/if}
        </DropdownMenu.Item>
      {/each}
    </DropdownMenu.Content>
  </DropdownMenu.Root>

  <div class="search" role="search">
    <Search size={12} aria-hidden="true" />
    <input
      bind:this={searchEl}
      bind:value={query}
      placeholder="Filter tasks…"
      aria-label="Filter tasks"
      spellcheck="false"
    />
    <kbd>⌘F</kbd>
  </div>
</div>

<style>
  .toolbar {
    height: 46px;
    background: linear-gradient(var(--chrome-2), var(--chrome-1));
    border-bottom: 1px solid var(--edge);
    box-shadow: inset 0 1px 0 var(--inset-hl);
    display: flex;
    align-items: stretch;
    padding: 0 10px;
    gap: 2px;
    flex: none;
    user-select: none;
  }
  .tdiv {
    width: 1px;
    margin: 7px 8px;
    background: var(--line-strong);
    flex: none;
  }
  .spacer { flex: 1; }

  .tool {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 3px;
    min-width: 58px;
    padding: 0 8px;
    border: 1px solid transparent;
    border-radius: 4px;
    background: none;
    color: color-mix(in oklch, var(--text) 78%, var(--bg));
    cursor: pointer;
  }
  .tool:hover:not(:disabled) {
    background: var(--elevated);
    border-color: var(--line-strong);
    box-shadow: 0 1px 2px var(--shade-1);
    color: var(--text);
  }
  .tool:active:not(:disabled) {
    box-shadow: inset 0 1px 3px var(--shade-2);
  }
  .tool:disabled {
    opacity: 0.38;
    cursor: default;
  }
  .tool svg { width: 19px; height: 19px; }
  .lbl {
    font: 600 10px var(--font-sans);
    letter-spacing: 0.035em;
  }

  .filter-btn {
    align-self: center;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 26px;
    padding: 0 10px;
    border: 1px solid var(--line-strong);
    border-radius: 4px;
    background: var(--bg);
    color: var(--text);
    font: 500 12px var(--font-sans);
    cursor: pointer;
  }
  .filter-btn:hover { border-color: var(--accent); }

  .search {
    align-self: center;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 26px;
    padding: 0 9px;
    border: 1px solid var(--line-strong);
    border-radius: 4px;
    color: var(--dim);
    font: 400 12px var(--font-sans);
    width: 180px;
    background: var(--bg);
    box-shadow: inset 0 1px 2px var(--inset-shade);
  }
  .search input {
    all: unset;
    color: var(--text);
    width: 100%;
    font: 400 12px var(--font-sans);
  }
  .search kbd {
    font: 500 10px var(--font-mono);
    padding: 1px 5px;
    border-radius: 3px;
    border: 1px solid var(--line);
    color: var(--dim);
    background: var(--panel);
  }

  .fmenu {
    min-width: 160px;
    background: var(--elevated);
    border: 1px solid var(--line-strong);
    border-radius: 6px;
    padding: 4px;
    box-shadow: 0 12px 32px var(--shade-2), 0 2px 8px var(--shade-1);
    z-index: 60;
  }
  .fitem {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 5px 10px;
    border-radius: 4px;
    font: 400 12px var(--font-sans);
    color: var(--text);
    cursor: pointer;
    outline: none;
  }
  .fitem[data-highlighted] {
    background: var(--accent);
    color: var(--on-accent);
  }
  .fitem .on { font-weight: 700; }
  .fsep {
    height: 1px;
    background: var(--line-strong);
    margin: 4px 6px;
  }
</style>
