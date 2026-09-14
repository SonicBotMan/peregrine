//! Library surface for the `pg` CLI: re-exports the daemon client
//! from `peregrine-api` so integration tests drive the exact client
//! code the binary uses (no process spawning, no drift between
//! tested and shipped).

pub use peregrine_api::uds_client::DaemonClient;
