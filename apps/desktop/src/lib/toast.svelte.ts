/**
 * In-page toast channel (U3, B39 normalization): transient, human-
 * facing feedback for action outcomes — completion notices, removals
 * with undo, copy confirmations. Errors still go to the banner
 * (they may need reading time); toasts are for "it happened".
 *
 * Module-level singleton state: exactly one toast stack per app,
 * importable from anywhere (App, store callbacks, palette).
 */
export type Toast = {
  id: number;
  msg: string;
  undo?: () => void;
};

const toasts = $state<Toast[]>([]);
let seq = 0;
const timers = new Map<number, ReturnType<typeof setTimeout>>();

function push(msg: string, opts: { undo?: () => void; dur?: number } = {}) {
  const id = ++seq;
  toasts.push({ id, msg, undo: opts.undo });
  const dur = opts.dur ?? 5000;
  if (dur > 0) {
    timers.set(
      id,
      setTimeout(() => dismiss(id), dur),
    );
  }
  return id;
}

function dismiss(id: number) {
  const t = timers.get(id);
  if (t) {
    clearTimeout(t);
    timers.delete(id);
  }
  const i = toasts.findIndex((x) => x.id === id);
  if (i >= 0) toasts.splice(i, 1);
}

function runUndo(id: number) {
  const t = toasts.find((x) => x.id === id);
  dismiss(id);
  t?.undo?.();
}

export const toast = {
  push,
  dismiss,
  runUndo,
  /** Reactive list for the <Toasts> renderer. */
  get list(): readonly Toast[] {
    return toasts;
  },
};
