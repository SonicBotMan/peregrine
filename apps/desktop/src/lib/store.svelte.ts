/**
 * The GUI's single source of task truth (Svelte 5 runes).
 *
 * Data flow (thin-client contract):
 *   REST snapshot (truth)  ──►  tasks: Map<id, TaskView>
 *   WS events (deltas)     ──►  applyEvent() folds into the map
 *   unknown id / reconnect ──►  resync() re-snapshots (throttled)
 *
 * Speed is NOT on the wire for a single task — it is derived here
 * from consecutive progress deltas (EMA, ~1s half-life), which keeps
 * the daemon protocol minimal and the GUI free to present.
 *
 * Time convention: ALL internal timestamps are Unix epoch SECONDS
 * (the wire unit, wire u64). Folded events stamp with nowSec(); REST
 * rows arrive as numbers. No ISO strings anywhere — mixing units
 * once silently broke every delta computation (M3-a R2 P1-2).
 */
import { writable } from 'svelte/store';
import { SvelteMap } from 'svelte/reactivity';
import type { Daemon, EventStream } from './daemon';
import type { EngineEvent, Task } from './types';

export interface TaskView extends Task {
  /** Derived: EMA bytes/sec (null before two samples, cleared on
   * terminal status and on gaps > 5s — a stalled download must not
   * render its last speed forever). */
  speed: number | null;
  /** Derived: 0..1, null when total unknown. */
  fraction: number | null;
  /** Derived: recent EMA samples (newest last), max 60 — feeds the
   * detail drawer's speed sparkline. Cleared when speed clears. */
  speed_history: number[];
}

export type Conn = 'connecting' | 'live' | 'down';

const nowSec = () => Math.floor(Date.now() / 1000);

function view(t: Task, prev?: TaskView): TaskView {
  const received = Math.max(t.received_bytes, prev?.received_bytes ?? 0);
  const dt = prev ? t.updated_at - prev.updated_at : 0;
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
    } else if (dt > 5) {
      speed = null; // stalled: last-known speed is now a lie
    }
  } else {
    speed = null;
  }
  return {
    ...t,
    received_bytes: received,
    speed,
    speed_history:
      speed !== null
        ? [...(prev?.speed_history ?? []), speed].slice(-60)
        : [],
    fraction:
      t.total_bytes && t.total_bytes > 0
        ? Math.min(1, received / t.total_bytes)
        : null,
  };
}

export function createStore(
  daemon: Daemon,
  stream: EventStream,
  /** Presentation hook: fired once per completion event (dedup by
   * the caller). Wired to OS notifications in Tauri, no-op on web. */
  onCompleted?: (id: string) => void,
) {
  // SvelteMap, NOT $state(new Map()): $state's deep proxy only
  // wraps plain objects/arrays — a raw Map passes through unproxied
  // (proxy.js returns non-plain prototypes as-is), so `.set()` folds
  // would mutate invisibly and the GUI would freeze at its snapshot
  // (U2 bug: pct stuck while the daemon streamed). SvelteMap carries
  // its own version signals, so mutations invalidate readers directly;
  // resync() therefore clear()+set()s in place instead of replacing
  // the instance (a bare reassignment would be untracked).
  const tasks = new SvelteMap<string, TaskView>();
  const conn = writable<Conn>('connecting');

  // resync throttle: startup + reconnect + unknown-id events can all
  // fire in a burst; without a guard each unknown frame triggers its
  // own full list() (N tasks × event rate = a self-inflicted storm).
  let resyncing = false;
  let lastAt = 0;
  let pending = false;
  let timer: ReturnType<typeof setTimeout> | null = null;

  function scheduleTrailing() {
    // Exactly one pending timer: callers that arrive inside the
    // 500ms window (or during an in-flight call) just set `pending`;
    // this timer is the only thing that can observe it later.
    if (timer !== null) return;
    const wait = Math.max(0, 500 - (Date.now() - lastAt));
    timer = setTimeout(() => {
      timer = null;
      void resync();
    }, wait);
  }

  async function resync() {
    if (resyncing) {
      pending = true;
      scheduleTrailing();
      return;
    }
    if (Date.now() - lastAt < 500) {
      pending = true;
      scheduleTrailing();
      return;
    }
    resyncing = true;
    lastAt = Date.now();
    try {
      const rows = await daemon.list();
      // In-place clear+set: every mutation path must go through the
      // SvelteMap's tracked methods (see declaration comment).
      // Snapshot prevs BEFORE clear() — after it they'd all be
      // undefined and the EMA would reset on every resync (R2 P2:
      // dead `tasks.get` after clear).
      const prevs = new Map(tasks);
      tasks.clear();
      for (const t of rows) tasks.set(t.id, view(t, prevs.get(t.id)));
      conn.set('live');
    } catch {
      conn.set('down');
    } finally {
      resyncing = false;
      if (pending) {
        pending = false;
        scheduleTrailing();
      }
    }
  }

  function fold(id: string, patch: Partial<Task>) {
    const cur = tasks.get(id);
    if (!cur) {
      void resync();
      return;
    }
    tasks.set(id, view({ ...cur, ...patch, updated_at: nowSec() }, cur));
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
      case 'task_started': {
        fold(e.id, { status: 'running' });
        break;
      }
      case 'task_progress': {
        const cur = tasks.get(e.id);
        if (!cur) {
          void resync();
          break;
        }
        tasks.set(
          e.id,
          view(
            {
              ...cur,
              received_bytes: Math.max(cur.received_bytes, e.received),
              total_bytes: e.total ?? cur.total_bytes,
              updated_at: nowSec(),
            },
            cur,
          ),
        );
        break;
      }
      case 'task_status_changed': {
        fold(e.id, { status: e.status });
        break;
      }
      case 'task_completed': {
        // Terminal: fold status; the REST row holds final byte
        // counts (and any error text is absent by definition).
        fold(e.id, { status: 'completed', error: null });
        void daemon.get(e.id).then((t) => tasks.set(t.id, view(t, tasks.get(t.id)))).catch(() => {});
        onCompleted?.(e.id);
        break;
      }
      case 'task_failed': {
        // reason IS on the wire (bus.rs TaskFailed.reason) — fold it
        // directly; the row is already terminal-persisted.
        fold(e.id, { status: 'failed', error: e.reason });
        break;
      }
      case 'task_removed': {
        tasks.delete(e.id);
        break;
      }
      case 'task_limit_changed': {
        // Another window (or MCP, M5) changed the throttle — fold it
        // in or this tab's select would keep showing a stale value.
        fold(e.id, { speed_limit_bps: e.speed_limit_bps });
        break;
      }
      case 'task_priority_changed': {
        // Same story for queue priority: another window re-ranked
        // the queue; fold or this tab's badge goes stale.
        fold(e.id, { priority: e.priority });
        break;
      }
      case 'resync_required': {
        void resync();
        break;
      }
      default: {
        // Future wire variant this client doesn't know: degrade to
        // a snapshot instead of silently dropping it (the M3-a
        // lesson — one dropped variant froze rows mid-flight).
        void resync();
      }
    }
  }

  stream.start();
  void resync();

  return {
    conn: { subscribe: conn.subscribe },
    get list(): TaskView[] {
      // Newest first; created_at has 1s resolution so ties are
      // common — break them by id for a deterministic order.
      return [...tasks.values()].sort(
        (a, b) => b.created_at - a.created_at || b.id.localeCompare(a.id),
      );
    },
    applyEvent,
    resync,
    async add(url: string, savePath: string, priority: 'low' | 'normal' | 'high') {
      const t = await daemon.add(url, savePath, priority);
      tasks.set(t.id, view(t));
    },
    async pause(id: string) {
      // 100ms rule (R2 P0): the click must gray the row NOW; the
      // REST echo replaces the optimistic row with truth. Failure
      // rolls back to the pre-click row and rethrows (App banners it).
      const prev = tasks.get(id);
      if (prev) tasks.set(id, { ...prev, status: 'paused', speed: null });
      try {
        const t = await daemon.pause(id);
        tasks.set(id, view(t, tasks.get(id)));
      } catch (e) {
        if (prev) tasks.set(id, prev);
        throw e;
      }
    },
    async resume(id: string) {
      const prev = tasks.get(id);
      if (prev) tasks.set(id, { ...prev, status: 'queued', speed: null });
      try {
        const t = await daemon.resume(id);
        tasks.set(id, view(t, tasks.get(id)));
      } catch (e) {
        if (prev) tasks.set(id, prev);
        throw e;
      }
    },
    async remove(id: string, purge = false) {
      await daemon.remove(id, purge);
      tasks.delete(id);
    },
    /** Optimistic limit (R2): apply locally, replace with the
     * daemon's echoed row on success, roll back on failure. */
    async setTaskLimit(id: string, bps: number) {
      const prev = tasks.get(id);
      if (prev) tasks.set(id, { ...prev, speed_limit_bps: bps });
      try {
        const t = await daemon.setTaskLimit(id, bps);
        tasks.set(id, view(t, tasks.get(id)));
      } catch (e) {
        if (prev) tasks.set(id, prev);
        throw e;
      }
    },
    async setPriority(id: string, priority: 'low' | 'normal' | 'high') {
      const prev = tasks.get(id);
      if (prev) tasks.set(id, { ...prev, priority });
      try {
        const t = await daemon.setPriority(id, priority);
        tasks.set(id, view(t, tasks.get(id)));
      } catch (e) {
        if (prev) tasks.set(id, prev);
        throw e;
      }
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
