<script lang="ts">
  /**
   * App shell (U1/U2): inverted-L layout (ui-proposal §5) —
   *   sidebar (filters + global limit) | toolbar / statusbar / main
   * Flat tiled grid, sharp edges, zero gaps; the task list owns the
   * pixel budget. All task truth stays in the runes store; this
   * component wires filters + banner + dialog + selection + the
   * window-wide URL drop zone (U2: drop = first add entry).
   */
  import { Daemon, EventStream, detectBases } from './lib/daemon';
  import { createStore, type TaskStore } from './lib/store.svelte';
  import TaskRow from './lib/TaskRow.svelte';
  import AddDialog from './lib/AddDialog.svelte';
  import Sidebar, { type StatusFilter } from './lib/Sidebar.svelte';
  import Toolbar from './lib/Toolbar.svelte';
  import StatusBar from './lib/StatusBar.svelte';
  import { categorize, type Category } from './lib/categorize';
  import { initialTheme, applyTheme, saveTheme, type Theme } from './lib/theme';
  import type { Conn } from './lib/store.svelte';
  import { notifyCompleted } from './lib/notify';
  import { revealSaved, openSaved } from './lib/open';
  import Toasts from './lib/Toasts.svelte';
  import CommandPalette from './lib/CommandPalette.svelte';
  import ShortcutsDialog from './lib/ShortcutsDialog.svelte';
  import { toast } from './lib/toast.svelte';

  // Runtime-dependent endpoints: relative under the vite proxy
  // (dev/served), absolute loopback inside the Tauri webview
  // (origin there is tauri.localhost — see detectBases).
  const daemon = new Daemon(detectBases().api);
  const store: TaskStore = createStore(
    daemon,
    new EventStream(
      detectBases().ws,
      (e) => store.applyEvent(e),
      () => void store.resync(),
      () => connDown(),
    ),
    (id) => {
      const t = store.list.find((x) => x.id === id);
      if (t) toast.push(`✓ ${fileName(t.url)} complete`);
      void notifyCompleted(id, () => store.list.find((t) => t.id === id));
    },
  );

  let showAdd = $state(false);
  let addUrl = $state(''); // drop payload → AddDialog prefill
  let conn: Conn = $state('connecting');
  // U3 overlays: command palette (⌘K / /) + shortcuts cheat sheet (?)
  let paletteOpen = $state(false);
  let helpOpen = $state(false);
  // Plain subscribe (not `$store.conn`): conn is a nested property
  // holding a Svelte store, not a store-valued binding target.
  // WS death flips the badge immediately (resync only runs on
  // reconnect; between attempts the socket is silently dead).
  function connDown() {
    conn = 'down';
  }
  store.conn.subscribe((c: Conn) => (conn = c));

  // ---- filters (sidebar state lifted here; store stays pure) ---
  let statusFilter = $state<StatusFilter>('all');
  let categoryFilter = $state<Category | null>(null);
  // Row selection = inline SegmentPanel expansion (U2). One open
  // row at a time; deselected on filter change (the row may leave
  // the visible set — a selected row behind a filter is a trap).
  let selectedId = $state<string | null>(null);
  // Filter changes clear the selection ONLY when the selected row
  // would be hidden — a blanket clear erased palette selections in
  // the same batch (R2 P1-2: filters reset to all + row selected →
  // the effect still fired and nulled it).
  function passesFilters(t: (typeof store.list)[number]): boolean {
    if (statusFilter === 'active' && !isActive(t.status)) return false;
    if (statusFilter === 'completed' && t.status !== 'completed') return false;
    if (statusFilter === 'failed' && t.status !== 'failed') return false;
    if (categoryFilter !== null && categorize(t.url) !== categoryFilter) return false;
    return true;
  }
  $effect(() => {
    // deps: re-run when either filter changes (void: rune reads for
    // the dependency graph, not for their values)
    void statusFilter;
    void categoryFilter;
    if (selectedId !== null) {
      const t = store.list.find((x) => x.id === selectedId);
      if (t && !passesFilters(t)) selectedId = null;
    }
  });

  function select(id: string) {
    selectedId = selectedId === id ? null : id;
  }

  const isActive = (s: string) => s === 'queued' || s === 'running' || s === 'paused';

  const counts = $derived.by(() => {
    const c = {
      all: store.list.length,
      active: 0,
      completed: 0,
      failed: 0,
      categories: {} as Record<Category, number>,
    };
    for (const t of store.list) {
      if (t.status === 'completed') c.completed++;
      else if (t.status === 'failed') c.failed++;
      else c.active++;
      const cat = categorize(t.url);
      c.categories[cat] = (c.categories[cat] ?? 0) + 1;
    }
    return c;
  });

  const visible = $derived(
    store.list.filter((t) => !removalHidden.has(t.id) && passesFilters(t)),
  );

  const totalSpeed = $derived(
    store.list.reduce((sum, t) => sum + (t.speed ?? 0), 0),
  );

  // ---- theme --------------------------------------------------
  let theme = $state<Theme>(initialTheme());
  function toggleTheme() {
    theme = theme === 'dark' ? 'light' : 'dark';
    applyTheme(theme);
    saveTheme(theme);
  }

  // ---- B39 banner ---------------------------------------------
  let bannerMsg = $state<string | null>(null);
  let bannerTimer: ReturnType<typeof setTimeout> | undefined;
  function banner(msg: string) {
    bannerMsg = msg;
    clearTimeout(bannerTimer);
    bannerTimer = setTimeout(() => (bannerMsg = null), 6000);
  }

  async function act(p: Promise<unknown>) {
    try {
      await p;
    } catch (e) {
      // B39: an action failure must reach the HUMAN — a console.warn
      // is invisible in the packaged app. Transient banner, and
      // keep the console line for dev triage.
      console.warn('action failed', e);
      banner(String(e instanceof Error ? e.message : e));
    }
  }

  // ---- P0-2: zero-step add (quick add + clipboard watch) --------
  // The mainstream path into a downloader is "copy link, open
  // app, download" — it must not walk the 3-field dialog. Same
  // validation set as AddDialog; the daemon stays the authority.
  const URL_OK = /^(https?|ftps?|magnet|bt|file):/i;
  const DEFAULT_DIR = '~/Downloads';

  /** One-shot add: URL → running task, defaults for the rest.
   *  Returns an error STRING on validation failure (so callers
   *  can inline it) instead of throwing. */
  async function quickAdd(url: string): Promise<string | null> {
    const u = url.trim();
    if (!URL_OK.test(u)) return 'URL must be http(s), ftp, magnet:, bt: or file:';
    try {
      await store.add(u, DEFAULT_DIR, 'normal');
      toast.push(`Downloading ${fileName(u)}`);
      return null;
    } catch (e) {
      const msg = String(e instanceof Error ? e.message : e);
      banner(msg);
      return msg;
    }
  }

  // Clipboard watch (P0-2b): on window focus, if the clipboard
  // holds a link that is not already tracked/offered, offer a
  // one-click download. Never auto-adds — an offer, not an action
  // (privacy: reading on focus only, dismiss is remembered).
  let clipHint = $state<{ url: string; label: string } | null>(null);
  let clipSeen = ''; // last offered/added URL — don't re-offer it
  let clipBusy = false; // readText in flight
  let clipDead = false; // permission denied / unsupported → stop trying

  async function checkClipboard() {
    if (clipDead || clipBusy || clipHint) return;
    if (paletteOpen || helpOpen || showAdd) return; // overlays own the screen
    let text: string;
    try {
      clipBusy = true;
      text = await navigator.clipboard.readText();
    } catch {
      clipDead = true; // No permission / headless: silent, permanent
      return;
    } finally {
      clipBusy = false;
    }
    const url = text.trim().split(/\s+/)[0] ?? '';
    if (!URL_OK.test(url) || url === clipSeen) return;
    if (store.list.some((t) => t.url === url)) return; // already tracked
    clipHint = { url, label: fileName(url) };
  }

  async function acceptClip() {
    const h = clipHint;
    if (!h) return;
    clipSeen = h.url;
    clipHint = null;
    const err = await quickAdd(h.url);
    if (err) clipSeen = ''; // failed → allow re-offer on next focus
  }

  function dismissClip() {
    if (clipHint) clipSeen = clipHint.url;
    clipHint = null;
  }

  // quick-form state (P0-2a): separate from AddDialog's on purpose —
  // this one lives in the empty state and never blocks the list.
  let quickUrl = $state('');
  let quickErr = $state<string | null>(null);

  async function submitQuick() {
    quickErr = await quickAdd(quickUrl);
    if (!quickErr) quickUrl = '';
  }

  // ---- U2: artifact reveal + URL copy (context menu / dblclick) -
  async function openFile(id: string) {
    const t = store.list.find((x) => x.id === id);
    if (!t || t.status !== 'completed') return;
    const r = await revealSaved(t);
    if (r === 'unsupported') banner('Opening files is available in the desktop app');
    else if (r === 'failed') banner(`Could not open ${t.save_path}`);
  }

  async function openSavedFile(id: string) {
    const t = store.list.find((x) => x.id === id);
    if (!t || t.status !== 'completed') return;
    const r = await openSaved(t);
    if (r === 'unsupported') banner('Opening files is available in the desktop app');
    else if (r === 'failed') banner(`Could not open ${t.save_path}`);
  }

  async function copyUrl(id: string) {
    const t = store.list.find((x) => x.id === id);
    if (!t) return;
    try {
      await navigator.clipboard.writeText(t.url);
      toast.push('URL copied'); // success is visible too (R2 P2: no silent actions)
    } catch {
      banner('Copy failed — clipboard unavailable');
    }
  }

  // ---- U3: optimistic removal + undo --------------------------------
  // Remove hides the row immediately and defers the daemon call by
  // 5s; Undo cancels the timer and un-hides. If the timer fires the
  // row is already gone from view, so store.remove() is just the
  // backend truth catching up with the UI.
  let hidden = $state<string[]>([]);
  const removalHidden = $derived(new Set(hidden));
  const removalTimers = new Map<string, ReturnType<typeof setTimeout>>();

  function removeTask(id: string) {
    const t = store.list.find((x) => x.id === id);
    if (!t || hidden.includes(id)) return;
    hidden.push(id);
    if (selectedId === id) selectedId = null;
    const name = fileName(t.url);
    removalTimers.set(
      id,
      setTimeout(() => {
        removalTimers.delete(id);
        hidden = hidden.filter((h) => h !== id); // store.remove re-folds
        // Guard (R2 P2): another window may have removed the task
        // during the undo window — the store fold already deleted it,
        // and calling daemon.remove would 404 and banner at no one.
        if (!store.list.some((x) => x.id === id)) return;
        void act(store.remove(id));
      }, 5000),
    );
    toast.push(`Removed ${name}`, {
      undo: () => {
        const tm = removalTimers.get(id);
        if (tm) {
          clearTimeout(tm);
          removalTimers.delete(id);
        }
        hidden = hidden.filter((h) => h !== id); // row returns, data intact
      },
    });
  }

  function fileName(url: string): string {
    try {
      const u = new URL(url);
      const last = u.pathname.split('/').filter(Boolean).pop();
      return last ? decodeURIComponent(last) : url;
    } catch {
      return url;
    }
  }

  // ---- U3: keyboard layer -------------------------------------------
  // Global bindings live here, overlay-local keys (Esc) live in the
  // overlay components. These are page-level handlers — inside the
  // Tauri webview they behave exactly like in a browser tab; no
  // OS-global-shortcut plugin is involved (that risk from the
  // proposal applies to system-wide hotkeys, which we don't claim).
  function isTyping(e: KeyboardEvent): boolean {
    const el = e.target as HTMLElement | null;
    if (!el) return false;
    return (
      el.isContentEditable ||
      el instanceof HTMLInputElement ||
      el instanceof HTMLTextAreaElement ||
      el instanceof HTMLSelectElement
    );
  }

  function togglePauseSelected() {
    if (!selectedId) return;
    const t = store.list.find((x) => x.id === selectedId);
    if (!t) return;
    if (t.status === 'running' || t.status === 'queued') void act(store.pause(t.id));
    else if (t.status === 'paused') void act(store.resume(t.id));
  }

  function onKeydown(e: KeyboardEvent) {
    const mod = e.metaKey || e.ctrlKey;
    if (mod && e.key.toLowerCase() === 'k') {
      e.preventDefault();
      helpOpen = false; // overlays are exclusive (R2 P2: no stacking)
      paletteOpen = !paletteOpen;
      return;
    }
    if (mod && e.key.toLowerCase() === 'n') {
      e.preventDefault();
      paletteOpen = false;
      helpOpen = false;
      addUrl = '';
      showAdd = true;
      return;
    }
    if (isTyping(e)) return; // never steal keys from any open input (palette search, add URL field)
    // `/` and `?` REPLACE any open overlay (R2 P2: exclusivity) —
    // they sit above the overlay guard for that, but below isTyping
    // so typed characters still land in inputs.
    if (e.key === '/') {
      e.preventDefault();
      showAdd = false;
      helpOpen = false;
      paletteOpen = true;
      return;
    }
    if (e.key === '?') {
      e.preventDefault();
      paletteOpen = false;
      showAdd = false;
      helpOpen = true;
      return;
    }
    if (paletteOpen || helpOpen || showAdd) return; // one overlay owns the keyboard
    if (e.key === ' ') {
      // Native Space activation on focused interactive elements must
      // survive (R2 P0): buttons/links/menu items own their Space —
      // swallowing it here broke keyboard activation app-wide.
      const el = e.target as HTMLElement | null;
      if (el?.closest('button, [role="button"], a, select, option, [role="menuitem"]')) {
        return;
      }
      e.preventDefault(); // Space also scrolls the list — own it
      togglePauseSelected();
    } else if (e.key === 'Delete' || e.key === 'Backspace') {
      if (selectedId) {
        e.preventDefault();
        removeTask(selectedId);
      }
    }
  }

  // ---- U2: window-wide URL drag & drop -------------------------
  let dropping = $state(false);
  let dragDepth = 0; // enter/leave nest — net counter, not booleans

  function dragHasUrl(dt: DataTransfer | null): boolean {
    if (!dt) return false;
    return dt.types.includes('text/uri-list') || dt.types.includes('text/plain');
  }

  function onDragOver(e: DragEvent) {
    if (!dragHasUrl(e.dataTransfer)) return;
    e.preventDefault(); // required to make the window a drop target
    e.dataTransfer!.dropEffect = 'link';
    dropping = true;
  }

  function onDragEnter(e: DragEvent) {
    if (!dragHasUrl(e.dataTransfer)) return;
    dragDepth++;
  }

  function onDragLeave() {
    dragDepth = Math.max(0, dragDepth - 1);
    if (dragDepth === 0) dropping = false;
  }

  function onDrop(e: DragEvent) {
    dragDepth = 0;
    dropping = false;
    if (!e.dataTransfer) return;
    const raw =
      e.dataTransfer.getData('text/uri-list') || e.dataTransfer.getData('text/plain');
    // uri-list may carry comments (#) and multiple URLs — first
    // non-comment line wins (the dialog is single-task by design).
    const url = raw
      .split(/\r?\n/)
      .map((l) => l.trim())
      .find((l) => l && !l.startsWith('#'));
    if (!url) return;
    e.preventDefault();
    addUrl = url;
    showAdd = true;
  }
</script>

<svelte:window
  ondragover={onDragOver}
  ondragenter={onDragEnter}
  ondragleave={onDragLeave}
  ondrop={onDrop}
  onkeydown={onKeydown}
  onfocus={() => void checkClipboard()}
/>

<div class="shell">
  <aside>
    <Sidebar
      {store}
      bind:status={statusFilter}
      bind:category={categoryFilter}
      {counts}
      onBanner={banner}
    />
  </aside>

  <Toolbar {theme} onToggleTheme={toggleTheme} onAdd={() => ((addUrl = ''), (showAdd = true))} onPalette={() => (paletteOpen = true)} />

  <StatusBar totalSpeed={totalSpeed} active={counts.active} failed={counts.failed} {conn} />

  {#if bannerMsg}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions, a11y_click_events_have_key_events -->
    <div class="error-banner" role="alert" onclick={() => (bannerMsg = null)}>
      <span>⚠ {bannerMsg}</span>
      <small>click to dismiss</small>
    </div>
  {/if}

  <main class:dropping>
    {#if clipHint && visible.length > 0}
      <!-- P0-2b: clipboard offer — an offer, never an auto-action.
           Sticky inside <main>; suppressed when the empty state is
           visible because the empty state already IS an add form. -->
      <div class="clip-banner" role="status">
        <span class="clip-url" title={clipHint.url}>📋 {clipHint.label || clipHint.url}</span>
        <button class="ctl primary" onclick={() => void acceptClip()}>Download</button>
        <button class="ctl" onclick={dismissClip}>Dismiss</button>
      </div>
    {/if}
    {#if visible.length === 0}
      <div class="empty">
        <div class="empty-icon">↓</div>
        {#if store.list.length === 0}
          <!-- P0-2a: the empty state IS the add form — paste a link,
               press Enter, done. No dialog for the first download. -->
          <form
            class="quick"
            novalidate
            onsubmit={(e) => {
              e.preventDefault();
              void submitQuick();
            }}
          >
            <input
              class="quick-input"
              type="url"
              bind:value={quickUrl}
              placeholder="Paste a download link and press Enter"
              aria-label="Download URL"
              spellcheck="false"
              autocomplete="off"
            />
            <button class="ctl primary" type="submit" disabled={!quickUrl.trim()}>Download</button>
          </form>
          {#if quickErr}
            <p class="quick-err">{quickErr}</p>
          {/if}
          <p class="hint">or drop a link anywhere — ＋ Add for save path &amp; priority</p>
        {:else}
          <p>Nothing matches this filter</p>
          <p class="hint">{counts.all} task{counts.all === 1 ? '' : 's'} in other views</p>
        {/if}
      </div>
    {:else}
      {#each visible as task (task.id)}
        <TaskRow
          {task}
          {daemon}
          selected={selectedId === task.id}
          onPause={(id) => void act(store.pause(id))}
          onResume={(id) => void act(store.resume(id))}
          onRemove={(id) => removeTask(id)}
          onLimit={(id, bps) => void act(store.setTaskLimit(id, bps))}
          onSelect={select}
          onOpenFile={(id) => void openFile(id)}
          onOpenSaved={(id) => void openSavedFile(id)}
          onCopyUrl={(id) => void copyUrl(id)}
        />
      {/each}
    {/if}
  </main>
</div>

{#if showAdd}
  <AddDialog
    initialUrl={addUrl}
    onAdd={(url, path, prio) => store.add(url, path, prio)}
    onClose={() => (showAdd = false)}
    defaultDir="~/Downloads"
  />
{/if}

{#if paletteOpen}
  <CommandPalette
    {store}
    onClose={() => (paletteOpen = false)}
    onBanner={banner}
    onAdd={() => ((addUrl = ''), (showAdd = true))}
    onToggleTheme={toggleTheme}
    onSelectTask={(id) => {
      // bring the row into view whatever the current filters are
      statusFilter = 'all';
      categoryFilter = null;
      selectedId = id;
    }}
  />
{/if}

{#if helpOpen}
  <ShortcutsDialog onClose={() => (helpOpen = false)} />
{/if}

<Toasts />

<style>
  .shell {
    display: grid;
    height: 100vh;
    /* Motrix geometry: ~184px expanded aside over the main area */
    grid-template-columns: 184px 1fr;
    grid-template-rows: 44px 32px auto 1fr;
    grid-template-areas:
      'sidebar toolbar'
      'sidebar statusbar'
      'sidebar banner'
      'sidebar main';
    overflow: hidden;
  }
  aside {
    grid-area: sidebar;
    min-height: 0;
  }
  .shell > :global(header) {
    grid-area: toolbar;
  }
  .shell > :global(.statusbar) {
    grid-area: statusbar;
  }
  .shell > :global(.error-banner) {
    grid-area: banner;
  }
  main {
    grid-area: main;
    overflow-y: auto;
    min-height: 0;
    position: relative;
  }
  /* Drop affordance: inset accent ring while a link hovers over
     the window (U2 drag-and-drop) */
  main.dropping::after {
    content: '';
    position: sticky;
    top: 0;
    display: block;
    height: 0;
    box-shadow: inset 0 0 0 2px var(--accent);
    pointer-events: none;
    z-index: 5;
  }
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 4px;
    height: 100%;
    color: var(--dim);
  }
  /* P0-2a: paste-and-go — the empty state is the add form */
  .quick {
    display: flex;
    gap: 8px;
    width: min(560px, 80%);
    margin: 14px 0 4px;
  }
  .quick-input {
    flex: 1;
    min-width: 0;
    height: 36px;
    padding: 0 12px;
    background: var(--panel);
    border: 1px solid var(--line-strong);
    border-radius: 6px;
    color: var(--text);
    font-size: 13px;
  }
  .quick-input:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 22%, transparent);
  }
  .quick-input::placeholder {
    color: var(--dim);
  }
  .quick-err {
    margin: 0;
    color: var(--err);
    font-size: 12px;
  }
  /* P0-2b: clipboard offer strip, sticky at the head of the list */
  .clip-banner {
    position: sticky;
    top: 0;
    z-index: 10;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 16px;
    background: color-mix(in srgb, var(--accent) 12%, var(--panel));
    border-bottom: 1px solid color-mix(in srgb, var(--accent) 35%, var(--line));
    font-size: 13px;
  }
  .clip-url {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text);
  }
  .empty p {
    margin: 0;
    font-size: 14px;
  }
  .empty .hint {
    font-size: 12px;
    color: var(--dim);
  }
  .empty-icon {
    font-size: 28px;
    color: var(--line-strong);
    margin-bottom: 6px;
  }
</style>
