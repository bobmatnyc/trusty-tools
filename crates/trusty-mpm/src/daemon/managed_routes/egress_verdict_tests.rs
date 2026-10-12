//! Tests for the egress verdict route (#8454 VC).
//!
//! Why: the route is a Fail-Open Check site. Each test goes red if an arm
//! that must serve no verdict (home mismatch, never loaded, stale, poisoned,
//! fingerprint 0, a changed fingerprint) serves one, or if a denied report
//! is served as ready.
//! What: [`answer`] over a scripted policy snapshot with a settable clock,
//! or over an injected read; one test drives the real router.
//! Test: this is the test module.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use trusty_channels::policy::{
    Channel, Clock, LoadReport, ProjectInput, merge_for, parse_host, parse_project_file,
};

use super::{VerdictCache, answer};
use crate::channels::snapshot::{
    MAX_AGE, PolicySnapshot, PolicySource, REFRESH_INTERVAL, SnapshotDeny,
};
use crate::core::channels_readiness::{HostBranch, NOT_READY_CHECK, NotReady};
use crate::core::egress_verdict::{Unserved, VERDICT_ROUTE, VerdictReply};

const BOB: &str = "U0ABCDEF1";
/// The longest a test polls for a refresh to land.
const POLL_GUARD: Duration = Duration::from_secs(10);

/// A clock the test moves by hand.
#[derive(Debug, Clone, Default)]
struct TestClock(Arc<AtomicU64>);

impl TestClock {
    fn set(&self, at: Duration) {
        let ms = u64::try_from(at.as_millis()).expect("ms fit");
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::SeqCst))
    }
}

type Script = Box<dyn FnMut(usize) -> LoadReport + Send>;

/// A source whose `n`th refresh runs `script(n)`. `calls` counts refreshes,
/// which stand for the git a real loader runs.
struct Scripted {
    calls: Arc<AtomicUsize>,
    script: Script,
}

impl PolicySource for Scripted {
    fn refresh(&mut self) -> LoadReport {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        (self.script)(n)
    }
}

type Snap = PolicySnapshot<Scripted, TestClock>;

fn scripted(
    script: impl FnMut(usize) -> LoadReport + Send + 'static,
) -> (Snap, TestClock, Arc<AtomicUsize>) {
    let clock = TestClock::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let source = Scripted {
        calls: Arc::clone(&calls),
        script: Box::new(script),
    };
    (PolicySnapshot::new(source, clock.clone()), clock, calls)
}

/// A home path; the route only compares it.
fn home() -> PathBuf {
    PathBuf::from("/home/tester")
}

/// An in-memory report with bob's route in effect and `fingerprint` set.
fn ready_report(fingerprint: u64) -> LoadReport {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonical");
    let dir = root.join("proj");
    let list = format!("\"{}\"", dir.display());
    let host = format!(
        "channels:\n  version: 1\n  slack:\n    enabled: true\n    connection: {{ bot_ref: slack, \
         app_ref: slack-app }}\n    projects: [{list}]\n  telegram:\n    enabled: true\n    \
         connection: {{ bot_ref: telegram }}\n    projects: [{list}]\n"
    );
    let routes = format!(
        "version = 2\n[[slack.routes]]\nname = \"bob-dm\"\nrecipient = \"{BOB}\"\nkinds = \
         [\"question\"]\n"
    );
    let mut report = merge_for(
        parse_host(&host, Some(&root)),
        vec![ProjectInput {
            file: dir.join(".trusty-channels/routes.toml"),
            parsed: parse_project_file(&routes, Some(&root)),
            project_dir: dir,
        }],
        &[Channel::Slack, Channel::Telegram],
    );
    assert!(!report.denied, "fixture: {:#?}", report.findings);
    assert_eq!(report.policy.routes().len(), 1, "fixture");
    report.fingerprint = fingerprint;
    report
}

/// The same route, but the load denied: only the flag stands between it
/// and a ready verdict.
fn denied_report(fingerprint: u64) -> LoadReport {
    let mut report = ready_report(fingerprint);
    report.denied = true;
    report
}

/// Ask the snapshot-backed route once under the daemon's own home.
fn ask(snap: &Snap, cache: &VerdictCache) -> VerdictReply {
    let home = home();
    answer(|| snap.current_aged(), cache, Some(&home), &home)
}

/// Ask until the answer is not `NeverLoaded`, or panic at [`POLL_GUARD`].
fn ask_loaded(snap: &Snap, cache: &VerdictCache) -> VerdictReply {
    let started = Instant::now();
    loop {
        let reply = ask(snap, cache);
        let cold = VerdictReply::Unserved {
            why: Unserved::NeverLoaded,
        };
        if reply != cold {
            return reply;
        }
        assert!(
            started.elapsed() < POLL_GUARD,
            "the first load never landed"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Wait until the snapshot publishes a report with `fingerprint`.
fn wait_for_fingerprint(snap: &Snap, fingerprint: u64) {
    let started = Instant::now();
    while !matches!(snap.current_aged(), Ok((r, _)) if r.fingerprint == fingerprint) {
        assert!(
            started.elapsed() < POLL_GUARD,
            "fingerprint {fingerprint} never landed"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn changed() -> VerdictReply {
    VerdictReply::Unserved {
        why: Unserved::FingerprintChanged,
    }
}

fn is_ready(reply: &VerdictReply) -> bool {
    matches!(reply, VerdictReply::Ready { .. })
}

/// 🔴 #8454 VC (plan :128): once a fingerprint's verdict is cached, the next
/// query is answered from it with no refresh, so no git runs.
#[test]
fn cached_verdict_reused_while_fingerprint_unchanged() {
    let (snap, _clock, calls) = scripted(|_| ready_report(11));
    let cache = VerdictCache::default();
    assert_eq!(ask_loaded(&snap, &cache), changed(), "first sight caches");
    let before = calls.load(Ordering::SeqCst);
    for _ in 0..3 {
        match ask(&snap, &cache) {
            VerdictReply::Ready {
                fingerprint,
                age_ms,
            } => {
                assert_eq!(fingerprint, 11);
                assert!(age_ms <= 60_000);
            }
            other => panic!("a cached ready verdict must be served: {other:?}"),
        }
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        before,
        "a cached answer ran a refresh"
    );
}

/// 🔴 #8454 VC (plan :129): a new fingerprint is never answered with the
/// old verdict. Ready under 21, then a denied load under 22: the first
/// query after the change serves nothing, the next serves the deny.
#[test]
fn fingerprint_change_invalidates_cached_verdict() {
    let (snap, clock, _calls) = scripted(|n| {
        if n == 0 {
            ready_report(21)
        } else {
            denied_report(22)
        }
    });
    let cache = VerdictCache::default();
    assert_eq!(ask_loaded(&snap, &cache), changed());
    assert!(is_ready(&ask(&snap, &cache)), "fixture: 21 is ready");
    clock.set(REFRESH_INTERVAL);
    let _ = snap.current_aged();
    wait_for_fingerprint(&snap, 22);
    assert_eq!(ask(&snap, &cache), changed(), "the old verdict was served");
    match ask(&snap, &cache) {
        VerdictReply::NotReady {
            fingerprint, slug, ..
        } => {
            assert_eq!(fingerprint, 22);
            assert_eq!(slug, NOT_READY_CHECK);
        }
        other => panic!("22 is denied: {other:?}"),
    }
}

/// 🔴 #8454 VC (plan :130): past max age no verdict is served, cached or
/// not, and an age past max age from any read is refused again here.
#[test]
fn snapshot_older_than_max_age_is_not_served() {
    let (snap, clock, _calls) = scripted(|_| ready_report(31));
    let cache = VerdictCache::default();
    assert_eq!(ask_loaded(&snap, &cache), changed());
    assert!(is_ready(&ask(&snap, &cache)), "fixture: 31 is ready");
    clock.set(MAX_AGE + Duration::from_millis(1));
    let stale = VerdictReply::Unserved {
        why: Unserved::Stale,
    };
    assert_eq!(ask(&snap, &cache), stale, "a stale snapshot was served");
    // The route's own age check, for a read that reports an old report.
    let report = Arc::new(ready_report(31));
    let old = || Ok((Arc::clone(&report), MAX_AGE + Duration::from_millis(1)));
    assert_eq!(answer(old, &cache, Some(&home()), &home()), stale);
}

/// 🔴 #8454 VC (plan :132): a denied report, even one that still carries a
/// route, is cached and served as not ready, never as ready.
#[test]
fn denied_policy_report_is_cached_as_deny_not_allow() {
    let (snap, _clock, _calls) = scripted(|_| denied_report(41));
    let cache = VerdictCache::default();
    assert_eq!(ask_loaded(&snap, &cache), changed());
    for _ in 0..2 {
        match ask(&snap, &cache) {
            VerdictReply::NotReady {
                slug,
                not_ready,
                fingerprint,
                ..
            } => {
                assert_eq!(slug, NOT_READY_CHECK);
                assert_eq!(fingerprint, 41);
                assert!(
                    matches!(not_ready, NotReady::Denied { .. }),
                    "{not_ready:?}"
                );
            }
            other => panic!("a denied report must be served as not ready: {other:?}"),
        }
    }
}

/// 🔴 #8454 VC (R1): a report with fingerprint 0 is never cached or served.
#[test]
fn zero_fingerprint_is_never_served() {
    let report = Arc::new(ready_report(0));
    let cache = VerdictCache::default();
    let none = VerdictReply::Unserved {
        why: Unserved::NoFingerprint,
    };
    for _ in 0..3 {
        let read = || Ok((Arc::clone(&report), Duration::ZERO));
        assert_eq!(answer(read, &cache, Some(&home()), &home()), none);
    }
}

/// 🔴 #8454 VC (R2): a hook under another home, or a daemon with no home,
/// gets no verdict, and the snapshot is not read for it.
#[test]
fn home_mismatch_is_not_served() {
    let cache = VerdictCache::default();
    let mismatch = VerdictReply::Unserved {
        why: Unserved::HomeMismatch,
    };
    let never = || -> Result<(Arc<LoadReport>, Duration), SnapshotDeny> {
        panic!("the snapshot was read for a foreign home")
    };
    let other = Path::new("/home/someone-else");
    assert_eq!(answer(never, &cache, Some(&home()), other), mismatch);
    assert_eq!(answer(never, &cache, None, &home()), mismatch);
}

/// 🔴 #8454 VC: each snapshot refusal serves no verdict.
#[test]
fn snapshot_refusals_are_not_served() {
    let cache = VerdictCache::default();
    let cases = [
        (SnapshotDeny::NeverLoaded, Unserved::NeverLoaded),
        (
            SnapshotDeny::Stale {
                age: MAX_AGE,
                max: MAX_AGE,
            },
            Unserved::Stale,
        ),
        (SnapshotDeny::Poisoned, Unserved::Poisoned),
        (SnapshotDeny::Denied, Unserved::Refused),
    ];
    for (deny, why) in cases {
        let reply = answer(|| Err(deny), &cache, Some(&home()), &home());
        assert_eq!(reply, VerdictReply::Unserved { why }, "{deny:?}");
    }
}

/// 🔴 #8454 VC: a poisoned verdict cache serves nothing.
#[test]
fn poisoned_verdict_cache_is_not_served() {
    let cache = Arc::new(VerdictCache::default());
    let poisoner = Arc::clone(&cache);
    let _ = std::thread::spawn(move || {
        let _guard = poisoner.entry.lock();
        panic!("poison the verdict cache");
    })
    .join();
    let report = Arc::new(ready_report(51));
    let read = || Ok((Arc::clone(&report), Duration::ZERO));
    assert_eq!(
        answer(read, &cache, Some(&home()), &home()),
        VerdictReply::Unserved {
            why: Unserved::Poisoned
        }
    );
}

/// POST the route on `router` under `home`; the parsed reply.
async fn post(router: &axum::Router, home: &Path) -> VerdictReply {
    use tower::ServiceExt as _;
    let body = serde_json::json!({ "home": home }).to_string();
    let request = axum::http::Request::post(VERDICT_ROUTE)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body))
        .expect("request");
    let response = router.clone().oneshot(request).await.expect("response");
    assert!(response.status().is_success(), "{}", response.status());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("a verdict reply")
}

/// 🔴 #8454 VC (R2): through the daemon router, a daemon whose home has no
/// `config.yaml` (a denied load) answers not ready, never ready; a foreign
/// home is not served.
#[tokio::test]
async fn route_answers_a_denied_daemon_policy_as_not_ready() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonical");
    let state = Arc::new(crate::daemon::DaemonState::with_root_isolated_managed(root).await);
    let home = state.user_home().expect("pinned home");
    let router = super::router().with_state(state);
    assert_eq!(
        post(&router, Path::new("/somewhere/else")).await,
        VerdictReply::Unserved {
            why: Unserved::HomeMismatch
        }
    );
    let started = Instant::now();
    let reply = loop {
        let reply = post(&router, &home).await;
        assert!(!is_ready(&reply), "a denied policy was served as ready");
        if matches!(reply, VerdictReply::NotReady { .. }) {
            break reply;
        }
        assert!(started.elapsed() < POLL_GUARD, "last reply {reply:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    match reply {
        VerdictReply::NotReady { not_ready, .. } => assert!(
            matches!(
                not_ready,
                NotReady::Host {
                    branch: HostBranch::Missing,
                    ..
                }
            ),
            "{not_ready:?}"
        ),
        other => panic!("{other:?}"),
    }
}
