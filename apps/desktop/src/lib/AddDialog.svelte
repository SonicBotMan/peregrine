<script lang="ts">
  /** Add-download dialog: URL, save path, priority. Validates the
   * same schemes the daemon's auto-router accepts (client-side
   * hint only — the daemon's 422/409 remains the authority).
   * U2: `initialUrl` prefills from drag-and-drop; the last save
   * dir persists in localStorage so the second add is one field
   * shorter. */
  let {
    onAdd,
    onClose,
    defaultDir,
    initialUrl = '',
  }: {
    onAdd: (url: string, savePath: string, priority: 'low' | 'normal' | 'high') => Promise<void>;
    onClose: () => void;
    defaultDir: string;
    initialUrl?: string;
  } = $props();

  const LAST_DIR_KEY = 'peregrine-last-dir';

  // Intentional snapshot: the dialog remounts per open ({#if}), so
  // $state(initialUrl) seeds from the CURRENT drop payload.
  // svelte-ignore state_referenced_locally
  let url = $state(initialUrl);
  let path = $state(localStorage.getItem(LAST_DIR_KEY) ?? '');
  let priority = $state<'low' | 'normal' | 'high'>('normal');
  let error = $state<string | null>(null);
  let busy = $state(false);

  // Schemes the engine registry routes today (http/hls via
  // auto-router, ftp, magnet/bt, .torrent via file://). Anything
  // else would 422 at the daemon anyway.
  const URL_OK = /^(https?|ftps?|magnet|bt|file):/i;

  async function submit() {
    error = null;
    if (!URL_OK.test(url)) {
      error = 'URL must be http(s), ftp, magnet:, bt: or file:';
      return;
    }
    if (!path.trim()) {
      error = 'Save path required';
      return;
    }
    busy = true;
    try {
      await onAdd(url.trim(), path.trim(), priority);
      localStorage.setItem(LAST_DIR_KEY, path.trim());
      onClose();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

<!-- Modal open ⇒ window-level Escape closes (U2 keyboard floor;
component mounts/unmounts with the dialog, so no leak). -->
<svelte:window onkeydown={(e) => e.key === 'Escape' && onClose()} />

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="overlay" onclick={onClose}>
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div
    class="dialog"
    onclick={(e) => e.stopPropagation()}
    role="dialog"
    aria-label="Add download"
    tabindex={-1}
  >
    <h2>Add download</h2>
    <label>
      URL
      <input bind:value={url} placeholder="https://… · magnet:?xt=… · ftp://…" autofocus />
      <p class="scheme-hint">HTTP/HTTPS, BitTorrent (magnet: or .torrent URL), FTP — the daemon routes by scheme.</p>
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
      <button class="ctl" onclick={onClose}>Cancel</button>
      <button class="ctl primary" onclick={submit} disabled={busy}>
        {busy ? 'Adding…' : 'Add'}
      </button>
    </div>
  </div>
</div>
