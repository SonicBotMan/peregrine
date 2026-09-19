//! Transport primitives shared by daemon and every client.
//!
//! The socket path is domain knowledge — defined here once so `peregrined`,
//! `pg`, and the future MCP bridge all agree (single source of truth).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::task::Priority;

/// REST request body for `POST /tasks` — the one write DTO clients
/// (CLI, future MCP bridge, UI dev-mode) share with the daemon.
/// `priority` defaults to Normal when absent (serde default keeps
/// the JSON minimal for humans).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddTaskRequest {
    pub url: String,
    pub save_path: String,
    #[serde(default)]
    pub priority: Priority,
}

/// Wire shape of every error the REST API returns: a machine code
/// plus a human message, never a bare string body.
#[derive(Debug, Serialize)]
pub struct ApiErrorBody {
    pub error: String,
    pub message: String,
}

/// Unix: the UDS control-channel path (XDG_RUNTIME_DIR, validated,
/// with an owner-only /tmp fallback). Windows: unsupported — the
/// control channel is loopback TCP (`--socket tcp:PORT` / the pg
/// default `tcp:127.0.0.1:8420`), so the path-based default is a
/// pointed error rather than a silent half-working path.
#[cfg(not(unix))]
pub fn default_socket_path() -> std::io::Result<PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "unix-domain control socket is unavailable on Windows;          use `--socket tcp:PORT` (the daemon default is tcp:8420)",
    ))
}

/// Default daemon database path: `$XDG_DATA_HOME/peregrine/tasks.db`,
/// falling back to `~/.local/share/peregrine/tasks.db` — the SAME
/// single source the daemon opens and clients print in diagnostics.
pub fn default_db_path() -> std::io::Result<PathBuf> {
    #[cfg(unix)]
    let base = match std::env::var_os("XDG_DATA_HOME").filter(|s| !s.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => {
            let home = std::env::var_os("HOME").ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "neither XDG_DATA_HOME nor HOME set",
                )
            })?;
            PathBuf::from(home).join(".local/share")
        }
    };
    // Windows: %APPDATA%\peregrine (roaming, matches where installers
    // put per-user app data; XDG has no meaning here).
    #[cfg(not(unix))]
    let base = match std::env::var_os("APPDATA").filter(|s| !s.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "APPDATA is not set; cannot derive the database directory",
            ));
        }
    };
    let dir = base.join("peregrine");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("tasks.db"))
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: `getuid` takes no arguments and cannot fail. We use the real OS
    // uid, never an env var like `UID`, which shells may not export.
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

/// Windows control-channel default: the loopback TCP authority the
/// desktop sidecar serves (`peregrined --tcp 8420`), so `pg` and
/// `peregrine-mcp` reach a running GUI daemon with no flags.
#[cfg(not(unix))]
pub fn default_tcp_authority() -> String {
    "127.0.0.1:8420".to_string()
}

/// Default socket path: `$XDG_RUNTIME_DIR/peregrine/peregrine.sock`,
/// falling back to a 0700 directory `/tmp/peregrine-<uid>/peregrine.sock`
/// when XDG_RUNTIME_DIR is unset, empty, or fails validation. A bare
/// file directly in /tmp would sit in a world-traversable directory
/// during the bind→chmod window; an owner-only directory closes that.
#[cfg(unix)]
pub fn default_socket_path() -> std::io::Result<PathBuf> {
    let xdg = std::env::var_os("XDG_RUNTIME_DIR").filter(|s| !s.is_empty());
    if let Some(runtime_dir) = xdg {
        // B13: trust XDG_RUNTIME_DIR only when it is owned by this
        // uid and not group/other-writable. A hijacked or mispointed
        // runtime dir (e.g. another user's, or /tmp itself) would put
        // the control socket somewhere an attacker can reach — fall
        // back to the private per-uid /tmp dir instead.
        if xdg_runtime_dir_ok(&runtime_dir) {
            let dir = PathBuf::from(runtime_dir).join("peregrine");
            std::fs::create_dir_all(&dir)?;
            return Ok(dir.join("peregrine.sock"));
        }
        // Explicitly-configured XDG rejected (missing / not a dir /
        // wrong owner / group- or other-writable): say so — a silent
        // fallback to the private /tmp dir hides a real
        // misconfiguration from the operator (R2 P1-1). No-op when
        // tracing isn't initialised (client side).
        tracing::warn!(
            xdg = %runtime_dir.to_string_lossy(),
            "XDG_RUNTIME_DIR failed validation — falling back to /tmp socket dir"
        );
    }
    Ok(tmp_fallback_dir()?.join("peregrine.sock"))
}

/// XDG_RUNTIME_DIR validation (B13): must exist, be a directory,
/// owned by the current uid, and carry no group/other write bits
/// (0700-style — the spec REQUIRES 0700; we accept anything without
/// foreign WRITE, e.g. 0755 read-only traversal, since reading the
/// socket path is harmless while writing to the socket is not).
#[cfg(unix)]
fn xdg_runtime_dir_ok(runtime_dir: &std::ffi::OsStr) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = match std::fs::metadata(runtime_dir) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if !meta.is_dir() {
        return false;
    }
    meta.uid() == current_uid() && (meta.permissions().mode() & 0o022) == 0
}

#[cfg(not(unix))]
fn xdg_runtime_dir_ok(_runtime_dir: &std::ffi::OsStr) -> bool {
    true
}

/// XDG-less fallback directory: owned by this uid, permissions pinned to
/// 0700 on every start (create_dir_all alone would honor a pre-existing
/// looser mode).
#[cfg(unix)]
fn tmp_fallback_dir() -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let dir = PathBuf::from(format!("/tmp/peregrine-{}", current_uid()));
    // B13: reject a symlink at the fallback path. create_dir_all
    // FOLLOWS symlinks, so one planted by a local attacker would put
    // the socket (and the chmod below) into an arbitrary directory;
    // the name embeds OUR uid, so no legitimate setup ever makes it
    // a symlink — refuse loudly instead.
    if let Ok(meta) = std::fs::symlink_metadata(&dir)
        && meta.file_type().is_symlink()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("refusing symlink at {}: remove it manually", dir.display()),
        ));
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

/// Effective socket path: explicit flag > `PGRG_SOCKET` env >
/// default. The env var is what test harnesses and the smoke
/// scripts set; users should use `--socket`.
pub fn socket_path(flag: Option<&str>) -> std::io::Result<PathBuf> {
    if let Some(p) = flag {
        return Ok(PathBuf::from(p));
    }
    if let Some(p) = std::env::var_os("PGRG_SOCKET").filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    default_socket_path()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_flag_beats_default() {
        let p = socket_path(Some("/run/x.sock")).unwrap();
        assert_eq!(p, PathBuf::from("/run/x.sock"));
    }

    #[test]
    #[cfg(unix)]
    fn fallback_uses_tmp() {
        // In test env XDG_RUNTIME_DIR may or may not be set; either branch must
        // produce an absolute path.
        let p = default_socket_path().unwrap();
        assert!(p.is_absolute());
        assert!(p.ends_with("peregrine.sock"));
    }

    #[test]
    #[cfg(unix)]
    fn xdg_validation_rejects_foreign_writable_dir() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        // 0700, owned by us → trusted.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(xdg_runtime_dir_ok(dir.path().as_os_str()));
        // Group-writable → a co-tenant could replace the socket dir.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o770)).unwrap();
        assert!(!xdg_runtime_dir_ok(dir.path().as_os_str()));
        // Other-writable → world.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o702)).unwrap();
        assert!(!xdg_runtime_dir_ok(dir.path().as_os_str()));
        // Nonexistent → fall back.
        assert!(!xdg_runtime_dir_ok(
            std::path::Path::new("/nonexistent/xdg/dir").as_os_str()
        ));
        // A regular file, not a directory → fall back.
        let f = dir.path().join("notadir");
        std::fs::write(&f, b"").unwrap();
        assert!(!xdg_runtime_dir_ok(f.as_os_str()));
    }
}
