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
  }: {
    theme: Theme;
    onToggleTheme: () => void;
    onAdd: () => void;
  } = $props();
</script>

<header class="toolbar">
  <span class="brand">Peregrine</span>
  <div class="right">
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
  .ctl.icon {
    padding: 5px 8px;
    line-height: 0;
  }
  .ctl.primary {
    line-height: 1;
  }
</style>
