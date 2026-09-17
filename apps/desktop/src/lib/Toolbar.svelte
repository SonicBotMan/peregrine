<script lang="ts">
  /**
   * Subnav strip (Motrix replication): #2d2d2d bar with the section
   * title on the left and flat white action icons + one Element-
   * primary "Add" button on the right. Kept minimal — actions live
   * next to their objects (row cluster / context menu).
   */
  import type { Theme } from './theme';
  import { Sun, Moon, Plus, Search } from '@lucide/svelte';

  let {
    theme,
    onToggleTheme,
    onAdd,
    onPalette,
  }: {
    theme: Theme;
    onToggleTheme: () => void;
    onAdd: () => void;
    onPalette: () => void;
  } = $props();
</script>

<header class="toolbar">
  <span class="title">Tasks</span>
  <div class="right">
    <button class="flat" onclick={onPalette} title="Command palette (⌘K or /)">
      <span class="mag"><Search size={13} /></span>
      <span class="sk">⌘K</span>
    </button>
    <button
      class="flat icon"
      onclick={onToggleTheme}
      title={theme === 'dark' ? 'Switch to light' : 'Switch to dark'}
      aria-label="Toggle theme"
    >
      {#if theme === 'dark'}
        <Sun size={15} />
      {:else}
        <Moon size={15} />
      {/if}
    </button>
    <button class="ctl primary add" onclick={onAdd}>
      <Plus size={14} />
      Add
    </button>
  </div>
</header>

<style>
  .toolbar {
    display: flex;
    align-items: center;
    height: 44px;
    padding: 0 16px;
    gap: 10px;
    background: var(--subnav);
    border-bottom: 1px solid var(--line-strong);
  }
  .title {
    font-size: 14px;
    font-weight: 600;
    color: var(--text);
  }
  .right {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  /* Motrix subnav actions: white text, transparent bg, #444 hover */
  .flat {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 5px 10px;
    color: var(--dim);
    background: transparent;
    border-radius: 4px;
    font-size: 12px;
    transition: background var(--dur) var(--ease), color var(--dur) var(--ease);
  }
  .flat:hover {
    color: var(--text);
    background: var(--elevated);
  }
  .flat.icon {
    padding: 5px 8px;
    line-height: 0;
    /* static grouping cue: subtle border so the icon trio reads as
     * a control cluster even without hover (VLM pass 5) */
    border: 1px solid var(--line-subtle);
    color: var(--dim);
  }
  .flat.icon:hover {
    border-color: var(--line);
  }
  .mag {
    display: inline-flex;
    line-height: 1;
  }
  .sk {
    border: 1px solid var(--line);
    border-radius: 4px;
    padding: 0 5px;
    font-size: 11px;
  }
  .add {
    padding: 6px 14px;
    line-height: 1;
    margin-left: 4px;
  }
</style>
