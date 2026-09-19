<script lang="ts">
  import { fileName } from './notify';
  import { composeSavePath } from './savepath';
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

  // Native directory picker (GUI-verify R2): desktop-only. A plain
  // web page cannot open an OS picker (security model), so the
  // button renders only inside the Tauri webview and the text
  // input stays the universal fallback — the browser/dev mode and
  // the CLI/MCP clients keep the same semantics.
  const canPick = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
  let picking = $state(false);
  /** True when the current `path` came from the OS picker — the
   * user chose a DIRECTORY, so submit must compose the filename
   * from the URL instead of using the dir as the file path. */
  let pickedDir = $state(false);
  async function browse() {
    if (picking) return;
    picking = true;
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const dir = await open({
        directory: true,
        multiple: false,
        defaultPath: path || defaultDir || undefined,
        title: 'Choose download directory',
      });
      if (typeof dir === 'string' && dir) {
        path = dir;
        pickedDir = true;
        localStorage.setItem(LAST_DIR_KEY, dir);
      }
    } catch {
      // Picker unavailable/cancelled: text input remains the path.
    } finally {
      picking = false;
    }
  }

  // Intentional snapshot: the dialog remounts per open ({#if}), so
  // $state(initialUrl) seeds from the CURRENT drop payload.
  // svelte-ignore state_referenced_locally
  let url = $state(initialUrl);
  // Prefill from the last dir, else the daemon's default — the
  // placeholder-only variant made every first add type the whole
  // path (GUI-verify R2).
  // Intentional snapshot (same rule as initialUrl above): the dialog
  // remounts per open, so seeding from the CURRENT defaultDir is the
  // point — it must not live-track a settings change mid-dialog.
  // svelte-ignore state_referenced_locally
  let path = $state(localStorage.getItem(LAST_DIR_KEY) ?? defaultDir);
  let priority = $state<'low' | 'normal' | 'high'>('normal');
  let error = $state<string | null>(null);
  let busy = $state(false);

  // Schemes the engine registry routes today (http/hls via
  // auto-router, ftp, magnet/bt, .torrent via file://). Anything
  // else would 422 at the daemon anyway.
  const URL_OK = /^(https?|ftps?|magnet|bt|file):/i;

  // Live preview of the composed save path (GUI-verify batch-1):
  // Save-to holds a FOLDER; the filename comes from the URL. Show
  // the exact file path the daemon will receive — no surprises
  // between the picker and the task list.
  const composedPreview = $derived.by(() => {
    const u = url.trim();
    const d = path.trim();
    if (!u || !d) return null;
    if (/^(magnet|bt):/i.test(u) || /\.torrent$/i.test(u)) return d;
    if (!/^(https?|ftps?|ftp|file):/i.test(u)) return null;
    const base = fileName(u);
    if (!base || base === u) return null;
    return `${d.replace(/\/+$/, '')}/${base}`;
  });

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
    // Directory + filename composition (GUI-verify R2): the daemon
    // takes a FILE path and rightly rejects one that is a directory
    // (os error 21). Save-to is a DESTINATION FOLDER — the filename
    // comes from the URL (IDM/browser convention), so the OS picker
    // and hand-typed folders compose identically. magnet/bt have no
    // URL filename: the engine treats the sink as a dir, pass it
    // through untouched.
    let savePath = path.trim();
    const fileNamed = /^(https?|ftps?|ftp|file):/i.test(url);
    if (fileNamed) {
      const base = fileName(url);
      if (!base || base === url) {
        error = 'URL has no filename — append one to the URL';
        return;
      }
      savePath = `${savePath.replace(/\/+$/, '')}/${base}`;
    }
    busy = true;
    try {
      await onAdd(url.trim(), savePath, priority);
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
      {#if canPick}
        <span class="path-row">
          <input bind:value={path} placeholder={defaultDir} />
          <button
            type="button"
            class="browse"
            title="Choose directory…"
            onclick={() => void browse()}
            disabled={picking}
          >
            {picking ? '…' : 'Browse…'}
          </button>
        </span>
      {:else}
        <input bind:value={path} placeholder={defaultDir} />
      {/if}
    </label>
    <label>
      Priority
      <select bind:value={priority}>
        <option value="low">low</option>
        <option value="normal">normal</option>
        <option value="high">high</option>
      </select>
    </label>
    {#if composedPreview}
      <p class="scheme-hint">→ {composedPreview}</p>
    {/if}
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
