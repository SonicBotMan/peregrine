<script lang="ts">
  import { Daemon, EventStream, detectBases } from './lib/daemon';
  import { createStore, type TaskStore } from './lib/store.svelte';
  import TaskRow from './lib/TaskRow.svelte';
  import AddDialog from './lib/AddDialog.svelte';
  import { LIMIT_PRESETS, presetFor } from './lib/format';
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

  const active = $derived(store.list.filter((t) => t.status !== 'completed'));
  const done = $derived(store.list.filter((t) => t.status === 'completed'));
  const totalSpeed = $derived(
    active.reduce((sum, t) => sum + (t.speed ?? 0), 0),
  );

  // ---- global limit (settings domain, not task truth) ----------
  let globalLimit = $state(0);
  let customGlobal = $state<number | null>(null);

  $effect(() => {
    // One-shot bootstrap; live changes by OTHER clients are
    // out of scope for v1 (B37 family: events carry task deltas only).
    void store
      .getSettings()
      .then((s) => (globalLimit = s.global_limit_bps))
      .catch(() => {});
  });

  const globalSel = $derived(
    customGlobal !== null ? 'custom' : (presetFor(globalLimit) ?? 'custom'),
  );

  function pickGlobal(ev: Event) {
    const v = (ev.currentTarget as HTMLSelectElement).value;
    if (v === 'custom') {
      customGlobal = globalLimit;
      return;
    }
    customGlobal = null;
    void store
      .setGlobalLimit(Number(v))
      .then((s) => (globalLimit = s.global_limit_bps))
      .catch((e) => {
        console.warn('global limit failed', e);
        banner(String(e instanceof Error ? e.message : e));
      });
  }

  function commitCustomGlobal() {
    if (customGlobal !== null && Number.isFinite(customGlobal) && customGlobal >= 0) {
      void store
        .setGlobalLimit(Math.round(customGlobal))
        .then((s) => (globalLimit = s.global_limit_bps))
        .catch((e) => {
          console.warn('global limit failed', e);
          banner(String(e instanceof Error ? e.message : e));
        });
    }
    customGlobal = null;
  }

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
      // B39: an action failure must reach the HUMAN — a console.warn
      // is invisible in the packaged app. Transient banner, and
      // keep the console line for dev triage.
      console.warn('action failed', e);
      banner(String(e instanceof Error ? e.message : e));
    }
  }

  // ---- error banner (B39) -------------------------------------
  let bannerMsg = $state<string | null>(null);
  let bannerTimer: ReturnType<typeof setTimeout> | undefined;
  function banner(msg: string) {
    bannerMsg = msg;
    clearTimeout(bannerTimer);
    bannerTimer = setTimeout(() => (bannerMsg = null), 6000);
  }</script>

<main>
  <header>
    <h1>Peregrine</h1>
    <span class="conn" data-kind={conn}>
      {conn === 'live' ? '●' : conn === 'connecting' ? '◌' : '✕'}
      {conn}
    </span>
    <span class="speed">{fmtSpeed(totalSpeed)}</span>
    <span class="global-limit" title="Global speed limit">
      {#if customGlobal !== null}
        <input
          type="number"
          min="0"
          bind:value={customGlobal}
          onblur={commitCustomGlobal}
          onkeydown={(e) => e.key === 'Enter' && commitCustomGlobal()}
        />
      {:else}
        <select value={globalSel} onchange={pickGlobal}>
          {#each LIMIT_PRESETS as p (p.bps)}
            <option value={String(p.bps)}>{p.label}</option>
          {/each}
          <option value="custom">custom…</option>
        </select>
      {/if}
      <span class="unit">B/s</span>
    </span>
    <button class="primary" onclick={() => (showAdd = true)}>＋ Add</button>
  </header>

  {#if bannerMsg}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div class="error-banner" role="alert" onclick={() => (bannerMsg = null)}>
      <span>⚠ {bannerMsg}</span>
      <small>click to dismiss</small>
    </div>
  {/if}

  <section>
    <h2>Active <small>({active.length})</small></h2>
    {#if active.length === 0}
      <p class="empty">Nothing in flight.</p>
    {:else}
      {#each active as task (task.id)}
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
  </section>

  <section>
    <h2>Completed <small>({done.length})</small></h2>
    {#each done as task (task.id)}
      <TaskRow
        {task}
        {daemon}
        onPause={(id) => void act(store.pause(id))}
        onResume={(id) => void act(store.resume(id))}
        onRemove={(id) => void act(store.remove(id))}
        onLimit={(id, bps) => void act(store.setTaskLimit(id, bps))}
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
