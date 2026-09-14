//! RFC 8216 playlist parser — the minimal-but-correct subset.
//!
//! Scope (M4-a): VOD media playlists and master playlists. Live
//! (no `#EXT-X-ENDLIST`) is REJECTED by the caller, not parsed
//! around. Supported tags: `EXTM3U`, `EXT-X-VERSION` (read),
//! `TARGETDURATION` (read), `MEDIA-SEQUENCE` (read — segment file
//! naming keys off it), `ENDLIST` (required), `EXTINF`,
//! `EXT-X-KEY` (METHOD=AES-128 only; SAMPLE-AES → Unsupported),
//! `EXT-X-MAP` (fMP4 init segment), `EXT-X-BYTERANGE`.
//! Unknown tags are IGNORED (RFC 8216 §4.3: clients MUST ignore
//! unrecognized tags) — a comment line starting `#` that isn't a
//! recognized tag is also ignored, per spec.
//!
//! Hand-rolled on purpose: the m3u8 crates on crates.io are thin
//! event scrapers with weak error typing; the grammar we accept is
//! ~8 tags and every line is `\n`-terminated ASCII. A wrong parse
//! here corrupts downloads silently — we own the whole grammar.

use crate::error::HlsError;

/// A resolved `EXT-X-KEY` (RFC 8216 §4.3.2.4). `NONE` decrypts to no
/// key at all.
#[derive(Debug, Clone, PartialEq)]
pub enum Key {
    None,
    Aes128 {
        /// Absolute URI of the key file.
        uri: String,
        /// Explicit IV attribute; when absent the segment's media
        /// sequence number (big-endian, 16 bytes) is the IV per
        /// §5.2.1.1.
        iv: Option<[u8; 16]>,
    },
}

/// One media segment (an `EXTINF` + URI line pair).
#[derive(Debug, Clone, PartialEq)]
pub struct MediaSegment {
    /// Absolute URI.
    pub uri: String,
    /// `#EXT-X-BYTERANGE` if present (`start` filled per §4.3.2.2:
    /// absent start continues after the previous range on the SAME
    /// resource — resolved at parse time so download never has to
    /// reason about it).
    pub byterange: Option<(u64 /*len*/, u64 /*start*/)>,
    /// Key in effect (carried forward until the next KEY tag).
    pub key: Key,
    /// Media sequence number of this segment.
    pub seq: u64,
}

/// A parsed media playlist.
#[derive(Debug, Clone)]
pub struct MediaPlaylist {
    pub segments: Vec<MediaSegment>,
    /// `EXT-X-MAP` init segment (fMP4): downloaded first, prepended.
    /// Per RFC 8216 §4.3.2.4 a KEY tag also applies to the MAP that
    /// follows it — the key travels WITH the map (R2 P1-1) and the
    /// merge decrypts an encrypted init before writing.
    pub map: Option<MapSegment>,
    /// First segment's `MEDIA-SEQUENCE` (0 when absent).
    pub media_sequence: u64,
    /// `EXT-X-TARGETDURATION` in seconds — live followers poll at
    /// half this (clamped) cadence per §6.2.
    pub target_duration: Option<f64>,
    /// `true` iff `#EXT-X-ENDLIST` present. VOD contract: required.
    pub ended: bool,
}

/// The `EXT-X-MAP` init segment with its effective key.
#[derive(Debug, Clone, PartialEq)]
pub struct MapSegment {
    pub uri: String,
    pub byterange: Option<(u64, u64)>,
    /// Key in effect where the MAP appeared (NONE → verbatim init).
    pub key: Key,
}

/// A variant stream entry in a master playlist.
#[derive(Debug, Clone)]
pub struct Variant {
    pub uri: String,
    pub bandwidth: u64,
}

/// What a downloaded `.m3u8` body parsed into.
#[derive(Debug, Clone)]
pub enum Playlist {
    /// Master: pick a variant and re-fetch its media playlist.
    Master(Vec<Variant>),
    Media(MediaPlaylist),
}

/// Parse a playlist body. `base` is the ABSOLUTE URL the body came
/// from — relative segment URIs resolve against it (RFC 8216 §4.3.2).
pub fn parse(body: &str, base: &str) -> Result<Playlist, HlsError> {
    let base_url = ::url::Url::parse(base)
        .map_err(|e| HlsError::BadPlaylist(format!("playlist url invalid: {e}")))?;
    // BOM tolerance (R2 P2-7): some encoders emit UTF-8 BOM before
    // EXTM3U; strip it so the header check sees the tag.
    let body = body.strip_prefix('\u{feff}').unwrap_or(body);
    if !body.starts_with("#EXTM3U") {
        return Err(HlsError::BadPlaylist("missing #EXTM3U header".into()));
    }

    let mut saw_inf = false; // media playlist marker
    let mut saw_stream_inf = false; // master playlist marker
    let mut segments: Vec<MediaSegment> = Vec::new();
    let mut variants: Vec<Variant> = Vec::new();
    let mut pending_key = Key::None;
    let mut pending_range: Option<(u64, Option<u64>)> = None;
    let mut pending_inf = false;
    let mut map: Option<MapSegment> = None;
    let mut media_sequence = 0u64;
    let mut media_sequence_seen = false;
    let mut ended = false;
    let mut target_duration: Option<f64> = None;

    for raw in body.lines() {
        let line = raw.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let _ = rest; // duration is metadata; no download effect
            pending_inf = true;
            saw_inf = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            pending_key = parse_key(rest, &base_url)?;
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            let (len, start) = parse_byterange(rest)?;
            // "start absent" resolves against the PREVIOUS segment's
            // range end when the previous URI is the same resource;
            // we approximate with a running cursor per distinct
            // resource handled at segment-URI time below.
            pending_range = Some((len, start));
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            let attrs = AttrIter(rest);
            let mut uri = None;
            let mut range = None;
            for (k, v) in attrs.clone() {
                match k {
                    "URI" => uri = Some(v.to_string()),
                    "BYTERANGE" => {
                        let (l, s) = parse_byterange(v)?;
                        // MAP's range has no "previous" to continue
                        // from (it IS the first bytes): a bare length
                        // is ambiguous; require the explicit start.
                        let s = s.ok_or_else(|| {
                            HlsError::BadPlaylist("EXT-X-MAP BYTERANGE without @start".into())
                        })?;
                        range = Some((l, s));
                    }
                    _ => {}
                }
            }
            let uri = uri.ok_or_else(|| HlsError::BadPlaylist("EXT-X-MAP without URI".into()))?;
            map = Some(MapSegment {
                uri: base_url.join(&uri).map_err(url_err)?.to_string(),
                byterange: range,
                key: pending_key.clone(),
            });
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            // RFC 8216 §4.3.3.1: it must precede the first segment —
            // a mid-list occurrence retro-renumbers earlier segments
            // under our index math; reject instead of corrupting seqs.
            if media_sequence_seen || !segments.is_empty() {
                return Err(HlsError::BadPlaylist(
                    "MEDIA-SEQUENCE must precede the first segment".into(),
                ));
            }
            media_sequence = rest
                .trim()
                .parse()
                .map_err(|_| HlsError::BadPlaylist("bad MEDIA-SEQUENCE".into()))?;
            media_sequence_seen = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            target_duration = rest.trim().parse::<f64>().ok();
            continue;
        }
        if line.trim() == "#EXT-X-ENDLIST" {
            ended = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let mut bw = 0;
            for (k, v) in AttrIter(rest) {
                if k == "BANDWIDTH" {
                    bw = v.parse().unwrap_or(0);
                }
            }
            // The variant URI is the NEXT non-comment line — park the
            // parsed bandwidth on a synthetic pending variant.
            variants.push(Variant {
                uri: format!("\0pending:{bw}"),
                bandwidth: bw,
            });
            saw_stream_inf = true;
            continue;
        }
        if line.starts_with('#') {
            continue; // unknown tag or comment: ignore (§4.3)
        }
        // A URI line. Which kind depends on what preceded it.
        let abs = base_url.join(line.trim()).map_err(url_err)?.to_string();
        if let Some(v) = variants.last_mut()
            && v.uri.starts_with('\u{0}')
        {
            v.uri = abs;
            continue;
        }
        if pending_inf {
            // Resolve an absent BYTERANGE start: §4.3.2.2 — it
            // continues right after the previous range IF that was
            // the same resource; when it wasn't, the playlist is
            // malformed (missing start on a new resource).
            let range = match pending_range.take() {
                Some((len, Some(start))) => Some((len, start)),
                Some((len, None)) => {
                    let prev = segments.last().filter(|p| p.uri == abs);
                    match prev {
                        Some(p) => match p.byterange {
                            // §4.3.2.2: continuation = right after the
                            // previous sub-range of the SAME resource.
                            Some((pl, ps)) => Some((len, ps + pl)),
                            None => {
                                return Err(HlsError::BadPlaylist(format!(
                                    "BYTERANGE continuation after a non-ranged segment ({abs})"
                                )));
                            }
                        },
                        None => {
                            return Err(HlsError::BadPlaylist(format!(
                                "BYTERANGE without start on first occurrence of {abs}"
                            )));
                        }
                    }
                }
                None => None,
            };
            let seq = media_sequence + segments.len() as u64;
            segments.push(MediaSegment {
                uri: abs,
                byterange: range,
                key: pending_key.clone(),
                seq,
            });
            pending_inf = false;
            continue;
        }
        // A bare URI with no EXTINF/STREAM-INF before it: illegal in
        // both playlist kinds.
        return Err(HlsError::BadPlaylist(format!("unattached URI line: {abs}")));
    }

    // Master vs media: whichever markers we saw (RFC 8216 §4.3.3.1
    // forbids mixing them).
    match (saw_stream_inf, saw_inf) {
        (true, false) => {
            if variants.iter().any(|v| v.uri.starts_with('\u{0}')) {
                return Err(HlsError::BadPlaylist(
                    "STREAM-INF without a following URI".into(),
                ));
            }
            Ok(Playlist::Master(variants))
        }
        (false, true) => Ok(Playlist::Media(MediaPlaylist {
            segments,
            map,
            media_sequence,
            target_duration,
            ended,
        })),
        (true, true) => Err(HlsError::BadPlaylist(
            "playlist mixes EXTINF and STREAM-INF (both master and media)".into(),
        )),
        (false, false) => Err(HlsError::BadPlaylist(
            "playlist is neither master nor media (no EXTINF / STREAM-INF)".into(),
        )),
    }
}

fn url_err(e: ::url::ParseError) -> HlsError {
    HlsError::BadPlaylist(format!("segment uri unresolvable: {e}"))
}

/// `#EXT-X-KEY:METHOD=AES-128,URI="…"[,IV=0x…]` (quoted-list attr
/// grammar, RFC 8216 §4.2).
fn parse_key(rest: &str, base: &::url::Url) -> Result<Key, HlsError> {
    let mut method = String::new();
    let mut uri = None;
    let mut iv = None;
    for (k, v) in AttrIter(rest) {
        match k {
            "METHOD" => method = v.to_string(),
            "URI" => {
                uri = Some(base.join(v).map_err(url_err)?.to_string());
            }
            "IV" => {
                let hex = v
                    .strip_prefix("0x")
                    .or_else(|| v.strip_prefix("0X"))
                    .unwrap_or(v);
                let mut buf = [0u8; 16];
                // Left-pad to 16 bytes per §5.2.1.1 example: IVs
                // shorter than 16 bytes are zero-padded on the LEFT.
                let bytes = hex_decode(hex)?;
                if bytes.len() > 16 {
                    return Err(HlsError::BadPlaylist("IV longer than 16 bytes".into()));
                }
                buf[16 - bytes.len()..].copy_from_slice(&bytes);
                iv = Some(buf);
            }
            _ => {}
        }
    }
    match method.as_str() {
        "NONE" => Ok(Key::None),
        "AES-128" => {
            let uri = uri.ok_or_else(|| HlsError::BadPlaylist("AES-128 key without URI".into()))?;
            Ok(Key::Aes128 { uri, iv })
        }
        m => Err(HlsError::Unsupported(format!("KEY METHOD {m}"))),
    }
}

/// `n[@s]` byte-range attribute.
fn parse_byterange(rest: &str) -> Result<(u64, Option<u64>), HlsError> {
    let rest = rest.trim().trim_matches('"');
    let mut parts = rest.splitn(2, '@');
    let len = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| HlsError::BadPlaylist(format!("bad BYTERANGE {rest}")))?;
    let start = parts.next().and_then(|s| s.parse().ok());
    Ok((len, start))
}

fn hex_decode(s: &str) -> Result<Vec<u8>, HlsError> {
    if !s.len().is_multiple_of(2) {
        return Err(HlsError::BadPlaylist("odd-length IV hex".into()));
    }
    (0..s.len() / 2)
        .map(|i| {
            u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
                .map_err(|_| HlsError::BadPlaylist("IV hex digit".into()))
        })
        .collect()
}

/// Attribute-list iterator: `KEY=VALUE,KEY2="quoted, comma"` —
/// handles quoted-string commas and attribute names that are NOT
/// quoted-lists themselves (the subset RFC 8216 uses).
#[derive(Clone)]
struct AttrIter<'a>(&'a str);

impl<'a> Iterator for AttrIter<'a> {
    type Item = (&'a str, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        let s = self.0;
        if s.is_empty() {
            return None;
        }
        let eq = s.find('=')?;
        let key = s[..eq].trim();
        let rest = &s[eq + 1..];
        let (value, tail) = if let Some(stripped) = rest.strip_prefix('"') {
            // Quoted: value runs to the first unescaped quote.
            let mut end = None;
            let mut esc = false;
            for (i, c) in stripped.char_indices() {
                match (esc, c) {
                    (false, '\\') => esc = true,
                    (false, '"') => {
                        end = Some(i);
                        break;
                    }
                    _ => esc = false,
                }
            }
            let end = end?;
            let after = &stripped[end + 1..];
            // Skip the comma separator (if any).
            let tail = after.strip_prefix(',').unwrap_or(after);
            (&stripped[..end], tail)
        } else {
            match rest.find(',') {
                Some(c) => (&rest[..c], &rest[c + 1..]),
                None => (rest, ""),
            }
        };
        self.0 = tail;
        Some((key, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://cdn.example/v/master.m3u8";

    #[test]
    fn parses_simple_vod() {
        let body = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n#EXTINF:5.0,\nseg-0.ts\n#EXTINF:6.0,\nseg-1.ts\n#EXT-X-ENDLIST\n";
        let p = parse(body, BASE).unwrap();
        let Playlist::Media(m) = p else {
            panic!("media")
        };
        assert!(m.ended);
        assert_eq!(m.segments.len(), 2);
        assert_eq!(m.segments[0].uri, "https://cdn.example/v/seg-0.ts");
        assert_eq!(m.segments[0].seq, 0);
        assert_eq!(m.segments[1].seq, 1);
        assert_eq!(m.segments[0].key, Key::None);
    }

    #[test]
    fn master_picks_variant_uris() {
        let body = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1280000,RESOLUTION=640x360\nlo/index.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=5120000,RESOLUTION=1920x1080\nhi/index.m3u8\n";
        let Playlist::Master(v) = parse(body, BASE).unwrap() else {
            panic!("master")
        };
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].uri, "https://cdn.example/v/lo/index.m3u8");
        assert_eq!(v[1].bandwidth, 5120000);
    }

    #[test]
    fn key_aes128_with_explicit_iv() {
        let body = "#EXTM3U\n#EXTINF:5.0,\n#EXT-X-KEY:METHOD=AES-128,URI=\"keys/enc.key\",IV=0x000102030405060708090a0b0c0d0e0f\na.ts\n#EXT-X-ENDLIST\n";
        let Playlist::Media(m) = parse(body, BASE).unwrap() else {
            panic!("media")
        };
        let Key::Aes128 { ref uri, iv } = m.segments[0].key else {
            panic!("aes")
        };
        assert_eq!(uri, "https://cdn.example/v/keys/enc.key");
        assert_eq!(iv.unwrap()[0], 0x00);
        assert_eq!(iv.unwrap()[15], 0x0f);
    }

    #[test]
    fn key_none_resets() {
        let body = "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXTINF:4.0,\na.ts\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:4.0,\nb.ts\n#EXT-X-ENDLIST\n";
        let Playlist::Media(m) = parse(body, BASE).unwrap() else {
            panic!("media")
        };
        assert!(matches!(m.segments[0].key, Key::Aes128 { .. }));
        assert_eq!(m.segments[1].key, Key::None);
    }

    #[test]
    fn sample_aes_rejected() {
        let body = "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"k\"\na.ts\n#EXT-X-ENDLIST\n";
        let err = parse(body, BASE).unwrap_err();
        assert!(err.to_string().contains("SAMPLE-AES"), "{err}");
    }

    #[test]
    fn byterange_with_start_and_continuation() {
        let body = "#EXTM3U\n#EXTINF:4.0,\n#EXT-X-BYTERANGE:100@0\nf.mp4\n#EXTINF:4.0,\n#EXT-X-BYTERANGE:200\nf.mp4\n#EXT-X-ENDLIST\n";
        let Playlist::Media(m) = parse(body, BASE).unwrap() else {
            panic!("media")
        };
        assert_eq!(m.segments[0].byterange, Some((100, 0)));
        assert_eq!(m.segments[1].byterange, Some((200, 100)));
    }

    #[test]
    fn byterange_missing_start_on_new_resource_rejected() {
        let body = "#EXTM3U\n#EXTINF:4.0,\n#EXT-X-BYTERANGE:100@0\na.mp4\n#EXTINF:4.0,\n#EXT-X-BYTERANGE:200\nb.mp4\n#EXT-X-ENDLIST\n";
        let err = parse(body, BASE).unwrap_err();
        assert!(err.to_string().contains("without start"), "{err}");
    }

    #[test]
    fn map_is_captured() {
        let body =
            "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4.0,\nm4s-0.m4s\n#EXT-X-ENDLIST\n";
        let Playlist::Media(m) = parse(body, BASE).unwrap() else {
            panic!("media")
        };
        let map = m.map.as_ref().unwrap();
        assert_eq!(map.uri, "https://cdn.example/v/init.mp4");
        assert_eq!(map.key, Key::None);
    }

    #[test]
    fn live_without_endlist_flags_not_ended() {
        let body = "#EXTM3U\n#EXTINF:5.0,\na.ts\n";
        let Playlist::Media(m) = parse(body, BASE).unwrap() else {
            panic!("media")
        };
        assert!(!m.ended);
    }

    #[test]
    fn unknown_tags_ignored() {
        let body = "#EXTM3U\n#EXT-X-SOME-FUTURE-TAG:value=1\n#EXTINF:5.0,\na.ts\n#EXT-X-ENDLIST\n";
        assert!(matches!(parse(body, BASE).unwrap(), Playlist::Media(_)));
    }

    #[test]
    fn missing_extm3u_rejected() {
        let err = parse("#EXTINF:5.0,\na.ts\n", BASE).unwrap_err();
        assert!(err.to_string().contains("#EXTM3U"));
    }

    #[test]
    fn media_sequence_offsets_seq() {
        let body = "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:7\n#EXTINF:5.0,\na.ts\n#EXT-X-ENDLIST\n";
        let Playlist::Media(m) = parse(body, BASE).unwrap() else {
            panic!("media")
        };
        assert_eq!(m.segments[0].seq, 7);
    }

    #[test]
    fn absolute_segment_uris_pass_through() {
        let body = "#EXTM3U\n#EXTINF:5.0,\nhttps://other.example/x.ts\n#EXT-X-ENDLIST\n";
        let Playlist::Media(m) = parse(body, BASE).unwrap() else {
            panic!("media")
        };
        assert_eq!(m.segments[0].uri, "https://other.example/x.ts");
    }
}
