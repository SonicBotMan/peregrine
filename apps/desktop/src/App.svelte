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
  import { revealSaved } from './lib/open';

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
    (id) => void notifyCompleted(id, () => store.list.find((t) => t.id === id)),
  );

  let showAdd = $state(false);
  let addUrl = $state(''); // drop payload → AddDialog prefill
  let conn: Conn = $state('connecting');
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
  $effect(() => {
    // deps: re-run when either filter changes
    statusFilter;
    categoryFilter;
    selectedId = null;
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
    store.list.filter((t) => {
      if (statusFilter === 'active' && !isActive(t.status)) return false;
      if (statusFilter === 'completed' && t.status !== 'completed') return false;
      if (statusFilter === 'failed' && t.status !== 'failed') return false;
      if (categoryFilter !== null && categorize(t.url) !== categoryFilter) return false;
      return true;
    }),
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

  // ---- U2: artifact reveal + URL copy (context menu / dblclick) -
  async function openFile(id: string) {
    const t = store.list.find((x) => x.id === id);
    if (!t || t.status !== 'completed') return;
    const r = await revealSaved(t);
    if (r === 'unsupported') banner('Opening files is available in the desktop app');
    else if (r === 'failed') banner(`Could not open ${t.save_path}`);
  }

  async function copyUrl(id: string) {
    const t = store.list.find((x) => x.id === id);
    if (!t) return;
    try {
      await navigator.clipboard.writeText(t.url);
    } catch {
      banner('Copy failed — clipboard unavailable');
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

  <Toolbar {theme} onToggleTheme={toggleTheme} onAdd={() => ((addUrl = ''), (showAdd = true))} />

  <StatusBar totalSpeed={totalSpeed} active={counts.active} failed={counts.failed} {conn} />

  {#if bannerMsg}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions, a11y_click_events_have_key_events -->
    <div class="error-banner" role="alert" onclick={() => (bannerMsg = null)}>
      <span>⚠ {bannerMsg}</span>
      <small>click to dismiss</small>
    </div>
  {/if}

  <main class:dropping>
    {#if visible.length === 0}
      <div class="empty">
        <div class="empty-icon">↓</div>
        {#if store.list.length === 0}
          <p>No downloads yet</p>
          <p class="hint">Drop a link anywhere — or hit ＋ Add</p>
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
          onRemove={(id) => void act(store.remove(id))}
          onLimit={(id, bps) => void act(store.setTaskLimit(id, bps))}
          onSelect={select}
          onOpenFile={(id) => void openFile(id)}
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

<style>
  .shell {
    display: grid;
    height: 100vh;
    grid-template-columns: 192px 1fr;
    grid-template-rows: 46px 36px auto 1fr;
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
