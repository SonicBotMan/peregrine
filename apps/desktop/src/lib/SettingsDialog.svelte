<script lang="ts">
  /**
   * Settings center (GUI-verify batch-3): one surface for every knob
   * that used to live scattered — status-bar limit select, File-menu
   * autostart toggle, theme toggle, per-task Browse. Layout follows
   * the Motrix/FDM convention: category rail left, settings right.
   *
   * Persistence is split by ownership:
   * - daemon  : default_dir, global_limit_bps, max_concurrent,
   *             seg_conns, user_agent → PUT /settings (partial,
   *             applies live)
   * - client  : theme mode, completion notification, clipboard watch
   *             (localStorage)
   * - system  : launch at login (autostart plugin)
   * Every change saves immediately — no submit button — with a
   * toast, matching the status-bar limit selector's contract.
   */
  import { Select } from 'bits-ui';
  import { X, SlidersHorizontal, Download, FolderOpen, Bell, Power, ClipboardCheck, Palette, Gauge, Layers, Type } from '@lucide/svelte';

  let {
    onClose,
    defaultDir,
    globalLimit,
    maxConcurrent,
    segConns,
    userAgent,
    themeMode,
    notifyEnabled,
    clipWatchEnabled,
    isDesktopShell,
    onDefaultDir,
    onGlobalLimit,
    onMaxConcurrent,
    onSegConns,
    onLaunchAtLogin,
    launchAtLogin,
    onThemeMode,
    onNotifyToggle,
    onClipWatchToggle,
    onUserAgent,
  }: {
    onClose: () => void;
    defaultDir: string;
    globalLimit: number | null;
    maxConcurrent: number;
    segConns: number;
    userAgent: string;
    themeMode: 'auto' | 'dark' | 'light';
    notifyEnabled: boolean;
    clipWatchEnabled: boolean;
    isDesktopShell: boolean;
    onDefaultDir: (dir: string) => void;
    onGlobalLimit: (bps: number | null) => void;
    onMaxConcurrent: (n: number) => void;
    onSegConns: (n: number) => void;
    onLaunchAtLogin: () => void;
    launchAtLogin: boolean;
    onThemeMode: (m: 'auto' | 'dark' | 'light') => void;
    onNotifyToggle: (on: boolean) => void;
    onClipWatchToggle: (on: boolean) => void;
    onUserAgent: (ua: string) => void;
  } = $props();

  let section = $state<'general' | 'download'>('general');
  let picking = $state(false);
  let dirDraft = $state(defaultDir);

  // Keep the draft in sync when the daemon value changes underneath.
  $effect(() => {
    dirDraft = defaultDir;
  });

  // Custom User-Agent draft (batch-3): syncs from the prop, commits
  // on change/blur via onUserAgent.
  let uaDraft = $state(userAgent);
  $effect(() => {
    uaDraft = userAgent;
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

  const LIMIT_OPTIONS = [
    { value: 'none', label: 'Unlimited' },
    { value: String(512 * 1024), label: '512 KB/s' },
    { value: String(1024 * 1024), label: '1 MB/s' },
    { value: String(2 * 1024 * 1024), label: '2 MB/s' },
    { value: String(4 * 1024 * 1024), label: '4 MB/s' },
  ];
  const limitValue = $derived(
    globalLimit === null ? 'none' : String(globalLimit),
  );
  const limitLabel = $derived(
    LIMIT_OPTIONS.find((o) => o.value === limitValue)?.label ??
      (globalLimit === null ? 'Unlimited' : `${Math.round(globalLimit / 1024)} KB/s`),
  );

  const SEG_OPTIONS = [4, 8, 16, 32];
  const THEMES = [
    { value: 'auto', label: 'Follow system' },
    { value: 'dark', label: 'Dark' },
    { value: 'light', label: 'Light' },
  ] as const;
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
    <header class="head">
      <h2>Settings</h2>
      <button class="close" title="Close" aria-label="Close settings" onclick={onClose}>
        <X size={14} />
      </button>
    </header>

    <div class="body">
      <nav class="cats" aria-label="Settings categories">
        <button
          class="cat"
          class:active={section === 'general'}
          onclick={() => (section = 'general')}
        >
          <SlidersHorizontal size={13} />
          General
        </button>
        <button
          class="cat"
          class:active={section === 'download'}
          onclick={() => (section = 'download')}
        >
          <Download size={13} />
          Download
        </button>
      </nav>

      {#if section === 'general'}
        <div class="pane">
          <div class="group">
            <p class="grouplab">Appearance</p>
            <div class="srow">
              <div class="stext">
                <span class="sname"><Palette size={13} /> Theme</span>
                <span class="sdesc">Follow the OS, or pin a look</span>
              </div>
              <div class="seg" role="radiogroup" aria-label="Theme">
                {#each THEMES as t (t.value)}
                  <button
                    role="radio"
                    aria-checked={themeMode === t.value}
                    class="segbtn"
                    class:on={themeMode === t.value}
                    onclick={() => onThemeMode(t.value)}
                  >
                    {t.label}
                  </button>
                {/each}
              </div>
            </div>
            <div class="srow">
              <div class="stext">
                <span class="sname"><Bell size={13} /> Completion notification</span>
                <span class="sdesc">System notification when a download finishes</span>
              </div>
              <button
                class="switch"
                role="switch"
                aria-checked={notifyEnabled}
                class:on={notifyEnabled}
                onclick={() => onNotifyToggle(!notifyEnabled)}
              >
                <span class="knob"></span>
              </button>
            </div>
            <div class="srow">
              <div class="stext">
                <span class="sname"><ClipboardCheck size={13} /> Clipboard link detection</span>
                <span class="sdesc">Offer to download links you copy — never auto-adds</span>
              </div>
              <button
                class="switch"
                role="switch"
                aria-checked={clipWatchEnabled}
                class:on={clipWatchEnabled}
                onclick={() => onClipWatchToggle(!clipWatchEnabled)}
              >
                <span class="knob"></span>
              </button>
            </div>
          </div>

          <div class="group">
            <p class="grouplab">System</p>
            <div class="srow">
              <div class="stext">
                <span class="sname"><Power size={13} /> Launch at login</span>
                <span class="sdesc">Start Peregrine when you sign in</span>
              </div>
              {#if isDesktopShell}
                <button
                  class="switch"
                  role="switch"
                  aria-checked={launchAtLogin}
                  class:on={launchAtLogin}
                  onclick={() => onLaunchAtLogin()}
                >
                  <span class="knob"></span>
                </button>
              {:else}
                <span class="na">Desktop only</span>
              {/if}
            </div>
          </div>
        </div>
      {:else}
        <div class="pane">
          <div class="group">
            <p class="grouplab">Destination</p>
            <div class="srow col">
              <div class="stext">
                <span class="sname"><FolderOpen size={13} /> Default save directory</span>
                <span class="sdesc">~ expands against the daemon's home directory</span>
              </div>
              <span class="path-row">
                <input
                  class="tin"
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
            </div>
          </div>

          <div class="group">
            <p class="grouplab">Speed & connections</p>
            <div class="srow">
              <div class="stext">
                <span class="sname"><Gauge size={13} /> Global speed limit</span>
                <span class="sdesc">Applies across all active downloads</span>
              </div>
              <Select.Root
                type="single"
                value={limitValue}
                onValueChange={(v) => onGlobalLimit(v === 'none' ? null : Number(v))}
              >
                <Select.Trigger class="sel" aria-label="Global speed limit">
                  {limitLabel}
                </Select.Trigger>
                <Select.Content class="selmenu">
                  {#each LIMIT_OPTIONS as o (o.value)}
                    <Select.Item value={o.value} label={o.label} class="selitem" />
                  {/each}
                </Select.Content>
              </Select.Root>
            </div>
            <div class="srow">
              <div class="stext">
                <span class="sname"><Layers size={13} /> Max parallel tasks</span>
                <span class="sdesc">New tasks queue beyond this many running</span>
              </div>
              <input
                class="tin num"
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
            </div>
            <div class="srow">
              <div class="stext">
                <span class="sname"><Type size={13} /> Connections per download</span>
                <span class="sdesc">Applies to NEW downloads; running tasks keep their plan</span>
              </div>
              <Select.Root
                type="single"
                value={String(segConns)}
                onValueChange={(v) => onSegConns(Number(v))}
              >
                <Select.Trigger class="sel" aria-label="Connections per download">
                  {segConns}
                </Select.Trigger>
                <Select.Content class="selmenu">
                  {#each SEG_OPTIONS as n (n)}
                    <Select.Item value={String(n)} label={String(n)} class="selitem" />
                  {/each}
                </Select.Content>
              </Select.Root>
            </div>
            <div class="srow col">
              <div class="stext">
                <span class="sname"><Type size={13} /> HTTP User-Agent</span>
                <span class="sdesc">Custom header for engine requests — empty = peregrine/&lt;version&gt;</span>
              </div>
              <input
                class="tin"
                bind:value={uaDraft}
                data-ua="1"
                placeholder="peregrine/<version>"
                onchange={() => onUserAgent(uaDraft.trim())}
              />
            </div>
          </div>
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  .settings {
    width: min(700px, 94vw);
    padding: 0;
    overflow: hidden;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 13px 18px 11px;
    border-bottom: 1px solid var(--line-subtle);
  }
  .head h2 {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
    letter-spacing: 0.02em;
    color: var(--text);
  }
  .close {
    background: transparent;
    border: 0;
    color: var(--dim);
    cursor: pointer;
    padding: 4px;
    border-radius: 5px;
    display: inline-flex;
  }
  .close:hover {
    color: var(--text);
    background: var(--hover-tint);
  }
  .body {
    display: flex;
    min-height: 300px;
  }
  /* ---- category rail ---- */
  .cats {
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: 150px;
    flex-shrink: 0;
    padding: 12px 8px;
    border-right: 1px solid var(--line-subtle);
    background: var(--chrome-1);
  }
  .cat {
    display: flex;
    align-items: center;
    gap: 8px;
    text-align: left;
    background: transparent;
    border: 0;
    border-left: 2px solid transparent;
    color: var(--dim);
    font-size: 12.5px;
    padding: 7px 10px;
    border-radius: 6px;
    cursor: pointer;
  }
  .cat:hover {
    color: var(--text);
    background: var(--hover-tint);
  }
  .cat.active {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border-left-color: var(--accent);
  }
  /* ---- rows ---- */
  .pane {
    flex: 1;
    padding: 6px 20px 14px;
    overflow-y: auto;
    max-height: 380px;
  }
  .group + .group {
    margin-top: 6px;
    border-top: 1px solid var(--line-subtle);
    padding-top: 10px;
  }
  .grouplab {
    margin: 8px 0 2px;
    font-size: 10.5px;
    font-weight: 600;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--dim);
    opacity: 0.75;
  }
  .srow {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    min-height: 46px;
    border-bottom: 1px solid var(--line-subtle);
  }
  .group .srow:last-child {
    border-bottom: 0;
  }
  .srow.col {
    flex-direction: column;
    align-items: stretch;
    gap: 8px;
  }
  .stext {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .sname {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 12.5px;
    color: var(--text);
  }
  .sdesc {
    font-size: 11px;
    color: var(--dim);
    line-height: 1.45;
  }
  .na {
    font-size: 11px;
    color: var(--dim);
  }
  /* ---- controls ---- */
  .tin {
    background: var(--elevated);
    border: 1px solid var(--line);
    border-radius: 6px;
    color: var(--text);
    font-size: 12.5px;
    padding: 6px 9px;
    width: 100%;
    box-shadow: inset 0 1px 0 var(--inset-hl), var(--inset-shade) 0 1px 1px inset;
  }
  .tin:focus-visible {
    outline: none;
    border-color: var(--accent);
  }
  .path-row {
    display: flex;
    gap: 6px;
    align-items: stretch;
    width: 100%;
  }
  .path-row .tin {
    flex: 1;
  }
  .browse {
    border: 1px solid var(--line);
    background: var(--elevated);
    color: var(--text);
    border-radius: 6px;
    padding: 0 12px;
    font-size: 12px;
    cursor: pointer;
    white-space: nowrap;
    box-shadow: 0 1px 1px var(--shade-1), inset 0 1px 0 var(--inset-hl);
  }
  .browse:hover:not(:disabled) {
    border-color: var(--accent);
    color: var(--accent);
  }
  .browse:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .num {
    width: 84px;
    text-align: center;
  }
  /* ---- theme segmented control ---- */
  .seg {
    display: inline-flex;
    background: var(--elevated);
    border: 1px solid var(--line);
    border-radius: 7px;
    padding: 2px;
    gap: 2px;
  }
  .segbtn {
    border: 0;
    background: transparent;
    color: var(--dim);
    font-size: 11.5px;
    padding: 4px 10px;
    border-radius: 5px;
    cursor: pointer;
    white-space: nowrap;
  }
  .segbtn:hover {
    color: var(--text);
  }
  .segbtn.on {
    background: var(--accent);
    color: var(--on-accent);
    font-weight: 550;
  }
  /* ---- switch ---- */
  .switch {
    position: relative;
    width: 34px;
    height: 19px;
    flex-shrink: 0;
    border: 0;
    border-radius: 10px;
    background: var(--line-strong);
    cursor: pointer;
    transition: background var(--dur) var(--ease);
    padding: 0;
  }
  .switch.on {
    background: var(--accent);
  }
  .knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 15px;
    height: 15px;
    border-radius: 50%;
    background: var(--text);
    transition: transform var(--dur) var(--ease);
    box-shadow: 0 1px 2px var(--shade-2);
  }
  .switch.on .knob {
    transform: translateX(15px);
    background: var(--on-accent);
  }
  .switch:focus-visible {
    outline: 1.5px solid var(--accent);
    outline-offset: 2px;
  }
  /* ---- bits-ui select ---- */
  .sel {
    display: inline-flex;
    align-items: center;
    justify-content: space-between;
    min-width: 148px;
    background: var(--elevated);
    border: 1px solid var(--line);
    border-radius: 6px;
    color: var(--text);
    font-size: 12.5px;
    padding: 5px 10px;
    cursor: pointer;
    box-shadow: inset 0 1px 0 var(--inset-hl), var(--inset-shade) 0 1px 1px inset;
  }
  .sel:hover {
    border-color: var(--line-strong);
  }
  .sel:focus-visible,
  .sel[data-state='open'] {
    outline: none;
    border-color: var(--accent);
  }
  .selmenu {
    background: var(--elevated);
    border: 1px solid var(--line);
    border-radius: 7px;
    padding: 4px;
    box-shadow: 0 8px 24px var(--shade-2), 0 2px 6px var(--shade-1);
    min-width: var(--bits-select-anchor-width, 148px);
    z-index: 60;
  }
  .selitem {
    font-size: 12.5px;
    color: var(--text);
    padding: 6px 10px;
    border-radius: 5px;
    cursor: pointer;
  }
  .selitem[data-highlighted] {
    background: var(--hover-tint);
    color: var(--text);
  }
  .selitem[data-selected] {
    color: var(--accent);
    font-weight: 550;
  }
</style>
