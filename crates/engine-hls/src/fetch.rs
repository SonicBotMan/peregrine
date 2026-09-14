//! Segment fetching over the SHARED hyper stack (one HTTP story —
//! engine-http owns the client policy; HLS is a client of it).

use tokio::io::AsyncWriteExt;

use crate::error::HlsError;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Method, Request, StatusCode};
use peregrine_api::budget::BudgetChain;
use peregrine_engine_http::{HttpsClient, USER_AGENT, https_client};

/// Redirect hops we'll chase for a segment/playlist/key URL. Signed
/// CDN URLs 302 routinely; loops are bounded like engine-http's
/// probe (5) but tighter — playlists shouldn't hop much.
const MAX_REDIRECTS: usize = 3;

/// Per-frame stall bound — same discipline as engine-http's
/// STALL_TIMEOUT (R2 P1-3): headers-then-silence must not park a
/// scheduler slot forever. Budget-throttled downloads still tick:
/// a throttled frame ARRIVES, gets consumed in slices, and the
/// NEXT frame's timer restarts.
const STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Clone)]
pub(crate) struct Fetcher {
    client: HttpsClient,
}

impl Fetcher {
    pub(crate) fn new() -> Result<Self, HlsError> {
        Ok(Self {
            client: https_client().map_err(|e| HlsError::Network(e.to_string()))?,
        })
    }

    /// Issue a GET (optional `Range`), chasing ≤3 redirects, and
    /// return (status, location-if-redirect, body-stream). The CALLER
    /// decides success semantics (206 vs 200 differs by use).
    pub(crate) async fn get(
        &self,
        url: &str,
        range: Option<(u64, u64)>,
    ) -> Result<(StatusCode, Incoming), HlsError> {
        self.get_cancellable(url, range, None).await
    }

    /// `get` with an optional cancel token selected against EVERY
    /// redirect hop's request phase (R2 P1-1): headers-then-silence
    /// must not park a slot, and a cancelled task must return even
    /// while the origin still owes us the response head.
    pub(crate) async fn get_cancellable(
        &self,
        url: &str,
        range: Option<(u64, u64)>,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<(StatusCode, Incoming), HlsError> {
        let mut current = url.to_string();
        for _hop in 0..=MAX_REDIRECTS {
            let mut builder = Request::builder()
                .method(Method::GET)
                .uri(&current)
                .header(hyper::header::USER_AGENT, USER_AGENT);
            if let Some((start, len)) = range {
                let end = start + len.saturating_sub(1);
                builder = builder.header(hyper::header::RANGE, format!("bytes={start}-{end}"));
            }
            let req = builder
                .body(Full::<hyper::body::Bytes>::default())
                .map_err(|e| HlsError::Network(e.to_string()))?;
            let mut fut = std::pin::pin!(self.client.request(req));
            let resp = match cancel {
                Some(c) => tokio::select! {
                    biased;
                    _ = c.cancelled() => {
                        return Err(HlsError::Network("cancelled".into()));
                    }
                    r = &mut fut => r.map_err(HlsError::network)?,
                },
                None => fut.await.map_err(HlsError::network)?,
            };
            if resp.status().is_redirection()
                && let Some(loc) = resp.headers().get(hyper::header::LOCATION)
            {
                let loc = loc.to_str().map_err(HlsError::network)?;
                let base = ::url::Url::parse(&current).map_err(HlsError::network)?;
                current = base.join(loc).map_err(HlsError::network)?.to_string();
                continue;
            }
            return Ok((resp.status(), resp.into_body()));
        }
        Err(HlsError::Network("too many redirects".into()))
    }

    /// GET `url` into `path.tmp…`, then atomically rename to
    /// `final_path`. Atomic-per-segment is what makes interruption
    /// FREE: a present part file is always complete, so resume =
    /// skip. Cancels remove the partial tmp first.
    #[allow(clippy::too_many_arguments)] // 8 is the honest part-fetch shape
    pub(crate) async fn fetch_part(
        &self,
        url: &str,
        range: Option<(u64, u64)>,
        budget: &BudgetChain,
        cancel: &tokio_util::sync::CancellationToken,
        tmp: &std::path::Path,
        final_path: &std::path::Path,
    ) -> Result<(), HlsError> {
        let (status, mut body) = self.get_cancellable(url, range, Some(cancel)).await?;
        if !status.is_success() {
            return Err(HlsError::Http {
                status: status.as_u16(),
                url: url.to_string(),
            });
        }
        // A range request the server IGNORED answers 200 with the
        // whole resource — accepting would corrupt the merge.
        if range.is_some() && status.as_u16() != 206 {
            return Err(HlsError::Http {
                status: status.as_u16(),
                url: format!("{url} (ignored Range)"),
            });
        }
        let mut file = tokio::fs::File::create(tmp).await?;
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    drop(file);
                    let _ = tokio::fs::remove_file(tmp).await;
                    return Err(HlsError::Network("cancelled".into()));
                }
                frame = tokio::time::timeout(STALL_TIMEOUT, body.frame()) => {
                    let frame = match frame {
                        Ok(f) => f,
                        Err(_) => {
                            // Stalled origin (R2 P1-3): headers came,
                            // body went silent for 30s.
                            drop(file);
                            let _ = tokio::fs::remove_file(tmp).await;
                            return Err(HlsError::Network("stalled: no body frame in 30s".into()));
                        }
                    };
                    match frame {
                        Some(Ok(f)) => {
                            let bytes = match f.into_data() {
                                Ok(b) => b,
                                Err(_) => continue, // trailers etc.
                            };
                            let mut off = 0usize;
                            while off < bytes.len() {
                                let want =
                                    (bytes.len() - off).min(budget.slice_hint() as usize) as u64;
                                budget.acquire(want).await;
                                let end = (off + want as usize).min(bytes.len());
                                if let Err(e) = file.write_all(&bytes[off..end]).await {
                                    drop(file);
                                    let _ = tokio::fs::remove_file(tmp).await;
                                    return Err(e.into());
                                }
                                off = end;
                            }
                        }
                        Some(Err(e)) => {
                            drop(file);
                            let _ = tokio::fs::remove_file(tmp).await;
                            return Err(HlsError::network(e));
                        }
                        None => {
                            use tokio::io::AsyncWriteExt;
                            if let Err(e) = file.flush().await {
                                drop(file);
                                let _ = tokio::fs::remove_file(tmp).await;
                                return Err(e.into());
                            }
                            drop(file);
                            break;
                        }
                    }
                }
            }
        }
        tokio::fs::rename(tmp, final_path).await?;
        Ok(())
    }
}

/// Map our cancel marker to a bool (port layer turns it into
/// `ApiError::Cancelled`).
pub(crate) fn is_cancel(e: &HlsError) -> bool {
    matches!(e, HlsError::Network(m) if m == "cancelled")
}

/// Read a whole small body (playlist text, key bytes).
pub(crate) async fn read_all(body: Incoming) -> Result<Vec<u8>, HlsError> {
    let collected = body.collect().await.map_err(HlsError::network)?.to_bytes();
    Ok(collected.to_vec())
}
