<script lang="ts">
  /**
   * V5 menubar: real native-style menus (bits-ui Menubar). The
   * entries cover the toolbar actions so keyboard/menu-first users
   * have a classic desktop path alongside the toolbar + shortcuts.
   */
  import { Menubar as MB } from 'bits-ui';
import { CATEGORIES, type Category } from './categorize';

  let {
    onAdd,
    onPauseAll,
    onResumeAll,
    onClearDone,
    onToggleTheme,
    themeLabel,
    onPalette,
    onShortcuts,
    categoryFilter = null,
    onCategoryFilter = () => {},
  }: {
    onAdd: () => void;
    onPauseAll: () => void;
    onResumeAll: () => void;
    onClearDone: () => void;
    onToggleTheme: () => void;
    themeLabel: string;
    onPalette: () => void;
    onShortcuts: () => void;
    categoryFilter?: Category | null;
    onCategoryFilter?: (c: Category | null) => void;
  } = $props();
</script>

<div class="menubar">
  <MB.Root>
    <MB.Menu>
      <MB.Trigger class="mtrigger">File</MB.Trigger>
      <MB.Content class="mcontent" align="start" sideOffset={4}>
        <MB.Item class="mitem" onclick={onAdd}>
          Add URL…
          <span class="mk">⌘N</span>
        </MB.Item>
        <MB.Item class="mitem" onclick={onPalette}>
          Command Palette
          <span class="mk">⌘K</span>
        </MB.Item>
      </MB.Content>
    </MB.Menu>

    <MB.Menu>
      <MB.Trigger class="mtrigger">Task</MB.Trigger>
      <MB.Content class="mcontent" align="start" sideOffset={4}>
        <MB.Item class="mitem" onclick={onPauseAll}>
          Pause All
          <span class="mk">⇧⌘P</span>
        </MB.Item>
        <MB.Item class="mitem" onclick={onResumeAll}>
          Resume All
          <span class="mk">⇧⌘R</span>
        </MB.Item>
        <MB.Separator class="msep" />
        <MB.Item class="mitem" onclick={onClearDone}>
          Clear Finished…
        </MB.Item>
      </MB.Content>
    </MB.Menu>

    <MB.Menu>
      <MB.Trigger class="mtrigger">View</MB.Trigger>
      <MB.Content class="mcontent" align="start" sideOffset={4}>
        <MB.Item
          class="mitem"
          onclick={() => onCategoryFilter(null)}
          data-on={categoryFilter === null}
        >
          All Types
          {#if categoryFilter === null}<span class="mk">✓</span>{/if}
        </MB.Item>
        {#each CATEGORIES as c (c.id)}
          <MB.Item
            class="mitem"
            onclick={() => onCategoryFilter(categoryFilter === c.id ? null : c.id)}
            data-on={categoryFilter === c.id}
          >
            {c.label}
            {#if categoryFilter === c.id}<span class="mk">✓</span>{/if}
          </MB.Item>
        {/each}
        <MB.Separator class="msep" />
        <MB.Item class="mitem" onclick={onToggleTheme}>
          {themeLabel}
          <span class="mk">⌘T</span>
        </MB.Item>
        <MB.Item class="mitem" onclick={onShortcuts}>
          Keyboard Shortcuts
          <span class="mk">?</span>
        </MB.Item>
      </MB.Content>
    </MB.Menu>

    <MB.Menu>
      <MB.Trigger class="mtrigger">Help</MB.Trigger>
      <MB.Content class="mcontent" align="start" sideOffset={4}>
        <MB.Item class="mitem" onclick={onShortcuts}>
          Keyboard Shortcuts
          <span class="mk">?</span>
        </MB.Item>
      </MB.Content>
    </MB.Menu>
  </MB.Root>
</div>

<style>
  .menubar {
    display: flex;
    align-items: center;
    font: 400 12px var(--font-sans);
    color: var(--dim);
    user-select: none;
  }
  .menubar :global(.mtrigger) {
    padding: 3px 9px;
    border-radius: 4px;
    cursor: default;
    font: 400 12px var(--font-sans);
    color: var(--dim);
    background: none;
    border: none;
    outline: none;
  }
  .menubar :global(.mtrigger:hover),
  .menubar :global(.mtrigger[data-state='open']) {
    background: var(--elevated);
    color: var(--text);
  }
  .menubar :global(.mcontent) {
    min-width: 190px;
    background: var(--elevated);
    border: 1px solid var(--line-strong);
    border-radius: 6px;
    padding: 4px;
    box-shadow: 0 12px 32px var(--shade-2), 0 2px 8px var(--shade-1);
    z-index: 60;
  }
  .menubar :global(.mitem) {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 24px;
    padding: 5px 10px;
    border-radius: 4px;
    font: 400 12px var(--font-sans);
    color: var(--text);
    cursor: pointer;
    outline: none;
    user-select: none;
  }
  .menubar :global(.mitem[data-highlighted]) {
    background: var(--accent);
    color: var(--on-accent);
  }
  .menubar :global(.mitem[data-highlighted] .mk) {
    color: var(--on-accent-dim);
  }
  .menubar :global(.mk) {
    font: 500 10px var(--font-mono);
    color: var(--dim);
  }
  .menubar :global(.msep) {
    height: 1px;
    background: var(--line-strong);
    margin: 4px 6px;
  }
</style>
