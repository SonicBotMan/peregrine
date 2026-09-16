<script lang="ts">
  /**
   * Task detail (U2): facts row + per-task throttle (moved here
   * from the U1 row — the row stays pure data) + per-segment
   * telemetry grid (1s poll). Read-only by design — the daemon is
   * truth; this panel only reflects it (B37 contract: REST for
   * snapshots, events for list deltas; segment frames stay
   * pull-based).
   */
  import type { Daemon } from './daemon';
  import type { SegmentView } from './types';
  import type { TaskView } from './store.svelte';
  import { LIMIT_PRESETS, presetFor, formatBytes, formatEta } from './format';

  let {
    task,
    daemon,
    onLimit,
  }: {
    task: TaskView;
    daemon: Daemon;
    onLimit: (id: string, bps: number) => void;
  } = $props();

  let segments = $state<SegmentView[] | null>(null);
  let failed = $state(false);

  $effect(() => {
    // Poll while mounted; task.id dependency re-arms on row reuse.
    const id = task.id;
    let alive = true;
    const tick = async () => {
      try {
        const rows = await daemon.segments(id);
        if (alive) {
          segments = rows;
          failed = false;
        }
      } catch {
        if (alive) failed = true;
      }
    };
    void tick();
    const timer = setInterval(tick, 1000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  });

  const eta = $derived(
    task.total_bytes !== null && task.speed !== null && task.status === 'running'
      ? formatEta(task.total_bytes - task.received_bytes, task.speed)
      : '—',
  );

  const avgBps = $derived.by(() => {
    // created_at is epoch SECONDS (types.ts) — ms for Date.
    const secs = (Date.now() - task.created_at * 1000) / 1000;
    if (secs <= 0 || task.received_bytes <= 0) return null;
    return task.received_bytes / secs;
  });

  // ---- per-task throttle (moved from the U1 row, U2) ----------
  let custom = $state<number | null>(null); // non-preset value being typed

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

<div class="detail">
  <div class="facts">
    <span class="kv"><b>ETA</b> {eta}</span>
    <span class="kv"><b>avg</b> {avgBps !== null ? `${formatBytes(avgBps)}/s` : '—'}</span>
    <span class="kv limit">
      <b>limit</b>
      {#if custom !== null}
        <input
          class="ctl-input"
          type="number"
          min="0"
          bind:value={custom}
          onblur={commitCustom}
          onkeydown={(e) => e.key === 'Enter' && commitCustom()}
          title="bytes/sec"
        />
      {:else}
        <select
          class="ctl-select"
          value={limitSel}
          onchange={pickLimit}
          aria-label="Speed limit"
        >
          {#each LIMIT_PRESETS as p (p.bps)}
            <option value={String(p.bps)}>{p.label}</option>
          {/each}
          <option value="custom">custom…</option>
        </select>
      {/if}
    </span>
    <span class="kv"><b>url</b> <span class="url" title={task.url}>{task.url}</span></span>
    <span class="kv"><b>path</b> <span class="url" title={task.save_path}>{task.save_path}</span></span>
    {#if task.error}
      <span class="kv"><b>error</b> <span class="url" title={task.error}>{task.error}</span></span>
    {/if}
  </div>

  {#if failed}
    <p class="err">telemetry unavailable — retrying…</p>
  {:else if segments === null}
    <p class="muted">loading segment plan…</p>
  {:else if segments.length === 0}
    <p class="muted">single stream (no segment plan)</p>
  {:else}
    <div class="seggrid" role="list">
      {#each segments as s (s.idx)}
        <div
          class="seg"
          role="listitem"
          title={`#${s.idx} ${formatBytes(s.done)} / ${formatBytes(s.len)} · frontier ${s.frontier}`}
        >
          <span class="idx">#{s.idx}</span>
          <div class="bar"><div class="fill" style:width="{Math.round(s.pct * 100)}%"></div></div>
          <span class="pct">{Math.round(s.pct * 100)}%</span>
        </div>
      {/each}
    </div>
  {/if}
</div>
