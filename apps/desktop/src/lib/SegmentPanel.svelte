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
  import { formatBytes, formatEta } from './format';

  let {
    task,
    daemon,
  }: {
    task: TaskView;
    daemon: Daemon;
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
</script>

<div class="detail">
  <div class="facts">
    <span class="kv"><b>ETA</b> {eta}</span>
    <span class="kv"><b>avg</b> {avgBps !== null ? `${formatBytes(avgBps)}/s` : '—'}</span>
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
