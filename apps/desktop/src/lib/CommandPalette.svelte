<script lang="ts">
  /**
   * ⌘K command palette (U3): one input over two groups — app actions
   * and every task (fuzzy via bits-ui's built-in command scoring).
   * Selecting a task closes the palette, selects the row and expands
   * its detail panel; selecting an action runs it.
   *
   * Keyboard: ↑↓/ctrl-n·j·p·k move, Enter runs, Esc closes (handled
   * here, not by the App keydown layer — App skips while open).
   */
  import { Command } from 'bits-ui';
  import { Plus, Pause, Play, SunMoon, Download, ChevronRight } from '@lucide/svelte';
  import type { TaskStore } from './store.svelte';
  import { toast } from './toast.svelte';

  let {
    store,
    onClose,
    onAdd,
    onToggleTheme,
    onSelectTask,
  }: {
    store: TaskStore;
    onClose: () => void;
    onAdd: () => void;
    onToggleTheme: () => void;
    onSelectTask: (id: string) => void;
  } = $props();

  function fileName(url: string): string {
    try {
      const u = new URL(url);
      const last = u.pathname.split('/').filter(Boolean).pop();
      return last ? decodeURIComponent(last) : url;
    } catch {
      return url;
    }
  }

  function pauseAll() {
    let n = 0;
    for (const t of store.list) {
      if (t.status === 'running' || t.status === 'queued') {
        n++;
        void store.pause(t.id).catch(() => {});
      }
    }
    toast.push(n ? `Pausing ${n} task${n === 1 ? '' : 's'}…` : 'Nothing to pause');
    onClose();
  }

  function resumeAll() {
    let n = 0;
    for (const t of store.list) {
      if (t.status === 'paused') {
        n++;
        void store.resume(t.id).catch(() => {});
      }
    }
    toast.push(n ? `Resuming ${n} task${n === 1 ? '' : 's'}…` : 'Nothing to resume');
    onClose();
  }
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onClose()} />

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="overlay" onclick={onClose}>
  <div class="palette" role="dialog" aria-label="Command palette" onclick={(e) => e.stopPropagation()}>
    <Command.Root loop={false} class="root" label="Commands">
      <Command.Input class="pinput" placeholder="Type a command or search tasks…" autofocus />
      <Command.List class="plist">
        <Command.Empty class="pempty">No matches</Command.Empty>

        <Command.Group heading="Actions">
          <Command.Item class="pitem" value="new download add url" keywords={['create', 'download', 'add']} onSelect={() => { onAdd(); onClose(); }}>
            <Plus size={14} /> New download…
            <span class="kbd-hint">⌘N</span>
          </Command.Item>
          <Command.Item class="pitem" value="pause all tasks" keywords={['stop']} onSelect={pauseAll}>
            <Pause size={14} /> Pause all
          </Command.Item>
          <Command.Item class="pitem" value="resume all tasks" keywords={['start', 'continue']} onSelect={resumeAll}>
            <Play size={14} /> Resume all
          </Command.Item>
          <Command.Item class="pitem" value="toggle theme light dark appearance" keywords={['theme', 'light', 'dark', 'appearance']} onSelect={() => { onToggleTheme(); onClose(); }}>
            <SunMoon size={14} /> Toggle theme
          </Command.Item>
        </Command.Group>

        <Command.Group heading="Tasks">
          {#each store.list as t (t.id)}
            <Command.Item
              class="pitem"
              value={`${fileName(t.url)} ${t.url} ${t.save_path} ${t.status}`}
              onSelect={() => {
                onSelectTask(t.id);
                onClose();
              }}
            >
              <Download size={14} />
              <span class="tname">{fileName(t.url)}</span>
              <span class="tmeta">{t.status}</span>
              <ChevronRight size={12} class="go" />
            </Command.Item>
          {/each}
        </Command.Group>
      </Command.List>
    </Command.Root>
  </div>
</div>

<style>
  .palette {
    width: min(560px, calc(100vw - 32px));
    background: var(--elevated);
    border: 1px solid var(--line-strong);
    border-radius: 10px;
    overflow: hidden;
    box-shadow:
      0 24px 64px rgb(0 0 0 / 0.35),
      0 4px 16px rgb(0 0 0 / 0.25);
    animation: palette-in var(--dur, 200ms) var(--ease, ease-out);
  }
  .root {
    display: flex;
    flex-direction: column;
  }
  .pinput {
    width: 100%;
    padding: 13px 16px;
    border: 0;
    border-bottom: 1px solid var(--line);
    background: transparent;
    color: var(--text);
    font-size: 14px;
    outline: none;
  }
  .pinput::placeholder {
    color: var(--dim);
  }
  .plist {
    max-height: 56vh;
    overflow-y: auto;
    padding: 6px;
  }
  .pempty {
    padding: 18px 12px;
    text-align: center;
    color: var(--dim);
    font-size: 13px;
  }
  .pitem {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px 10px;
    border-radius: 6px;
    font-size: 13px;
    color: var(--text);
    cursor: pointer;
    user-select: none;
    outline: none;
    transition: background 80ms ease-out;
  }
  /* bits-ui marks the keyboard-moved selection on the item element */
  .pitem:global([data-selected='true']),
  .pitem:global([data-highlighted]),
  .pitem:hover {
    background: var(--line);
  }
  .pitem:global([data-selected='true']) {
    background: var(--line-strong);
  }
  .pitem > :global(svg) {
    color: var(--dim);
    flex-shrink: 0;
  }
  .tname {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tmeta {
    font-size: 11px;
    color: var(--dim);
    text-transform: capitalize;
    flex-shrink: 0;
  }
  .go {
    opacity: 0;
    transition: opacity var(--dur, 200ms) ease-out;
  }
  .pitem:hover .go,
  .pitem:global([data-selected='true']) .go {
    opacity: 0.7;
  }
  .kbd-hint {
    margin-left: auto;
    font-size: 11px;
    color: var(--dim);
    border: 1px solid var(--line);
    border-radius: 4px;
    padding: 0 5px;
  }
  :global(.plist [cmdk-group-heading]) {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--dim);
    padding: 10px 10px 4px;
  }
  @keyframes palette-in {
    from {
      opacity: 0;
      transform: translateY(-8px) scale(0.99);
    }
  }
</style>
