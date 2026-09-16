<!-- Minimal consumer that mirrors how App.svelte reads the store:
     a $derived over store.list re-rendered into the DOM. Exists only
     for the reactivity regression test (U2: rows froze mid-download). -->
<script lang="ts">
  import type { TaskStore } from '../../src/lib/store.svelte';
  let { store }: { store: TaskStore } = $props();
  const rows = $derived(store.list.map((t) => `${t.id}:${t.status}:${t.received_bytes}`).join('|'));
</script>
<p data-testid="rows">{rows}</p>
