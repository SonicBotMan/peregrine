/**
 * Wire-shape mirrors of the daemon's API types (peregrine-api).
 * Kept hand-written on purpose: the GUI is a thin client and these
 * are the ONLY daemon facts it may depend on. When api changes shape,
 * these change in the same commit (typecheck fails loudly).
 *
 * Source of truth: crates/api/src/{task.rs,bus.rs}.
 */

export type TaskStatus =
  | 'queued'
  | 'running'
  | 'paused'
  | 'completed'
  | 'failed';

export type Priority = 'low' | 'normal' | 'high';

export interface Task {
  id: string;
  url: string;
  save_path: string;
  status: TaskStatus;
  priority: Priority;
  total_bytes: number | null;
  received_bytes: number;
  /** Per-task throttle, bytes/sec. 0 = unlimited (wire snake_case). */
  speed_limit_bps: number;
  error: string | null;
  /** Unix epoch SECONDS (wire u64) — not ISO strings. Multiply by
   * 1000 for Date math; the store keeps all internal time in epoch
   * seconds so REST rows and folded events share one unit. */
  created_at: number;
  updated_at: number;
}

/** Wire view of one planned segment (M3-c1 telemetry). `pct` is
 * done/len in [0,1]; derived server-side so all clients render
 * identically. Empty array = single-stream task. */
export interface SegmentView {
  idx: number;
  start: number;
  end: number;
  len: number;
  done: number;
  frontier: number;
  pct: number;
}

/** Wire view of a connected BT peer (GET /tasks/{id}/peers).
 * One response shape for every task — non-BT tasks return
 * `{bt:false, peers:[]}`, so the panel renders uniformly. */
export interface BtPeer {
  addr: string;
  client: string | null;
  state: string;
  conn_kind: string | null;
  fetched_bytes: number;
  uploaded_bytes: number;
  errors: number;
}

export interface BtPeersSnapshot {
  bt: boolean;
  /** Torrent present in the session (may be paused). */
  live: boolean;
  paused: boolean;
  /** BT task still resolving magnet metadata — no session entry
   * yet; the panel says "resolving…" instead of "not BT". */
  resolving: boolean;
  total_bytes: number;
  peers: BtPeer[];
}

/** Daemon-wide knobs (GET/PUT /settings). Shape is additive. */
export interface Settings {
  global_limit_bps: number;
  /** Default save DIRECTORY for path-composing clients (quick-add,
   * AddDialog prefill). `~` is daemon-resolved at task-add time. */
  default_dir: string;
}

/** Mirrors crates/api/src/bus.rs `EngineEvent` (serde tag="type",
 * snake_case) PLUS the server's synthetic lag frame — the daemon's
 * WS bridge serializes the bus enum verbatim, so this union must
 * list every variant or applyEvent silently drops it. When bus.rs
 * gains a variant, mirror it here in the same commit. */
export type EngineEvent =
  | { type: 'task_added'; id: string; status: TaskStatus }
  | { type: 'task_started'; id: string }
  | { type: 'task_progress'; id: string; received: number; total: number | null }
  | { type: 'task_status_changed'; id: string; status: TaskStatus }
  | { type: 'task_completed'; id: string }
  | { type: 'task_failed'; id: string; reason: string }
  | { type: 'task_removed'; id: string; url: string; save_path: string }
  | { type: 'task_limit_changed'; id: string; speed_limit_bps: number }
  | { type: 'task_priority_changed'; id: string; priority: Priority }
  /** Synthetic (server/src/ws.rs): subscriber lagged — refetch. */
  | { type: 'resync_required'; skipped: number };

export interface Health {
  name: string;
  version: string;
  pid: number;
  uptime_secs: number;
  status: string;
}

export const TERMINAL: ReadonlySet<TaskStatus> = new Set(['completed', 'failed']);

export type StatusFilter = 'all' | 'active' | 'completed' | 'failed';
