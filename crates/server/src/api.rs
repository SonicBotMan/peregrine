//! REST surface (M2-d): the daemon's whole control plane.
//!
//! Design invariants:
//! - Reads (`list`/`get`) go straight to the task manager through
//!   `Daemon::tasks()` — no scheduler involvement, no lock, no policy.
//! - Writes go through `Scheduler` facade methods ONLY (add/pause/
//!   resume/remove). The scheduler owns wake + cancel tokens; an
//!   endpoint that wrote through the task manager directly would
//!   leave a Running task un-cancelled or a Queued task asleep.
//! - Errors map task-state truth to HTTP truth: `NotFound` → 404,
//!   `IllegalTransition`/`DuplicateActive`/bad input → 409/422, the
//!   rest → 500. Clients get `{error, message}` (ApiErrorBody),
//!   never a bare string.
//! - Response bodies are the `peregrine_api` domain types verbatim —
//!   Task/TaskStatus are the wire format (v1 lesson: no parallel DTO
//!   layer to drift).

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};

use peregrine_api::task::{Priority, SegmentView, Task, TaskId, TaskStatus};
use peregrine_api::{AddTaskRequest, ApiErrorBody};
use peregrine_task_manager::TaskError;

use crate::daemon::Daemon;

#[derive(Clone)]
pub struct AppState(pub Arc<Daemon>);

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/tasks", get(list_tasks).post(create_task))
        .route("/tasks/{id}", get(get_task).delete(remove_task))
        .route("/tasks/{id}/pause", post(pause_task))
        .route("/tasks/{id}/resume", post(resume_task))
        .route("/tasks/{id}/limit", put(set_task_limit))
        .route("/tasks/{id}/priority", put(set_task_priority))
        .route("/tasks/{id}/segments", get(get_task_segments))
        .route("/tasks/{id}/peers", get(get_task_peers))
        .route("/settings", get(get_settings).put(put_settings))
        .route("/events", get(crate::ws::handler))
        // Wire contract (R2 P1-3): EVERY non-2xx is an ApiErrorBody.
        // Axum's built-in extractor rejections and router misses
        // would otherwise leak plain-text bodies that a GUI/CLI
        // cannot decode uniformly.
        .fallback(not_found_fallback)
        .method_not_allowed_fallback(method_not_allowed_fallback)
        .with_state(state)
}

async fn not_found_fallback() -> (StatusCode, Json<ApiErrorBody>) {
    (
        StatusCode::NOT_FOUND,
        Json(ApiErrorBody {
            error: "not_found".into(),
            message: "no such route".into(),
        }),
    )
}

async fn method_not_allowed_fallback() -> (StatusCode, Json<ApiErrorBody>) {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(ApiErrorBody {
            error: "method_not_allowed".into(),
            message: "method not allowed on this route".into(),
        }),
    )
}

/// [`axum::Json`] wrapper that turns every rejection (malformed
/// body, wrong content-type) into the standard ApiErrorBody shape
/// instead of axum's plain-text default (R2 P1-3).
pub struct JsonBody<T>(pub T);

impl<S, T> axum::extract::FromRequest<S> for JsonBody<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = (StatusCode, Json<ApiErrorBody>);

    async fn from_request(
        req: axum::http::Request<axum::body::Body>,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(Self(v)),
            Err(rej) => Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ApiErrorBody {
                    error: "invalid_input".into(),
                    message: rej.body_text(),
                }),
            )),
        }
    }
}

/// Map task-domain errors to (status, body). The message carries the
/// Display text — specific enough for a CLI to print verbatim —
/// EXCEPT the 500 arm (R2 P1-3): internal storage/anyhow detail can
/// contain db paths or rusqlite internals; that stays in the server
/// log, and the wire gets a fixed string.
fn map_err(e: TaskError) -> (StatusCode, Json<ApiErrorBody>) {
    match &e {
        TaskError::Storage(inner) => {
            tracing::error!(error = %inner, "storage error surfaced to REST");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiErrorBody {
                    error: "internal".to_string(),
                    message: "internal storage error".to_string(),
                }),
            )
        }
        _ => {
            let (status, code) = match &e {
                TaskError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
                TaskError::IllegalTransition { .. } => (StatusCode::CONFLICT, "illegal_transition"),
                TaskError::DuplicateActive { .. } => (StatusCode::CONFLICT, "duplicate_active"),
                TaskError::EmptyUrl | TaskError::InvalidSavePath(_) => {
                    (StatusCode::UNPROCESSABLE_ENTITY, "invalid_input")
                }
                _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
            };
            (
                status,
                Json(ApiErrorBody {
                    error: code.to_string(),
                    message: e.to_string(),
                }),
            )
        }
    }
}

/// Same treatment for [`axum::extract::Query`]: a bad `?status=`
/// value must yield ApiErrorBody, not axum's plain text.
pub struct QueryBody<T>(pub T);

impl<S, T> axum::extract::FromRequestParts<S> for QueryBody<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = (StatusCode, Json<ApiErrorBody>);

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(v)) => Ok(Self(v)),
            Err(rej) => Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ApiErrorBody {
                    error: "invalid_input".into(),
                    message: rej.body_text(),
                }),
            )),
        }
    }
}

type ApiResult<T> = Result<(StatusCode, Json<T>), (StatusCode, Json<ApiErrorBody>)>;

#[derive(Debug, Deserialize, Default)]
pub struct ListFilter {
    /// Optional status filter (`?status=queued`).
    status: Option<TaskStatus>,
}

async fn list_tasks(
    State(state): State<AppState>,
    QueryBody(f): QueryBody<ListFilter>,
) -> Result<Json<Vec<Task>>, (StatusCode, Json<ApiErrorBody>)> {
    state
        .0
        .tasks()
        .list(f.status)
        .await
        .map(Json)
        .map_err(map_err)
}

async fn get_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Task>, (StatusCode, Json<ApiErrorBody>)> {
    let id = TaskId::new(id);
    match state.0.tasks().get(&id).await.map_err(map_err)? {
        Some(task) => Ok(Json(task)),
        None => Err(map_err(TaskError::NotFound(id))),
    }
}

async fn create_task(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<AddTaskRequest>,
) -> ApiResult<Task> {
    let task = state
        .0
        .sched
        .add(req.url, req.save_path, req.priority)
        .await
        .map_err(map_err)?;
    Ok((StatusCode::CREATED, Json(task)))
}

/// Telemetry (M3-c1): the planned segment rows of a segmented
/// download. 404 for an unknown id; `[]` for a single-stream task
/// (no plan). Not part of the WS event stream — cursors churn at
/// worker speed; a client that wants the live view polls this
/// while its panel is open.
async fn get_task_segments(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<SegmentView>>, (StatusCode, Json<ApiErrorBody>)> {
    let id = TaskId::new(id);
    match state.0.segments_of(&id).await.map_err(map_err)? {
        Some(views) => Ok(Json(views)),
        None => Err(map_err(TaskError::NotFound(id))),
    }
}

/// BT deep-link (peers panel). One response shape for every task:
/// non-BT tasks get `{"bt":false}`, so the GUI renders a uniform
/// panel instead of branching on status codes.
async fn get_task_peers(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<peregrine_engine_bt::BtPeersSnapshot>, (StatusCode, Json<ApiErrorBody>)> {
    let id = TaskId::new(id);
    match state.0.bt_peers_of(&id).await.map_err(map_err)? {
        Some(snap) => Ok(Json(snap)),
        None => Err(map_err(TaskError::NotFound(id))),
    }
}

async fn pause_task(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Task> {
    let task = state
        .0
        .sched
        .pause(&TaskId::new(id))
        .await
        .map_err(map_err)?;
    Ok((StatusCode::OK, Json(task)))
}

async fn resume_task(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Task> {
    let task = state
        .0
        .sched
        .resume(&TaskId::new(id))
        .await
        .map_err(map_err)?;
    Ok((StatusCode::OK, Json(task)))
}

/// Request/response pair for the rate-limit endpoints. `0` means
/// unlimited everywhere — one representation, no nullable tricks.
#[derive(Debug, Deserialize)]
struct SetLimitBody {
    bps: u64,
}

/// GUI-verify R2 P3: clients that compose a save path from a
/// directory (quick-add, the AddDialog placeholder) need ONE shared
/// default — the daemon's, not each client's guess. Stored in the
/// settings table; `~` is daemon-resolved at task-add time.
pub const DEFAULT_SAVE_DIR: &str = "~/Downloads";

#[derive(Debug, Serialize)]
pub struct SettingsBody {
    pub global_limit_bps: u64,
    /// Default save DIRECTORY for path-composing clients. A
    /// directory, not a file path — engines receive dir + filename.
    pub default_dir: String,
}

/// Partial update: apply what's present, leave the rest alone.
#[derive(Debug, Deserialize)]
struct PutSettingsBody {
    global_limit_bps: Option<u64>,
    default_dir: Option<String>,
}

/// `PUT /tasks/{id}/limit {"bps": 131072}` — persists and pokes a
/// running engine live. 0 = unlimited.
async fn set_task_limit(
    State(state): State<AppState>,
    Path(id): Path<String>,
    JsonBody(b): JsonBody<SetLimitBody>,
) -> ApiResult<Task> {
    let task = state
        .0
        .sched
        .set_task_limit(&TaskId::new(id), b.bps)
        .await
        .map_err(map_err)?;
    Ok((StatusCode::OK, Json(task)))
}

#[derive(Debug, Deserialize)]
struct SetPriorityBody {
    priority: Priority,
}

/// `PUT /tasks/{id}/priority {"priority": "high"}` — re-ranks the
/// queue; the scheduler claims by the new order on its next pass.
/// Running tasks keep their slot (priority orders, it does not
/// preempt).
async fn set_task_priority(
    State(state): State<AppState>,
    Path(id): Path<String>,
    JsonBody(b): JsonBody<SetPriorityBody>,
) -> ApiResult<Task> {
    let task = state
        .0
        .sched
        .set_priority(&TaskId::new(id), b.priority)
        .await
        .map_err(map_err)?;
    Ok((StatusCode::OK, Json(task)))
}

/// `GET /settings` — daemon-wide knobs. Only the global rate limit
/// exists today; the shape is extensible (new keys are additive).
async fn get_settings(State(state): State<AppState>) -> Json<SettingsBody> {
    let default_dir = state
        .0
        .store
        .get_setting("default_dir")
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| DEFAULT_SAVE_DIR.to_string());
    Json(SettingsBody {
        global_limit_bps: state.0.global_budget.bps(),
        default_dir,
    })
}

/// `PUT /settings` — partial persist + apply live. Provided keys are
/// written; absent keys are untouched (a limit-only write must not
/// clobber the default dir and vice versa).
async fn put_settings(
    State(state): State<AppState>,
    JsonBody(b): JsonBody<PutSettingsBody>,
) -> Result<Json<SettingsBody>, (StatusCode, Json<ApiErrorBody>)> {
    if let Some(bps) = b.global_limit_bps {
        state.0.sched.set_global_limit(bps).await.map_err(map_err)?;
    }
    if let Some(dir) = b.default_dir {
        let dir = dir.trim().to_string();
        if dir.is_empty() {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ApiErrorBody {
                    error: "invalid_default_dir".into(),
                    message: "default_dir must not be empty".into(),
                }),
            ));
        }
        state
            .0
            .store
            .set_setting("default_dir", &dir)
            .await
            .map_err(|e| map_err(TaskError::Storage(e)))?;
    }
    Ok(get_settings(State(state)).await)
}

/// `?purge=true` opt-in: also delete downloaded data (M5.1 P0-2).
#[derive(serde::Deserialize)]
pub struct RemoveQuery {
    pub purge: Option<bool>,
}

async fn remove_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
    QueryBody(q): QueryBody<RemoveQuery>,
) -> ApiResult<serde_json::Value> {
    state
        .0
        .sched
        .remove(&TaskId::new(id), q.purge.unwrap_or(false))
        .await
        .map_err(map_err)?;
    Ok((StatusCode::OK, Json(serde_json::json!({"removed": true}))))
}

/// TCP-only DNS-rebinding guard (M3-a R2 P1-5): the loopback bind
/// keeps other MACHINES out, but a malicious page can rebind its
/// origin to 127.0.0.1 and POST "same-origin" (no CORS involved).
/// Rejecting any Host that isn't a loopback name kills the rebind.
/// Layer this on the TCP listener ONLY — the UDS surface's boundary
/// is filesystem permissions (0700), where Host is meaningless.
/// Loopback Host names the guard accepts. `tauri.localhost` is the
/// Tauri 2 webview origin on Linux/Windows (macOS uses
/// `tauri://localhost`, which never sends a Host header the HTTP
/// stack sees) — the shell's fetches come from there.
pub(crate) const LOOPBACK_HOSTS: [&str; 4] = ["127.0.0.1", "localhost", "[::1]", "tauri.localhost"];

/// CORS for the TCP face (M3-c2): the Tauri webview origin is not
/// the daemon origin, so the browser stack demands CORS headers on
/// every fetch. Origins pinned to the two Tauri webview origins plus
/// the vite dev server; host_guard stays the outer perimeter (both
/// layers, in order: host → cors).
pub fn with_tcp_cors(app: axum::Router) -> axum::Router {
    use tower_http::cors::{AllowMethods, AllowOrigin};
    let cors = tower_http::cors::CorsLayer::new()
        .allow_origin(AllowOrigin::list([
            "http://tauri.localhost".parse().unwrap(),
            "tauri://localhost".parse().unwrap(),
            "http://localhost:5199".parse().unwrap(),
        ]))
        .allow_methods(AllowMethods::list([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PATCH,
        ]))
        .allow_headers(tower_http::cors::Any);
    app.layer(cors)
}

pub fn with_host_guard(app: axum::Router) -> axum::Router {
    use axum::response::IntoResponse;
    app.layer(axum::middleware::from_fn(
        |req: axum::extract::Request, next: axum::middleware::Next| async move {
            let ok = req
                .headers()
                .get(axum::http::header::HOST)
                .and_then(|h| h.to_str().ok())
                .map(|h| {
                    let host = h.rsplit_once(':').map(|(n, _)| n).unwrap_or(h);
                    LOOPBACK_HOSTS.contains(&host)
                })
                .unwrap_or(false); // HTTP/1.1 requires Host; absence = hostile
            if ok {
                next.run(req).await
            } else {
                tracing::warn!(
                    host = ?req.headers().get(axum::http::header::HOST),
                    "tcp: non-loopback Host rejected (DNS rebinding guard)"
                );
                (
                    axum::http::StatusCode::FORBIDDEN,
                    "forbidden: loopback Host required",
                )
                    .into_response()
            }
        },
    ))
}

/// Timing-safe `Bearer` token check for the TCP face.
///
/// Loopback binding is the base perimeter, but loopback is still a
/// shared bus: any local process (notably browsers, via DNS
/// rebinding or a rogue page hitting `127.0.0.1`) can dial it.
/// `with_host_guard` kills rebinding; this layer is the second
/// factor — a secret the legitimate client holds. When the daemon
/// runs with `--auth-token`, every request except `/health` must
/// carry `Authorization: Bearer <token>`.
///
/// `/health` stays open on purpose: systemd probes and the
/// sidecar readiness check hit it before any client could know a
/// token, and its body is pid/uptime only — nothing sensitive.
///
/// The comparison folds XOR across the full bearer value so a
/// mismatch leaks nothing about how many leading bytes matched —
/// the usual defense against comparing secrets byte-by-byte.
pub fn with_bearer_auth(app: axum::Router, token: String) -> axum::Router {
    use axum::response::IntoResponse;
    let token = std::sync::Arc::new(token);
    app.layer(axum::middleware::from_fn_with_state(
        token.clone(),
        |axum::extract::State(token): axum::extract::State<std::sync::Arc<String>>,
         req: axum::extract::Request,
         next: axum::middleware::Next| async move {
            if req.uri().path() == "/health" {
                return next.run(req).await;
            }
            let presented = req
                .headers()
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|h| h.to_str().ok())
                .and_then(|h| h.strip_prefix("Bearer "));
            let ok = presented.is_some_and(|p| {
                let expect = token.as_bytes();
                let got = p.as_bytes();
                // Length is not secret (bearer syntax is public);
                // compare the overlapping prefix and fold the length
                // mismatch into the same accumulator.
                let mut diff = (expect.len() ^ got.len()) as u8;
                for (a, b) in expect.iter().zip(got) {
                    diff |= a ^ b;
                }
                diff == 0
            });
            if ok {
                next.run(req).await
            } else {
                tracing::warn!(
                    path = %req.uri().path(),
                    "tcp: bearer auth rejected"
                );
                (
                    axum::http::StatusCode::UNAUTHORIZED,
                    [("www-authenticate", "Bearer")],
                    "unauthorized: bearer token required",
                )
                    .into_response()
            }
        },
    ))
}

#[cfg(test)]
mod bearer_auth_tests {
    use super::with_bearer_auth;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt; // oneshot

    fn app() -> axum::Router {
        // Two routes: the exempt probe and a representative API hit.
        axum::Router::new()
            .route("/health", get(|| async { "ok" }))
            .route("/tasks", get(|| async { "tasks" }))
    }

    #[tokio::test]
    async fn no_token_401_on_api() {
        let res = with_bearer_auth(app(), "s3cret".into())
            .oneshot(Request::get("/tasks").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(res.headers().get("www-authenticate").unwrap(), "Bearer");
    }

    #[tokio::test]
    async fn wrong_token_401() {
        let res = with_bearer_auth(app(), "s3cret".into())
            .oneshot(
                Request::get("/tasks")
                    .header("authorization", "Bearer wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn right_token_200() {
        let res = with_bearer_auth(app(), "s3cret".into())
            .oneshot(
                Request::get("/tasks")
                    .header("authorization", "Bearer s3cret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn health_exempt() {
        // The probe surface must stay open: systemd and the sidecar
        // readiness check hit /health before any token is known.
        let res = with_bearer_auth(app(), "s3cret".into())
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn prefix_collision_rejected() {
        // "s3cret-extra" shares the prefix — the folded length
        // mismatch must still reject it.
        let res = with_bearer_auth(app(), "s3cret".into())
            .oneshot(
                Request::get("/tasks")
                    .header("authorization", "Bearer s3cret-extra")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn non_bearer_scheme_rejected() {
        let res = with_bearer_auth(app(), "s3cret".into())
            .oneshot(
                Request::get("/tasks")
                    .header("authorization", "Basic c2VjcmV0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
}
