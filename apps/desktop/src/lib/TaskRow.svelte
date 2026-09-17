<script lang="ts">
  /**
   * One task card — Motrix TaskItem replication: progress ring
   * (status-colored SVG circle), file name + meta line, hover-
   * revealed round action chips (#4a4a4a, primary on hover).
   * Behavior surface is unchanged from the tiled-row era: click /
   * Enter toggles the inline SegmentPanel expansion, Space belongs
   * to the global layer, double-click a completed row opens the
   * artifact, right-click opens the context menu. All list truth
   * lives in the store — the card stays dumb.
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

  // ring geometry: 46px SVG, r=19 → C = 2πr ≈ 119.4
  const R = 19;
  const C = 2 * Math.PI * R;
  const dash = $derived(
    pct !== null ? `${(pct / 100) * C} ${C - (pct / 100) * C}` : `0 ${C}`,
  );

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
        class="row"
        class:selected
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
        <!-- Motrix TaskProgress: circular ring, status-colored -->
        <span class="ring" data-status={task.status} aria-hidden="true">
          <svg viewBox="0 0 46 46" width="46" height="46">
            <circle class="track" cx="23" cy="23" r={R} />
            <circle
              class="bar"
              cx="23"
              cy="23"
              r={R}
              stroke-dasharray={dash}
              transform="rotate(-90 23 23)"
            />
          </svg>
          <span class="ring-num">
            {#if task.status === 'completed'}
              <span class="check">✓</span>
            {:else if task.status === 'failed'}
              <span class="bang">!</span>
            {:else if pct !== null}
              {pct}<small>%</small>
            {:else}
              …
            {/if}
          </span>
        </span>

        <span class="info">
          <span class="name">{name}</span>
          {#if task.error}
            <!-- VLM: keep the title neutral white — status color lives
                 in the meta line only, or a failed list turns into a
                 wall of red -->
            <span class="meta">
              <b class="st">{task.status}</b>
              <span class="err-line">{task.error}</span>
            </span>
          {:else}
            <span class="meta">
              <b class="st">{task.status}</b>
              <span class="num">{done}{size !== '—' ? ` / ${size}` : ''}</span>
              <span class="dot">·</span>
              <span class="num">{speed}</span>
              <span class="dot">·</span>
              <span class="num">{eta}</span>
            </span>
          {/if}
        </span>

        <span class="cluster" role="group" aria-label="Row actions">
          {#if running}
            <button
              class="ctl round"
              title="Pause"
              aria-label={`Pause ${name}`}
              onclick={stop(() => onPause(task.id))}
            >
              <Pause size={13} />
            </button>
          {:else if resumable}
            <button
              class="ctl round"
              title={task.status === 'failed' ? 'Retry' : 'Resume'}
              aria-label={`${task.status === 'failed' ? 'Retry' : 'Resume'} ${name}`}
              onclick={stop(() => onResume(task.id))}
            >
              <Play size={13} />
            </button>
          {/if}
          <button
            class="ctl round"
            title="Remove"
            aria-label={`Remove ${name}`}
            onclick={stop(() => onRemove(task.id))}
          >
            <X size={13} />
          </button>
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
  /* Motrix task-item card: #2d2d2d tile, #555 border, 4px radius,
   * separated by the #343434 main background. */
  .row {
    position: relative;
    display: grid;
    grid-template-columns: 46px minmax(0, 1fr) auto;
    align-items: center;
    column-gap: 14px;
    min-height: 64px;
    padding: 9px 16px;
    margin: 8px 12px;
    background: var(--panel);
    border: 1px solid var(--line-strong);
    border-radius: 4px;
    cursor: default;
    user-select: none;
  }
  .row:hover {
    border-color: var(--accent);
  }
  .row:focus-visible {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }
  .row.selected {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }

  /* progress ring */
  .ring {
    position: relative;
    width: 46px;
    height: 46px;
    flex: none;
  }
  .ring svg {
    display: block;
  }
  .ring .track {
    fill: none;
    stroke: var(--line-subtle);
    stroke-width: 3.5;
  }
  .ring .bar {
    fill: none;
    stroke: var(--accent);
    stroke-width: 3.5;
    stroke-linecap: round;
    transition: stroke-dasharray 0.4s linear;
  }
  .ring[data-status='completed'] .bar {
    stroke: var(--ok);
  }
  .ring[data-status='paused'] .bar {
    stroke: var(--warn);
  }
  .ring[data-status='failed'] .bar {
    stroke: var(--err);
  }
  .ring[data-status='queued'] .bar {
    stroke: var(--info);
  }
  .ring-num {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 11px;
    font-weight: 600;
    color: var(--text);
    font-variant-numeric: tabular-nums;
  }
  .ring-num small {
    font-size: 7px;
    font-weight: 500;
    margin-left: 1px;
  }
  .ring-num .check {
    color: var(--ok);
    font-size: 15px;
  }
  .ring-num .bang {
    color: var(--err);
    font-size: 15px;
    font-weight: 700;
  }

  /* info column */
  .info {
    display: grid;
    gap: 4px;
    min-width: 0;
  }
  .name {
    font-weight: 600;
    font-size: 14px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .err-line {
    /* lift toward text so the reason is readable, not just tinted
     * (VLM pass 5: raw --err on near-black was ~4.3:1) */
    color: color-mix(in srgb, var(--err) 62%, var(--text));
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meta {
    display: flex;
    align-items: baseline;
    gap: 8px;
    font-size: 12px;
    color: var(--dim);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .meta .st {
    font-weight: 500;
    text-transform: capitalize;
  }
  /* three-step grey ramp inside the meta line (VLM pass 3):
   * status = status color, numbers = full text, separators = line */
  .meta .num {
    color: var(--text);
    font-variant-numeric: tabular-nums;
  }
  .row[data-status='running'] .meta .st,
  .row[data-status='queued'] .meta .st {
    color: var(--accent);
  }
  .row[data-status='completed'] .meta .st {
    color: var(--ok);
  }
  .row[data-status='paused'] .meta .st {
    color: var(--warn);
  }
  .row[data-status='failed'] .meta .st {
    color: var(--err);
  }
  .meta .dot {
    color: var(--line);
  }
  /* failed rows carry a 3px red rail + faint tint so a failure is
   * locatable by scan alone, no sidebar count needed (VLM final) */
  .row[data-status='failed'] {
    box-shadow: inset 3px 0 0 var(--err);
    background: color-mix(in srgb, var(--err) 4%, var(--panel));
  }

  /* hover action cluster — Motrix round chips */
  .cluster {
    display: flex;
    gap: 8px;
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--dur) var(--ease);
  }
  .row:hover .cluster,
  .row:focus-within .cluster,
  .row.selected .cluster {
    opacity: 1;
    pointer-events: auto;
  }
  .ctl.round {
    width: 30px;
    height: 30px;
    justify-content: center;
    padding: 0;
    border-radius: 50%;
    background: var(--elevated);
    border: 1px solid var(--line);
    color: var(--text);
    line-height: 0;
  }
  .ctl.round:hover {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
</style>
