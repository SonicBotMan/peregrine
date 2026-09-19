<script lang="ts">
  /**
   * V5 menubar: real native-style menus (bits-ui Menubar). The
   * entries cover the toolbar actions so keyboard/menu-first users
   * have a classic desktop path alongside the toolbar + shortcuts.
   */
  import { Menubar as MB } from 'bits-ui';
  import {
    Plus,
    Command,
    Pause,
    Play,
    Trash2,
    Eye,
    SunMoon,
    Keyboard,
    Settings,
    Power,
    Check,
    Info,
  } from '@lucide/svelte';
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
    launchAtLogin,
    onToggleAutostart,
    onSettings,
    version,
    onAbout,
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
    launchAtLogin?: boolean;
    onToggleAutostart?: () => void;
    onSettings?: () => void;
    version?: string;
    onAbout?: () => void;
  } = $props();
</script>

<div class="menubar">
  <MB.Root>
    <MB.Menu>
      <MB.Trigger class="mtrigger">File</MB.Trigger>
      <MB.Content class="mcontent" align="start" sideOffset={4}>
        <MB.Item class="mitem" onclick={onAdd}>
          <span class="mic"><Plus size={13} /></span>
          Add URL…
          <span class="mk">⌘N</span>
        </MB.Item>
        <MB.Item class="mitem" onclick={onPalette}>
          <span class="mic"><Command size={13} /></span>
          Command Palette
          <span class="mk">⌘K</span>
        </MB.Item>
        <MB.Separator class="msep" />
        {#if onToggleAutostart}
          <MB.CheckboxItem
            class="mitem"
            checked={launchAtLogin ?? false}
            onCheckedChange={() => onToggleAutostart?.()}
          >
            <span class="mic"><Power size={13} /></span>
            Launch at login
            <span class="mk">✓</span>
          </MB.CheckboxItem>
        {/if}
        {#if onSettings}
          <MB.Item class="mitem" onclick={onSettings}>
            <span class="mic"><Settings size={13} /></span>
            Settings…
            <span class="mk">⌘,</span>
          </MB.Item>
        {/if}
      </MB.Content>
    </MB.Menu>

    <MB.Menu>
      <MB.Trigger class="mtrigger">Task</MB.Trigger>
      <MB.Content class="mcontent" align="start" sideOffset={4}>
        <MB.Item class="mitem" onclick={onPauseAll}>
          <span class="mic"><Pause size={13} /></span>
          Pause All
          <span class="mk">⇧⌘P</span>
        </MB.Item>
        <MB.Item class="mitem" onclick={onResumeAll}>
          <span class="mic"><Play size={13} /></span>
          Resume All
          <span class="mk">⇧⌘R</span>
        </MB.Item>
        <MB.Separator class="msep" />
        <MB.Item class="mitem danger" onclick={onClearDone}>
          <span class="mic"><Trash2 size={13} /></span>
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
          <span class="mic"><Eye size={13} /></span>
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
          <span class="mic"><SunMoon size={13} /></span>
          {themeLabel}
          <span class="mk">⌘T</span>
        </MB.Item>
        <MB.Item class="mitem" onclick={onShortcuts}>
          <span class="mic"><Keyboard size={13} /></span>
          Keyboard Shortcuts
          <span class="mk">?</span>
        </MB.Item>
      </MB.Content>
    </MB.Menu>

    <MB.Menu>
      <MB.Trigger class="mtrigger">Help</MB.Trigger>
      <MB.Content class="mcontent" align="start" sideOffset={4}>
        <MB.Item class="mitem" onclick={onShortcuts}>
          <span class="mic"><Keyboard size={13} /></span>
          Keyboard Shortcuts
          <span class="mk">?</span>
        </MB.Item>
        {#if onAbout}
          <MB.Item class="mitem" onclick={onAbout}>
            <span class="mic"><Info size={13} /></span>
            About Peregrine
          </MB.Item>
        {/if}
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
  .menubar :global(.mic) {
    display: inline-flex;
    color: var(--dim);
  }
  .menubar :global(.mitem[data-highlighted] .mic) {
    color: var(--on-accent-dim);
  }
  .menubar :global(.mitem.danger) {
    color: var(--err);
  }
  .menubar :global(.mitem.danger .mic) {
    color: var(--err);
  }
  .menubar :global(.msep) {
    height: 1px;
    background: var(--line-strong);
    margin: 4px 6px;
  }
</style>
