//! Transport primitives shared by daemon and every client.
//!
//! The socket path is domain knowledge — defined here once so `peregrined`,
//! `pg`, and the future MCP bridge all agree (single source of truth).

use std::path::PathBuf;

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
/// falling back to `/tmp/peregrine-<uid>.sock` when XDG_RUNTIME_DIR is unset
/// or empty.
pub fn default_socket_path() -> std::io::Result<PathBuf> {
    let xdg = std::env::var_os("XDG_RUNTIME_DIR").filter(|s| !s.is_empty());
    if let Some(runtime_dir) = xdg {
        let dir = PathBuf::from(runtime_dir).join("peregrine");
        std::fs::create_dir_all(&dir)?;
        return Ok(dir.join("peregrine.sock"));
    }
    Ok(PathBuf::from(format!(
        "/tmp/peregrine-{}.sock",
        current_uid()
    )))
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
