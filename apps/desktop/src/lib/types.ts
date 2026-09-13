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
  | 'failed'
  | 'removed';

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
  created_at: string;
  updated_at: string;
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

/** Daemon-wide knobs (GET/PUT /settings). Shape is additive. */
export interface Settings {
  global_limit_bps: number;
}

export type EngineEvent =
  | { type: 'task_added'; id: string; status: TaskStatus }
  | { type: 'task_progress'; id: string; received: number; total: number | null }
  | { type: 'task_status'; id: string; status: TaskStatus; error?: string | null }
  | { type: 'task_removed'; id: string }
  | { type: 'task_limit_changed'; id: string; speed_limit_bps: number };

export interface Health {
  name: string;
  version: string;
  pid: number;
  uptime_secs: number;
  status: string;
}

export const TERMINAL: ReadonlySet<TaskStatus> = new Set([
  'completed',
  'failed',
  'removed',
]);
