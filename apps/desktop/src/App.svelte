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
  import Toolbar from './lib/Toolbar.svelte';
  import StatusBar from './lib/StatusBar.svelte';
  import Titlebar from './lib/Titlebar.svelte';
  import DetailPanel from './lib/DetailPanel.svelte';
  import type { StatusFilter } from './lib/types';
  import { categorize, type Category } from './lib/categorize';
  import { initialTheme, applyTheme, saveTheme, type Theme } from './lib/theme';
  import type { Conn } from './lib/store.svelte';
  import { notifyCompleted } from './lib/notify';
  import { revealSaved, openSaved } from './lib/open';
  import Toasts from './lib/Toasts.svelte';
  import CommandPalette from './lib/CommandPalette.svelte';
  import { ArrowDownToLine, ClipboardPaste, TriangleAlert } from '@lucide/svelte';
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
      if (t) toast.push(`${fileName(t.url)} complete`);
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
  // V5: live text filter (toolbar search) + global limit (statusbar)
  let query = $state('');
  // V5: clickable column sort (mock: NAME/SIZE carry indicators)
  type SortKey = 'name' | 'size' | 'received' | 'speed';
  let sortKey: SortKey | null = $state(null);
  let sortDir: 'asc' | 'desc' = $state('asc');
  function sortBy(k: 'name' | 'size' | 'received' | 'speed') {
    if (sortKey === k) {
      sortDir = sortDir === 'asc' ? 'desc' : 'asc';
      // third click resets to daemon order
      if (sortDir === 'asc') sortKey = null;
    } else {
      sortKey = k;
      sortDir = 'desc'; // default: biggest/newest first
    }
  }
  let globalLimit = $state<number | null>(null);
  let toolbarRef = $state<Toolbar | undefined>(undefined);
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

  const visible = $derived.by(() => {
    const q = query.trim().toLowerCase();
    const rows = store.list.filter(
      (t) =>
        !removalHidden.has(t.id) &&
        passesFilters(t) &&
        (!q || t.url.toLowerCase().includes(q) || fileName(t.url).toLowerCase().includes(q)),
    );
    if (sortKey === null) return rows;
    const dir = sortDir === 'asc' ? 1 : -1;
    const key = (t: (typeof rows)[number]): number | string =>
      sortKey === 'name'
        ? fileName(t.url).toLowerCase()
        : sortKey === 'size'
          ? (t.total_bytes ?? Infinity)
          : sortKey === 'received'
            ? t.received_bytes
            : (t.speed ?? -1);
    return rows.sort((a, b) => {
      const ka = key(a);
      const kb = key(b);
      return ka < kb ? -dir : ka > kb ? dir : 0;
    });
  });

  const selectedTask = $derived(
    selectedId !== null ? (store.list.find((x) => x.id === selectedId) ?? null) : null,
  );

  // ---- V5: bulk actions (toolbar / menubar / shortcuts) -------
  async function bulkPause() {
    const ids = store.list.filter((t) => t.status === 'running' || t.status === 'queued').map((t) => t.id);
    if (ids.length === 0) return;
    const n = ids.length;
    toast.push(`Pausing ${n} task${n === 1 ? '' : 's'}…`);
    await Promise.allSettled(ids.map((id) => store.pause(id)));
  }

  async function bulkResume() {
    const ids = store.list.filter((t) => t.status === 'paused').map((t) => t.id);
    if (ids.length === 0) return;
    const n = ids.length;
    toast.push(`Resuming ${n} task${n === 1 ? '' : 's'}…`);
    await Promise.allSettled(ids.map((id) => store.resume(id)));
  }

  function clearDone() {
    const done = store.list.filter((t) => t.status === 'completed');
    if (done.length === 0) return;
    const n = done.length;
    toast.push(`Clearing ${n} finished task${n === 1 ? '' : 's'}…`);
    void Promise.allSettled(done.map((t) => store.remove(t.id)));
  }

  async function applyGlobalLimit(bps: number | null) {
    try {
      await store.setGlobalLimit(bps ?? 0);
      globalLimit = bps;
    } catch (e) {
      banner(String(e instanceof Error ? e.message : e));
    }
  }

  $effect(() => {
    // initial global limit read (statusbar selector source of truth)
    void daemon
      .getSettings()
      .then((st) => {
        // daemon convention: 0 = unlimited → normalize to null
        globalLimit = st.global_limit_bps || null;
      })
      .catch(() => {});
  });

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
    if (mod && e.key.toLowerCase() === 'f') {
      e.preventDefault(); // V5: toolbar search focus
      toolbarRef?.focusSearch();
      return;
    }
    if (mod && e.shiftKey && e.key.toLowerCase() === 'p') {
      e.preventDefault(); // V5: ⇧⌘P pause all
      void bulkPause();
      return;
    }
    if (mod && e.shiftKey && e.key.toLowerCase() === 'r') {
      e.preventDefault(); // V5: ⇧⌘R resume all
      void bulkResume();
      return;
    }
    if (e.key === 'Escape' && !paletteOpen && !helpOpen && !showAdd && selectedId) {
      selectedId = null; // V5: Esc closes the detail drawer
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
  <Titlebar
    title="Peregrine"
    onAdd={() => ((addUrl = ''), (showAdd = true))}
    onPauseAll={() => void bulkPause()}
    onResumeAll={() => void bulkResume()}
    onClearDone={clearDone}
    onToggleTheme={toggleTheme}
    themeLabel={theme === 'dark' ? 'Light Theme' : 'Dark Theme'}
    onPalette={() => (paletteOpen = true)}
    onShortcuts={() => (helpOpen = true)}
  />

  <Toolbar
    bind:this={toolbarRef}
    bind:query
    bind:statusFilter
    bind:categoryFilter
    {counts}
    hasSelection={selectedId !== null}
    onAdd={() => ((addUrl = ''), (showAdd = true))}
    onPauseAll={() => void bulkPause()}
    onResumeAll={() => void bulkResume()}
    onClearDone={clearDone}
    onOpenFolder={() => selectedId && void openFile(selectedId)}
  />

  {#if bannerMsg}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions, a11y_click_events_have_key_events -->
    <div class="error-banner" role="alert" onclick={() => (bannerMsg = null)}>
      <span class="banner-ic" aria-hidden="true"><TriangleAlert size={14} /></span>
      <span class="banner-txt">{bannerMsg}</span>
      <small>click to dismiss</small>
    </div>
  {/if}

  <main class:dropping>
    {#if clipHint && visible.length > 0}
      <!-- P0-2b: clipboard offer — an offer, never an auto-action.
           Sticky inside <main>; suppressed when the empty state is
           visible because the empty state already IS an add form. -->
      <div class="clip-banner" role="status">
        <span class="clip-url" title={clipHint.url}>
          <ClipboardPaste size={14} aria-hidden="true" />
          <span class="clip-label">{clipHint.label || clipHint.url}</span>
        </span>
        <button class="ctl primary" onclick={() => void acceptClip()}>Download</button>
        <button class="ctl" onclick={dismissClip}>Dismiss</button>
      </div>
    {/if}
    {#if visible.length === 0}
      <div class="empty">
        <div class="empty-icon" aria-hidden="true"><ArrowDownToLine size={30} strokeWidth={1.5} /></div>
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
          <p class="hint">or drop a link anywhere — press N for save path &amp; priority</p>
        {:else}
          <p>Nothing matches this filter</p>
          <p class="hint">{counts.all} task{counts.all === 1 ? '' : 's'} in other views</p>
        {/if}
      </div>
    {:else}
      <div class="thead" role="row">
        <button class="th" role="columnheader" onclick={() => sortBy('name')}>
          Name{#if sortKey === 'name'}<i class="caret">{sortDir === 'asc' ? '▴' : '▾'}</i>{/if}
        </button>
        <span class="th">Progress</span>
        <button class="th num" role="columnheader" onclick={() => sortBy('size')}>
          Size{#if sortKey === 'size'}<i class="caret">{sortDir === 'asc' ? '▴' : '▾'}</i>{/if}
        </button>
        <button class="th num" role="columnheader" onclick={() => sortBy('speed')}>
          Speed{#if sortKey === 'speed'}<i class="caret">{sortDir === 'asc' ? '▴' : '▾'}</i>{/if}
        </button>
        <span class="th num">ETA</span>
        <span class="th">Status</span>
        <span class="th"></span>
      </div>
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

  {#if selectedTask}
    <DetailPanel
      task={selectedTask}
      {daemon}
      onLimit={(id, bps) => void act(store.setTaskLimit(id, bps))}
      onClose={() => (selectedId = null)}
    />
  {/if}

  <StatusBar
    totalSpeed={totalSpeed}
    active={counts.active}
    paused={store.list.filter((t) => t.status === 'paused').length}
    done={counts.completed}
    failed={counts.failed}
    {conn}
    {globalLimit}
    onGlobalLimit={(bps) => void applyGlobalLimit(bps)}
    {theme}
    onToggleTheme={toggleTheme}
    version="dev"
  />
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
    display: flex;
    flex-direction: column;
    height: 100vh;
    overflow: hidden;
  }
  main {
    flex: 1;
    overflow-y: auto;
    min-height: 0;
    position: relative;
  }
  .shell > :global(.error-banner) {
    position: absolute;
    top: 74px;
    left: 50%;
    transform: translateX(-50%);
    z-index: 40;
  }
  /* V5: table header aligned with TaskRow columns */
  .thead {
    display: grid;
    grid-template-columns:
      minmax(0, 1fr)
      150px
      128px
      86px
      72px
      92px
      64px;
    column-gap: 14px;
    padding: 0 14px;
    height: 28px;
    align-items: center;
    font: 650 10px var(--font-sans);
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: color-mix(in oklch, var(--text) 62%, var(--bg));
    background: color-mix(in oklch, var(--chrome-2) 82%, var(--inset-hl));
    border-bottom: 1px solid var(--line-strong);
    position: sticky;
    top: 0;
    z-index: 5;
    user-select: none;
  }
  .thead .num {
    text-align: right;
  }
  .th {
    all: unset;
    display: block;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    cursor: default;
  }
  button.th { cursor: pointer; }
  button.th:hover { color: var(--text); }
  .caret {
    font-style: normal;
    margin-left: 3px;
    color: var(--accent);
  }
  .th.num {
    display: flex;
    justify-content: flex-end;
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
    display: inline-flex;
    align-items: center;
    gap: 8px;
    color: var(--text);
    overflow: hidden;
  }
  .clip-label {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
    display: inline-flex;
    color: var(--line-strong);
    margin-bottom: 6px;
    padding: 14px;
    border: 1px solid var(--line-subtle);
    border-radius: 999px;
    background: var(--panel);
  }
</style>
