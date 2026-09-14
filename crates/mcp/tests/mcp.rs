//! M5 integration tests: the MCP server (tools, resources,
//! notifications) end-to-end against a REAL daemon stack —
//! scheduler + store + bus — over a REAL UDS socket and a REAL WS
//! events stream. Only the network engine is scripted (a hanging
//! port, same rig philosophy as the server's own api tests).
//!
//! The MCP client side is rmcp itself (`serve_client` over an
//! in-memory duplex): what these tests assert is what a real MCP
//! host (Claude Desktop, etc.) would see on the wire.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use peregrine_api::download::{DownloadJob, DownloadOutcome, SharedProgressSink};
use peregrine_api::error::ApiError;
use peregrine_scheduler::{DownloadPort, SchedulerConfig};
use peregrine_server::daemon::Daemon;
use rmcp::ServiceExt;
use rmcp::handler::client::ClientHandler;
use rmcp::model::{
    CallToolResult, ReadResourceRequestParams, ResourceUpdatedNotificationParam,
    SubscribeRequestParams,
};
use rmcp::service::{NotificationContext, RoleClient};
use tokio_util::sync::CancellationToken;

// ---- rig ----------------------------------------------------------

/// Scripted engine: accepts jobs and hangs until cancelled — a
/// task added via MCP reaches `running` and stays there.
struct HangingPort;

impl DownloadPort for HangingPort {
    fn auto_download(
        &self,
        _job: DownloadJob,
        _progress: SharedProgressSink,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<DownloadOutcome, ApiError>> + Send + '_>> {
        Box::pin(async move {
            cancel.cancelled().await;
            Err(ApiError::Cancelled)
        })
    }

    fn purge(
        &self,
        _url: &str,
        _sink: &std::path::Path,
        _purge_files: bool,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}

/// A daemon on a temp UDS socket + temp TCP port serving the SAME
/// router (REST + WS /events). Returns the `TempDir` guard so the
/// caller's binding keeps the whole rig (socket, db, daemon) alive
/// AND cleans it on drop — M5.1 P2-8: `.keep()` leaked one dir per
/// test invocation.
async fn daemon_rig() -> (tempfile::TempDir, PathBuf, String, Arc<Daemon>) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("mcp-test.sock");
    let daemon = Arc::new(
        Daemon::build_with_port(
            Some(&dir.path().join("tasks.db")),
            SchedulerConfig::default(),
            Arc::new(HangingPort) as Arc<dyn DownloadPort>,
        )
        .unwrap(),
    );
    daemon.start().await.unwrap();
    let app = daemon.router();

    let (uds_l, _id) = peregrine_server::uds::bind(&socket).await.unwrap();
    tokio::spawn({
        let app = app.clone();
        async move {
            let _ = axum::serve(uds_l, app).await;
        }
    });

    let tcp_l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_port = tcp_l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(tcp_l, app).await;
    });

    let events_url = format!("ws://127.0.0.1:{ws_port}/events");
    (dir, socket, events_url, daemon)
}

/// MCP client capturing every resource-updated push the server
/// sends.
struct CapturingClient {
    updates: tokio::sync::mpsc::UnboundedSender<String>,
}

impl Default for CapturingClient {
    fn default() -> Self {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        Self { updates: tx }
    }
}

impl ClientHandler for CapturingClient {
    async fn on_resource_updated(
        &self,
        params: ResourceUpdatedNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        let _ = self.updates.send(params.uri);
    }
}

/// Wire an MCP server (pointed at `socket`) to an in-memory rmcp
/// client; returns the running client peer and the update channel.
async fn mcp_pair(
    socket: PathBuf,
    events_url: String,
) -> (
    rmcp::service::RunningService<RoleClient, CapturingClient>,
    tokio::sync::mpsc::UnboundedReceiver<String>,
) {
    let (server_io, client_io) = tokio::io::duplex(8192);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let handler = CapturingClient { updates: tx };

    // ServiceExt::serve on both ends — same wiring rmcp's own
    // integration tests use (serve_server blocks on a different
    // transport contract and deadlocks over raw duplex).
    tokio::spawn(async move {
        let server = peregrine_mcp::PeregrineMcp::new(socket, events_url)
            .serve(server_io)
            .await
            .unwrap();
        let _ = server.waiting().await;
    });

    let client = handler.serve(client_io).await.unwrap();
    (client, rx)
}

fn tool_args(json: serde_json::Value) -> rmcp::model::JsonObject {
    match json {
        serde_json::Value::Object(m) => m,
        _ => panic!("tool args must be an object"),
    }
}

async fn call(
    client: &rmcp::service::RunningService<RoleClient, CapturingClient>,
    name: &str,
    args: serde_json::Value,
) -> CallToolResult {
    let mut params = rmcp::model::CallToolRequestParams::new(name.to_string());
    params.arguments = Some(tool_args(args));
    client.peer().call_tool(params).await.unwrap()
}

fn text_of(r: &rmcp::model::CallToolResult) -> String {
    r.content
        .iter()
        .map(|b| match b {
            rmcp::model::ContentBlock::Text(t) => t.text.as_str().to_string(),
            _ => String::new(),
        })
        .collect()
}

// ---- tests --------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn tools_catalog_and_roundtrip() {
    let (_dir, socket, events_url, _daemon) = daemon_rig().await;
    let (client, _rx) = mcp_pair(socket, events_url).await;

    // Catalog: ten tools, object schemas.
    let tools = client.peer().list_tools(None).await.unwrap();
    assert_eq!(tools.tools.len(), 10, "tool catalog size");
    for t in &tools.tools {
        assert_eq!(
            t.input_schema.get("type").unwrap(),
            "object",
            "{} schema",
            t.name
        );
    }

    // add → running (scripted engine hangs) → list → get.
    let tmp = tempfile::tempdir().unwrap();
    let save = tmp.path().join("f.bin");
    let added = call(
        &client,
        "add_download",
        serde_json::json!({
            "url": "http://127.0.0.1:1/file.bin",
            "save_path": save.display().to_string()
        }),
    )
    .await;
    assert!(
        !added.is_error.unwrap_or(false),
        "add_download: {}",
        text_of(&added)
    );
    let task: serde_json::Value = serde_json::from_str(&text_of(&added)).unwrap();
    let id = task["id"].as_str().unwrap().to_string();

    let status_of = |v: &serde_json::Value| v["status"].as_str().unwrap().to_string();
    // Poll get_download until the scheduler marks the task running
    // (add → queue → dispatch is async).
    let mut running = false;
    for _ in 0..400 {
        let v = text_of(&call(&client, "get_download", serde_json::json!({"id": id})).await);
        if v.contains("running") {
            running = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(running, "task never reached running status");

    let listed = call(&client, "list_downloads", serde_json::json!({})).await;
    let arr: serde_json::Value = serde_json::from_str(&text_of(&listed)).unwrap();
    let arr = arr.as_array().unwrap();
    assert!(arr.iter().any(|t| t["id"] == *id));

    let one = call(&client, "get_download", serde_json::json!({"id": id})).await;
    let one: serde_json::Value = serde_json::from_str(&text_of(&one)).unwrap();
    assert_eq!(status_of(&one), "running");

    // Per-task limit — live write path.
    let limited = call(
        &client,
        "set_download_limit",
        serde_json::json!({"id": id, "bps": 4096}),
    )
    .await;
    assert!(
        !limited.is_error.unwrap_or(false),
        "set limit: {}",
        text_of(&limited)
    );

    // Segments endpoint exists for this task (may be empty for the
    // hanging engine — must not error).
    let segs = call(
        &client,
        "get_download_segments",
        serde_json::json!({"id": id}),
    )
    .await;
    assert!(
        !segs.is_error.unwrap_or(false),
        "segments: {}",
        text_of(&segs)
    );

    // Settings + global limit.
    let s = call(&client, "get_settings", serde_json::json!({})).await;
    assert!(!s.is_error.unwrap_or(false));
    let g = call(
        &client,
        "set_global_limit",
        serde_json::json!({"bps": 1024}),
    )
    .await;
    assert!(
        !g.is_error.unwrap_or(false),
        "global limit: {}",
        text_of(&g)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn add_download_priority_is_case_insensitive() {
    let (_dir, socket, events_url, _daemon) = daemon_rig().await;
    let (client, _rx) = mcp_pair(socket, events_url).await;

    let tmp = tempfile::tempdir().unwrap();
    // "HIGH" (not "high") — the tool layer normalizes so the
    // strict REST contract never rejects a model's casing.
    let added = call(
        &client,
        "add_download",
        serde_json::json!({
            "url": "http://127.0.0.1:1/f.bin",
            "save_path": tmp.path().join("f.bin").display().to_string(),
            "priority": "HIGH"
        }),
    )
    .await;
    assert!(
        !added.is_error.unwrap_or(false),
        "mixed-case priority must be accepted: {}",
        text_of(&added)
    );
    let task: serde_json::Value = serde_json::from_str(&text_of(&added)).unwrap();
    assert_eq!(task["priority"].as_str().unwrap(), "high");

    // Garbage priority is a readable tool error, not a panic.
    let bad = call(
        &client,
        "add_download",
        serde_json::json!({
            "url": "http://127.0.0.1:1/f.bin",
            "save_path": tmp.path().join("g.bin").display().to_string(),
            "priority": "urgent"
        }),
    )
    .await;
    assert!(bad.is_error.unwrap_or(false), "garbage priority rejected");
    assert!(text_of(&bad).contains("priority"));
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_task_is_tool_error_not_protocol_error() {
    let (_dir, socket, events_url, _daemon) = daemon_rig().await;
    let (client, _rx) = mcp_pair(socket, events_url).await;

    let r = call(&client, "get_download", serde_json::json!({"id": "nope"})).await;
    assert!(r.is_error.unwrap(), "expected tool-level error");
    assert!(text_of(&r).contains("nope"), "error mentions the id");
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_down_is_tool_error() {
    // No daemon on this socket: every proxy tool degrades to a
    // readable tool error, never a protocol crash.
    let dir = tempfile::tempdir().unwrap();
    let (server_io, client_io) = tokio::io::duplex(1024);
    tokio::spawn(async move {
        let server = peregrine_mcp::PeregrineMcp::new(
            dir.path().join("absent.sock"),
            "ws://127.0.0.1:1/events".to_string(),
        )
        .serve(server_io)
        .await
        .unwrap();
        let _ = server.waiting().await;
    });
    let client = CapturingClient::default().serve(client_io).await.unwrap();

    let r = call(&client, "list_downloads", serde_json::json!({})).await;
    assert!(r.is_error.unwrap(), "daemon down must be a tool error");
}

#[tokio::test(flavor = "multi_thread")]
async fn resources_list_read_and_404() {
    let (_dir, socket, events_url, _daemon) = daemon_rig().await;
    let (client, _rx) = mcp_pair(socket, events_url).await;

    let tmp = tempfile::tempdir().unwrap();
    let save = tmp.path().join("f.bin");
    let added = call(
        &client,
        "add_download",
        serde_json::json!({
            "url": "http://127.0.0.1:1/file.bin",
            "save_path": save.display().to_string()
        }),
    )
    .await;
    let task: serde_json::Value = serde_json::from_str(&text_of(&added)).unwrap();
    let id = task["id"].as_str().unwrap().to_string();

    // List: statics + one entry per task.
    let res = client.peer().list_resources(None).await.unwrap();
    let uris: Vec<String> = res.resources.iter().map(|r| r.uri.to_string()).collect();
    assert!(uris.contains(&"tasks://".to_string()));
    assert!(uris.contains(&"settings://".to_string()));
    assert!(uris.contains(&format!("task://{id}")));

    // Read the list resource: JSON array with our task.
    let read = client
        .peer()
        .read_resource(ReadResourceRequestParams::new("tasks://"))
        .await
        .unwrap();
    let text = match &read.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text.clone(),
        _ => panic!("expected text contents"),
    };
    let arr: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(arr.as_array().unwrap().iter().any(|t| t["id"] == *id));

    // Read one task + settings.
    let one = client
        .peer()
        .read_resource(ReadResourceRequestParams::new(format!("task://{id}")))
        .await
        .unwrap();
    assert!(one.contents.len() == 1);
    let s = client
        .peer()
        .read_resource(ReadResourceRequestParams::new("settings://"))
        .await
        .unwrap();
    assert!(s.contents.len() == 1);

    // Unknown task URI → RESOURCE_NOT_FOUND protocol error.
    let err = client
        .peer()
        .read_resource(ReadResourceRequestParams::new("task://nope"))
        .await
        .unwrap_err();
    assert!(
        format!("{err:?}").contains("-32002"),
        "expected RESOURCE_NOT_FOUND, got {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribe_pushes_updates_through_real_ws() {
    let (_dir, socket, events_url, _daemon) = daemon_rig().await;
    let (client, mut rx) = mcp_pair(socket, events_url).await;

    // Subscribe the list resource (legacy resources/subscribe).
    #[allow(deprecated)]
    client
        .peer()
        .subscribe(SubscribeRequestParams::new("tasks://"))
        .await
        .unwrap();

    // Trigger a bus event: add a task (TaskAdded → tasks://
    // invalidated). The bridge is lazy (first subscribe) and the
    // WS connect takes a moment — tolerate by polling for the
    // notification with generous retries.
    let tmp = tempfile::tempdir().unwrap();
    let save = tmp.path().join("f.bin");
    let added = call(
        &client,
        "add_download",
        serde_json::json!({
            "url": "http://127.0.0.1:1/file.bin",
            "save_path": save.display().to_string()
        }),
    )
    .await;
    assert!(!added.is_error.unwrap_or(false));

    let mut got = false;
    for _ in 0..400 {
        if let Ok(uri) = rx.try_recv() {
            assert_eq!(uri, "tasks://");
            got = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(got, "expected a resources/updated push for tasks://");
}
