<script lang="ts">
  /**
   * Global throughput strip — the "watch the pipe" anchor
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
  <span class="stat num" title="Aggregate download speed"
    ><i class="arrow" aria-hidden="true">↓</i> {formatBytes(totalSpeed)}/s</span
  >
  <span class="sep"></span>
  <span class="stat num">{active} active</span>
  {#if failed > 0}
    <span class="sep"></span>
    <span class="stat num danger">{failed} failed</span>
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
    height: 32px;
    padding: 0 16px;
    background: var(--subnav);
    border-bottom: 1px solid var(--line-strong);
    font-size: 12px;
    color: var(--dim);
  }
  /* disconnected: the whole strip turns amber — impossible to miss,
     list stays frozen-but-visible below (ui-proposal §5) */
  .statusbar.down {
    background: color-mix(in srgb, var(--warn) 14%, var(--subnav));
    color: var(--warn);
  }
  .stat {
    color: var(--text);
  }
  /* live downlink arrow reads green — the "pipe is flowing" cue
   * (VLM pass 3: neutral grey arrow looked dead) */
  .arrow {
    font-style: normal;
    color: var(--ok);
    font-weight: 700;
  }
  .stat.danger {
    color: var(--err);
    /* weak red chip so a failed count is a first-class alarm,
     * not just tinted text (VLM pass 3). Same 2px vertical padding
     * as the .conn pill — keeps one baseline across the strip
     * (VLM pass 6 flagged the 1px/2px mismatch). */
    background: color-mix(in srgb, var(--err) 14%, transparent);
    border-radius: 4px;
    padding: 2px 7px;
  }
  .statusbar.down .stat {
    color: var(--warn);
  }
  .sep {
    width: 1px;
    height: 14px;
    background: var(--line-subtle);
  }
  .conn {
    margin-left: auto;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    /* pill so the live indicator holds its own weight against the
     * stat cluster instead of floating orphaned (VLM pass 4) */
    border: 1px solid color-mix(in srgb, currentColor 35%, transparent);
    border-radius: 999px;
    padding: 2px 10px;
  }
  .conn[data-kind='live'] {
    color: var(--ok);
  }
  .conn[data-kind='down'] {
    color: var(--err);
  }
</style>
