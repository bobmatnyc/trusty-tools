//! A reader that resumes with `since_seq` is never silently skipped past
//! events, even when a crash made the bus hand out a `seq` twice (#9075,
//! HIGH-1 of the PR2 review).

mod common;

use common::{
    event, next_frame, next_full_event, push, resume, stall_the_writer, start, subscribe,
};
use trusty_common::control_bus::{SubscribeFilter, SubscribeItem};

/// SIGKILL with the writer undrained, restart, push new events: the old
/// `seq` now names a different event, and a resume from it is refused rather
/// than served from the next seq.
#[tokio::test(flavor = "multi_thread")]
async fn a_resume_after_a_kill_with_an_undrained_log_is_refused_not_skipped() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut first = start(tmp.path(), &[]).await;
    let fifo = stall_the_writer(tmp.path());

    let mut stream = subscribe(&first.socket, Some(0), SubscribeFilter::default()).await;
    push(&first.socket, vec![event("before-crash")]).await;
    let held = next_full_event(&mut stream).await;
    assert_eq!(
        held.seq, 1,
        "the reader holds seq 1, which was never logged"
    );
    drop(stream);

    // SIGKILL: no shutdown path runs, the queued event is lost.
    first.kill();
    std::fs::remove_file(&fifo).expect("remove the FIFO");

    let second = start(tmp.path(), &[]).await;
    push(&second.socket, vec![event("after-1"), event("after-2")]).await;

    // Resuming from the held event must not start at seq 2: seq 1 is now
    // "after-1", which this reader has never seen.
    let mut stale = resume(
        &second.socket,
        Some(held.seq),
        Some(held.id),
        SubscribeFilter::default(),
    )
    .await;
    match next_frame(&mut stale).await {
        Some(Err(e)) => assert!(
            e.to_string().contains("since_id"),
            "the refusal names the failed check: {e}"
        ),
        Some(Ok(SubscribeItem::Event { event, .. })) => panic!(
            "silent skip: resumed at seq {} ({:?}) past the unseen seq 1",
            event.seq, event.session
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }

    // The same seq with the id now stored there resumes normally.
    let mut stream = subscribe(&second.socket, Some(0), SubscribeFilter::default()).await;
    let now_first = next_full_event(&mut stream).await;
    assert_eq!(now_first.session.as_deref(), Some("after-1"));
    let mut good = resume(
        &second.socket,
        Some(1),
        Some(now_first.id),
        SubscribeFilter::default(),
    )
    .await;
    let next = next_full_event(&mut good).await;
    assert_eq!((next.seq, next.session.as_deref()), (2, Some("after-2")));
}
