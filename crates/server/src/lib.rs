//! Peregrine server library — the daemon's engine room, exposed as a lib so
//! the socket lifecycle can be integration-tested without spawning a binary.

pub mod api;
pub mod daemon;
pub mod health;
pub mod uds;
pub mod ws;
