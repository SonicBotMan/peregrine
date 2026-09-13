<script lang="ts">
  import { Daemon, EventStream } from './lib/daemon';
  import { createStore, type TaskStore } from './lib/store.svelte';
  import TaskRow from './lib/TaskRow.svelte';
  import AddDialog from './lib/AddDialog.svelte';
  import type { Conn } from './lib/store.svelte';

  // Dev: Vite proxies /api + /ws to the daemon's loopback TCP.
  // Tauri build: same-origin webview serving, base '' hits the
  // bundled proxy (M3-b wires the exact base).
  const daemon = new Daemon('/api');
  const stream = new EventStream(
    `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws/events`,
    (e) => store.applyEvent(e),
    () => void store.resync(),
  );
  const store: TaskStore = createStore(daemon, stream);

  let showAdd = $state(false);
  let conn: Conn = $state('connecting');
  // Plain subscribe (not `$store.conn`): conn is a nested property
  // holding a Svelte store, not a store-valued binding target.
  store.conn.subscribe((c: Conn) => (conn = c));

  const active = $derived(store.list.filter((t) => t.status !== 'completed' && t.status !== 'removed'));
  const done = $derived(store.list.filter((t) => t.status === 'completed'));
  const totalSpeed = $derived(
    active.reduce((sum, t) => sum + (t.speed ?? 0), 0),
  );

  function fmtSpeed(n: number): string {
    const units = ['B/s', 'KB/s', 'MB/s', 'GB/s'];
    let v = n;
    let i = 0;
    while (v >= 1024 && i < units.length - 1) {
      v /= 1024;
      i++;
    }
    return `${v >= 100 || i === 0 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
  }

  async function act(p: Promise<unknown>) {
    try {
      await p;
    } catch (e) {
      // Action failures surface as a transient banner row for now;
      // M3-c adds toast + retry UX.
      console.warn('action failed', e);
    }
  }
</script>

<main>
  <header>
    <h1>Peregrine</h1>
    <span class="conn" data-kind={conn}>
      {conn === 'live' ? '●' : conn === 'connecting' ? '◌' : '✕'}
      {conn}
    </span>
    <span class="speed">{fmtSpeed(totalSpeed)}</span>
    <button class="primary" onclick={() => (showAdd = true)}>＋ Add</button>
  </header>

  <section>
    <h2>Active <small>({active.length})</small></h2>
    {#if active.length === 0}
      <p class="empty">Nothing in flight.</p>
    {:else}
      {#each active as task (task.id)}
        <TaskRow
          {task}
          onPause={(id) => void act(store.pause(id))}
          onResume={(id) => void act(store.resume(id))}
          onRemove={(id) => void act(store.remove(id))}
        />
      {/each}
    {/if}
  </section>

  <section>
    <h2>Completed <small>({done.length})</small></h2>
    {#each done as task (task.id)}
      <TaskRow
        {task}
        onPause={(id) => void act(store.pause(id))}
        onResume={(id) => void act(store.resume(id))}
        onRemove={(id) => void act(store.remove(id))}
      />
    {/each}
  </section>

  {#if showAdd}
    <AddDialog
      onAdd={(url, path, prio) => store.add(url, path, prio)}
      onClose={() => (showAdd = false)}
      defaultDir="~/Downloads"
    />
  {/if}
</main>
