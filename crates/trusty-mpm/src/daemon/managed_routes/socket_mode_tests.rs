//! Tests for the daemon side of the Socket Mode source (#9923).
//!
//! Why: the daemon env is the production readiness, claim and effect; a
//! daemon with no channel policy must hold no socket and write no claim, and
//! its claims must survive a new daemon over the same home.
//! What: hermetic daemons over a temp home; no Slack, no network.
//! Test: this is the test module.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::DaemonSocketEnv;
use crate::channels::question_table::claims::{Claim, ClaimStore, claims_path};
use crate::channels::socket_mode::envelope::{Frame, parse_frame};
use crate::channels::socket_mode::transport::{SocketConn, SocketTransport};
use crate::channels::socket_mode::{SocketEnv, Timing, handle_event, run};
use crate::daemon::DaemonState;

async fn daemon(root: &std::path::Path) -> Arc<DaemonState> {
    Arc::new(DaemonState::with_root_isolated_managed(root.to_path_buf()).await)
}

/// The daemon's home, canonical, so a project under it is canonical too.
fn home_of(state: &DaemonState) -> PathBuf {
    let home = state.user_home().expect("pinned home");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::canonicalize(home).expect("canonical home")
}

/// Write the host ceiling under `home`: Slack enabled with an `app_ref`
/// (so the host-ceiling arm passes) for the one project `project`.
fn write_host(home: &Path, project: &Path) {
    let path = trusty_common::crate_config::crate_config_path_at(home, "trusty-mpm");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    let text = format!(
        "channels:\n  version: 1\n  slack:\n    enabled: true\n    connection: {{ bot_ref: \
         slack, app_ref: slack-app }}\n    projects: [\"{}\"]\n",
        project.display()
    );
    std::fs::write(&path, text).expect("write host");
}

/// `ready` for `env`, polled until `done` accepts it; bounded.
async fn ready_until(
    env: &DaemonSocketEnv,
    done: impl Fn(&Result<(), String>) -> bool,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let ready = env.ready().await;
        if done(&ready) || tokio::time::Instant::now() >= deadline {
            return ready;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A transport that counts connects and opens none.
#[derive(Default)]
struct NoSocket(AtomicUsize);

#[async_trait::async_trait]
impl SocketTransport for NoSocket {
    async fn connect(&self) -> Result<Box<dyn SocketConn>, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err("no socket in this test".into())
    }
}

/// A closed connection that records its acks.
#[derive(Default)]
struct Acks(Vec<String>);

#[async_trait::async_trait]
impl SocketConn for Acks {
    async fn recv(&mut self) -> Option<Result<String, String>> {
        None
    }

    async fn ack(&mut self, envelope_id: &str) -> Result<(), String> {
        self.0.push(envelope_id.to_owned());
        Ok(())
    }
}

/// The production env refuses `ready` naming `code`, and so an event is
/// neither claimed nor acked and `run` opens no socket; the claim log at
/// `home` is byte-for-byte as it was.
async fn assert_refuses(state: &Arc<DaemonState>, home: &Path, code: &str) {
    let env = Arc::new(DaemonSocketEnv::new(Arc::clone(state)));
    let refused = env.ready().await.expect_err("not ready");
    assert!(refused.contains(code), "{code}: {refused}");
    let before = std::fs::read(claims_path(home)).ok();
    let raw = r#"{"envelope_id":"E1","type":"events_api",
        "payload":{"event_id":"Ev1","event":{"type":"app_mention"}}}"#;
    let Frame::Event(event) = parse_frame(raw) else {
        panic!("not an event frame");
    };
    let mut conn = Acks::default();
    handle_event(&*env, &mut conn, event).await;
    assert!(conn.0.is_empty(), "{code}: acked {:?}", conn.0);
    let transport = Arc::new(NoSocket::default());
    let cancel = CancellationToken::new();
    let timing = Timing {
        poll: Duration::from_millis(10),
        ..Timing::default()
    };
    let task = tokio::spawn(run(
        env,
        Arc::clone(&transport) as Arc<dyn SocketTransport>,
        cancel.clone(),
        timing,
    ));
    tokio::time::sleep(Duration::from_millis(300)).await;
    cancel.cancel();
    task.await.expect("run ends");
    assert_eq!(transport.0.load(Ordering::SeqCst), 0, "{code}: a socket");
    assert_eq!(
        std::fs::read(claims_path(home)).ok(),
        before,
        "{code}: claimed"
    );
}

/// 🔴 #9923 Q8 (r-078): the host names an `app_ref`, but the policy does
/// not serve (a symlinked project denies the load), so the production
/// `ready` refuses, nothing is claimed and no socket opens.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_socket_env_refuses_while_the_policy_does_not_serve() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let state = daemon(tmp.path()).await;
    let home = home_of(&state);
    std::fs::create_dir(home.join("real")).expect("real dir");
    std::os::unix::fs::symlink(home.join("real"), home.join("link")).expect("link");
    write_host(&home, &home.join("link"));
    let env = DaemonSocketEnv::new(Arc::clone(&state));
    let settled = ready_until(&env, |r| r.as_ref().is_err_and(|e| e == "snapshot_denied")).await;
    assert_eq!(settled, Err("snapshot_denied".to_owned()));
    assert_refuses(&state, &home, "snapshot_denied").await;
    assert!(
        !claims_path(&home).exists(),
        "a claim log while not serving"
    );
}

/// 🔴 #9923 (r-078): the policy serves, but the claim log is corrupt or
/// unsealed, so the production `ready` refuses, nothing is claimed and no
/// socket opens.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_socket_env_refuses_a_corrupt_or_unsealed_claim_log() {
    for (case, code) in [
        ("corrupt", "undecodable claim line"),
        ("unsealed", "not sealed"),
    ] {
        let tmp = crate::test_support::hermetic_temp_dir();
        let state = daemon(tmp.path()).await;
        let home = home_of(&state);
        write_host(&home, &home.join("proj"));
        let log = claims_path(&home);
        if case == "corrupt" {
            std::fs::create_dir_all(log.parent().expect("dir")).expect("dir");
            std::fs::write(&log, "not json\n").expect("log");
            std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o600)).expect("0600");
        } else {
            drop(ClaimStore::open_at(&log).expect("create"));
            std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644)).expect("0644");
        }
        let serving = tokio::time::timeout(Duration::from_secs(10), async {
            while state.channel_policy().current().is_err() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(serving.is_ok(), "{case}: the policy never served");
        assert_refuses(&state, &home, code).await;
    }
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
