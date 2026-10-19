//! The `trusty-events` daemon fails closed: a socket it cannot own or a log it
//! cannot open is a non-zero exit, never a process that runs on without the
//! bus (#9075, Fail-Open check).

mod common;

use std::time::Duration;

use common::{BOUND, event, push, socket_in, spawn, start, subscribe};
use trusty_common::control_bus::SubscribeFilter;
use trusty_common::uds::{SocketVerdict, probe_socket_verdict};

/// A second instance on a socket the first still serves exits non-zero and
/// leaves the first serving. Under launchd this is the restart loop the
/// crate README describes.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_instance_exits_non_zero_while_the_first_serves() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let first = start(tmp.path(), &[]).await;

    let mut second = spawn(tmp.path(), "second", &[]);
    let status = second.wait_exit(BOUND).await.unwrap_or_else(|| {
        panic!(
            "the second instance kept running; stderr:\n{}",
            second.stderr_text()
        )
    });
    assert!(
        !status.success(),
        "a refused bind is a failure exit, got {status}"
    );
    assert!(
        second.stderr_text().contains("already serving"),
        "the exit names the cause; stderr:\n{}",
        second.stderr_text()
    );

    // The first instance is untouched: still serving, still sequencing from 1.
    push(&first.socket, vec![event("still-up")]).await;
    let mut stream = subscribe(&first.socket, Some(0), SubscribeFilter::default()).await;
    let (seq, sess, _) = common::next_event(&mut stream).await;
    assert_eq!((seq, sess.as_str()), (1, "still-up"));
}

/// A socket path the daemon cannot bind is a non-zero exit.
#[tokio::test(flavor = "multi_thread")]
async fn an_unbindable_socket_path_exits_non_zero() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    // A directory where the socket should be: not a socket, so never taken over.
    std::fs::create_dir_all(socket_in(tmp.path())).expect("occupy the socket path");

    let mut daemon = spawn(tmp.path(), "daemon", &[]);
    let status = daemon.wait_exit(BOUND).await.unwrap_or_else(|| {
        panic!(
            "the daemon ran on without its socket; stderr:\n{}",
            daemon.stderr_text()
        )
    });
    assert!(
        !status.success(),
        "a failed bind is a failure exit, got {status}"
    );
}

/// A log directory the daemon cannot open is a non-zero exit, and the socket
/// is not left answering for a bus that cannot keep `seq` across a restart.
#[tokio::test(flavor = "multi_thread")]
async fn an_unopenable_log_directory_exits_non_zero() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    // A regular file where the log directory should be.
    std::fs::write(common::log_dir_in(tmp.path()), b"not a directory").expect("occupy log dir");

    let mut daemon = spawn(tmp.path(), "daemon", &[]);
    let status = daemon.wait_exit(BOUND).await.unwrap_or_else(|| {
        panic!(
            "the daemon ran on without its log; stderr:\n{}",
            daemon.stderr_text()
        )
    });
    assert!(
        !status.success(),
        "a failed log open is a failure exit, got {status}"
    );
    assert_ne!(
        probe_socket_verdict(&socket_in(tmp.path()), Duration::from_millis(200)).await,
        SocketVerdict::Serving,
        "nothing answers on the socket after the exit"
    );
}
