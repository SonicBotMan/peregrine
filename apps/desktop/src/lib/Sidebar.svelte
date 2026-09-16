<script lang="ts">
  /**
   * Left rail (U1): status filter + category filter + global limit.
   * Flat data surface — sharp edges, border separation, no cards
   * (ui-proposal §4.2). All list truth stays in the store; this
   * component only owns filter + throttle UI state.
   */
  import { CATEGORIES, type Category } from './categorize';
  import { LIMIT_PRESETS, presetFor } from './format';
  import type { TaskStore } from './store.svelte';

  export type StatusFilter = 'all' | 'active' | 'completed' | 'failed';

  let {
    store,
    status = $bindable('all' as StatusFilter),
    category = $bindable(null as Category | null),
    counts,
    onBanner,
  }: {
    store: TaskStore;
    status: StatusFilter;
    category: Category | null;
    counts: {
      all: number;
      active: number;
      completed: number;
      failed: number;
      categories: Record<Category, number>;
    };
    onBanner: (msg: string) => void;
  } = $props();

  const STATUS_ITEMS: readonly { id: StatusFilter; label: string }[] = [
    { id: 'all', label: 'All' },
    { id: 'active', label: 'Active' },
    { id: 'completed', label: 'Completed' },
    { id: 'failed', label: 'Failed' },
  ];

  function setStatus(id: StatusFilter) {
    status = id;
  }

  // ---- global limit (settings domain, moved here from App U1) --
  let globalLimit = $state(0);
  let customGlobal = $state<number | null>(null);

  $effect(() => {
    // One-shot bootstrap; live changes by OTHER clients are out of
    // scope for v1 (B37: events carry task deltas only).
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
        onBanner(String(e instanceof Error ? e.message : e));
      });
  }

  function commitCustomGlobal() {
    if (customGlobal !== null && Number.isFinite(customGlobal) && customGlobal >= 0) {
      void store
        .setGlobalLimit(Math.round(customGlobal))
        .then((s) => (globalLimit = s.global_limit_bps))
        .catch((e) => {
          console.warn('global limit failed', e);
          onBanner(String(e instanceof Error ? e.message : e));
        });
    }
    customGlobal = null;
  }
</script>

<nav class="flex h-full flex-col overflow-y-auto border-r border-line-strong bg-panel">
  <div class="px-3 pb-1 pt-3 text-[10px] font-semibold uppercase tracking-[0.08em] text-dim">
    Status
  </div>
  {#each STATUS_ITEMS as it (it.id)}
    <button
      class="nav-item"
      class:active={status === it.id}
      onclick={() => setStatus(it.id)}
    >
      <span class="truncate">{it.label}</span>
      {#if it.id === 'failed' && counts.failed > 0}
        <span class="nav-count text-err">{counts.failed}</span>
      {:else}
        <span class="nav-count">{counts[it.id]}</span>
      {/if}
    </button>
  {/each}

  <div class="mt-4 px-3 pb-1 text-[10px] font-semibold uppercase tracking-[0.08em] text-dim">
    Category
  </div>
  {#each CATEGORIES as it (it.id)}
    {@const n = counts.categories[it.id]}
    {#if n > 0 || category === it.id}
      <button
        class="nav-item"
        class:active={category === it.id}
        onclick={() => (category = category === it.id ? null : it.id)}
      >
        <span class="truncate">{it.label}</span>
        <span class="nav-count">{n}</span>
      </button>
    {/if}
  {/each}

  <div class="mt-auto border-t border-line px-3 py-3">
    <div class="mb-1.5 text-[10px] font-semibold uppercase tracking-[0.08em] text-dim">
      Speed limit
    </div>
    <div class="global-limit" title="Global speed limit">
      {#if customGlobal !== null}
        <input
          class="ctl-input w-full"
          type="number"
          min="0"
          bind:value={customGlobal}
          onblur={commitCustomGlobal}
          onkeydown={(e) => e.key === 'Enter' && commitCustomGlobal()}
        />
      {:else}
        <select class="ctl-select w-full" value={globalSel} onchange={pickGlobal}>
          {#each LIMIT_PRESETS as p (p.bps)}
            <option value={String(p.bps)}>{p.label}</option>
          {/each}
          <option value="custom">custom…</option>
        </select>
      {/if}
      <span class="unit">B/s</span>
    </div>
  </div>
</nav>

<style>
  .nav-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 5px 12px;
    font-size: 13px;
    color: var(--dim);
    border-left: 2px solid transparent;
    transition: color 0.12s ease, background 0.12s ease;
    text-align: left;
  }
  .nav-item:hover {
    color: var(--text);
    background: var(--elevated);
  }
  .nav-item.active {
    color: var(--text);
    background: color-mix(in oklch, var(--accent) 10%, transparent);
    border-left-color: var(--accent); /* Linear: accent left-border marks selection */
    font-weight: 500;
  }
  .nav-count {
    font-size: 11px;
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    color: var(--dim);
  }
  .nav-item.active .nav-count {
    color: var(--text);
  }
  .global-limit {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .global-limit .unit {
    color: var(--dim);
    font-size: 11px;
    white-space: nowrap;
  }
</style>
