//! #9741 (#8454 G3 S4): the policy readiness gate's feature fallback.
//!
//! Why: a build without the `channels` feature cannot load the route policy,
//! so it must read not ready (B9), never ready.
//! What: B9 runs only in a build without `channels`
//! (`--no-default-features --features daemon`); the default build checks
//! that the real check runs behind [`policy_ready`].
//! Test: this is the test module.

use super::channels_readiness::{HOOK_LOAD_LIMIT, NOT_READY_CHECK, NotReady, policy_ready};

/// 🔴 #9741 B9: without the `channels` feature the policy is never ready.
#[cfg(not(feature = "channels"))]
#[test]
fn build_without_channels_feature_denies() {
    let home = tempfile::tempdir().expect("tempdir");
    assert_eq!(policy_ready(Some(home.path())), Err(NotReady::Unsupported));
    assert_eq!(policy_ready(None), Err(NotReady::Unsupported));
    assert!(
        NotReady::Unsupported.to_string().contains("channels"),
        "the reason names the branch"
    );
}

/// #9741: the deny slug and the hook's wait are the ruled values; with the
/// `channels` feature a home with no `config.yaml` reads not ready.
#[test]
fn policy_ready_slug_budget_and_missing_config() {
    assert_eq!(NOT_READY_CHECK, "egress-policy-not-ready");
    assert_eq!(HOOK_LOAD_LIMIT, std::time::Duration::from_millis(1500));
    let home = tempfile::tempdir().expect("tempdir");
    assert!(policy_ready(Some(home.path())).is_err());
}
