//! The `trusty-events` daemon fails closed: a socket it cannot own or a log it
//! cannot open is a non-zero exit, never a process that runs on without the
//! bus (#9075, Fail-Open check).

mod common;

use std::time::Duration;

use common::{
    BOUND, chmod, event, log_dir_in, push, socket_in, spawn, stall_the_writer, start, subscribe,
};
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

/// A log write the writer cannot make ends the daemon with exit 1, instead of
/// an `error!` while it goes on handing out `seq` it never records.
#[tokio::test(flavor = "multi_thread")]
async fn a_log_write_failure_exits_non_zero() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut daemon = start(tmp.path(), &[]).await;
    // Read-only log directory: the writer cannot create today's day file.
    let log_dir = log_dir_in(tmp.path());
    chmod(&log_dir, 0o500);

    push(&daemon.socket, vec![event("unwritable")]).await;
    let status = daemon.wait_exit(BOUND).await;
    chmod(&log_dir, 0o700);
    let status = status.unwrap_or_else(|| {
        panic!(
            "the daemon ran on after a failed log write; stderr:\n{}",
            daemon.stderr_text()
        )
    });
    assert!(
        !status.success(),
        "a failed log write is a failure exit, got {status}"
    );
}

/// A SIGTERM whose log writer cannot drain in time exits non-zero, so
/// launchd and the operator see that queued events were not written.
#[tokio::test(flavor = "multi_thread")]
async fn a_drain_timeout_on_sigterm_exits_non_zero() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut daemon = start(tmp.path(), &[]).await;
    let fifo = stall_the_writer(tmp.path());
    push(&daemon.socket, vec![event("queued")]).await;
    // Read the event back first, so it is ingested and queued for the stalled
    // writer before SIGTERM arrives.
    let mut seen = subscribe(&daemon.socket, Some(0), SubscribeFilter::default()).await;
    assert_eq!(common::next_event(&mut seen).await.0, 1);
    drop(seen);

    daemon.sigterm();
    let status = daemon.wait_exit(BOUND).await;
    std::fs::remove_file(&fifo).expect("remove the FIFO");
    let status = status.unwrap_or_else(|| {
        panic!(
            "the daemon did not exit after SIGTERM; stderr:\n{}",
            daemon.stderr_text()
        )
    });
    assert!(
        !status.success(),
        "an undrained log at shutdown is a failure exit, got {status}"
    );
}
