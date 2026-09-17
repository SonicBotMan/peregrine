<script lang="ts">
  /**
   * Keyboard cheat sheet (U3): "?" opens, Esc/overlay-click closes.
   * Mirrors the handlers in App.svelte's window keydown layer — keep
   * the two lists in sync when adding bindings.
   */
  let { onClose }: { onClose: () => void } = $props();
  const groups: { title: string; rows: [string, string][] }[] = [
    {
      title: 'Global',
      rows: [
        ['⌘K / Ctrl K', 'Command palette'],
        ['⌘N / Ctrl N', 'New download'],
        ['/', 'Command palette (search tasks)'],
        ['?', 'This cheat sheet'],
      ],
    },
    {
      title: 'Task (row selected)',
      rows: [
        ['j / k', 'Move selection down / up'],
        ['Space', 'Pause / resume'],
        ['Del / ⌫', 'Remove (with undo window)'],
        ['Enter', 'Select / expand details'],
        ['Double-click', 'Open completed file'],
        ['Right-click', 'Context menu'],
      ],
    },
    {
      title: 'Panel',
      rows: [
        ['↑ ↓', 'Move palette selection'],
        ['Enter', 'Run palette item'],
        ['Esc', 'Close overlay'],
      ],
    },
  ];
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onClose()} />

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="overlay" onclick={onClose}>
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class="sheet" role="dialog" aria-label="Keyboard shortcuts" onclick={(e) => e.stopPropagation()}>
    <h2>Keyboard shortcuts</h2>
    {#each groups as g}
      <h3>{g.title}</h3>
      <table>
        {#each g.rows as [k, v]}
          <tr>
            <td><kbd>{k}</kbd></td>
            <td>{v}</td>
          </tr>
        {/each}
      </table>
    {/each}
  </div>
</div>

<style>
  .sheet {
    width: min(460px, calc(100vw - 32px));
    background: var(--elevated);
    border: 1px solid var(--line-strong);
    border-radius: 8px;
    padding: 18px 22px;
    box-shadow:
      0 24px 64px rgb(0 0 0 / 0.35),
      0 4px 16px rgb(0 0 0 / 0.25);
    animation: sheet-in var(--dur, 200ms) var(--ease, ease-out);
  }
  h2 {
    font-size: 14px;
    margin: 0 0 12px;
  }
  h3 {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--dim);
    margin: 14px 0 6px;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 12.5px;
  }
  td {
    padding: 4px 0;
    vertical-align: top;
  }
  td:first-child {
    width: 130px;
    white-space: nowrap;
  }
  kbd {
    display: inline-block;
    padding: 1px 7px;
    border: 1px solid var(--line-strong);
    border-bottom-width: 2px;
    border-radius: 5px;
    background: var(--panel);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 11.5px;
  }
  @keyframes sheet-in {
    from {
      opacity: 0;
      transform: translateY(-6px) scale(0.99);
    }
  }
</style>
