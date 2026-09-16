<script lang="ts">
  /**
   * Top toolbar (U1): 48px strip. Kept minimal — actions live next
   * to their objects (row buttons / context menu in U2/U3); the
   * toolbar hosts only app-level chrome: title, theme, Add.
   * The ⌘K search box lands here in U3.
   */
  import type { Theme } from './theme';

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
      {theme === 'dark' ? '☾' : '☀'}
    </button>
    <button class="ctl primary" onclick={onAdd}>＋ Add</button>
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
    padding: 4px 9px;
    font-size: 13px;
  }
</style>
