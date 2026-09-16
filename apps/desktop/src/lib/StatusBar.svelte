<script lang="ts">
  /**
   * Global throughput strip (U1) — the "watch the pipe" anchor
   * (ui-proposal §2: speed is the heartbeat, always visible).
   * Mono tabular numerals so live values never jitter width.
   */
  import { formatBytes } from './format';

  let {
    totalSpeed,
    active,
    failed,
    conn,
  }: {
    totalSpeed: number;
    active: number;
    failed: number;
    conn: 'connecting' | 'live' | 'down';
  } = $props();

  const connLabel = $derived(
    conn === 'live' ? 'connected' : conn === 'connecting' ? 'connecting…' : 'disconnected',
  );
</script>

<div class="statusbar" class:down={conn === 'down'}>
  <span class="stat num" title="Aggregate download speed">↓ {formatBytes(totalSpeed)}/s</span>
  <span class="sep"></span>
  <span class="stat num">{active} active</span>
  {#if failed > 0}
    <span class="sep"></span>
    <span class="stat num text-err">{failed} failed</span>
  {/if}
  <span class="conn" data-kind={conn}>
    {conn === 'live' ? '●' : conn === 'connecting' ? '◌' : '✕'} {connLabel}
  </span>
</div>

<style>
  .statusbar {
    display: flex;
    align-items: center;
    gap: 14px;
    height: 36px;
    padding: 0 14px;
    background: var(--panel);
    border-bottom: 1px solid var(--line-strong);
    font-size: 12px;
    color: var(--dim);
  }
  /* disconnected: the whole strip turns amber — impossible to miss,
     list stays frozen-but-visible below (ui-proposal §5) */
  .statusbar.down {
    background: color-mix(in oklch, var(--warn) 12%, var(--panel));
    color: var(--warn);
  }
  .stat {
    color: var(--text);
  }
  .statusbar.down .stat {
    color: var(--warn);
  }
  .sep {
    width: 1px;
    height: 14px;
    background: var(--line);
  }
  .conn {
    margin-left: auto;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .conn[data-kind='live'] {
    color: var(--ok);
  }
  .conn[data-kind='down'] {
    color: var(--err);
  }
</style>
