//! MCP server exposing the peregrine daemon (M5, PROPOSAL §3.4:
//! "Agent 原生"). One handler, three capability surfaces:
//!
//! - **Tools** — thin, typed proxies over the daemon's REST surface.
//!   The MCP layer deliberately adds no policy of its own: every
//!   rule (queue, throttle, resume semantics) already lives in the
//!   scheduler; an MCP tool that re-decided any of it would be a
//!   second brain drifting from the first (v1 lesson, same as the
//!   GUI).
//! - **Resources** — `tasks://` (list), `task://{id}` (one task),
//!   `settings://` (daemon knobs). Read-only views of the same
//!   domain types the REST API returns, JSON-encoded — no parallel
//!   DTO layer to rot.
//! - **Notifications** — clients that `subscribe` a resource URI
//!   get `notifications/resources/updated` pushes. The feed is the
//!   daemon's WS `/events` stream (typed `EngineEvent`s), bridged
//!   per session: map event → URIs, intersect with subscriptions,
//!   push through the session's [`Peer`]. Legacy
//!   `resources/subscribe` (what today's Claude Desktop speaks) —
//!   the newer `subscriptions/listen` path is left to a follow-up
//!   (BACKLOG).
//!
//! Error contract (rmcp's two failure modes, used as documented):
//! daemon-side failures (unknown id, illegal transition, daemon
//! down) are **tool-level errors** — `Ok(CallToolResult::error(..))`
//! so the model SEES them and can react. Only unroutable requests
//! (unknown tool name) become protocol errors.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use peregrine_api::task::Task;
use peregrine_api::uds_client::DaemonClient;
use rmcp::Peer;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ErrorCode, ErrorData, Implementation,
    InitializeResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, SubscribeRequestParams, UnsubscribeRequestParams,
};
use rmcp::service::{RequestContext, RoleServer};
use tokio::sync::Mutex;

mod events;
mod tools;

pub use events::RES_TASKS_URI;

/// Default daemon WS `/events` endpoint (loopback TCP the daemon
/// serves for the GUI; the bridge reconnects forever, so a daemon
/// that comes up later is fine).
pub const DEFAULT_EVENTS_URL: &str = "ws://127.0.0.1:8800/events";

/// Session-shared state (peer handle, subscription set, bridge
/// task). Clones of [`PeregrineMcp`] share one `Inner`.
pub struct PeregrineMcp {
    http: DaemonClient,
    events_url: String,
    inner: Arc<Inner>,
}

struct Inner {
    peer: OnceLock<Peer<RoleServer>>,
    subs: Mutex<HashSet<String>>,
    /// The events bridge task is started lazily on the first
    /// `subscribe` — no WS connection is opened for sessions that
    /// never subscribe.
    bridge: OnceLock<tokio::task::JoinHandle<()>>,
}

impl PeregrineMcp {
    pub fn new(socket: PathBuf, events_url: impl Into<String>) -> Self {
        Self {
            http: DaemonClient::new(socket),
            events_url: events_url.into(),
            inner: Arc::new(Inner {
                peer: OnceLock::new(),
                subs: Mutex::new(HashSet::new()),
                bridge: OnceLock::new(),
            }),
        }
    }
}

impl ServerHandler for PeregrineMcp {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(
            Implementation::new("peregrine", env!("CARGO_PKG_VERSION"))
                .with_title("Peregrine Download Manager"),
        )
        .with_instructions(
            "Manage downloads through the peregrine daemon. Tools are thin \
             proxies over the daemon REST API; task JSON fields are \
             self-describing (status: queued|running|paused|completed|failed|\
             cancelled; received_bytes/total_bytes). Resources: tasks:// lists \
             all tasks, task://{id} reads one, settings:// reads daemon \
             settings (no push — REST writes don't flow on the event bus). \
             Subscribe a resource URI to receive update pushes while \
             downloads progress.",
        )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools::tool_catalog()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let _ = self.inner.peer.set(context.peer);
        let name = request.name.as_ref().to_string();
        let args = request.arguments.unwrap_or_default();
        Ok(tools::dispatch(self, &name, args).await.into())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        // Dynamic listing: one Resource per task + the two statics.
        let mut resources = vec![
            resource(RES_TASKS_URI, "All download tasks (JSON array)"),
            resource("settings://", "Daemon settings (JSON)"),
        ];
        match self.list_tasks().await {
            Ok(tasks) => {
                for t in tasks {
                    resources.push(resource(
                        &format!("task://{}", t.id),
                        &format!("Download task {} ({})", t.id, t.url),
                    ));
                }
            }
            Err(e) => {
                // Daemon down: still list the statics (reads will
                // surface the error properly).
                tracing::warn!(error = %e, "listing task resources failed");
            }
        }
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let _ = self.inner.peer.set(context.peer);
        let uri = request.uri.as_str().to_string();
        let text = match uri.as_str() {
            RES_TASKS_URI => serde_json::to_string(&self.list_tasks().await.map_err(daemon_down)?)
                .map_err(internal)?,
            "settings://" => serde_json::to_string(
                &self
                    .http
                    .request_json::<(), serde_json::Value>("GET", "/settings", None)
                    .await
                    .map_err(daemon_down)?,
            )
            .map_err(internal)?,
            u if u.starts_with("task://") => {
                let id = &u["task://".len()..];
                if id.is_empty() || id.contains('/') {
                    return Err(ErrorData::new(
                        ErrorCode::INVALID_PARAMS,
                        "expected uri task://{id}",
                        None,
                    ));
                }
                serde_json::to_string(&self.get_task(id).await.map_err(|e| {
                    // The daemon's 404 body carries {"error":"not_found"};
                    // match on that, not on the status digits (a size
                    // or byte-count mentioning 404 must not 404 here).
                    if e.to_string().contains("\"not_found\"") {
                        ErrorData::resource_not_found(format!("no task {id}"), None)
                    } else {
                        daemon_down(e)
                    }
                })?)
                .map_err(internal)?
            }
            other => {
                return Err(ErrorData::resource_not_found(
                    format!("unknown resource {other}"),
                    None,
                ));
            }
        };
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(text, uri).with_mime_type("application/json"),
        ])
        .into())
    }

    /// Legacy `resources/subscribe` — what today's Claude Desktop
    /// speaks. Deprecated in favor of `subscriptions/listen` in
    /// 2026-07-28 spec; supported here deliberately for client
    /// compatibility (the new path is a BACKLOG item).
    #[allow(deprecated)]
    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let _ = self.inner.peer.set(context.peer.clone());
        {
            let mut subs = self.inner.subs.lock().await;
            subs.insert(request.uri.to_string());
        }
        events::ensure_bridge(self.inner.clone(), &self.events_url);
        tracing::debug!(uri = %request.uri, "subscribed");
        Ok(())
    }

    #[allow(deprecated)]
    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let mut subs = self.inner.subs.lock().await;
        subs.remove(&request.uri.to_string());
        tracing::debug!(uri = %request.uri, "unsubscribed");
        Ok(())
    }
}

impl PeregrineMcp {
    pub(crate) fn client(&self) -> &DaemonClient {
        &self.http
    }

    async fn list_tasks(&self) -> anyhow::Result<Vec<Task>> {
        self.http.request_json("GET", "/tasks", None::<&u8>).await
    }

    async fn get_task(&self, id: &str) -> anyhow::Result<Task> {
        self.http
            .request_json("GET", &format!("/tasks/{id}"), None::<&u8>)
            .await
    }
}

fn resource(uri: &str, name: &str) -> Resource {
    Resource::new(uri.to_string(), name.to_string()).with_mime_type("application/json")
}

fn daemon_down(e: anyhow::Error) -> ErrorData {
    ErrorData::new(
        ErrorCode::INTERNAL_ERROR,
        format!("daemon unreachable: {e}"),
        None,
    )
}

fn internal(e: serde_json::Error) -> ErrorData {
    ErrorData::new(
        ErrorCode::INTERNAL_ERROR,
        format!("encode failed: {e}"),
        None,
    )
}
