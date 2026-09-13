/**
 * Shared formatting + throttle presets (M3-c1).
 *
 * Presets are a UI convenience only — the daemon accepts any bps.
 * `custom` is represented by `null` in `presetFor` (the row falls
 * back to a numeric input when the current value matches no preset).
 */

export const LIMIT_PRESETS: readonly { label: string; bps: number }[] = [
  { label: 'off', bps: 0 },
  { label: '512 KB/s', bps: 512 * 1024 },
  { label: '1 MB/s', bps: 1024 * 1024 },
  { label: '2 MB/s', bps: 2 * 1024 * 1024 },
  { label: '4 MB/s', bps: 4 * 1024 * 1024 },
];

export function presetFor(bps: number): number | null {
  return LIMIT_PRESETS.find((p) => p.bps === bps)?.bps ?? null;
}

export function formatBytes(n: number | null): string {
  if (n === null) return '—';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 || i === 0 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

export function formatBps(n: number): string {
  return `${formatBytes(n)}/s`;
}

/** mm:ss / h:mm:ss for a bytes-remaining + speed pair. */
export function formatEta(bytesLeft: number, bps: number | null): string {
  if (bps === null || bps <= 0) return '—';
  const s = Math.round(bytesLeft / bps);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const pad = (x: number) => String(x).padStart(2, '0');
  return h > 0 ? `${h}:${pad(m)}:${pad(sec)}` : `${m}:${pad(sec)}`;
}
