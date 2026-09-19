import { describe, expect, it } from 'vitest';
import { composeSavePath, isTorrentSource } from './savepath';

describe('composeSavePath', () => {
  const DIR = '/tmp/pgrg-dl';

  it('composes dir + URL filename for http', () => {
    expect(composeSavePath(DIR, 'https://example.com/a/b/README.html').savePath).toBe(
      '/tmp/pgrg-dl/README.html',
    );
  });

  it('collapses trailing slashes on the dir', () => {
    expect(composeSavePath(DIR + '/', 'https://example.com/f.bin').savePath).toBe(
      '/tmp/pgrg-dl/f.bin',
    );
  });

  it('decodes percent-escaped filenames', () => {
    expect(composeSavePath(DIR, 'https://example.com/my%20file.zip').savePath).toBe(
      '/tmp/pgrg-dl/my file.zip',
    );
  });

  it('passes the dir through for magnet/bt (BT sink is a directory)', () => {
    expect(composeSavePath(DIR, 'magnet:?xt=urn:btih:abc&dn=x').savePath).toBe(DIR);
    expect(composeSavePath(DIR, 'bt:0123').savePath).toBe(DIR);
  });

  it('passes the dir through for .torrent sources (any scheme)', () => {
    expect(composeSavePath(DIR, 'file:///tmp/x.torrent').savePath).toBe(DIR);
    expect(composeSavePath(DIR, 'https://example.com/x.torrent').savePath).toBe(DIR);
  });

  it('flags a file-named URL with no derivable filename', () => {
    expect(composeSavePath(DIR, 'https://example.com/').filenameless).toBe(true);
  });

  it('does not compose for unknown schemes (daemon rejects them anyway)', () => {
    expect(composeSavePath(DIR, 'somejunk:abc').savePath).toBe(DIR);
  });
});

describe('isTorrentSource', () => {
  it('detects magnet/bt/.torrent case-insensitively', () => {
    expect(isTorrentSource('MAGNET:?xt=urn:btih:x')).toBe(true);
    expect(isTorrentSource('https://host/pkg.torrent')).toBe(true);
    expect(isTorrentSource('https://host/pkg.zip')).toBe(false);
  });
});
