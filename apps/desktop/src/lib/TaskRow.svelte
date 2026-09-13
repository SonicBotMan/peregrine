<script lang="ts">
  /**
   * One task row: name, progress bar (or spinner for unknown size),
   * derived speed, status chip, throttle select, and the action
   * cluster (pause / resume / remove). Nearly dumb by design — all
   * list truth lives in the store; the row owns only its expand
   * toggle + throttle select state.
   */
  import SegmentPanel from './SegmentPanel.svelte';
  import { LIMIT_PRESETS, presetFor, formatBytes } from './format';
  import type { TaskView } from './store.svelte';

  let {
    task,
    daemon,
    onPause,
    onResume,
    onRemove,
    onLimit,
  }: {
    task: TaskView;
    daemon: import('./daemon').Daemon;
    onPause: (id: string) => void;
    onResume: (id: string) => void;
    onRemove: (id: string) => void;
    onLimit: (id: string, bps: number) => void;
  } = $props();

  let open = $state(false);
  let custom = $state<number | null>(null); // non-preset value being typed

  const name = $derived(task.url.split('/').filter(Boolean).pop() ?? task.url);
  const size = $derived(formatBytes(task.total_bytes));
  const done = $derived(formatBytes(task.received_bytes));
  const pct = $derived(task.fraction !== null ? Math.round(task.fraction * 100) : null);
  const statusClass = $derived(task.status);

  /** Select value: preset bps as string, or 'custom'. */
  const limitSel = $derived(
    custom !== null ? 'custom' : (presetFor(task.speed_limit_bps) ?? 'custom'),
  );

  function pickLimit(ev: Event) {
    const v = (ev.currentTarget as HTMLSelectElement).value;
    if (v === 'custom') {
      custom = task.speed_limit_bps; // start editing from current
      return;
    }
    custom = null;
    onLimit(task.id, Number(v));
  }

  function commitCustom() {
    if (custom !== null && Number.isFinite(custom) && custom >= 0) {
      onLimit(task.id, Math.round(custom));
    }
    custom = null;
  }
</script>

<div class="row" data-status={statusClass}>
  <div class="main">
    <div class="title">
      <span class="name" title={task.url}>{name}</span>
      <span class="path" title={task.save_path}>{task.save_path}</span>
    </div>

    <div class="progress" class:indeterminate={pct === null}>
      {#if pct !== null}
        <div class="bar" style:width="{pct}%"></div>
      {:else}
        <div class="bar anim"></div>
      {/if}
    </div>

    <div class="meta">
      <button
        class="chevron"
        class:open
        aria-expanded={open}
        aria-label="Toggle segment details"
        onclick={() => (open = !open)}
        title="Details"
      >
        {open ? '▾' : '▸'}
      </button>
      <span class="chip" data-kind={task.status}>{task.status}</span>
      <span>{done}{size !== '—' ? ` / ${size}` : ''}</span>
      {#if pct !== null}
        <span>{pct}%</span>
      {/if}
      {#if task.speed !== null && task.status === 'running'}
        <span>{formatBytes(task.speed)}/s</span>
      {/if}
      {#if task.error}
        <span class="err" title={task.error}>{task.error}</span>
      {/if}
      <span class="limit">
        {#if custom !== null}
          <input
            type="number"
            min="0"
            bind:value={custom}
            onblur={commitCustom}
            onkeydown={(e) => e.key === 'Enter' && commitCustom()}
            title="bytes/sec"
          />
        {:else}
          <select
            value={limitSel}
            onchange={pickLimit}
            title="Speed limit"
            aria-label={`Speed limit for ${name}`}
          >
            {#each LIMIT_PRESETS as p (p.bps)}
              <option value={String(p.bps)}>{p.label}</option>
            {/each}
            <option value="custom">custom…</option>
          </select>
        {/if}
        <span class="unit">B/s</span>
      </span>
    </div>
  </div>

  <div class="actions">
    {#if task.status === 'running' || task.status === 'queued'}
      <button onclick={() => onPause(task.id)} title="Pause">⏸</button>
    {/if}
    {#if task.status === 'paused' || task.status === 'failed'}
      <button onclick={() => onResume(task.id)} title="Resume">▶</button>
    {/if}
    <button class="danger" onclick={() => onRemove(task.id)} title="Remove">✕</button>
  </div>
</div>

{#if open}
  <SegmentPanel {task} {daemon} onLimit={onLimit} />
{/if}
