<script lang="ts">
  /**
   * V5 titlebar: merged macOS traffic lights zone + app name +
   * inline menubar — one 27px chrome strip above the toolbar,
   * data-tauri-drag-region for native window dragging.
   */
  import Menubar from './Menubar.svelte';

  let {
    title,
    categoryFilter = null,
    onAdd,
    onPauseAll,
    onResumeAll,
    onClearDone,
    onToggleTheme,
    themeLabel,
    onPalette,
    onShortcuts,
    onCategoryFilter,
    launchAtLogin,
    onToggleAutostart,
  }: {
    title: string;
    categoryFilter?: import('./categorize').Category | null;
    onAdd: () => void;
    onPauseAll: () => void;
    onResumeAll: () => void;
    onClearDone: () => void;
    onToggleTheme: () => void;
    themeLabel: string;
    onPalette: () => void;
    onShortcuts: () => void;
    onCategoryFilter: (c: import('./categorize').Category | null) => void;
    launchAtLogin?: boolean;
    onToggleAutostart?: () => void;
  } = $props();
</script>

<div class="titlebar" data-tauri-drag-region>
  <span class="lights" data-tauri-drag-region>
    <i class="l close"></i><i class="l min"></i><i class="l max"></i>
  </span>
  <Menubar
    {onAdd}
    {onPauseAll}
    {onResumeAll}
    {onClearDone}
    {onToggleTheme}
    {themeLabel}
    {onPalette}
    {onShortcuts}
    {categoryFilter}
    onCategoryFilter={onCategoryFilter}
    launchAtLogin={launchAtLogin}
    onToggleAutostart={onToggleAutostart}
  />
  <span class="appname" data-tauri-drag-region>{title}</span>
</div>

<style>
  .titlebar {
    height: 27px;
    display: flex;
    align-items: center;
    background: var(--chrome-1);
    border-bottom: 1px solid var(--edge);
    flex: none;
    user-select: none;
    padding-left: 12px;
    gap: 10px;
  }
  .lights {
    display: inline-flex;
    gap: 7px;
    align-items: center;
  }
  .l {
    width: 11px;
    height: 11px;
    border-radius: 50%;
    display: inline-block;
    border: 1px solid var(--shade-1);
  }
  .l.close { background: oklch(62% 0.19 25); }
  .l.min   { background: oklch(72% 0.15 85); }
  .l.max   { background: oklch(72% 0.15 145); }
  .appname {
    margin-left: auto;
    margin-right: 12px;
    font: 600 11px var(--font-sans);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--dim);
    opacity: 0.7;
  }
</style>
