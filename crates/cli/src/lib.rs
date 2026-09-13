//! Library surface for the `pg` CLI: the UDS client lives here so
//! integration tests can drive the exact client code the binary
//! uses (no process spawning, no drift between tested and shipped).

pub mod uds_client;

pub use uds_client::DaemonClient;
