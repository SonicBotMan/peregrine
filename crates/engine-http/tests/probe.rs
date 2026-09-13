//! Probe behaviour against a local axum mock server (ephemeral port):
//! metadata extraction, redirect following, loop detection, error paths,
//! registry routing end-to-end, and B7 real-Range confirmation (the ranged
//! GET that settles `accept_ranges` by observed server behaviour).

use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use peregrine_api::{ApiError, EngineRegistry, ProtocolEngine};
use peregrine_engine_http::HttpEngine;
use std::net::SocketAddr;
use tokio::net::TcpListener;

const BODY: &[u8] = &[0u8; 1000];

/// Range-aware file endpoint: HEAD/GET without `Range` → 200 full metadata;
/// `GET Range: bytes=0-0` → 206 with a one-byte slice and authoritative
/// `Content-Range` (what a spec-compliant origin does).
async fn range_aware_file(headers: HeaderMap) -> Response {
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if range.as_deref() == Some("bytes=0-0") {
        let mut resp = (StatusCode::PARTIAL_CONTENT, vec![0u8; 1]).into_response();
        let h = resp.headers_mut();
        h.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_static("bytes 0-0/1000"),
        );
        h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        h.insert(header::ETAG, HeaderValue::from_static("\"abc123\""));
        return resp;
    }
    let mut resp = (StatusCode::OK, BODY.to_vec()).into_response();
    let h = resp.headers_mut();
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(header::ETAG, HeaderValue::from_static("\"abc123\""));
    resp
}

/// Liar: advertises `Accept-Ranges: bytes` but ignores `Range` on GET —
/// the exact server class BACKLOG B7 exists for.
async fn lying_file() -> Response {
    let mut resp = (StatusCode::OK, BODY.to_vec()).into_response();
    resp.headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    resp
}

/// Modest: never advertises `Accept-Ranges`, yet honors `Range` — probe
/// should upgrade to segmented instead of trusting the missing header.
async fn modest_file(headers: HeaderMap) -> Response {
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if range.as_deref() == Some("bytes=0-0") {
        let mut resp = (StatusCode::PARTIAL_CONTENT, vec![0u8; 1]).into_response();
        resp.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_static("bytes 0-0/500"),
        );
        return resp;
    }
    (StatusCode::OK, vec![0u8; 500]).into_response()
}

/// Miscounting origin: HEAD says 999 bytes, the ranged GET's
/// `Content-Range` says 500 — the authoritative total must win.
async fn miscounted_file(headers: HeaderMap) -> Response {
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if range.as_deref() == Some("bytes=0-0") {
        let mut resp = (StatusCode::PARTIAL_CONTENT, vec![0u8; 1]).into_response();
        resp.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_static("bytes 0-0/500"),
        );
        return resp;
    }
    let mut resp = (StatusCode::OK, vec![0u8; 999]).into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_LENGTH, HeaderValue::from_static("999"));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    resp
}

async fn spawn_mock() -> SocketAddr {
    let app = Router::new()
        .route("/file", get(range_aware_file))
        .route("/liar", get(lying_file))
        .route("/modest", get(modest_file))
        .route("/miscount", get(miscounted_file))
        .route("/plain", get(|| async { "plain body" }))
        .route(
            "/redirect",
            get(|| async { ([(header::LOCATION, "/file")], StatusCode::FOUND) }),
        )
        .route(
            "/loop",
            get(|| async { ([(header::LOCATION, "/loop")], StatusCode::FOUND) }),
        )
        .route("/noloc", get(|| async { StatusCode::FOUND }))
        .route("/missing", get(|| async { StatusCode::NOT_FOUND }))
        .route("/chain/{n}", get(chain_hop))
        .route("/absred", get(abs_redirect))
        .route("/weak", get(weak_etag_file))
        .route("/stalled", get(stalled_liar))
        .route("/confused", get(confused_file))
        .route("/empty", get(empty_file));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

/// Redirect chain: `/chain/{n}` hops to `/chain/{n-1}` … down to `/chain/0`
/// which answers plainly. Exercises the off-by-one boundary of the redirect
/// budget (exactly N hops must succeed, N+1 must fail).
async fn chain_hop(Path(n): Path<u32>) -> Response {
    if n == 0 {
        return "chain end".into_response();
    }
    (
        [(header::LOCATION, format!("/chain/{}", n - 1))],
        StatusCode::FOUND,
    )
        .into_response()
}

/// Redirect with an ABSOLUTE Location (built from the request's Host
/// header) — the join path for absolute targets, not just relative ones.
async fn abs_redirect(headers: HeaderMap) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1");
    (
        [(header::LOCATION, format!("http://{host}/file"))],
        StatusCode::FOUND,
    )
        .into_response()
}

/// Weak validator: `W/"v1"` etag + Last-Modified — exercises
/// `etag_strong == false` and `last_modified` extraction.
async fn weak_etag_file(headers: HeaderMap) -> Response {
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if range.as_deref() == Some("bytes=0-0") {
        let mut resp = (StatusCode::PARTIAL_CONTENT, vec![0u8; 1]).into_response();
        resp.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_static("bytes 0-0/100"),
        );
        return resp;
    }
    let mut resp = (StatusCode::OK, vec![0u8; 100]).into_response();
    let h = resp.headers_mut();
    h.insert(header::ETAG, HeaderValue::from_static("W/\"v1\""));
    h.insert(
        header::LAST_MODIFIED,
        HeaderValue::from_static("Mon, 07 Sep 2026 00:00:00 GMT"),
    );
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    resp
}

/// The P0 regression endpoint: HEAD advertises ranges and a size, but the
/// confirm GET answers 200 with a body stream that NEVER yields a byte.
/// An implementation that reads the body stalls until the probe budget
/// expires; the correct one drops it instantly and downgrades.
async fn stalled_liar(method: Method) -> Response {
    if method == Method::HEAD {
        let mut resp = (StatusCode::OK, "").into_response();
        let h = resp.headers_mut();
        h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        h.insert(header::CONTENT_LENGTH, HeaderValue::from_static("8388608"));
        return resp;
    }
    let never = futures::stream::pending::<Result<&'static [u8], std::io::Error>>();
    let mut resp = (StatusCode::OK, axum::body::Body::from_stream(never)).into_response();
    resp.headers_mut()
        .insert(header::CONTENT_LENGTH, HeaderValue::from_static("8388608"));
    resp
}

/// Redirects the CONFIRM GET mid-round (302 while confirming): the
/// answer is inconclusive — HEAD's advertised `Accept-Ranges` must survive.
async fn confused_file(method: Method) -> Response {
    if method == Method::HEAD {
        let mut resp = (StatusCode::OK, "").into_response();
        let h = resp.headers_mut();
        h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        h.insert(header::CONTENT_LENGTH, HeaderValue::from_static("100"));
        return resp;
    }
    ([(header::LOCATION, "/file")], StatusCode::FOUND).into_response()
}

/// Empty resource: every `Range` request is unsatisfiable (416). The
/// confirm is inconclusive — HEAD's answer must survive.
async fn empty_file(method: Method) -> Response {
    if method == Method::HEAD {
        let mut resp = (StatusCode::OK, "").into_response();
        let h = resp.headers_mut();
        h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        h.insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
        return resp;
    }
    (StatusCode::RANGE_NOT_SATISFIABLE, "").into_response()
}

#[tokio::test]
async fn probe_extracts_resumability_metadata() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();

    let info = engine.probe(&format!("http://{addr}/file")).await.unwrap();

    assert_eq!(info.content_length, Some(1000));
    assert!(info.accept_ranges);
    assert_eq!(info.etag.as_deref(), Some("\"abc123\""));
    assert_eq!(info.url, format!("http://{addr}/file"));
}

#[tokio::test]
async fn probe_handles_servers_without_metadata() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();

    let info = engine.probe(&format!("http://{addr}/plain")).await.unwrap();

    assert_eq!(info.content_length, Some(10)); // axum sets it from the body
    assert!(
        !info.accept_ranges,
        "no Accept-Ranges and 200 on ranged GET"
    );
    assert_eq!(info.etag, None);
}

#[tokio::test]
async fn probe_follows_relative_redirects() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();

    let info = engine
        .probe(&format!("http://{addr}/redirect"))
        .await
        .unwrap();

    assert_eq!(info.url, format!("http://{addr}/file"));
    assert_eq!(info.content_length, Some(1000));
}

#[tokio::test]
async fn probe_detects_redirect_loops() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();

    let err = engine
        .probe(&format!("http://{addr}/loop"))
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::TooManyRedirects(u) if u.contains("/loop")));
}

#[tokio::test]
async fn probe_rejects_redirect_without_location() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();

    let err = engine
        .probe(&format!("http://{addr}/noloc"))
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Network(_)));
}

#[tokio::test]
async fn probe_reports_http_error_status() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();

    let err = engine
        .probe(&format!("http://{addr}/missing"))
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Http { status: 404, .. }));
}

#[tokio::test]
async fn supports_only_http_schemes() {
    let engine = HttpEngine::new().unwrap();
    assert!(engine.supports("http://a/f"));
    assert!(engine.supports("https://a/f"));
    assert!(!engine.supports("ftp://a/f"));
    assert!(!engine.supports("magnet:?xt=urn:btih:x"));
    assert!(!engine.supports("not a url"));
}

#[tokio::test]
async fn registry_routes_http_urls_end_to_end() {
    let addr = spawn_mock().await;
    let mut registry = EngineRegistry::new();
    registry
        .register(Box::new(HttpEngine::new().unwrap()))
        .unwrap();

    let url = format!("http://{addr}/file");
    let engine = registry.route(&url).expect("http engine routed");
    let info = engine.probe(&url).await.unwrap();
    assert_eq!(info.content_length, Some(1000));

    assert!(registry.route("magnet:?xt=urn:btih:x").is_none());
}

// --- B7: real-Range confirmation --------------------------------------

#[tokio::test]
async fn probe_downgrades_servers_that_ignore_range() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();

    let info = engine.probe(&format!("http://{addr}/liar")).await.unwrap();

    assert!(
        !info.accept_ranges,
        "server claims Accept-Ranges but answered 200 to a ranged GET → single-segment"
    );
    assert_eq!(info.content_length, Some(1000));
}

#[tokio::test]
async fn probe_upgrades_servers_that_honor_range_without_advertising() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();

    let info = engine
        .probe(&format!("http://{addr}/modest"))
        .await
        .unwrap();

    assert!(
        info.accept_ranges,
        "server never sent Accept-Ranges but honored Range: bytes=0-0 → segmented"
    );
    assert_eq!(info.content_length, Some(500));
}

#[tokio::test]
async fn probe_trusts_content_range_over_head_length() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();

    let info = engine
        .probe(&format!("http://{addr}/miscount"))
        .await
        .unwrap();

    assert_eq!(
        info.content_length,
        Some(500),
        "Content-Range total is authoritative when HEAD undercounts"
    );
}

// --- R2 review round: boundary and branch coverage -------------------

#[tokio::test]
async fn probe_survives_a_stalled_body_without_reading_it() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();

    // If the implementation ever reads the 200 body it stalls until the
    // 15s probe budget expires; 3s is far inside the budget, so completing
    // at all proves the body was dropped.
    let info = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        engine.probe(&format!("http://{addr}/stalled")),
    )
    .await
    .expect("probe must not wait on a stalled 200 body")
    .unwrap();

    assert!(
        !info.accept_ranges,
        "liar downgraded without reading its body"
    );
    assert_eq!(info.content_length, Some(8_388_608));
}

#[tokio::test]
async fn redirect_budget_boundary_is_exact() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap(); // default budget: 5 hops

    // Exactly 5 hops lands on /chain/0 → success.
    let info = engine
        .probe(&format!("http://{addr}/chain/5"))
        .await
        .expect("exactly N hops must succeed (N == budget)");
    assert_eq!(info.url, format!("http://{addr}/chain/0"));

    // One hop beyond the budget is refused.
    let err = engine
        .probe(&format!("http://{addr}/chain/6"))
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::TooManyRedirects(u) if u.contains("chain")));
}

#[tokio::test]
async fn zero_redirect_budget_follows_nothing() {
    let addr = spawn_mock().await;
    let strict = HttpEngine::with_max_redirects(0).unwrap();

    let info = strict
        .probe(&format!("http://{addr}/chain/0"))
        .await
        .expect("a non-redirect answer needs no budget");
    assert_eq!(info.url, format!("http://{addr}/chain/0"));

    let err = strict
        .probe(&format!("http://{addr}/chain/1"))
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::TooManyRedirects(_)));
}

#[tokio::test]
async fn probe_follows_absolute_redirect_locations() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();

    let info = engine
        .probe(&format!("http://{addr}/absred"))
        .await
        .unwrap();
    assert_eq!(info.url, format!("http://{addr}/file"));
    assert_eq!(info.content_length, Some(1000));
}

#[tokio::test]
async fn weak_etag_and_last_modified_are_captured() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();

    let info = engine.probe(&format!("http://{addr}/weak")).await.unwrap();

    assert_eq!(info.etag.as_deref(), Some("W/\"v1\""));
    assert!(!info.etag_strong, "W/-prefixed etag is a weak validator");
    assert_eq!(
        info.last_modified.as_deref(),
        Some("Mon, 07 Sep 2026 00:00:00 GMT")
    );
    assert!(info.accept_ranges, "206 confirm upgrades despite weak etag");
}

#[tokio::test]
async fn confirm_inconclusive_on_redirect_keeps_head_answer() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();

    let info = engine
        .probe(&format!("http://{addr}/confused"))
        .await
        .unwrap();
    assert!(
        info.accept_ranges,
        "a 302 mid-confirm is inconclusive; HEAD's advertisement stands"
    );
}

#[tokio::test]
async fn confirm_inconclusive_on_416_keeps_head_answer() {
    let addr = spawn_mock().await;
    let engine = HttpEngine::new().unwrap();

    let info = engine.probe(&format!("http://{addr}/empty")).await.unwrap();
    assert!(
        info.accept_ranges,
        "416 on an empty resource is inconclusive; HEAD's advertisement stands"
    );
    assert_eq!(info.content_length, Some(0));
}
