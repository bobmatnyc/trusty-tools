//! Tests for the daemon side of the Socket Mode source (#9923).
//!
//! Why: the daemon env is the production readiness, claim and effect; a
//! daemon with no channel policy must hold no socket and write no claim, and
//! its claims must survive a new daemon over the same home.
//! What: hermetic daemons over a temp home; no Slack, no network.
//! Test: this is the test module.

use std::sync::Arc;

use super::DaemonSocketEnv;
use crate::channels::question_table::claims::{Claim, claims_path};
use crate::channels::socket_mode::SocketEnv;
use crate::daemon::DaemonState;

async fn daemon(root: &std::path::Path) -> Arc<DaemonState> {
    Arc::new(DaemonState::with_root_isolated_managed(root.to_path_buf()).await)
}

/// 🔴 #9923 Q8: no host channel policy, so not ready, and no claim log is
/// created (the check refuses before the store opens).
#[tokio::test]
async fn daemon_socket_env_is_not_ready_without_policy() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let state = daemon(tmp.path()).await;
    let home = state.user_home().expect("pinned home");
    let env = DaemonSocketEnv::new(Arc::clone(&state));
    let refused = env.ready().await.expect_err("no policy, not ready");
    assert!(refused.contains("host ceiling"), "{refused}");
    assert!(!claims_path(&home).exists(), "a claim log without policy");
}

/// 🔴 #9923 (plan item 5): a claim made by one daemon is a duplicate for
/// the next daemon over the same home.
#[tokio::test]
async fn daemon_socket_env_claims_once_across_a_reopen() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let state = daemon(tmp.path()).await;
    let env = DaemonSocketEnv::new(Arc::clone(&state));
    assert_eq!(env.claim("Ev1").await.expect("claim"), Claim::Fresh);
    env.settle("Ev1", "delivered").await.expect("settle");
    drop(env);
    drop(state);
    let state = daemon(tmp.path()).await;
    let env = DaemonSocketEnv::new(Arc::clone(&state));
    assert_eq!(env.claim("Ev1").await.expect("claim"), Claim::Duplicate);
}

/// 🔴 #9923 Q3: the daemon's claim log is the one under its pinned home,
/// and one hold serves every call.
#[tokio::test]
async fn daemon_claim_store_opens_under_its_user_home() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let state = daemon(tmp.path()).await;
    let home = state.user_home().expect("pinned home");
    let store = state.inbound_claims().expect("open");
    assert!(claims_path(&home).exists());
    assert!(Arc::ptr_eq(&store, &state.inbound_claims().expect("held")));
}
