/**
 * Store logic tests (pure, no DOM): event folding rules, monotone
 * progress, resync-on-unknown, ordering. The daemon/transport are
 * fakes — this pins the GUI's LOCAL invariants only.
 */
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { createStore } from '../src/lib/store.svelte';
import type { Task } from '../src/lib/types';

function fakeTask(over: Partial<Task> = {}): Task {
  return {
    id: 't1',
    url: 'http://x/f.bin',
    save_path: '/tmp/f.bin',
    status: 'queued',
    priority: 'normal',
    total_bytes: 1000,
    received_bytes: 0,
    error: null,
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    ...over,
  };
}

function rig(rows: Task[] = []) {
  const calls: string[] = [];
  const daemon = {
    list: vi.fn(async () => rows),
    get: vi.fn(async (id: string) => rows.find((r) => r.id === id) ?? fakeTask({ id })),
    add: vi.fn(async (url: string, savePath: string, priority: string) => {
      const t = fakeTask({ url, save_path: savePath, priority: priority as Task['priority'] });
      rows.push(t);
      return t;
    }),
    pause: vi.fn(async (id: string) => {
      const r = rows.find((x) => x.id === id)!;
      r.status = 'paused';
      return { ...r };
    }),
    resume: vi.fn(async (id: string) => {
      const r = rows.find((x) => x.id === id)!;
      r.status = 'queued';
      return { ...r };
    }),
    remove: vi.fn(async () => ({ removed: true })),
  };
  const stream = { start: vi.fn(), stop: vi.fn() };
  calls.push('rig');
  const store = createStore(daemon as never, stream as never);
  return { store, daemon, stream, calls };
}

async function settle() {
  // Let the microtasks (get/list promises) flush.
  for (let i = 0; i < 5; i++) await Promise.resolve();
}

describe('store', () => {
  beforeEach(() => vi.clearAllMocks());

  it('bootstraps from the REST snapshot', async () => {
    const { store } = rig([fakeTask()]);
    await settle();
    expect(store.list).toHaveLength(1);
    expect(store.list[0].id).toBe('t1');
    expect(store.list[0].fraction).toBe(0);
  });

  it('folds progress monotonically and never regresses received', async () => {
    const t = fakeTask({ status: 'running' });
    const { store } = rig([t]);
    await settle();

    store.applyEvent({ type: 'task_progress', id: 't1', received: 500, total: 1000 });
    store.applyEvent({ type: 'task_progress', id: 't1', received: 200, total: 1000 }); // stale frame
    const row = store.list.find((r) => r.id === 't1')!;
    expect(row.received_bytes).toBe(500);
    expect(row.fraction).toBe(0.5);
  });

  it('unknown progress id triggers resync', async () => {
    const { store, daemon } = rig([]);
    await settle();
    expect(daemon.list).toHaveBeenCalledTimes(1);

    store.applyEvent({ type: 'task_progress', id: 'ghost', received: 1, total: 2 });
    await settle();
    expect(daemon.list).toHaveBeenCalledTimes(2);
  });

  it('task_removed drops the row without a round-trip', async () => {
    const { store } = rig([fakeTask()]);
    await settle();
    expect(store.list).toHaveLength(1);

    store.applyEvent({ type: 'task_removed', id: 't1' });
    expect(store.list).toHaveLength(0);
    expect(store.list.find((r) => r.id === 't1')).toBeUndefined();
  });

  it('total unknown → fraction null, not zero', async () => {
    const { store } = rig([fakeTask({ status: 'running', total_bytes: null, received_bytes: 42 })]);
    await settle();
    const row = store.list[0];
    expect(row.fraction).toBeNull();
    expect(row.received_bytes).toBe(42);
  });

  it('status events keep the row and clear speed on non-running', async () => {
    const { store } = rig([fakeTask({ status: 'running', received_bytes: 100 })]);
    await settle();
    store.applyEvent({ type: 'task_status', id: 't1', status: 'paused' });
    const row = store.list.find((r) => r.id === 't1')!;
    expect(row.status).toBe('paused');
    expect(row.speed).toBeNull();
  });

  it('add() optimistically inserts the returned row', async () => {
    const { store } = rig([]);
    await settle();
    await store.add('http://x/new.bin', '/tmp/new.bin', 'high');
    expect(store.list.find((r) => r.url === 'http://x/new.bin')).toBeTruthy();
  });

  it('lists newest first by created_at', async () => {
    const { store } = rig([
      fakeTask({ id: 'old', created_at: '2026-01-01T00:00:00Z' }),
      fakeTask({ id: 'new', created_at: '2026-01-02T00:00:00Z' }),
    ]);
    await settle();
    expect(store.list.map((r) => r.id)).toEqual(['new', 'old']);
  });
});
