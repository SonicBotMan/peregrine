/**
 * URL → category derivation for sidebar filtering (U1).
 *
 * The daemon wire carries no MIME type — the filename extension in
 * the URL is the only signal available client-side. BT tasks
 * (bt:// / magnet:) have no filename at all → 'other'.
 * Grouping mirrors ABDM's category set (the genre convention).
 */

export type Category = 'video' | 'audio' | 'doc' | 'archive' | 'program' | 'other';

export const CATEGORIES: readonly { id: Category; label: string }[] = [
  { id: 'video', label: 'Video' },
  { id: 'audio', label: 'Audio' },
  { id: 'doc', label: 'Documents' },
  { id: 'archive', label: 'Archives' },
  { id: 'program', label: 'Programs' },
  { id: 'other', label: 'Other' },
];

const EXT: Readonly<Record<string, Category>> = {
  // video
  mp4: 'video', mkv: 'video', avi: 'video', mov: 'video', webm: 'video',
  flv: 'video', wmv: 'video', m4v: 'video', ts: 'video', mpg: 'video',
  // audio
  mp3: 'audio', flac: 'audio', wav: 'audio', aac: 'audio', ogg: 'audio',
  m4a: 'audio', opus: 'audio', wma: 'audio',
  // documents
  pdf: 'doc', doc: 'doc', docx: 'doc', xls: 'doc', xlsx: 'doc',
  ppt: 'doc', pptx: 'doc', txt: 'doc', md: 'doc', epub: 'doc', csv: 'doc',
  // archives
  zip: 'archive', rar: 'archive', '7z': 'archive', tar: 'archive',
  gz: 'archive', bz2: 'archive', xz: 'archive', zst: 'archive', iso: 'archive',
  // programs
  exe: 'program', msi: 'program', dmg: 'program', deb: 'program',
  rpm: 'program', appimage: 'program', apk: 'program', bin: 'program',
};

export function categorize(url: string): Category {
  if (url.startsWith('bt://') || url.startsWith('magnet:')) return 'other';
  // Strip query/fragment, then take the last path segment.
  const path = url.split(/[?#]/, 1)[0];
  const name = path.split('/').filter(Boolean).pop();
  if (!name) return 'other';
  const dot = name.lastIndexOf('.');
  if (dot <= 0 || dot === name.length - 1) return 'other';
  return EXT[name.slice(dot + 1).toLowerCase()] ?? 'other';
}
