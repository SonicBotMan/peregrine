<script lang="ts">
  /**
   * V5 detail drawer (bottom pane, mock spec): three tabs —
   * Segments / Speed / Info. Info is a key/value sheet (File, URL,
   * Total size, Downloaded, Speed, ETA, Segments, Status); Speed is
   * a live sparkline over the task's recent speed history; Segments
   * reuses SegmentPanel verbatim.
   */
  import SegmentPanel from './SegmentPanel.svelte';
  import { LIMIT_PRESETS, presetFor, formatBytes, formatBps, formatEta } from './format';
  import type { TaskView } from './store.svelte';
  import type { Daemon } from './daemon';
  import { X } from '@lucide/svelte';
  

  let {
    task,
    daemon,
    onLimit,
    onClose,
  }: {
    task: TaskView;
    daemon: Daemon;
    onLimit: (id: string, bps: number) => void;
    onClose: () => void;
  } = $props();

  type Tab = 'segments' | 'speed' | 'info';
  let tab: Tab = $state('segments');

  function switchTab(id: string) {
    if (id === 'segments' || id === 'speed' || id === 'info') tab = id;
  }

  const name = $derived(task.url.split('/').filter(Boolean).pop() ?? task.url);
  const info = $derived([
    ['File', name],
    ['URL', task.url],
    ['Total size', task.total_bytes !== null ? formatBytes(task.total_bytes) : 'unknown'],
    ['Downloaded', formatBytes(task.received_bytes)],
    ['Speed', task.speed !== null ? formatBps(task.speed) : '—'],
    [
      'ETA',
      task.status === 'running' &&
        task.total_bytes !== null &&
        task.speed !== null &&
        task.speed > 0
        ? formatEta(task.total_bytes - task.received_bytes, task.speed)
        : '—',
    ],
    ['Priority', task.priority],
    ['Speed limit', task.speed_limit_bps > 0 ? formatBps(task.speed_limit_bps) : 'unlimited'],
    ['Status', task.status],
  ]);

  // Speed tab: sparkline over the row's recent speed samples kept
  // by the store (same window the Speed column derives from).
  const samples = $derived(task.speed_history ?? []);
  const W = 260;
  const H = 46;
  const points = $derived.by(() => {
    if (samples.length < 2) return '';
    const max = Math.max(...samples, 1);
    const step = W / (samples.length - 1);
    return samples
      .map((v, i) => `${(i * step).toFixed(1)},${(H - (v / max) * (H - 4) - 2).toFixed(1)}`)
      .join(' ');
  });
</script>

<aside class="drawer" aria-label={`Details for ${name}`}>
  <header class="dstrip">
    <nav class="dtabs" role="tablist">
      {#each [['segments', 'Segments'], ['speed', 'Speed graph'], ['info', 'Info']] as [id, label] (id)}
        <button
          class="dtab"
          role="tab"
          aria-selected={tab === id}
          class:on={tab === id}
          onclick={() => switchTab(id)}
        >
          {label}
        </button>
      {/each}
    </nav>
    <span class="dname" title={name}>{name}</span>
    <select
      class="dlimit"
      title="Per-task speed limit"
      aria-label="Per-task speed limit"
      value={presetFor(task.speed_limit_bps) ?? 'custom'}
      onchange={(e) => onLimit(task.id, Number(e.currentTarget.value))}
    >
      {#each LIMIT_PRESETS as p (p.label)}
        <option value={p.bps}>{p.bps === 0 ? 'no limit' : p.label}</option>
      {/each}
      {#if presetFor(task.speed_limit_bps) === null}
        <option value={task.speed_limit_bps} selected>{formatBps(task.speed_limit_bps)}/s</option>
      {/if}
    </select>
    <button class="dclose" title="Close (Esc)" aria-label="Close details" onclick={onClose}>
      <X size={13} />
    </button>
  </header>

  <div class="dbody" role="tabpanel">
    {#if tab === 'segments'}
      <SegmentPanel {task} {daemon} />
    {:else if tab === 'speed'}
      <div class="speedbox">
        {#if points}
          <svg viewBox="0 0 {W} {H}" width="{W}" height="{H}" preserveAspectRatio="none" aria-hidden="true">
            <polyline class="fill" points="{points} {W},{H} 0,{H}" />
            <polyline class="line" points={points} />
          </svg>
          <span class="now num">{task.speed !== null ? formatBps(task.speed) : '—'}</span>
        {:else}
          <span class="hint">collecting speed samples…</span>
        {/if}
      </div>
    {:else}
      <dl class="kv">
        {#each info as [k, v] (k)}
          <div class="pair">
            <dt>{k}</dt>
            <dd class:mono={k === 'URL' || k === 'File'} title={v}>{v}</dd>
          </div>
        {/each}
      </dl>
    {/if}
  </div>
</aside>

<style>
  .drawer {
    flex: none;
    max-height: 220px;
    display: flex;
    flex-direction: column;
    border-top: 1px solid var(--line-strong);
    background: var(--chrome-1);
  }
  .dstrip {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 0 12px;
    height: 30px;
    border-bottom: 1px solid var(--line-subtle);
    background: var(--chrome-2);
    flex: none;
  }
  .dtabs {
    display: flex;
    gap: 2px;
    height: 100%;
  }
  .dtab {
    appearance: none;
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    padding: 0 10px;
    height: 100%;
    font: 600 11px var(--font-sans);
    color: var(--dim);
    cursor: pointer;
  }
  .dtab:hover { color: var(--text); }
  .dtab.on {
    color: var(--accent);
    border-bottom-color: var(--accent);
  }
  .dname {
    flex: 1;
    min-width: 0;
    text-align: right;
    font: 500 11.5px var(--font-sans);
    color: var(--dim);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dlimit {
    font-size: 11px;
    color: var(--dim);
    background: var(--elevated);
    border: 1px solid var(--line);
    border-radius: 4px;
    padding: 2px 4px;
    flex: none;
    max-width: 110px;
  }
  .dlimit:hover { color: var(--text); border-color: var(--line-strong); }
  .dclose {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    border-radius: 4px;
    border: 1px solid transparent;
    background: none;
    color: var(--dim);
    cursor: pointer;
  }
  .dclose:hover {
    background: var(--elevated);
    border-color: var(--line-strong);
    color: var(--text);
  }
  .dbody {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 8px 12px;
  }
  .speedbox {
    display: flex;
    align-items: baseline;
    gap: 12px;
    height: 100%;
  }
  .line {
    fill: none;
    stroke: var(--accent);
    stroke-width: 1.5;
  }
  .fill {
    fill: color-mix(in srgb, var(--accent) 14%, transparent);
    stroke: none;
  }
  .now { font-size: 13px; color: var(--text); }
  .hint { color: var(--dim); font-size: 12px; }
  .kv {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(230px, 1fr));
    gap: 2px 28px;
    margin: 0;
  }
  .pair {
    display: flex;
    align-items: baseline;
    gap: 12px;
    border-bottom: 1px dotted var(--line-subtle);
    padding: 2px 0;
  }
  .kv dt {
    color: var(--dim);
    font-size: 11px;
    flex: none;
    width: 86px;
  }
  .kv dd {
    margin: 0;
    font-size: 12px;
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .kv dd.mono {
    font-family: var(--font-mono);
    font-size: 11px;
  }
</style>
