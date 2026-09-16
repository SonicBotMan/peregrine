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
  return 'dark'; // dark-first by design (ui-proposal §4.1)
}

export function applyTheme(t: Theme) {
  document.documentElement.dataset.theme = t;
}

export function saveTheme(t: Theme) {
  localStorage.setItem(KEY, t);
}
