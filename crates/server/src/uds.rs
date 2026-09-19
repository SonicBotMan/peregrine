//! Unix socket binding: live-daemon detection, stale-socket cleanup, and
//! owner-only permissions. Path resolution lives in `peregrine_api::transport`
//! (single source).
//!
//! Windows has no UDS control channel (tokio gates `UnixListener` to
//! unix targets): `bind` exists as a stub that fails with a pointed
//! error, and the daemon's Windows story is loopback TCP only — the
//! unreachable code paths in `main.rs` stay type-correct against the
//! stub without a second copy of the listener wiring.

use anyhow::Context;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::Path;
#[cfg(unix)]
use tokio::net::UnixListener;

/// How long the liveness probe may wait. A live listener with a full accept
/// backlog makes `connect` queue (EAGAIN) instead of failing fast; without a
/// deadline that manifests as startup hanging forever.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// The `(dev, inode)` identity of the socket file we bound, captured right
/// after bind. Shutdown cleanup unlinks the path only if it still points at
/// this exact file — between our shutdown and cleanup, another daemon may
/// have probed our dead socket, removed the stale file, and bound its own.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
pub struct SocketIdentity {
    dev: u64,
    ino: u64,
}

/// Windows stub: no UDS, no socket file to identify.
#[cfg(not(unix))]
#[derive(Clone, Copy, Debug)]
pub struct SocketIdentity;

/// Bind a Unix listener, returning it with the socket file's identity for
/// safe cleanup later.
///
/// If a socket file already exists at `path`, probe it first: a *live* daemon
/// answers `connect()` and we must refuse (never steal another daemon's
/// socket); a refused connection means the file is stale from an unclean exit
/// and is removed before rebinding.
#[cfg(unix)]
pub async fn bind(path: &Path) -> anyhow::Result<(UnixListener, SocketIdentity)> {
    if path.exists() {
        match tokio::time::timeout(PROBE_TIMEOUT, tokio::net::UnixStream::connect(path)).await {
            // Timed out: most plausibly a live daemon with a full backlog.
            // Fail safe — assume a daemon owns the socket rather than delete
            // a live daemon's file.
            Err(_elapsed) => anyhow::bail!(
                "socket probe timed out; assuming another daemon is listening on {}",
                path.display()
            ),
            Ok(Ok(_)) => anyhow::bail!("another daemon is already listening on {}", path.display()),
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                ensure_socket_file(path).await?;
                tracing::warn!(path = %path.display(), "removing stale socket file");
                tokio::fs::remove_file(path)
                    .await
                    .with_context(|| format!("remove stale socket {}", path.display()))?;
            }
            Ok(Err(e)) => {
                // Not refused (e.g. ENOENT raced away, or a permission error):
                // let the bind itself surface the real problem.
                tracing::debug!(path = %path.display(), error = %e, "socket probe failed");
            }
        }
    }
    // B13: bind under a tightened umask so the socket file is never
    // briefly group/world-accessible between `bind()` and the
    // explicit chmod below. The umask is process-wide; this runs on
    // the startup path before any file-creating tasks are spawned,
    // so the exposure is a microsecond window in which the ONLY
    // effect on a racing creator would be an over-restrictive mode
    // — the safe direction.
    let prev_umask = unsafe { libc::umask(0o077) };
    let listener =
        UnixListener::bind(path).with_context(|| format!("bind unix socket at {}", path.display()));
    unsafe { libc::umask(prev_umask) };
    let listener = listener?;
    restrict_permissions(path)?;
    let meta = std::fs::metadata(path).context("stat socket for cleanup identity")?;
    Ok((
        listener,
        SocketIdentity {
            dev: meta.dev(),
            ino: meta.ino(),
        },
    ))
}

/// Windows stub: unreachable in practice (`Listen::Unix` and the
/// `+unix:PATH` dual-bind are refused at parse time on this platform);
/// kept type-compatible so the shared listener wiring compiles.
#[cfg(not(unix))]
pub async fn bind(_path: &Path) -> anyhow::Result<(tokio::net::TcpListener, SocketIdentity)> {
    anyhow::bail!("unix-domain sockets are unavailable on Windows; serve via `--listen tcp:PORT`")
}

/// Security default: the socket is a control channel to the daemon —
/// owner-only, always (0755-and-umask would expose it to the whole group).
#[cfg(unix)]
fn restrict_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

/// Only remove the stale candidate if it really is a socket file: `connect`
/// to an ordinary file also fails with ECONNREFUSED, and a user pointing
/// `--socket` at one of their files must not get it deleted.
#[cfg(unix)]
async fn ensure_socket_file(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::FileTypeExt;
    let meta = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("stat stale socket candidate {}", path.display()))?;
    anyhow::ensure!(
        meta.file_type().is_socket(),
        "refusing to remove {}: not a socket file",
        path.display()
    );
    Ok(())
}

/// Best-effort socket cleanup on shutdown: unlink only when the
/// successor is DEAD and the path still points at the exact file
/// we bound (same device and inode).
///
/// Liveness first, identity second — on overlayfs (CI runners)
/// the successor's fresh socket file can land on the SAME inode
/// number we captured (eager inode reuse right after the stale
/// file is removed), which defeats the (dev, ino) comparison
/// alone. A live successor answers `connect()`; that is the
/// authoritative "leave it alone" signal.
#[cfg(unix)]
pub async fn remove_socket_file(path: &Path, id: SocketIdentity) {
    match tokio::time::timeout(PROBE_TIMEOUT, tokio::net::UnixStream::connect(path)).await {
        Ok(Ok(_stream)) => {
            // Someone is listening — never unlink a live daemon's
            // socket, even if the inode matches (it may be a
            // reused number, not our file).
            tracing::debug!(
                path = %path.display(),
                "socket path is served by a live successor; not removing"
            );
            return;
        }
        Ok(Err(_)) | Err(_) => {
            // Refused/timed out/ENOENT: the path is dead (or gone) —
            // fall through to the identity check.
        }
    }
    match std::fs::metadata(path) {
        Ok(meta) if meta.dev() == id.dev && meta.ino() == id.ino => {
            tokio::fs::remove_file(path).await.ok();
        }
        _ => tracing::debug!(
            path = %path.display(),
            "socket file changed hands since bind; not removing"
        ),
    }
}

/// Windows stub: nothing to clean up without a socket file.
#[cfg(not(unix))]
pub async fn remove_socket_file(_path: &Path, _id: SocketIdentity) {}
