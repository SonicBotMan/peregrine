<script lang="ts">
  /**
   * V5 statusbar: bottom strip like a native app — speed heartbeat,
   * counters, daemon conn pill, GLOBAL speed limit selector (moved
   * here from the old sidebar — a pipe-level control belongs at the
   * pipe), theme toggle, version tag.
   */
  import { formatBytes, LIMIT_PRESETS } from './format';
  import { ArrowDown, Circle, CircleDashed, Unplug, Sun, Moon } from '@lucide/svelte';

  let {
    totalSpeed,
    speedHist = [],
    active,
    paused,
    done,
    failed,
    conn,
    globalLimit,
    onGlobalLimit,
    theme,
    onToggleTheme,
    version,
  }: {
    totalSpeed: number;
    /** ~2 min of aggregate-speed samples (App samples on the
     * derived totalSpeed, ~1/s). Empty on first paint — the
     * sparkline is hidden until it has a story to tell. */
    speedHist?: number[];
    active: number;
    paused: number;
    done: number;
    failed: number;
    conn: 'connecting' | 'live' | 'down';
    globalLimit: number | null;
    onGlobalLimit: (bps: number | null) => void;
    theme: 'dark' | 'light';
    onToggleTheme: () => void;
    version: string;
  } = $props();

  const connLabel = $derived(
    conn === 'live' ? 'connected' : conn === 'connecting' ? 'connecting…' : 'disconnected',
  );
  const currentPreset = $derived(
    globalLimit === null
      ? '∞'
      : (LIMIT_PRESETS.find((p) => p.bps === globalLimit)?.label ?? `${formatBytes(globalLimit)}/s`),
  );

  // 72x14 sparkline path from the samples (right-aligned: oldest
  // clips out the left edge — the NOW edge never moves).
  const spark = $derived.by(() => {
    if (speedHist.length < 4) return null;
    const pts = speedHist.slice(-120);
    const max = Math.max(...pts, 1);
    const n = pts.length;
    return pts
      .map((v, i) => `${(i / (n - 1)) * 72},${14 - (v / max) * 13}`)
      .join(' ');
  });
</script>

<div class="statusbar" class:down={conn === 'down'}>
  <span class="stat num" title="Aggregate download speed">
    <i class="arrow" aria-hidden="true"><ArrowDown size={12} strokeWidth={2.5} /></i>
    {formatBytes(totalSpeed)}/s
  </span>
  {#if spark}
    <svg
      class="spark"
      viewBox="0 0 72 14"
      width="72"
      height="14"
      preserveAspectRatio="none"
      aria-hidden="true"
    >
      <title>Aggregate speed, last ~2 minutes</title>
      <polyline points={spark} fill="none" stroke="var(--accent)" stroke-width="1.25" />
    </svg>
  {/if}
  <span class="sep"></span>
  <span class="stat num">{active} active</span>
  {#if paused > 0}
    <span class="sep"></span>
    <span class="stat num">{paused} paused</span>
  {/if}
  {#if done > 0}
    <span class="sep"></span>
    <span class="stat num">{done} done</span>
  {/if}
  {#if failed > 0}
    <span class="sep"></span>
    <span class="stat num danger">{failed} failed</span>
  {/if}

  <span class="grow"></span>

  <label class="glim" title="Global speed limit">
    Speed Limit
    <select
      class="glim-select"
      value={globalLimit === null ? 'none' : String(globalLimit)}
      onchange={(e) => {
        const v = e.currentTarget?.value;
        onGlobalLimit(!v || v === 'none' ? null : Number(v));
      }}
    >
      {#each LIMIT_PRESETS as p (p.label)}
        <option value={p.bps}>{p.label}</option>
      {/each}
      <option value="none">Unlimited</option>
    </select>
  </label>

  <button class="ghost" onclick={onToggleTheme} title="Toggle theme (⌘T)" aria-label="Toggle theme">
    {#if theme === 'dark'}<Sun size={12} />{:else}<Moon size={12} />{/if}
  </button>

  <span class="conn" data-kind={conn}>
    {#if conn === 'live'}
      <Circle size={8} strokeWidth={0} fill="currentColor" aria-hidden="true" />
    {:else if conn === 'connecting'}
      <CircleDashed size={11} aria-hidden="true" />
    {:else}
      <Unplug size={11} aria-hidden="true" />
    {/if}
    {connLabel}
  </span>

  <span class="ver">Peregrine {version}</span>
</div>

<style>
  .statusbar {
    display: flex;
    align-items: center;
    gap: 12px;
    height: 28px;
    padding: 0 12px;
    background: var(--chrome-1);
    border-top: 1px solid var(--edge);
    font-size: 11px;
    color: var(--dim);
    flex: none;
  }
  .statusbar.down {
    background: color-mix(in srgb, var(--warn) 14%, var(--chrome-1));
    color: var(--warn);
  }
  .stat {
    color: var(--text);
    font-variant-numeric: tabular-nums;
    /* flex so the inline ↓ arrow and the ::before danger dot share
     * one optical centerline with the digits — text-flow baseline
     * alignment left them 1-2px off (VLM audit). */
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  /* aggregate-speed sparkline: sits right of the live number so
   * the digit is the now-value and the line is the trend. */
  .spark {
    flex: none;
    opacity: 0.9;
  }

  .arrow {
    font-style: normal;
    color: var(--ok);
    display: inline-flex;
    vertical-align: -2px;
  }
  .stat.danger {
    /* quiet variant: no pill chrome — a red dot + red text reads
     * 'failed' at statusbar density without becoming a siren. */
    color: var(--err);
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .stat.danger::before {
    content: '';
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentColor;
  }
  .statusbar.down .stat {
    color: var(--warn);
  }
  .sep {
    width: 1px;
    height: 13px;
    background: var(--line-subtle);
    flex: none;
  }
  .grow { flex: 1; }

  .glim {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    font-size: 11px;
    color: var(--dim);
  }
  .glim-select {
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 4px;
    padding: 2px 6px;
    font-size: 11px;
    color: var(--text);
    max-width: 110px;
  }
  .ghost {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    padding: 0;
    border-radius: 4px;
    border: 1px solid transparent;
    background: none;
    color: var(--dim);
    cursor: pointer;
  }
  .ghost:hover {
    background: var(--elevated);
    border-color: var(--line-strong);
    color: var(--text);
  }
  .conn {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    border: 1px solid color-mix(in srgb, currentColor 35%, transparent);
    border-radius: 999px;
    padding: 2px 9px;
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .conn[data-kind='live'] {
    color: var(--ok);
  }
  .conn[data-kind='down'] {
    color: var(--err);
  }
  .ver {
    font-size: 10.5px;
    color: var(--dim);
    opacity: 0.75;
  }
</style>
