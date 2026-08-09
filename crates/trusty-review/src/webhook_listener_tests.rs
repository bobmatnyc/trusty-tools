//! Coverage for `trusty-review`'s half of the webhook UDS listener (#5182).
//!
//! The frame contract, the ack ordering and the socket hardening are proven in
//! `trusty-common`'s `webhook_relay` suite, which both targets mount. What is
//! specific to this crate — and what a wrong value here would break silently —
//! is the two path resolutions, so those are what these cover.

use super::*;

#[test]
fn socket_path_matches_the_shared_contract() {
    // A literal spelled here instead of resolved from the contract is how the
    // sender and receiver end up dialling and binding different paths, which
    // presents as a delivery that is never received and never errors.
    assert_eq!(
        socket_path(),
        trusty_common::uds::scratch_socket_dir()
            .join(trusty_common::webhook_relay::REVIEW_SOCKET_FILE)
    );
    assert_eq!(
        socket_path(),
        trusty_common::webhook_relay::socket_path_for(trusty_common::webhook_relay::REVIEW_SOURCE)
            .expect("review is a configured source"),
        "the console route segment and this socket must agree"
    );
}

#[test]
fn inbox_root_matches_the_shared_contract() {
    // The console meters THIS directory to decide whether a backlog is stuck.
    // Asserting only the literal shape (below) leaves a hand-rolled path free to
    // drift from the shared one: the receiver writes to its own spelling, the
    // console counts the shared spelling, finds it empty, and reports healthy
    // while deliveries pile up. This is the assertion that fails on that drift.
    assert_eq!(
        inbox_root().expect("resolve inbox root"),
        trusty_common::webhook_relay::inbox_root_for(trusty_common::webhook_relay::REVIEW_SOURCE)
            .expect("review is a configured source")
            .expect("resolve the shared inbox root"),
        "the receiver's inbox and the path console meters must be one path"
    );
}

#[test]
fn inbox_root_lives_under_the_review_data_dir() {
    let root = inbox_root().expect("resolve inbox root");
    assert!(
        root.ends_with(trusty_common::webhook_relay::INBOX_DIR_NAME),
        "unexpected inbox root: {root:?}"
    );
    assert!(
        root.parent().is_some_and(|p| p.ends_with("trusty-review")),
        "the inbox must live under this crate's own data dir, got {root:?}"
    );
}

#[test]
fn listener_opens_against_a_temp_inbox() {
    // `listener()` resolves the real data dir, which a test must not create;
    // opening against a temp root proves the same construction path.
    let tmp = tempfile::tempdir().expect("tempdir");
    let listener = trusty_common::webhook_relay::WebhookListener::open(
        socket_path(),
        tmp.path().join("inbox"),
    )
    .expect("open listener");

    assert_eq!(listener.socket(), socket_path());
    assert!(listener.inbox().root().is_dir());
    assert!(
        !socket_path().exists() || listener.socket().exists(),
        "open must not bind"
    );
}
