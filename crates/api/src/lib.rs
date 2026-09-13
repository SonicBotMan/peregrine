//! Peregrine API — the single source of truth.
//!
//! Every shared type (Task, EngineEvent, Config), every trait
//! ([`engine::ProtocolEngine`]) and the event bus live here exactly once.
//! Clients (GUI / CLI / MCP) and engines both consume this crate; nothing in
//! here may depend on any shell or engine crate (architecture rule #3/#1).

pub mod bus;
pub mod download;
pub mod engine;
pub mod error;
pub mod health;
pub mod registry;
pub mod task;
pub mod transport;

pub use bus::{EngineEvent, EventBus};
pub use download::{
    DownloadFuture, DownloadJob, DownloadOutcome, DownloadProgress, IfRangeValidator, NoProgress,
    ProgressSink, ResumeContext, SharedProgressSink,
};
pub use engine::{ProbeFuture, ProbeInfo, ProtocolEngine};
pub use error::ApiError;
pub use health::HealthInfo;
pub use registry::EngineRegistry;
pub use task::{Task, TaskId, TaskStatus};
pub use transport::{default_socket_path, socket_path};

/// Crate version, surfaced by `/health` and `pg --version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Well-known daemon identity.
pub const DAEMON_NAME: &str = "peregrine";
