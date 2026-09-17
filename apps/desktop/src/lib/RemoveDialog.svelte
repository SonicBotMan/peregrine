<script lang="ts">
  /**
   * Remove-semantics dialog (R2-gap "delete is ambiguous"): removing
   * a row and deleting the user's file are DIFFERENT acts, and
   * conflating them is how download managers lose trust. Terminal
   * tasks offer "delete the finished file"; active ones offer
   * "delete the partial file". Either way the file checkbox is OFF
   * by default — data loss must be opted into, never defaulted.
   *
   * "Don't ask again" pins the no-delete path (localStorage); the
   * delete-file path always asks (irreversible, one misclick too
   * many). Purged removals skip the undo toast: undoing a deletion
   * that already shredded the file would be a false promise.
   */
  let {
    name,
    terminal,
    onConfirm,
    onClose,
  }: {
    name: string;
    terminal: boolean;
    onConfirm: (deleteFile: boolean, neverAsk: boolean) => void;
    onClose: () => void;
  } = $props();

  let deleteFile = $state(false);
  let neverAsk = $state(false);
  // Keyboard parity with the main table (GUI-verify R2 P3): the
  // primary action answers to Enter, Escape already closes. Guarded
  // against double-fire because buttons also receive Enter when
  // focused — the confirm is idempotent-safe but the toast isn't.
  let done = $state(false);
  function confirmNow() {
    if (done) return;
    done = true;
    onConfirm(deleteFile, neverAsk);
  }
</script>

<svelte:window
  onkeydown={(e) => {
    if (e.key === 'Escape') onClose();
    else if (e.key === 'Enter' && !(e.target instanceof HTMLButtonElement)) confirmNow();
  }}
/>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="overlay" onclick={onClose}>
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div
    class="dialog"
    onclick={(e) => e.stopPropagation()}
    role="alertdialog"
    aria-label="Remove download"
    tabindex={-1}
  >
    <h2>Remove “{name}”?</h2>
    <label class="opt">
      <input type="checkbox" bind:checked={deleteFile} />
      {terminal ? 'Also delete the downloaded file on disk' : 'Also delete the partial file on disk'}
    </label>
    {#if deleteFile}
      <p class="warn">The file is deleted permanently — this cannot be undone.</p>
    {/if}
    <label class="opt">
      <input type="checkbox" bind:checked={neverAsk} disabled={deleteFile} />
      Don't ask again (always remove without deleting files)
    </label>
    <div class="btns">
      <button class="ctl" onclick={onClose}>Cancel</button>
      <button
        class="ctl"
        class:danger={deleteFile}
        class:primary={!deleteFile}
        onclick={() => confirmNow()}
      >
        {deleteFile ? 'Remove + delete file' : 'Remove'}
      </button>
    </div>
  </div>
</div>
