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
  error: string | null;
  created_at: string;
  updated_at: string;
}

export type EngineEvent =
  | { type: 'task_added'; id: string; status: TaskStatus }
  | { type: 'task_progress'; id: string; received: number; total: number | null }
  | { type: 'task_status'; id: string; status: TaskStatus; error?: string | null }
  | { type: 'task_removed'; id: string };

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
