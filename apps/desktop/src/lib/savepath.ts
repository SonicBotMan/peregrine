/**
 * Save-path composition rules (GUI-verify batch-1/2), extracted from
 * AddDialog/quick-add so the contract is unit-testable in one place:
 *
 * - Save-to holds a DESTINATION FOLDER; the filename comes from the
 *   URL (IDM/browser convention). The daemon takes a FILE path and
 *   rightly rejects a directory (os error 21).
 * - BT sources (magnet:/bt:/…torrent) have no URL filename: the
 *   engine treats the sink as the download DIRECTORY — pass the dir
 *   through untouched.
 * - A URL whose last path segment is empty (https://host/) has no
 *   derivable filename: return null, caller decides the UX.
 *
 * All rules here MUST stay in sync with the shell's deep-link
 * composition (src-tauri forward_deep_link) — one contract, three
 * clients (GUI/CLI/MCP).
 */
import { fileName } from './notify';

const FILE_NAMED = /^(https?|ftps?|ftp|file):/i;
const TORRENT_SRC = /^magnet:/i.test('') ? null : undefined; // placeholder guard — real check below

export function isTorrentSource(url: string): boolean {
  return /^magnet:/i.test(url) || /^bt:/i.test(url) || /\.torrent$/i.test(url);
}

export interface ComposeResult {
  /** The save path to hand to the daemon (file path or BT dir). */
  savePath: string;
  /** True when the URL has no derivable filename — the caller shows
   * the 'append one to the URL' hint instead of submitting. */
  filenameless: boolean;
}

/**
 * Compose the daemon's file-path save target from a destination
 * folder + source URL. `dir` is trimmed; trailing slashes are
 * collapsed before appending the filename.
 */
export function composeSavePath(dir: string, url: string): ComposeResult {
  const d = dir.trim().replace(/\/+$/, '');
  if (isTorrentSource(url)) return { savePath: d, filenameless: false };
  if (!FILE_NAMED.test(url)) return { savePath: d, filenameless: false };
  const base = fileName(url);
  if (!base || base === url) return { savePath: d, filenameless: true };
  return { savePath: `${d}/${base}`, filenameless: false };
}
