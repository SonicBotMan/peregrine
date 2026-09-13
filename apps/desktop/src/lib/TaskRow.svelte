<script lang="ts">
  /**
   * One task row: name, progress bar (or spinner for unknown size),
   * derived speed, status chip, and the action cluster (pause /
   * resume / remove). Dumb by design — all truth lives in the store.
   */
  import type { TaskView } from '../lib/store.svelte';

  let { task, onPause, onResume, onRemove }: {
    task: TaskView;
    onPause: (id: string) => void;
    onResume: (id: string) => void;
    onRemove: (id: string) => void;
  } = $props();

  const name = $derived(task.url.split('/').filter(Boolean).pop() ?? task.url);
  const size = $derived(formatBytes(task.total_bytes));
  const done = $derived(formatBytes(task.received_bytes));
  const pct = $derived(task.fraction !== null ? Math.round(task.fraction * 100) : null);

  function formatBytes(n: number | null): string {
    if (n === null) return '—';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let v = n;
    let i = 0;
    while (v >= 1024 && i < units.length - 1) {
      v /= 1024;
      i++;
    }
    return `${v >= 100 || i === 0 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
  }

  const statusClass = $derived(task.status);
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
