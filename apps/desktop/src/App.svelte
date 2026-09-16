<script lang="ts">
  /**
   * App shell (U1): inverted-L layout (ui-proposal §5) —
   *   sidebar (filters + global limit) | toolbar / statusbar / main
   * Flat tiled grid, sharp edges, zero gaps; the task list owns the
   * pixel budget. All task truth stays in the runes store; this
   * component wires filters + banner + dialog state only.
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
</script>

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

  <Toolbar {theme} onToggleTheme={toggleTheme} onAdd={() => (showAdd = true)} />

  <StatusBar totalSpeed={totalSpeed} active={counts.active} failed={counts.failed} {conn} />

  {#if bannerMsg}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions, a11y_click_events_have_key_events -->
    <div class="error-banner" role="alert" onclick={() => (bannerMsg = null)}>
      <span>⚠ {bannerMsg}</span>
      <small>click to dismiss</small>
    </div>
  {/if}

  <main>
    {#if visible.length === 0}
      <div class="empty">
        <div class="empty-icon">↓</div>
        {#if store.list.length === 0}
          <p>No downloads yet</p>
          <p class="hint">Hit ＋ Add to start your first download</p>
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
          onPause={(id) => void act(store.pause(id))}
          onResume={(id) => void act(store.resume(id))}
          onRemove={(id) => void act(store.remove(id))}
          onLimit={(id, bps) => void act(store.setTaskLimit(id, bps))}
        />
      {/each}
    {/if}
  </main>
</div>

{#if showAdd}
  <AddDialog
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
