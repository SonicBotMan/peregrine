/**
 * Daemon REST client (thin fetch wrapper over loopback TCP).
 * The daemon also serves the same router on a UDS, but a webview
 * can only speak http(s) — hence TCP. `base` comes from
 * `VITE_DAEMON_URL` (dev: the Vite proxy; tauri: direct).
 */
import type { EngineEvent, Health, SegmentView, Settings, Task, TaskStatus } from './types';

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

export class Daemon {
  constructor(private base: string = '') {}

  private async call<T>(method: string, path: string, body?: unknown): Promise<T> {
    const res = await fetch(this.base + path, {
      method,
      headers: body === undefined ? undefined : { 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (!res.ok) {
      // The daemon's error shape is always {error, message}.
      let code = 'transport';
      let msg = res.statusText;
      try {
        const j = await res.json();
        code = j.error ?? code;
        msg = j.message ?? msg;
      } catch {
        /* non-JSON error body — keep statusText */
      }
      throw new ApiError(res.status, code, msg);
    }
    return res.json() as Promise<T>;
  }

  health(): Promise<Health> {
    return this.call('GET', '/health');
  }

  list(status?: TaskStatus): Promise<Task[]> {
    const q = status ? `?status=${encodeURIComponent(status)}` : '';
    return this.call('GET', `/tasks${q}`);
  }

  get(id: string): Promise<Task> {
    return this.call('GET', `/tasks/${id}`);
  }

  add(url: string, savePath: string, priority: 'low' | 'normal' | 'high'): Promise<Task> {
    return this.call('POST', '/tasks', { url, save_path: savePath, priority });
  }

  pause(id: string): Promise<Task> {
    return this.call('POST', `/tasks/${id}/pause`);
  }

  resume(id: string): Promise<Task> {
    return this.call('POST', `/tasks/${id}/resume`);
  }

  remove(id: string): Promise<{ removed: boolean }> {
    return this.call('DELETE', `/tasks/${id}`);
  }

  segments(id: string): Promise<SegmentView[]> {
    return this.call('GET', `/tasks/${id}/segments`);
  }

  /** Per-task throttle; 0 = unlimited. Persists + applies live. */
  setTaskLimit(id: string, bps: number): Promise<Task> {
    return this.call('PUT', `/tasks/${id}/limit`, { bps });
  }

  getSettings(): Promise<Settings> {
    return this.call('GET', '/settings');
  }

  setGlobalLimit(bps: number): Promise<Settings> {
    return this.call('PUT', '/settings', { global_limit_bps: bps });
  }
}

/** Typed WebSocket stream with exponential-backoff reconnect and a
 * resync hook: after a reconnect (or a `task_added` we have no row
 * for), the store re-snapshots via REST — the bus carries deltas,
 * never truth (B37 contract). */
export class EventStream {
  private ws: WebSocket | null = null;
  private backoff = 500;
  private closed = false;

  constructor(
    private url: string,
    private onEvent: (e: EngineEvent) => void,
    private onResync: () => void,
    /** Fired when the socket dies — the store flips `conn` to 'down'
     * immediately instead of waiting for the next resync to fail. */
    private onDown: () => void,
  ) {}

  start() {
    this.closed = false;
    this.open();
  }

  private open() {
    if (this.closed) return;
    const ws = new WebSocket(this.url);
    this.ws = ws;
    ws.onmessage = (ev) => {
      try {
        this.onEvent(JSON.parse(ev.data as string));
      } catch {
        // Malformed frame: drop it; the resync path is the net.
        this.onResync();
      }
    };
    ws.onopen = () => {
      this.backoff = 500;
      this.onResync();
    };
    ws.onclose = () => {
      if (this.closed) return;
      this.onDown();
      // Jittered exponential backoff (0.75–1.25×): several clients
      // reconnecting in lockstep would otherwise stampede the daemon.
      const wait = this.backoff * (0.75 + Math.random() * 0.5);
      this.backoff = Math.min(this.backoff * 2, 10_000);
      setTimeout(() => this.open(), wait);
    };
  }

  stop() {
    this.closed = true;
    this.ws?.close();
    this.ws = null;
  }
}
