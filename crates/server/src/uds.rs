//! Unix socket binding: live-daemon detection, stale-socket cleanup, and
//! owner-only permissions. Path resolution lives in `peregrine_api::transport`
//! (single source).

use anyhow::Context;
use std::path::Path;
use tokio::net::UnixListener;

/// Bind a Unix listener.
///
/// If a socket file already exists at `path`, probe it first: a *live* daemon
/// answers `connect()` and we must refuse (never steal another daemon's
/// socket); a refused connection means the file is stale from an unclean exit
/// and is removed before rebinding.
pub async fn bind(path: &Path) -> anyhow::Result<UnixListener> {
    if path.exists() {
        match tokio::net::UnixStream::connect(path).await {
            Ok(_) => anyhow::bail!("another daemon is already listening on {}", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                tracing::warn!(path = %path.display(), "removing stale socket file");
                tokio::fs::remove_file(path)
                    .await
                    .with_context(|| format!("remove stale socket {}", path.display()))?;
            }
            Err(e) => {
                // Not refused (e.g. ENOENT raced away, or a permission error):
                // let the bind itself surface the real problem.
                tracing::debug!(path = %path.display(), error = %e, "socket probe failed");
            }
        }
    }
    let listener = UnixListener::bind(path)
        .with_context(|| format!("bind unix socket at {}", path.display()))?;
    restrict_permissions(path)?;
    Ok(listener)
}

/// Security default: the socket is a control channel to the daemon —
/// owner-only, always (0755-and-umask would expose it to the whole group).
fn restrict_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

/// Best-effort socket cleanup on shutdown.
pub async fn remove_socket_file(path: &Path) {
    tokio::fs::remove_file(path).await.ok();
}
