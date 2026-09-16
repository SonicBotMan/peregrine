// @vitest-environment happy-dom
/**
 * Reactivity regression test (U2): the GUI froze mid-download — rows
 * stuck at their REST snapshot values while the daemon kept moving.
 * Root cause: $state(new Map()) is NOT deeply reactive (Svelte's
 * proxy skips non-plain prototypes), so every fold mutated the map
 * invisibly. The store now uses SvelteMap; this test mounts a real
 * component that derives from store.list (exactly like App.svelte's
 * `visible = $derived(store.list...)`) and asserts that folds, action
 * results and removals all re-render it.
 */
import { describe, expect, it, vi } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import TaskProbe from './ui/TaskProbe.svelte';
import { createStore } from '../src/lib/store.svelte';
import type { Task } from '../src/lib/types';

function fakeTask(over: Partial<Task> = {}): Task {
  const t0 = 1_767_225_600;
  return {
    id: 't1',
    url: 'http://x/f.bin',
    save_path: '/tmp/f.bin',
    status: 'queued',
    priority: 'normal',
    total_bytes: 1000,
    received_bytes: 0,
    error: null,
    created_at: t0,
    updated_at: t0,
    ...over,
  };
}

function rig(rows: Task[] = []) {
  const daemon = {
    list: vi.fn(async () => rows),
    get: vi.fn(async (id: string) => rows.find((r) => r.id === id) ?? rows[0]),
    add: vi.fn(async (t: Partial<Task>) => ({ ...fakeTask(), ...t })),
    pause: vi.fn(async (id: string) => ({ ...fakeTask({ id, status: 'paused' }) })),
    resume: vi.fn(async (id: string) => ({ ...fakeTask({ id, status: 'running' }) })),
    remove: vi.fn(async () => ({ removed: true })),
    segments: vi.fn(async () => []),
    setTaskLimit: vi.fn(async () => fakeTask()),
    getSettings: vi.fn(async () => ({})),
    setGlobalLimit: vi.fn(async () => ({})),
  };
  const stream = { start: vi.fn(), stop: vi.fn() };
  return { daemon, stream };
}

function text(el: HTMLElement) {
  return el.querySelector('[data-testid="rows"]')?.textContent ?? '';
}

describe('store reactivity through a mounted component', () => {
  it('applyEvent(task_progress) re-renders a store.list consumer', async () => {
    const { daemon, stream } = rig([fakeTask({ status: 'running', received_bytes: 100 })]);
    const store = createStore(daemon as never, stream as never);
    const target = document.createElement('div');
    document.body.appendChild(target);
    mount(TaskProbe, { target, props: { store } });

    await vi.waitFor(() => expect(text(target)).toContain('t1:running:100'));

    store.applyEvent({ type: 'task_progress', id: 't1', received: 500, total: 1000 } as never);
    flushSync();
    expect(text(target)).toContain('t1:running:500'); // was frozen at :100 before the fix

    store.applyEvent({ type: 'task_removed', id: 't1' } as never);
    flushSync();
    expect(text(target)).toBe(''); // removals must re-render too
    unmount(document.body.firstElementChild as never);
  });

  it('pause() re-renders a store.list consumer', async () => {
    const { daemon, stream } = rig([fakeTask({ status: 'running' })]);
    const store = createStore(daemon as never, stream as never);
    const target = document.createElement('div');
    document.body.appendChild(target);
    mount(TaskProbe, { target, props: { store } });

    await vi.waitFor(() => expect(text(target)).toContain('t1:running'));

    await store.pause('t1');
    flushSync();
    expect(text(target)).toContain('t1:paused'); // chip was frozen at running before the fix
    unmount(document.body.firstElementChild as never);
  });
});
