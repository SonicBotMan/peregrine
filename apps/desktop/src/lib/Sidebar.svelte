<script lang="ts">
  /**
   * Left aside (Motrix replication): dark rail with app wordmark,
   * nav sections (status / category), and the settings domain
   * (global speed limit) pinned to the bottom. Active nav =
   * #444 block with white text (Motrix subnav-active), no accent
   * borders. All list truth stays in the store; this component
   * only owns filter + throttle UI state.
   */
  import { CATEGORIES, type Category } from './categorize';
  import { LIMIT_PRESETS, presetFor } from './format';
  import type { TaskStore } from './store.svelte';
  import {
    ArrowDownToLine,
    CircleCheck,
    CircleX,
    FileArchive,
    FileAudio,
    FileText,
    FileVideo,
    Globe,
    Layers,
  } from '@lucide/svelte';
  import type { Component } from 'svelte';

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
      paused: number;
      completed: number;
      failed: number;
      categories: Record<Category, number>;
    };
    onBanner: (msg: string) => void;
  } = $props();

  const STATUS_ITEMS: readonly { id: StatusFilter; label: string; icon: Component<{ size?: number }> }[] = [
    { id: 'all', label: 'All', icon: Layers },
    { id: 'active', label: 'Active', icon: ArrowDownToLine },
    { id: 'completed', label: 'Completed', icon: CircleCheck },
    { id: 'failed', label: 'Failed', icon: CircleX },
  ];

  // category id → lucide icon (Motrix-style rail pictograms)
  const CATEGORY_ICONS: Record<Category, Component<{ size?: number }>> = {
    video: FileVideo,
    audio: FileAudio,
    doc: FileText,
    archive: FileArchive,
    program: FileArchive,
    other: Globe,
  };

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

<nav class="aside">
  <div class="brand">
    <span class="logo">Peregrine</span>
    <span class="ver">v0.1</span>
  </div>

  <div class="sec">Status</div>
  {#each STATUS_ITEMS as it (it.id)}
    <button
      class="nav-item"
      class:active={status === it.id}
      onclick={() => setStatus(it.id)}
    >
      <span class="nav-ic" aria-hidden="true"><it.icon size={15} /></span>
      <span class="truncate">{it.label}</span>
      {#if it.id === 'failed' && counts.failed > 0}
        <span class="nav-count danger">{counts.failed}</span>
      {:else}
        <span class="nav-count">{counts[it.id]}</span>
      {/if}
    </button>
  {/each}

  <div class="sec">Category</div>
  {#each CATEGORIES as it (it.id)}
    {@const n = counts.categories[it.id]}
    {@const Ico = CATEGORY_ICONS[it.id]}
    {#if n > 0 || category === it.id}
      <button
        class="nav-item"
        class:active={category === it.id}
        onclick={() => (category = category === it.id ? null : it.id)}
      >
        <span class="nav-ic" aria-hidden="true"><Ico size={15} /></span>
        <span class="truncate">{it.label}</span>
        <span class="nav-count">{n}</span>
      </button>
    {/if}
  {/each}

  <div class="foot">
    <div class="sec">Speed limit</div>
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
  /* Motrix aside: rgba(0,0,0,.9) over the #343434 main. */
  .aside {
    height: 100%;
    display: flex;
    flex-direction: column;
    overflow-y: auto;
    background: var(--aside);
    color: var(--text);
  }
  .brand {
    display: flex;
    align-items: baseline;
    gap: 6px;
    padding: 18px 16px 14px;
  }
  .logo {
    font-size: 15px;
    font-weight: 700;
    color: var(--text);
    letter-spacing: 0.01em;
  }
  .ver {
    font-size: 11px;
    color: var(--dim);
  }
  .sec {
    padding: 10px 16px 4px;
    font-size: 11px;
    color: var(--dim);
    /* systematic section headers: small caps + tracking, same rhythm
     * for Status / Category / Speed limit (VLM pass 4) */
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }
  .nav-item {
    display: flex;
    align-items: center;
    gap: 9px;
    margin: 2px 8px;
    padding: 6px 10px;
    font-size: 13px;
    color: var(--dim);
    border-radius: 4px;
    border: none;
    transition: background var(--dur) var(--ease), color var(--dur) var(--ease);
    text-align: left;
  }
  .nav-ic {
    display: inline-flex;
    flex-shrink: 0;
    color: inherit;
    opacity: 0.75;
  }
  .nav-item .truncate {
    flex: 1;
    min-width: 0;
  }
  .nav-item:hover {
    color: var(--text);
    background: var(--elevated);
  }
  /* Motrix subnav-active: #444 block + white text */
  .nav-item.active {
    color: #fff;
    background: var(--elevated);
    font-weight: 500;
  }
  :global([data-theme='light']) .nav-item.active {
    color: var(--accent);
  }
  .nav-count {
    margin-left: auto;
    font-size: 11px;
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    color: var(--dim);
  }
  .nav-count.danger {
    color: var(--err);
    /* chip, not floating text (VLM V4: a failed count is an alarm) */
    background: color-mix(in oklch, var(--err) 16%, transparent);
    border-radius: 999px;
    padding: 1px 7px;
  }
  .nav-item.active .nav-count {
    color: inherit;
  }
  .foot {
    margin-top: auto;
    padding-bottom: 12px;
    border-top: 1px solid var(--line-subtle);
  }
  .foot .sec {
    padding-top: 10px;
  }
  .global-limit {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 0 12px 0 16px;
  }
  /* inside the 184px aside the select must actually fit: kill the
   * shared 120px max-width and let it shrink (VLM: truncated) */
  .global-limit :global(.ctl-select) {
    flex: 1;
    min-width: 0;
    max-width: none;
  }
  .global-limit :global(.ctl-input) {
    width: 100%;
  }
  .global-limit .unit {
    color: var(--dim);
    font-size: 11px;
    white-space: nowrap;
  }
</style>
