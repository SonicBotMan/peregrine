/**
 * Dual-theme switch (U1). Dark is the default; the choice persists
 * in localStorage and boots before first paint via main.ts so the
 * webview never flashes the wrong theme.
 */

export type Theme = 'dark' | 'light';

const KEY = 'peregrine-theme';

export function initialTheme(): Theme {
  const saved = localStorage.getItem(KEY);
  if (saved === 'dark' || saved === 'light') return saved;
  // No explicit user choice: follow the OS (GUI-verify batch-2).
  // First explicit toggle pins the choice and detaches below.
  if (typeof matchMedia !== 'undefined' && matchMedia('(prefers-color-scheme: light)').matches) {
    return 'light';
  }
  return 'dark'; // dark-first by design (ui-proposal §4.1)
}

/** Live OS-theme sync while the user has not pinned a choice.
 * Returns an unlisten fn; safe in plain-browser dev too. */
export function onSystemThemeChange(cb: (t: Theme) => void): () => void {
  if (typeof matchMedia === 'undefined') return () => {};
  const mq = matchMedia('(prefers-color-scheme: light)');
  const fn = (e: MediaQueryListEvent) => cb(e.matches ? 'light' : 'dark');
  mq.addEventListener('change', fn);
  return () => mq.removeEventListener('change', fn);
}

export function hasPinnedTheme(): boolean {
  return localStorage.getItem(KEY) === 'dark' || localStorage.getItem(KEY) === 'light';
}

export function applyTheme(t: Theme) {
  document.documentElement.dataset.theme = t;
}

export function saveTheme(t: Theme) {
  localStorage.setItem(KEY, t);
}
