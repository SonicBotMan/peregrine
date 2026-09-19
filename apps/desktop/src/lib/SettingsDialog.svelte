<script lang="ts">
  /**
   * Settings center (GUI-verify batch-2): one surface for every knob
   * that used to live scattered — status-bar limit select, File-menu
   * autostart toggle, theme toggle, per-task Browse. Layout follows
   * the Motrix/FDM convention: category rail left, settings right.
   *
   * Persistence is split by ownership:
   * - daemon  : default_dir, global_limit_bps, max_concurrent,
   *             seg_conns  → PUT /settings (partial, applies live)
   * - client  : theme (localStorage), completion notification,
   *             clipboard watch (localStorage)
   * - system  : launch at login (autostart plugin)
   * Every change saves immediately — no submit button — with a
   * toast, matching the status-bar limit selector's contract.
   */
  import { onMount } from 'svelte';
  import { LIMIT_PRESETS } from './format';
  import { X } from '@lucide/svelte';

  let {
    onClose,
    defaultDir,
    globalLimit,
    maxConcurrent,
    segConns,
    userAgent,
    launchAtLogin,
    themeMode,
    notifyEnabled,
    clipWatchEnabled,
    isDesktopShell,
    onDefaultDir,
    onGlobalLimit,
    onMaxConcurrent,
    onSegConns,
    onLaunchAtLogin,
    onUserAgent,
    onThemeMode,
    onNotifyToggle,
    onClipWatchToggle,
  }: {
    onClose: () => void;
    defaultDir: string;
    globalLimit: number | null;
    maxConcurrent: number;
    segConns: number;
    userAgent: string;
    launchAtLogin: boolean;
    themeMode: 'auto' | 'dark' | 'light';
    notifyEnabled: boolean;
    clipWatchEnabled: boolean;
    isDesktopShell: boolean;
    onDefaultDir: (dir: string) => void;
    onGlobalLimit: (bps: number | null) => void;
    onMaxConcurrent: (n: number) => void;
    onSegConns: (n: number) => void;
    onLaunchAtLogin: () => void;
    onUserAgent: (ua: string) => void;
    onThemeMode: (m: 'auto' | 'dark' | 'light') => void;
    onNotifyToggle: (on: boolean) => void;
    onClipWatchToggle: (on: boolean) => void;
  } = $props();

  let section = $state<'general' | 'download'>('general');
  // Svelte 5's declarative select-value binding can silently miss
  // when it races sibling option patches (the status-bar limit
  // select had the same disease) — assert the DOM value explicitly
  // on every prop change instead of trusting the patch order.
  let glimEl: HTMLSelectElement | undefined = $state();
  let segEl: HTMLSelectElement | undefined = $state();
  // UA text input keeps a local draft; commits on change (blur) —
  // a per-keystroke PUT would spam the daemon with mid-typing values.
  let uaDraft = $state(userAgent);
  $effect(() => {
    uaDraft = userAgent;
  });
  $effect(() => {
    if (glimEl) glimEl.value = globalLimit === null ? 'none' : String(globalLimit);
  });
  $effect(() => {
    if (segEl) segEl.value = String(segConns);
  });
  let picking = $state(false);
  // Intentional snapshot: the dialog remounts per open, so seeding
  // from the CURRENT daemon value is the point; the $effect below
  // re-syncs when the daemon value changes underneath.
  // svelte-ignore state_referenced_locally
  let dirDraft = $state(defaultDir);

  // Keep the draft in sync when the daemon value changes underneath.
  $effect(() => {
    dirDraft = defaultDir;
  });

  async function browseDir() {
    if (picking) return;
    picking = true;
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const dir = await open({
        directory: true,
        multiple: false,
        defaultPath: dirDraft || undefined,
        title: 'Choose default download directory',
      });
      if (typeof dir === 'string' && dir) onDefaultDir(dir);
    } catch {
      // cancelled / unavailable
    } finally {
      picking = false;
    }
  }

  const onKeydown = (e: KeyboardEvent) => {
    if (e.key === 'Escape') onClose();
  };
</script>

<svelte:window onkeydown={onKeydown} />

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="overlay" onclick={onClose}>
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div
    class="dialog settings"
    onclick={(e) => e.stopPropagation()}
    role="dialog"
    aria-label="Settings"
    tabindex={-1}
  >
    <h2>Settings</h2>
    <div class="body">
      <nav class="cats" aria-label="Settings categories">
        <button
          class="cat"
          class:active={section === 'general'}
          onclick={() => (section = 'general')}>General</button
        >
        <button
          class="cat"
          class:active={section === 'download'}
          onclick={() => (section = 'download')}>Download</button
        >
      </nav>

      <div class="pane">
        {#if section === 'general'}
          <label class="row">
            <span class="lab">Theme</span>
            <select
              value={themeMode}
              onchange={(e) => onThemeMode(e.currentTarget.value as 'auto' | 'dark' | 'light')}
            >
              <option value="auto">Follow system</option>
              <option value="dark">Dark</option>
              <option value="light">Light</option>
            </select>
          </label>
          <label class="row">
            <span class="lab">Completion notification</span>
            <input
              type="checkbox"
              checked={notifyEnabled}
              onchange={(e) => onNotifyToggle(e.currentTarget.checked)}
            />
          </label>
          {#if isDesktopShell}
            <label class="row">
              <span class="lab">Launch at login</span>
              <input
                type="checkbox"
                checked={launchAtLogin}
                onchange={() => onLaunchAtLogin()}
              />
            </label>
          {/if}
          <label class="row">
            <span class="lab">Clipboard link detection</span>
            <input
              type="checkbox"
              checked={clipWatchEnabled}
              onchange={(e) => onClipWatchToggle(e.currentTarget.checked)}
            />
          </label>
        {:else if section === 'download'}
          <div class="row col">
            <span class="lab">Default save directory</span>
            <span class="path-row">
              <input
                bind:value={dirDraft}
                placeholder={defaultDir}
                onchange={() => dirDraft.trim() && onDefaultDir(dirDraft.trim())}
              />
              {#if isDesktopShell}
                <button
                  type="button"
                  class="browse"
                  onclick={() => void browseDir()}
                  disabled={picking}
                >
                  {picking ? '…' : 'Browse…'}
                </button>
              {/if}
            </span>
            <p class="scheme-hint">~/ expands against the daemon's home directory.</p>
          </div>
          <label class="row">
            <span class="lab">Global speed limit</span>
            <select
              bind:this={glimEl}
              value={globalLimit === null ? 'none' : String(globalLimit)}
              onchange={(e) => {
                const v = e.currentTarget?.value;
                onGlobalLimit(!v || v === 'none' ? null : Number(v));
              }}
            >
              <!-- A value outside the presets (set via API or an older
                   build) must still display — dynamic option keeps the
                   select from rendering blank. -->
              {#if globalLimit !== null && !LIMIT_PRESETS.some((p) => p.bps === globalLimit)}
                <option value={globalLimit}>{Math.round(globalLimit / 1024)} KB/s (custom)</option>
              {/if}
              <option value="none">Unlimited</option>
              {#each LIMIT_PRESETS as p (p.label)}
                <option value={p.bps}>{p.label}</option>
              {/each}
            </select>
          </label>
          <label class="row">
            <span class="lab">Max parallel tasks</span>
            <input
              type="number"
              min="1"
              max="10"
              value={maxConcurrent}
              onchange={(e) => {
                const n = Math.max(1, Math.min(10, Number(e.currentTarget.value) || 1));
                e.currentTarget.value = String(n);
                onMaxConcurrent(n);
              }}
            />
          </label>
          <label class="row">
            <span class="lab">Connections per download</span>
            <select
              bind:this={segEl}
              value={String(segConns)}
              onchange={(e) => onSegConns(Number(e.currentTarget.value))}
            >
              <option value={4}>4</option>
              <option value={8}>8</option>
              <option value={16}>16</option>
              <option value={32}>32</option>
            </select>
            <p class="scheme-hint">Applies to NEW downloads; running tasks keep their plan.</p>
          </label>
          <div class="row col">
            <span class="lab">HTTP User-Agent</span>
            <input
              bind:value={uaDraft}
              placeholder="peregrine/<version>"
              onchange={() => onUserAgent(uaDraft.trim())}
            />
            <p class="scheme-hint">Empty = the peregrine/&lt;version&gt; default. Applies to NEW downloads.</p>
          </div>
        {/if}
      </div>
    </div>

    <button class="close" title="Close" aria-label="Close settings" onclick={onClose}>
      <X size={14} />
    </button>
  </div>
</div>

<style>
  .settings {
    width: min(640px, 92vw);
    min-height: 320px;
  }
  .body {
    display: flex;
    gap: 16px;
    min-height: 240px;
  }
  .cats {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 96px;
    border-right: 1px solid var(--border);
    padding-right: 12px;
  }
  .cat {
    text-align: left;
    background: transparent;
    border: 0;
    color: var(--dim);
    font-size: 13px;
    padding: 6px 10px;
    border-radius: 6px;
    cursor: pointer;
  }
  .cat:hover {
    color: var(--text);
  }
  .cat.active {
    background: color-mix(in oklab, var(--accent) 18%, transparent);
    color: var(--text);
  }
  .pane {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    font-size: 13px;
    color: var(--text);
  }
  .row.col {
    flex-direction: column;
    align-items: stretch;
    gap: 6px;
  }
  .lab {
    color: var(--dim);
  }
  .path-row {
    display: flex;
    gap: 6px;
    align-items: stretch;
    flex: 1;
  }
  .path-row input {
    flex: 1;
    min-width: 0;
  }
  .browse {
    border: 1px solid var(--border);
    background: transparent;
    color: var(--text);
    border-radius: 6px;
    padding: 0 10px;
    font-size: 12px;
    cursor: pointer;
    white-space: nowrap;
  }
  .browse:hover {
    border-color: var(--accent);
    color: var(--accent);
  }
  .close {
    position: absolute;
    top: 10px;
    right: 10px;
    background: transparent;
    border: 0;
    color: var(--dim);
    cursor: pointer;
  }
</style>
