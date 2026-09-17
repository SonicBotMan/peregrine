<script lang="ts">
  /**
   * V5 task row: a 38px dense table line, not a media card. The
   * progress ring is gone — a slim inline bar + tabular columns
   * carry the same information at tool density. Interaction
   * surface is unchanged: click/Enter select (and expand the
   * SegmentPanel), Space belongs to the global layer, double-click
   * a completed row opens the artifact, right-click opens the
   * context menu.
   */
  import { ContextMenu } from 'bits-ui';
  import { formatBytes, formatBps, formatEta } from './format';
  import type { TaskView } from './store.svelte';
  import { Pause, Play, X } from '@lucide/svelte';
  import {
    FileArchive,
    FileVideo,
    FileAudio,
    FileImage,
    FileText,
    Binary,
  } from '@lucide/svelte';

  // file-type glyph: a 15px outlined lucide icon, tinted by kind —
  // recognizable semantics (archive≠video≠image) without a full
  // 32px file-icon system. Letter tiles (B/A/V) failed VLM review:
  // nobody knows what the letters mean.
  const FILE_KINDS: Record<
    string,
    { icon: typeof FileArchive; color: string }
  > = {
    archive: { icon: FileArchive, color: 'var(--warn)' }, // zip tar gz 7z
    video: { icon: FileVideo, color: 'var(--err)' }, // mp4 mkv avi
    audio: { icon: FileAudio, color: 'var(--accent)' }, // mp3 flac
    image: { icon: FileImage, color: 'var(--ok)' }, // png jpg svg
    doc: { icon: FileText, color: 'var(--info)' }, // pdf epub txt md
    bin: { icon: Binary, color: 'var(--dim)' }, // exe iso bin dat
  };
  const kind = $derived.by(() => {
    const ext = name.split('.').pop()?.toLowerCase() ?? '';
    if (['zip', 'tar', 'gz', '7z', 'rar', 'xz', 'bz2'].includes(ext)) return FILE_KINDS.archive;
    if (['mp4', 'mkv', 'avi', 'mov', 'webm', 'flv'].includes(ext)) return FILE_KINDS.video;
    if (['mp3', 'flac', 'wav', 'ogg', 'm4a', 'opus'].includes(ext)) return FILE_KINDS.audio;
    if (['png', 'jpg', 'jpeg', 'gif', 'svg', 'webp', 'bmp'].includes(ext)) return FILE_KINDS.image;
    if (['pdf', 'epub', 'txt', 'md', 'doc', 'docx', 'mobi'].includes(ext)) return FILE_KINDS.doc;
    if (['exe', 'msi', 'iso', 'bin', 'dat', 'dmg', 'appimage'].includes(ext)) return FILE_KINDS.bin;
    return null;
  });

  let {
    task,
    daemon,
    selected = false,
    onPause,
    onResume,
    onRemove,
    onLimit,
    onSelect,
    onOpenFile,
    onOpenSaved,
    onCopyUrl,
  }: {
    task: TaskView;
    daemon: import('./daemon').Daemon;
    selected?: boolean;
    onPause: (id: string) => void;
    onResume: (id: string) => void;
    onRemove: (id: string) => void;
    onLimit: (id: string, bps: number) => void;
    onSelect: (id: string) => void;
    onOpenFile: (id: string) => void;
    onOpenSaved: (id: string) => void;
    onCopyUrl: (id: string) => void;
  } = $props();

  const name = $derived(task.url.split('/').filter(Boolean).pop() ?? task.url);
  const pct = $derived(task.fraction !== null ? Math.round(task.fraction * 100) : null);
  const done = $derived(formatBytes(task.received_bytes));
  const size = $derived(formatBytes(task.total_bytes));
  const speed = $derived(
    task.speed !== null && task.status === 'running' ? formatBps(task.speed) : '—',
  );
  const eta = $derived(
    task.status === 'running' &&
      task.total_bytes !== null &&
      task.speed !== null &&
      task.speed > 0
      ? formatEta(task.total_bytes - task.received_bytes, task.speed)
      : '—',
  );
  const running = $derived(task.status === 'running' || task.status === 'queued');
  const resumable = $derived(task.status === 'paused' || task.status === 'failed');

  function stop<T extends () => void>(fn: T) {
    return (e: Event) => {
      e.stopPropagation();
      fn();
    };
  }
</script>

<ContextMenu.Root>
  <ContextMenu.Trigger>
    {#snippet child({ props })}
      <!-- svelte-ignore a11y_no_noninteractive_element_interactions, a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
      <div
        {...props}
        class="row"
        class:selected
        data-id={task.id}
        data-status={task.status}
        role="button"
        tabindex={0}
        aria-pressed={selected}
        aria-label={`${name} — ${task.status}`}
        title={task.error ? `${task.error}\n${task.url}` : task.url}
        onclick={() => onSelect(task.id)}
        ondblclick={() => task.status === 'completed' && onOpenSaved(task.id)}
        onkeydown={(e) => {
          // Enter only: Space belongs to the global layer (pause/
          // resume selected). Buttons keep native Space activation
          // (exempted in App.onKeydown).
          if (e.key === 'Enter') {
            e.preventDefault();
            onSelect(task.id);
          }
        }}
      >
        <span class="c namecell">
          {#if kind}
            {@const Icon = kind.icon}
            <span class="ftype" aria-hidden="true" style="--k: {kind.color}">
              <Icon size={14} stroke-width={1.75} />
            </span>
          {/if}
          <span class="name" title={name}>{name}</span>
        </span>

        <span class="c progress" aria-hidden="true">
          {#if task.status === 'completed'}
            <svg class="okmark" viewBox="0 0 16 16" width="14" height="14">
              <path d="M3 8.5l3.2 3.2L13 5" fill="none" stroke="var(--ok)" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
            </svg>
          {:else if task.status === 'failed' && task.error}
            <span class="errsum" title={task.error}>{task.error}</span>
          {:else}
            <span class="bar"><span class="fill" data-status={task.status} style="width:{pct ?? 0}%"></span></span>
            <span class="pct">{pct !== null ? `${pct}%` : '—'}</span>
          {/if}
        </span>

        <span class="c num size-col">{done}{size !== '—' ? ` / ${size}` : ''}</span>
        <span class="c num speed-col">{speed}</span>
        <span class="c num eta-col">{eta}</span>

        <span class="c status"><i></i>{task.status}</span>

        <span class="c acts" role="group" aria-label="Row actions">
          {#if running}
            <button class="mini" title="Pause" aria-label={`Pause ${name}`} onclick={stop(() => onPause(task.id))}>
              <Pause size={12} />
            </button>
          {:else if resumable}
            <button
              class="mini"
              title={task.status === 'failed' ? 'Retry' : 'Resume'}
              aria-label={`${task.status === 'failed' ? 'Retry' : 'Resume'} ${name}`}
              onclick={stop(() => onResume(task.id))}
            >
              <Play size={12} />
            </button>
          {/if}
          <button class="mini" title="Remove" aria-label={`Remove ${name}`} onclick={stop(() => onRemove(task.id))}>
            <X size={12} />
          </button>
        </span>
      </div>
    {/snippet}
  </ContextMenu.Trigger>

  <ContextMenu.Content class="ctx">
    {#if running}
      <ContextMenu.Item class="ctx-item" onSelect={() => onPause(task.id)}>Pause</ContextMenu.Item>
    {/if}
    {#if resumable}
      <ContextMenu.Item class="ctx-item" onSelect={() => onResume(task.id)}>
        {task.status === 'failed' ? 'Retry' : 'Resume'}
      </ContextMenu.Item>
    {/if}
    <ContextMenu.Item class="ctx-item" onSelect={() => onCopyUrl(task.id)}>Copy URL</ContextMenu.Item>
    {#if task.status === 'completed'}
      <ContextMenu.Item class="ctx-item" onSelect={() => onOpenSaved(task.id)}>
        Open file
      </ContextMenu.Item>
      <ContextMenu.Item class="ctx-item" onSelect={() => onOpenFile(task.id)}>
        Show in folder
      </ContextMenu.Item>
    {/if}
    <ContextMenu.Separator class="ctx-sep" />
    <ContextMenu.Item class="ctx-item danger" onSelect={() => onRemove(task.id)}>
      Remove
    </ContextMenu.Item>
  </ContextMenu.Content>
</ContextMenu.Root>

<style>
  /* V5: dense table line — one row = one glance, columns are
   * right-aligned tabular numbers like a real transfer table. */
  .row {
    display: grid;
    grid-template-columns:
      minmax(0, 1fr)
      150px
      128px
      86px
      72px
      92px
      64px;
    align-items: center;
    height: 42px;
    padding: 0 14px;
    column-gap: 14px;
    border-bottom: 1px solid var(--row-line);
    cursor: default;
    user-select: none;
    font-size: 12.5px;
  }
  .row:nth-child(even) {
    background: var(--zebra-tint);
  }
  .row:hover {
    background: var(--hover-tint);
  }
  .row:focus-visible {
    outline: 1px solid var(--accent);
    outline-offset: -1px;
  }
  .row.selected {
    background: color-mix(in oklch, var(--accent) 30%, transparent);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  :root[data-theme='light'] .row.selected {
    /* 30% of a 54%-light accent over white reads washed-out pink;
     * light wants a lighter tint + a hairline so selection still
     * snaps without darkening the row. */
    background: color-mix(in oklch, var(--accent) 15%, transparent);
    box-shadow: inset 2px 0 0 var(--accent), inset 0 0 0 1px color-mix(in oklch, var(--accent) 25%, transparent);
  }
  .row[data-status='failed'] {
    background: color-mix(in srgb, var(--err) 5%, transparent);
  }
  .row[data-status='failed'].selected {
    background: color-mix(in srgb, var(--err) 12%, transparent);
    box-shadow: inset 2px 0 0 var(--err);
  }
  /* light: err-tinted rows read as pastel pink on white; a red
   * hairline carries 'failed' without painting the row — keep the
   * hairline faint (35%) so it reads as a cue, not a siren. */
  /* light: status pills wash out at 12%/32% mix — deepen fill so
   * the semantic color survives on white without going neon. */
  :root[data-theme='light'] .status {
    background: color-mix(in srgb, currentColor 16%, transparent);
    border-color: color-mix(in srgb, currentColor 40%, transparent);
  }
  :root[data-theme='light'] .row[data-status='failed'] {
    background: transparent;
    box-shadow: inset 2px 0 0 color-mix(in oklch, var(--err) 35%, transparent);
  }
  :root[data-theme='light'] .row[data-status='failed'].selected {
    background: color-mix(in oklch, var(--err) 8%, transparent);
    box-shadow: inset 2px 0 0 var(--err);
  }

  .c {
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .namecell {
    display: flex;
    align-items: center;
    min-width: 0;
  }
  .ftype {
    width: 21px;
    height: 21px;
    border-radius: 5px;
    flex: none;
    display: grid;
    place-items: center;
    color: var(--k);
    background: color-mix(in srgb, var(--k) 11%, transparent);
    border: 1px solid color-mix(in srgb, var(--k) 26%, transparent);
    margin-right: 8px;
  }
  .name {
    font-weight: 550;
    color: var(--text);
  }

  /* progress column: slim bar + percent */
  .progress {
    display: flex;
    align-items: center;
    gap: 7px;
  }
  .bar {
    flex: 1;
    height: 4px;
    border-radius: 2px;
    background: var(--bar-track);
    overflow: hidden;
  }
  .fill {
    display: block;
    height: 100%;
    border-radius: 2px;
    background: var(--accent);
    transition: width 0.35s linear;
  }
  .fill[data-status='completed'] { background: var(--ok); }
  .fill[data-status='paused'] { background: var(--warn); }
  .fill[data-status='failed'] { background: var(--err); }
  .errsum {
    font-size: 11px;
    color: var(--err);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .fill[data-status='queued'] { background: var(--info); }
  .pct {
    font-size: 11px;
    color: var(--dim);
    font-variant-numeric: tabular-nums;
    min-width: 34px;
    text-align: right;
  }
  .okmark { flex: none; }

  .num {
    text-align: right;
    color: var(--dim);
    font-variant-numeric: tabular-nums;
    font-size: 12px;
  }
  .row[data-status='running'] .speed-col {
    color: var(--text);
  }

  .status {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    justify-content: flex-end;
    font-size: 11px;
    color: var(--dim);
    text-transform: capitalize;
    border-radius: 999px;
    padding: 2px 9px;
    border: 1px solid transparent;
    background: transparent;
    justify-self: end;
  }
  .status i {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: currentColor;
    flex: none;
  }
  .row[data-status='running'] .status {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border-color: color-mix(in srgb, var(--accent) 32%, transparent);
  }
  .row[data-status='running'] .status i { background: var(--accent); }
  .row[data-status='queued'] .status {
    color: var(--info);
    background: color-mix(in srgb, var(--info) 12%, transparent);
    border-color: color-mix(in srgb, var(--info) 32%, transparent);
  }
  .row[data-status='queued'] .status i { background: var(--info); }
  .row[data-status='completed'] .status {
    color: var(--ok);
    background: color-mix(in srgb, var(--ok) 12%, transparent);
    border-color: color-mix(in srgb, var(--ok) 32%, transparent);
  }
  .row[data-status='completed'] .status i { background: var(--ok); }
  .row[data-status='paused'] .status {
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 12%, transparent);
    border-color: color-mix(in srgb, var(--warn) 32%, transparent);
  }
  .row[data-status='paused'] .status i { background: var(--warn); }
  .row[data-status='failed'] .status {
    color: var(--err);
    background: color-mix(in srgb, var(--err) 12%, transparent);
    border-color: color-mix(in srgb, var(--err) 32%, transparent);
  }
  /* light: saturated err on white turns the pill bubblegum-pink;
   * halve the chrome, keep the dot+text signal. */
  :root[data-theme='light'] .row[data-status='failed'] .status {
    background: color-mix(in srgb, var(--err) 6%, transparent);
    border-color: color-mix(in srgb, var(--err) 22%, transparent);
  }
  .row[data-status='failed'] .status i { background: var(--err); }

  /* hover actions — quiet mini buttons */
  .acts {
    display: flex;
    gap: 4px;
    justify-content: flex-end;
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--dur) var(--ease);
  }
  .row:hover .acts,
  .row:focus-within .acts,
  .row.selected .acts {
    opacity: 1;
    pointer-events: auto;
  }
  .mini {
    width: 22px;
    height: 22px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    border-radius: 4px;
    background: var(--elevated);
    border: 1px solid var(--line);
    color: var(--dim);
    line-height: 0;
  }
  .mini:hover {
    color: var(--text);
    border-color: var(--accent);
    background: var(--panel);
  }
</style>
