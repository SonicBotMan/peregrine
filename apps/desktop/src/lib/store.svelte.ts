/**
 * The GUI's single source of task truth (Svelte 5 runes).
 *
 * Data flow (thin-client contract):
 *   REST snapshot (truth)  ──►  tasks: Map<id, TaskView>
 *   WS events (deltas)     ──►  applyEvent() folds into the map
 *   unknown id / reconnect ──►  resync() re-snapshots
 *
 * Speed is NOT on the wire for a single task — it is derived here
 * from consecutive progress deltas (EMA, ~1s half-life), which keeps
 * the daemon protocol minimal and the GUI free to present.
 */
import { writable } from 'svelte/store';
import type { Daemon, EventStream } from './daemon';
import type { EngineEvent, Task } from './types';

export interface TaskView extends Task {
  /** Derived: EMA bytes/sec (null before two samples). */
  speed: number | null;
  /** Derived: 0..1, null when total unknown. */
  fraction: number | null;
}

export type Conn = 'connecting' | 'live' | 'down';

function view(t: Task, prev?: TaskView): TaskView {
  const received = Math.max(t.received_bytes, prev?.received_bytes ?? 0);
  const dt = prev
    ? (new Date(t.updated_at).getTime() - new Date(prev.updated_at).getTime()) / 1000
    : 0;
  // EMA with ~1s half-life; only fold positive samples inside a
  // sane window: a huge dt (REST snapshot → first live frame) would
  // compute a nonsense instantaneous speed, and a tiny dt a huge
  // one — clamp both sides and skip folding outside the window.
  let speed = prev?.speed ?? null;
  if (prev && t.status === 'running') {
    if (dt >= 0.1 && dt <= 5 && received > prev.received_bytes) {
      const inst = (received - prev.received_bytes) / dt;
      const alpha = 1 - 0.5 ** dt;
      speed = speed === null ? inst : speed + alpha * (inst - speed);
    }
  } else {
    speed = null;
  }
  return {
    ...t,
    received_bytes: received,
    speed,
    fraction:
      t.total_bytes && t.total_bytes > 0
        ? Math.min(1, received / t.total_bytes)
        : null,
  };
}

export function createStore(daemon: Daemon, stream: EventStream) {
  let tasks = $state(new Map<string, TaskView>());
  const conn = writable<Conn>('connecting');

  async function resync() {
    try {
      const rows = await daemon.list();
      const next = new Map<string, TaskView>();
      for (const t of rows) next.set(t.id, view(t, tasks.get(t.id)));
      tasks = next;
      conn.set('live');
    } catch {
      conn.set('down');
    }
  }

  function applyEvent(e: EngineEvent) {
    switch (e.type) {
      case 'task_added': {
        // No full row on this frame — fetch it (cheap, rare).
        void daemon
          .get(e.id)
          .then((t) => tasks.set(t.id, view(t)))
          .catch(() => resync());
        break;
      }
      case 'task_progress': {
        const cur = tasks.get(e.id);
        if (!cur) {
          void resync();
          break;
        }
        const now = new Date().toISOString();
        tasks.set(
          e.id,
          view(
            {
              ...cur,
              received_bytes: Math.max(cur.received_bytes, e.received),
              total_bytes: e.total ?? cur.total_bytes,
              updated_at: now,
            },
            cur,
          ),
        );
        break;
      }
      case 'task_status': {
        const cur = tasks.get(e.id);
        if (!cur) {
          void resync();
          break;
        }
        tasks.set(
          e.id,
          view({ ...cur, status: e.status, error: e.error ?? null, updated_at: new Date().toISOString() }, cur),
        );
        break;
      }
      case 'task_removed': {
        tasks.delete(e.id);
        break;
      }
      case 'task_limit_changed': {
        // Another window (or MCP, M5) changed the throttle — fold it
        // in or this tab's select would keep showing a stale value.
        const cur = tasks.get(e.id);
        if (!cur) {
          void resync();
          break;
        }
        tasks.set(
          e.id,
          view({ ...cur, speed_limit_bps: e.speed_limit_bps, updated_at: new Date().toISOString() }, cur),
        );
        break;
      }
    }
  }

  stream.start();
  void resync();

  return {
    conn: { subscribe: conn.subscribe },
    get list(): TaskView[] {
      // Newest first; stable within equal timestamps by id.
      return [...tasks.values()].sort(
        (a, b) => new Date(b.created_at).getTime() - new Date(a.created_at).getTime(),
      );
    },
    applyEvent,
    resync,
    async add(url: string, savePath: string, priority: 'low' | 'normal' | 'high') {
      const t = await daemon.add(url, savePath, priority);
      tasks.set(t.id, view(t));
    },
    async pause(id: string) {
      const t = await daemon.pause(id);
      tasks.set(id, view(t, tasks.get(id)));
    },
    async resume(id: string) {
      const t = await daemon.resume(id);
      tasks.set(id, view(t, tasks.get(id)));
    },
    async remove(id: string) {
      await daemon.remove(id);
      tasks.delete(id);
    },
    /** Optimistic: the daemon echoes the full Task back, so a
     * success replaces the row with truth (no rollback window). A
     * failure leaves the row untouched — the select re-reads the
     * still-old value on the next render. */
    async setTaskLimit(id: string, bps: number) {
      const t = await daemon.setTaskLimit(id, bps);
      tasks.set(id, view(t, tasks.get(id)));
    },
    async setGlobalLimit(bps: number) {
      return daemon.setGlobalLimit(bps);
    },
    async getSettings() {
      return daemon.getSettings();
    },
  };
}

export type TaskStore = ReturnType<typeof createStore>;
