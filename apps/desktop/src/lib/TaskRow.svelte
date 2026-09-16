<script lang="ts">
  /**
   * One task row (U2 rebuild): flat tiled column grid —
   *   chevron | name | pct | done/total | speed | eta | status chip
   * with a 2px status-tinted progress underline (indeterminate
   * shimmer when size is unknown) and a hover-revealed action
   * cluster fading over the tail columns. Click / Enter / Space
   * toggles selection (inline SegmentPanel expansion below the
   * row); double-click a completed row reveals the artifact;
   * right-click opens the context menu. The row element is ours
   * (bits-ui `child` snippet pattern) so scoped styles apply.
   * All list truth lives in the store — the row stays dumb.
   */
  import { ContextMenu } from 'bits-ui';
  import SegmentPanel from './SegmentPanel.svelte';
  import { formatBytes, formatBps, formatEta } from './format';
  import type { TaskView } from './store.svelte';
  import { Pause, Play, X } from '@lucide/svelte';

  let {
    task,
    daemon,
    selected = false,
    onPause,
    onResume,
    onRemove,
    onLimit,
    onSelect,
    onOpenFile,
    onOpenSaved,
    onCopyUrl,
  }: {
    task: TaskView;
    daemon: import('./daemon').Daemon;
    selected?: boolean;
    onPause: (id: string) => void;
    onResume: (id: string) => void;
    onRemove: (id: string) => void;
    onLimit: (id: string, bps: number) => void;
    onSelect: (id: string) => void;
    onOpenFile: (id: string) => void;
    onOpenSaved: (id: string) => void;
    onCopyUrl: (id: string) => void;
  } = $props();

  const name = $derived(task.url.split('/').filter(Boolean).pop() ?? task.url);
  const pct = $derived(task.fraction !== null ? Math.round(task.fraction * 100) : null);
  const done = $derived(formatBytes(task.received_bytes));
  const size = $derived(formatBytes(task.total_bytes));
  const speed = $derived(
    task.speed !== null && task.status === 'running' ? formatBps(task.speed) : '—',
  );
  const eta = $derived(
    task.status === 'running' &&
      task.total_bytes !== null &&
      task.speed !== null &&
      task.speed > 0
      ? formatEta(task.total_bytes - task.received_bytes, task.speed)
      : '—',
  );
  const running = $derived(task.status === 'running' || task.status === 'queued');
  const resumable = $derived(task.status === 'paused' || task.status === 'failed');

  function stop<T extends () => void>(fn: T) {
    return (e: Event) => {
      e.stopPropagation();
      fn();
    };
  }
</script>

<ContextMenu.Root>
  <ContextMenu.Trigger>
    {#snippet child({ props })}
      <!-- svelte-ignore a11y_no_noninteractive_element_interactions, a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
      <div
        {...props}
        class="row {selected ? 'selected' : ''}"
        data-id={task.id}
        data-status={task.status}
        role="button"
        tabindex={0}
        aria-pressed={selected}
        aria-label={`${name} — ${task.status}`}
        title={task.error ? task.error : task.url}
        onclick={() => onSelect(task.id)}
        ondblclick={() => task.status === 'completed' && onOpenSaved(task.id)}
        onkeydown={(e) => {
          // Enter only: Space belongs to the global layer (pause/
          // resume selected) — a row-local Space would double-fire
          // after bubbling to window (R2 P0). Buttons inside the row
          // keep native Space activation (exempted in App.onKeydown).
          if (e.key === 'Enter') {
            e.preventDefault();
            onSelect(task.id);
          }
        }}
      >
        <span class="chev" class:open={selected} aria-hidden="true">▸</span>
        <span class="name" class:errored={!!task.error}>{name}</span>
        <span class="col pct">{pct !== null ? `${pct}%` : '—'}</span>
        <span class="col size">{done}{size !== '—' ? ` / ${size}` : ''}</span>
        <span class="col speed">{speed}</span>
        <span class="col eta">{eta}</span>
        <span class="chip" data-kind={task.status}>{task.status}</span>

        <span class="cluster" role="group" aria-label="Row actions">
          {#if running}
            <button
              class="ctl icon"
              title="Pause"
              aria-label={`Pause ${name}`}
              onclick={stop(() => onPause(task.id))}
            >
              <Pause size={13} />
            </button>
          {:else if resumable}
            <button
              class="ctl icon"
              title={task.status === 'failed' ? 'Retry' : 'Resume'}
              aria-label={`${task.status === 'failed' ? 'Retry' : 'Resume'} ${name}`}
              onclick={stop(() => onResume(task.id))}
            >
              <Play size={13} />
            </button>
          {/if}
          <button
            class="ctl icon danger"
            title="Remove"
            aria-label={`Remove ${name}`}
            onclick={stop(() => onRemove(task.id))}
          >
            <X size={13} />
          </button>
        </span>

        <span class="line" aria-hidden="true">
          {#if pct !== null}
            <i style:width="{pct}%"></i>
          {:else}
            <i class="anim"></i>
          {/if}
        </span>
      </div>
    {/snippet}
  </ContextMenu.Trigger>

  <ContextMenu.Content class="ctx">
    {#if running}
      <ContextMenu.Item class="ctx-item" onSelect={() => onPause(task.id)}>Pause</ContextMenu.Item>
    {/if}
    {#if resumable}
      <ContextMenu.Item class="ctx-item" onSelect={() => onResume(task.id)}>
        {task.status === 'failed' ? 'Retry' : 'Resume'}
      </ContextMenu.Item>
    {/if}
    <ContextMenu.Item class="ctx-item" onSelect={() => onCopyUrl(task.id)}>Copy URL</ContextMenu.Item>
    {#if task.status === 'completed'}
      <ContextMenu.Item class="ctx-item" onSelect={() => onOpenSaved(task.id)}>
        Open file
      </ContextMenu.Item>
      <ContextMenu.Item class="ctx-item" onSelect={() => onOpenFile(task.id)}>
        Show in folder
      </ContextMenu.Item>
    {/if}
    <ContextMenu.Separator class="ctx-sep" />
    <ContextMenu.Item class="ctx-item danger" onSelect={() => onRemove(task.id)}>
      Remove
    </ContextMenu.Item>
  </ContextMenu.Content>
</ContextMenu.Root>

{#if selected}
  <SegmentPanel {task} {daemon} {onLimit} />
{/if}

<style>
  .row {
    position: relative;
    display: grid;
    grid-template-columns: 22px minmax(0, 1fr) 48px 118px 72px 56px 84px;
    align-items: center;
    column-gap: 10px;
    height: 40px;
    padding: 0 14px 0 10px;
    background: var(--panel);
    border-bottom: 1px solid var(--line-subtle);
    cursor: default;
    user-select: none;
    transition: background 0.12s ease;
  }
  .row:hover {
    background: var(--elevated);
  }
  .row:focus-visible {
    outline: none;
    box-shadow: inset 0 0 0 2px var(--accent);
  }
  .row.selected {
    box-shadow: inset 0 0 0 2px var(--accent);
  }
  .chev {
    color: var(--dim);
    font-size: 11px;
    line-height: 1;
    text-align: center;
    transition: transform 0.15s ease;
  }
  .chev.open {
    transform: rotate(90deg);
    color: var(--text);
  }
  .name {
    font-weight: 600;
    font-size: 13px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .name.errored {
    color: var(--err);
  }
  .col {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: 12px;
    color: var(--dim);
    text-align: right;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .row:hover .col,
  .row.selected .col {
    color: var(--text);
  }
  /* pct carries the row's status tint — the number itself becomes
   * the scan anchor (VLM V3 note), dimmer than the chip so the chip
   * still wins on priority */
  .col.pct {
    color: color-mix(in oklch, var(--accent) 70%, var(--dim));
    font-weight: 500;
  }
  .row[data-status='completed'] .col.pct {
    color: color-mix(in oklch, var(--ok) 70%, var(--dim));
  }
  .row[data-status='paused'] .col.pct {
    color: color-mix(in oklch, var(--warn) 70%, var(--dim));
  }
  .row[data-status='failed'] .col.pct {
    color: color-mix(in oklch, var(--err) 70%, var(--dim));
  }
  /* hover action cluster — fades over the tail columns */
  .cluster {
    position: absolute;
    right: 10px;
    top: 50%;
    transform: translateY(-50%);
    display: flex;
    gap: 6px;
    padding-left: 28px;
    background: linear-gradient(
      90deg,
      transparent,
      color-mix(in oklch, var(--elevated) 88%, transparent) 35%
    );
    opacity: 0;
    pointer-events: none;
    transition: opacity 0.12s ease;
  }
  .row:hover .cluster,
  .row:focus-within .cluster {
    opacity: 1;
    pointer-events: auto;
  }
  .cluster button {
    padding: 4px 7px;
    line-height: 0;
  }
  /* 3px progress underline (VLM: 2px was too light to scan —
   * the eye needs an anchor that says "downloading here") */
  .line {
    position: absolute;
    left: 0;
    right: 0;
    bottom: -1px; /* overlap the row border — the line IS the border while active */
    height: 3px;
    background: transparent;
    pointer-events: none;
  }
  .line i {
    display: block;
    height: 100%;
    background: var(--accent);
    transition: width 0.3s ease;
  }
  .row[data-status='completed'] .line i {
    background: var(--ok);
  }
  .row[data-status='paused'] .line i {
    background: var(--warn);
  }
  .row[data-status='failed'] .line i {
    background: var(--err);
  }
  .line i.anim {
    width: 30%;
    animation: row-slide 1.1s ease-in-out infinite;
  }
  @keyframes row-slide {
    0% {
      margin-left: -30%;
    }
    100% {
      margin-left: 100%;
    }
  }
</style>
