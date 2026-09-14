//! Tool surface (M5): thin proxies over the daemon REST API.
//!
//! Catalog is hand-rolled `Tool` entries with explicit JSON Schemas
//! — ten small tools, no codegen dependency (schemars stays out of
//! the build). `dispatch` is the single router: a match on the tool
//! name, serde-typed argument structs, `CallToolResult` on the way
//! out. Daemon failures map to tool-level errors (the model sees
//! them); only unknown tool names return an error *result* the
//! model can read and correct.

use std::sync::Arc;

use peregrine_api::task::Task;
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool};
use serde::Deserialize;
use serde_json::json;

use crate::PeregrineMcp;

pub fn tool_catalog() -> Vec<Tool> {
    TOOL_SPECS
        .iter()
        .map(|(name, desc, schema)| {
            let obj: JsonObject = serde_json::from_value(schema()).expect("tool schema valid");
            Tool::new(*name, *desc, Arc::new(obj))
        })
        .collect()
}

/// (name, description, schema-builder). Kept adjacent so a tool
/// can't exist in the catalog but not in `dispatch` (tests assert
/// the sets match).
type SchemaBuilder = fn() -> serde_json::Value;

const TOOL_SPECS: &[(&str, &str, SchemaBuilder)] = &[
    (
        "add_download",
        "Add a URL to the download queue (http/https/m3u8/ftp). Returns the created task. \
         save_path must be an ABSOLUTE file path including the filename (relative paths \
         resolve against the daemon's cwd — pass absolute). No dedup: adding the same \
         URL twice creates two independent tasks (clean up with remove_download). \
         priority is low|normal|high (default normal; case-insensitive).",
        || {
            json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "source URL"},
                    "save_path": {"type": "string", "description": "absolute destination file path (incl. filename)"},
                    "priority": {"type": "string", "enum": ["low", "normal", "high"], "default": "normal"}
                },
                "required": ["url", "save_path"]
            })
        },
    ),
    (
        "list_downloads",
        "List all tasks (optionally filtered by status: queued|running|paused|completed|failed|cancelled).",
        || {
            json!({
                "type": "object",
                "properties": {
                    "status": {"type": "string", "enum": ["queued", "running", "paused", "completed", "failed", "cancelled"]}
                }
            })
        },
    ),
    (
        "get_download",
        "Get one task by id (status, received/total bytes, error, limits).",
        || {
            json!({
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"]
            })
        },
    ),
    (
        "pause_download",
        "Pause a running/queued download. Returns the updated task.",
        || {
            json!({
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"]
            })
        },
    ),
    (
        "resume_download",
        "Resume a paused download. Returns the updated task.",
        || {
            json!({
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"]
            })
        },
    ),
    (
        "remove_download",
        "Remove a task from the list. purge=true (default false) also deletes partial files.",
        || {
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "purge": {"type": "boolean", "default": false}
                },
                "required": ["id"]
            })
        },
    ),
    (
        "set_download_limit",
        "Set a per-task rate limit in bytes/sec (0 = unlimited). Applies live.",
        || {
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "bps": {"type": "integer", "minimum": 0}
                },
                "required": ["id", "bps"]
            })
        },
    ),
    (
        "get_download_segments",
        "Segment telemetry for a segmented (http) download: per-segment ranges and progress.",
        || {
            json!({
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"]
            })
        },
    ),
    (
        "get_settings",
        "Read daemon settings (global rate limit etc.).",
        || json!({"type": "object", "properties": {}}),
    ),
    (
        "set_global_limit",
        "Set the daemon-wide rate limit in bytes/sec (0 = unlimited). Applies live to all downloads.",
        || {
            json!({
                "type": "object",
                "properties": {"bps": {"type": "integer", "minimum": 0}},
                "required": ["bps"]
            })
        },
    ),
];

pub async fn dispatch(mcp: &PeregrineMcp, name: &str, args: JsonObject) -> CallToolResult {
    macro_rules! call {
        ($method:expr, $path:expr, $body:expr, $t:ty) => {
            match mcp
                .client()
                .request_json::<_, $t>($method, &$path, $body)
                .await
            {
                Ok(v) => return ok_json(&v),
                Err(e) => return tool_error(e),
            }
        };
    }
    match name {
        "add_download" => {
            let Some(a) = decode::<AddArgs>(args) else {
                return bad_args::<AddArgs>();
            };
            // Models send "High"/"NORMAL" as readily as "high";
            // normalize here so the REST contract stays strict
            // while the tool surface is forgiving.
            let priority = match a.priority.to_ascii_lowercase().as_str() {
                "low" | "normal" | "high" => a.priority.to_ascii_lowercase(),
                other => {
                    return tool_error(anyhow::anyhow!(
                        "invalid priority {other:?}: expected low|normal|high (case-insensitive)"
                    ));
                }
            };
            let body = json!({"url": a.url, "save_path": a.save_path, "priority": priority});
            call!("POST", "/tasks", Some(&body), Task)
        }
        "list_downloads" => {
            let Some(a) = decode::<ListArgs>(args) else {
                return bad_args::<ListArgs>();
            };
            let path = match a.status {
                // M5.1 P2-3: case-normalize like priority — a model
                // sending "Running" must not 422 on a wall it can't
                // see through.
                Some(s) => format!("/tasks?status={}", s.to_lowercase()),
                None => "/tasks".to_string(),
            };
            call!("GET", path, None::<&serde_json::Value>, Vec<Task>)
        }
        "get_download" => {
            let Some(id) = id_arg(args) else {
                return bad_args::<IdArgs>();
            };
            call!("GET", format!("/tasks/{}", id), None::<&u8>, Task)
        }
        "pause_download" => {
            let Some(id) = id_arg(args) else {
                return bad_args::<IdArgs>();
            };
            call!("POST", format!("/tasks/{}/pause", id), None::<&u8>, Task)
        }
        "resume_download" => {
            let Some(id) = id_arg(args) else {
                return bad_args::<IdArgs>();
            };
            call!("POST", format!("/tasks/{}/resume", id), None::<&u8>, Task)
        }
        "remove_download" => {
            let Some(a) = decode::<RemoveArgs>(args) else {
                return bad_args::<RemoveArgs>();
            };
            // P2-2 boundary check, same as id_arg().
            if a.id.is_empty() || a.id.contains('/') {
                return bad_args::<RemoveArgs>();
            }
            let path = if a.purge.unwrap_or(false) {
                format!("/tasks/{}?purge=true", a.id)
            } else {
                format!("/tasks/{}", a.id)
            };
            call!("DELETE", path, None::<&u8>, serde_json::Value)
        }
        "set_download_limit" => {
            let Some(a) = decode::<LimitArgs>(args) else {
                return bad_args::<LimitArgs>();
            };
            // P2-2 boundary check, same as id_arg().
            if a.id.is_empty() || a.id.contains('/') {
                return bad_args::<LimitArgs>();
            }
            let body = json!({"bps": a.bps});
            call!("PUT", format!("/tasks/{}/limit", a.id), Some(&body), Task)
        }
        "get_download_segments" => {
            let Some(id) = id_arg(args) else {
                return bad_args::<IdArgs>();
            };
            call!(
                "GET",
                format!("/tasks/{}/segments", id),
                None::<&u8>,
                Vec<peregrine_api::task::SegmentView>
            )
        }
        "get_settings" => {
            call!("GET", "/settings", None::<&u8>, serde_json::Value)
        }
        "set_global_limit" => {
            let Some(a) = decode::<GlobalLimitArgs>(args) else {
                return bad_args::<GlobalLimitArgs>();
            };
            let body = json!({"global_limit_bps": a.bps});
            call!("PUT", "/settings", Some(&body), serde_json::Value)
        }
        other => tool_error(anyhow::anyhow!("unknown tool {other}")),
    }
}

// ---- argument structs (serde-typed; defaults match the CLI) ----

#[derive(Deserialize)]
struct AddArgs {
    url: String,
    save_path: String,
    #[serde(default = "default_priority")]
    priority: String,
}

fn default_priority() -> String {
    "normal".into()
}

#[derive(Deserialize)]
struct ListArgs {
    status: Option<String>,
}

#[derive(Deserialize)]
struct IdArgs {
    id: String,
}

#[derive(Deserialize)]
struct RemoveArgs {
    id: String,
    purge: Option<bool>,
}

#[derive(Deserialize)]
struct LimitArgs {
    id: String,
    bps: u64,
}

#[derive(Deserialize)]
struct GlobalLimitArgs {
    bps: u64,
}

/// Decode an id-bearing argument set + boundary-check the id
/// (M5.1 P2-2: reject `/`-containing ids BEFORE they hit the
/// path — same rule the resource layer applies; a `/` would
/// silently address a different route).
fn id_arg(args: JsonObject) -> Option<String> {
    let a: IdArgs = decode(args)?;
    if a.id.is_empty() || a.id.contains('/') {
        return None;
    }
    Some(a.id)
}

/// Decode typed args; `None` on shape mismatch (caller turns it
/// into a tool error).
fn decode<T: serde::de::DeserializeOwned>(args: JsonObject) -> Option<T> {
    serde_json::from_value(serde_json::Value::Object(args)).ok()
}

/// A readable tool-level error for argument-shape failures: names
/// the expected fields from the (static) schema.
fn bad_args<T>() -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(format!(
        "invalid arguments: expected object matching the tool's inputSchema ({} fields)",
        std::any::type_name::<T>()
    ))])
}

fn ok_json(v: &impl serde::Serialize) -> CallToolResult {
    match serde_json::to_string(v) {
        Ok(s) => CallToolResult::success(vec![ContentBlock::text(s)]),
        Err(e) => CallToolResult::error(vec![ContentBlock::text(format!("encode failed: {e}"))]),
    }
}

fn tool_error(e: anyhow::Error) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(format!("{e}"))])
}
