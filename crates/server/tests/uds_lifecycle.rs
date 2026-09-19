#![cfg(unix)]
//! Socket lifecycle integration tests — the security contract of the daemon's
//! front door, exercised against the real filesystem in temp dirs:
//! 1. a live daemon's socket is NEVER stolen by a second bind;
//! 2. a stale socket (unclean exit) is detected and taken over;
//! 3. the socket file is always owner-only (0700).

use std::path::PathBuf;
use std::time::Duration;

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("peregrine-test-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn refuse_live_takeover_but_allow_stale_rebind() {
    let dir = tempdir("lifecycle");
    let path = dir.join("t.sock");

    // First bind creates the file.
    let first = peregrine_server::uds::bind(&path).await.unwrap();
    assert!(path.exists());

    // A live daemon owns the socket: a second bind must be REFUSED and the
    // file must survive (we never steal another daemon's socket).
    assert!(
        peregrine_server::uds::bind(&path).await.is_err(),
        "second bind against a live listener must fail"
    );
    assert!(path.exists());

    // Unclean-exit simulation: drop the listener without cleanup. The file is
    // now stale — rebind must take over successfully.
    drop(first);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let second = peregrine_server::uds::bind(&path)
        .await
        .expect("stale socket must be taken over");
    drop(second);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn socket_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir("perms");
    let path = dir.join("p.sock");
    let _listener = peregrine_server::uds::bind(&path).await.unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700, "socket must be owner-only");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn delayed_cleanup_never_unlinks_a_successors_socket() {
    let dir = tempdir("takeover-race");
    let path = dir.join("race.sock");

    // Daemon A binds, then dies uncleanly (no cleanup, file goes stale).
    let (_listener_a, id_a) = peregrine_server::uds::bind(&path).await.unwrap();
    drop(_listener_a);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Daemon B probes the stale file, removes it, binds its own socket
    // (new inode) at the same path.
    let (_listener_b, id_b) = peregrine_server::uds::bind(&path)
        .await
        .expect("stale takeover must succeed");

    // Daemon A's shutdown cleanup finally runs (the race window). It must
    // NOT unlink the file — that is B's socket now. B is LIVE (its
    // listener answers connect), and live successors are untouchable —
    // inode reuse on some filesystems (overlayfs on CI runners) can
    // hand B's fresh file the SAME inode number A captured, so the
    // identity check alone cannot tell them apart.
    peregrine_server::uds::remove_socket_file(&path, id_a).await;
    assert!(path.exists(), "A's cleanup must not unlink B's socket");

    // And B's own cleanup still works: same inode, unlink succeeds.
    // Real shutdown order — B's listener closes FIRST (daemon exits),
    // THEN cleanup runs: connect() is refused, identity matches.
    drop(_listener_b);
    peregrine_server::uds::remove_socket_file(&path, id_b).await;
    assert!(!path.exists(), "B's cleanup removes its own socket");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn stale_cleanup_refuses_to_delete_ordinary_files() {
    let dir = tempdir("not-a-socket");
    let path = dir.join("precious.txt");
    std::fs::write(&path, "user data").unwrap();

    // connect() to a plain file fails ECONNREFUSED, which is the stale-
    // socket signal; bind must refuse rather than delete the user's file.
    let err = peregrine_server::uds::bind(&path).await.unwrap_err();
    assert!(
        err.to_string().contains("not a socket file"),
        "must refuse with explicit reason, got: {err}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "user data");
    let _ = std::fs::remove_dir_all(&dir);
}
