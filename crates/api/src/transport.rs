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

/// Default daemon database path: `$XDG_DATA_HOME/peregrine/tasks.db`,
/// falling back to `~/.local/share/peregrine/tasks.db` — the SAME
/// single source the daemon opens and clients print in diagnostics.
pub fn default_db_path() -> std::io::Result<PathBuf> {
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

/// Default socket path: `$XDG_RUNTIME_DIR/peregrine/peregrine.sock`,
/// falling back to a 0700 directory `/tmp/peregrine-<uid>/peregrine.sock`
/// when XDG_RUNTIME_DIR is unset or empty. A bare file directly in /tmp
/// would sit in a world-traversable directory during the
/// bind→chmod window; an owner-only directory closes that.
pub fn default_socket_path() -> std::io::Result<PathBuf> {
    let xdg = std::env::var_os("XDG_RUNTIME_DIR").filter(|s| !s.is_empty());
    if let Some(runtime_dir) = xdg {
        let dir = PathBuf::from(runtime_dir).join("peregrine");
        std::fs::create_dir_all(&dir)?;
        return Ok(dir.join("peregrine.sock"));
    }
    Ok(tmp_fallback_dir()?.join("peregrine.sock"))
}

/// XDG-less fallback directory: owned by this uid, permissions pinned to
/// 0700 on every start (create_dir_all alone would honor a pre-existing
/// looser mode).
fn tmp_fallback_dir() -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let dir = PathBuf::from(format!("/tmp/peregrine-{}", current_uid()));
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

/// Effective socket path: explicit flag wins, else default.
pub fn socket_path(flag: Option<&str>) -> std::io::Result<PathBuf> {
    match flag {
        Some(p) => Ok(PathBuf::from(p)),
        None => default_socket_path(),
    }
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
    fn fallback_uses_tmp() {
        // In test env XDG_RUNTIME_DIR may or may not be set; either branch must
        // produce an absolute path.
        let p = default_socket_path().unwrap();
        assert!(p.is_absolute());
        assert!(p.ends_with("peregrine.sock"));
    }
}
