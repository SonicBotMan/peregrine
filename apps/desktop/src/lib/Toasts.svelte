<script lang="ts">
  /**
   * Toast stack renderer (U3): bottom-right column above the status
   * bar. Reads the module-singleton toast channel; nothing to wire.
   * Undo buttons call back into the toast's closure (e.g. restore an
   * optimistically-removed row).
   */
  import { toast } from './toast.svelte';
  import { X } from '@lucide/svelte';
</script>

{#if toast.list.length > 0}
  <div class="toasts" role="status" aria-live="polite">
    {#each toast.list as t (t.id)}
      <div class="toast">
        <span class="msg">{t.msg}</span>
        {#if t.undo}
          <button class="undo" onclick={() => toast.runUndo(t.id)}>Undo</button>
        {/if}
        <button class="x" aria-label="Dismiss" onclick={() => toast.dismiss(t.id)}>
          <X size={13} />
        </button>
      </div>
    {/each}
  </div>
{/if}

<style>
  .toasts {
    position: fixed;
    right: 14px;
    bottom: 46px; /* above the 36px status bar */
    display: flex;
    flex-direction: column;
    gap: 8px;
    z-index: 60;
    max-width: min(420px, calc(100vw - 28px));
  }
  .toast {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px 8px 14px;
    background: var(--elevated);
    border: 1px solid var(--line-strong);
    border-radius: 8px;
    /* VLM V3 note: must read as floating ABOVE the list, not part of
     * it — stronger shadow + darker translucent lift */
    box-shadow:
      0 12px 32px rgb(0 0 0 / 0.45),
      0 3px 8px rgb(0 0 0 / 0.3);
    font-size: 12.5px;
    color: var(--text);
    animation: toast-in var(--dur, 200ms) var(--ease, ease-out);
  }
  .msg {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  button {
    all: unset;
    cursor: pointer;
    font-size: 12px;
    border-radius: 5px;
    padding: 2px 8px;
    transition:
      background var(--dur, 200ms) var(--ease, ease-out),
      color var(--dur, 200ms) var(--ease, ease-out);
  }
  .undo {
    color: var(--accent);
    font-weight: 600;
  }
  .undo:hover {
    background: var(--accent);
    color: var(--bg);
  }
  .x {
    color: var(--dim);
    padding: 2px 6px;
    display: inline-flex;
  }
  .x:hover {
    color: var(--text);
    background: var(--line);
  }
  @keyframes toast-in {
    from {
      opacity: 0;
      transform: translateY(6px);
    }
  }
</style>
