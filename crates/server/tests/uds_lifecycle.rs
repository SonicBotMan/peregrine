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
