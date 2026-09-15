import type { TaskView } from './store.svelte';

/**
 * OS notification on task completion (M3-c2). No-op outside Tauri —
 * the web build has no notification permission story, and the dev
 * browser already shows everything in-page.
 *
 * Dynamic import: @tauri-apps/plugin-notification only exists in the
 * desktop bundle; pulling it statically would break `vite build` for
 * the web target... actually it bundles fine either way, but the
 * dynamic import also keeps the web bundle free of the plugin.
 */
export async function notifyCompleted(
  id: string,
  task: () => TaskView | undefined,
): Promise<void> {
  if (typeof window === 'undefined' || !('__TAURI_INTERNALS__' in window)) return;
  try {
    const mod = await import('@tauri-apps/plugin-notification');
    let granted = await mod.isPermissionGranted();
    if (!granted) granted = (await mod.requestPermission()) === 'granted';
    if (!granted) return;
    const t = task();
    const name = t ? fileName(t.url) : id;
    const pct = t?.fraction != null ? Math.round(t.fraction * 100) : 100;
    mod.sendNotification({
      title: 'Peregrine — download complete',
      body: `${name} finished (${pct}%)`,
    });
  } catch {
    // Notification channel broken ≠ download broken; never throw on
    // a presentation nicety.
  }
}

function fileName(url: string): string {
  try {
    const u = new URL(url);
    const last = u.pathname.split('/').filter(Boolean).pop();
    return last ? decodeURIComponent(last) : url;
  } catch {
    return url;
  }
}
