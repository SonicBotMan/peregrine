/**
 * Open/reveal the finished artifact (U2). Desktop-only — in the
 * plain-browser dev server there is no Tauri runtime, so callers
 * get 'unsupported' and show a banner instead of silently
 * no-op'ing. Dynamic import keeps the web build free of the
 * plugin (it would throw at module scope under vite dev proxy).
 */
import type { Task } from './types';

/** True when running inside the Tauri webview (any window global). */
export function inTauri(): boolean {
  return typeof globalThis.__TAURI_INTERNALS__ !== 'undefined';
}

declare global {
  // eslint-disable-next-line no-var
  var __TAURI_INTERNALS__: unknown | undefined;
}

async function opener() {
  return await import('@tauri-apps/plugin-opener');
}

export type OpenResult = 'ok' | 'unsupported' | 'failed';

/**
 * Reveal the saved file/folder in the system file manager
 * (Finder/Explorer/DE file manager), selecting it when the OS
 * supports selection. For BT multi-file tasks save_path is the
 * containing folder — revealing the folder itself is right.
 */
export async function revealSaved(task: Task): Promise<OpenResult> {
  if (!inTauri()) return 'unsupported';
  try {
    await (await opener()).revealItemInDir(task.save_path);
    return 'ok';
  } catch (e) {
    console.warn('revealItemInDir failed', e);
    return 'failed';
  }
}

/** Open the artifact with the OS default handler (dblclick alt). */
export async function openSaved(task: Task): Promise<OpenResult> {
  if (!inTauri()) return 'unsupported';
  try {
    await (await opener()).openPath(task.save_path);
    return 'ok';
  } catch (e) {
    console.warn('openPath failed', e);
    return 'failed';
  }
}
