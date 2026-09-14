//! Process-wide TLS provider selection.
//!
//! The workspace links BOTH rustls crypto providers: `ring` (our
//! choice — pure-rust builds, no system OpenSSL) and `aws-lc-rs`
//! (pulled transitively by librqbit → reqwest/rustls, whose
//! `rust-tls` feature has no ring variant). With exactly-one-
//! provider auto-detection defeated by feature unification, the
//! first TLS consumer panics unless a provider was installed
//! explicitly (observed as `Could not automatically determine the
//! process-level CryptoProvider` in engine tests).
//!
//! Every binary entry point (`peregrine`, `peregrined`,
//! `peregrine-mcp`) and every test harness that can construct a
//! TLS client calls this FIRST. Idempotent: later calls are no-ops
//! (install_default errors if one is already installed — ignored).

/// Install the `ring` CryptoProvider process-wide. Call before the
/// first TLS connection; safe to call more than once.
pub fn init_tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
