// @vitest-environment happy-dom
/**
 * Toast channel (U3): push/undo/dismiss lifecycle. Uses fake timers
 * for the expiry sweep; asserts the undo closure runs and exactly-
 * once dismissal (no double-fire after undo).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { toast } from '../src/lib/toast.svelte';

describe('toast channel', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    for (const t of [...toast.list]) toast.dismiss(t.id);
  });
  afterEach(() => vi.useRealTimers());

  it('push adds a toast and auto-expires after the duration', () => {
    toast.push('hello', { dur: 5000 });
    expect(toast.list.length).toBe(1);
    expect(toast.list[0].msg).toBe('hello');
    vi.advanceTimersByTime(5100);
    expect(toast.list.length).toBe(0);
  });

  it('undo runs the closure exactly once and dismisses', () => {
    let undos = 0;
    const id = toast.push('remove x', { undo: () => undos++, dur: 0 });
    expect(toast.list.length).toBe(1);
    toast.runUndo(id);
    expect(undos).toBe(1);
    expect(toast.list.length).toBe(0);
    // second undo attempt (double click race) must not re-run it
    toast.runUndo(id);
    expect(undos).toBe(1);
  });

  it('explicit dismiss cancels the pending expiry timer', () => {
    const id = toast.push('stay', { dur: 1000 });
    toast.dismiss(id);
    vi.advanceTimersByTime(5000);
    // already gone, and no error from a stale timer
    expect(toast.list.length).toBe(0);
  });
});
