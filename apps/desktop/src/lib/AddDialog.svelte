<script lang="ts">
  /** Add-download dialog: URL, save path, priority. Validates the
   * same rules the daemon enforces (client-side hint only — the
   * daemon's 422/409 remains the authority). */
  let {
    onAdd,
    onClose,
    defaultDir,
  }: {
    onAdd: (url: string, savePath: string, priority: 'low' | 'normal' | 'high') => Promise<void>;
    onClose: () => void;
    defaultDir: string;
  } = $props();

  let url = $state('');
  let path = $state('');
  let priority = $state<'low' | 'normal' | 'high'>('normal');
  let error = $state<string | null>(null);
  let busy = $state(false);

  async function submit() {
    error = null;
    if (!/^https?:\/\//.test(url)) {
      error = 'URL must be http(s)';
      return;
    }
    if (!path.trim()) {
      error = 'Save path required';
      return;
    }
    busy = true;
    try {
      await onAdd(url.trim(), path.trim(), priority);
      onClose();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

<div class="overlay" onclick={onClose}>
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class="dialog" onclick={(e) => e.stopPropagation()} role="dialog" aria-label="Add download">
    <h2>Add download</h2>
    <label>
      URL
      <input bind:value={url} placeholder="https://…" autofocus />
    </label>
    <label>
      Save to
      <input bind:value={path} placeholder={defaultDir} />
    </label>
    <label>
      Priority
      <select bind:value={priority}>
        <option value="low">low</option>
        <option value="normal">normal</option>
        <option value="high">high</option>
      </select>
    </label>
    {#if error}
      <p class="error">{error}</p>
    {/if}
    <div class="btns">
      <button onclick={onClose}>Cancel</button>
      <button class="primary" onclick={submit} disabled={busy}>
        {busy ? 'Adding…' : 'Add'}
      </button>
    </div>
  </div>
</div>
