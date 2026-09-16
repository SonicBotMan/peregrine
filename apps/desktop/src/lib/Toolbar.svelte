<script lang="ts">
  /**
   * Top toolbar (U1): 46px strip. Kept minimal — actions live next
   * to their objects (row cluster / context menu); the toolbar
   * hosts only app-level chrome: title, theme, Add. U2: lucide
   * icons replace text glyphs. The ⌘K search box lands here in U3.
   */
  import type { Theme } from './theme';
  import { Sun, Moon, Plus } from '@lucide/svelte';

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
  <span class="brand">Peregrine</span>
  <div class="right">
    <!-- U3: palette entry — visible affordance for ⌘K//; the
         dialog itself lives in App (single instance, {#if}-mounted) -->
    <button class="ctl search" onclick={onPalette} title="Command palette (⌘K or /)">
      <span class="mag">⌕</span>
      <span class="sk">⌘K</span>
    </button>
    <button
      class="ctl icon"
      onclick={onToggleTheme}
      title={theme === 'dark' ? 'Switch to light' : 'Switch to dark'}
      aria-label="Toggle theme"
    >
      {#if theme === 'dark'}
        <Sun size={14} />
      {:else}
        <Moon size={14} />
      {/if}
    </button>
    <button class="ctl primary" onclick={onAdd}>
      <Plus size={14} />
      Add
    </button>
  </div>
</header>

<style>
  .toolbar {
    display: flex;
    align-items: center;
    height: 46px;
    padding: 0 14px;
    gap: 10px;
    background: var(--panel);
    border-bottom: 1px solid var(--line-strong);
  }
  .brand {
    font-size: 13px;
    font-weight: 700;
    letter-spacing: 0.02em;
  }
  .right {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .search {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--dim);
    font-size: 12px;
    padding: 3px 10px;
    min-width: 120px;
  }
  .search:hover {
    color: var(--text);
  }
  .mag {
    font-size: 13px;
  }
  .sk {
    margin-left: auto;
    border: 1px solid var(--line);
    border-radius: 4px;
    padding: 0 5px;
    font-size: 11px;
  }
  .ctl.icon {
    padding: 5px 8px;
    line-height: 0;
  }
  .ctl.primary {
    line-height: 1;
  }
</style>
