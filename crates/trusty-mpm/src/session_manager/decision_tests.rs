//! Answer idempotency at the session manager (#9841).
//!
//! Why: an answer to a pending decision must reach the pane once, however many
//! callers race it, and a repeat or stale answer must learn who answered.
//! What: [`AnswerDriver`] wraps [`FakeTmuxDriver`], records every pane send,
//! can hold the first send until a second arrives (as `rename_race_tests.rs`
//! does), can fail a send, and can make the store read-only mid-send. The
//! route-level tests in `managed_routes::answer_route_tests` reuse it.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use tempfile::TempDir;
use tokio::sync::RwLock;

use super::decision::{AnswerClaim, AnswerOutcome, DecisionId};
use super::manager::{ManagedError, ManagedTmuxDriver, SessionManager};
use super::pane_identity::PaneIdentity;
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::store::SessionStore;
use super::tests::{FakeTmuxDriver, bind_pane, seed_record};

/// How long the first send waits for a racing second one to reach tmux.
const RENDEZVOUS: Duration = Duration::from_millis(500);

/// The failure message when `set_pending_decision` minted no id.
const MINTED: &str = "set_pending_decision must mint a decision id";

/// What [`AnswerDriver`] does on a pane send.
#[derive(Default)]
pub(crate) struct SendHooks {
    /// Hold the first send until a second arrives, or [`RENDEZVOUS`].
    pub(crate) hold_first: bool,
    /// Fail every send after recording it.
    pub(crate) fail: bool,
    /// Make this directory read-only after the send, so the next store write
    /// fails.
    pub(crate) read_only_after: Option<PathBuf>,
}

/// A [`FakeTmuxDriver`] whose pane sends a test can race, fail and inspect.
pub(crate) struct AnswerDriver {
    pub(crate) inner: Arc<FakeTmuxDriver>,
    pub(crate) hooks: Mutex<SendHooks>,
    /// Every `send_line_to_pane` text, in arrival order.
    pub(crate) sends: Mutex<Vec<String>>,
    arrivals: Mutex<usize>,
    arrived: Condvar,
    /// The store whose write guard a send must never run under.
    pub(crate) store: Mutex<Option<Arc<RwLock<SessionStore>>>>,
    /// Set when a send ran while `store` was locked.
    pub(crate) sent_under_guard: Mutex<bool>,
}

impl AnswerDriver {
    pub(crate) fn new() -> Arc<Self> {
        let inner = FakeTmuxDriver::new();
        *inner.pane_id_override.lock().unwrap() = Some("%1".into());
        Arc::new(Self {
            inner,
            hooks: Mutex::new(SendHooks::default()),
            sends: Mutex::new(Vec::new()),
            arrivals: Mutex::new(0),
            arrived: Condvar::new(),
            store: Mutex::new(None),
            sent_under_guard: Mutex::new(false),
        })
    }

    pub(crate) fn sends(&self) -> Vec<String> {
        self.sends.lock().unwrap().clone()
    }

    /// Count this caller in, then wait (bounded) until two callers are in.
    fn rendezvous(&self) {
        let mut n = self.arrivals.lock().unwrap();
        *n += 1;
        self.arrived.notify_all();
        let _ = self
            .arrived
            .wait_timeout_while(n, RENDEZVOUS, |n| *n < 2)
            .unwrap();
    }
}

impl ManagedTmuxDriver for AnswerDriver {
    fn create_session(&self, name: &str, workdir: &str) -> Result<(), ManagedError> {
        self.inner.create_session(name, workdir)
    }

    fn kill_session(&self, name: &str) -> Result<(), ManagedError> {
        self.inner.kill_session(name)
    }

    fn send_line(&self, name: &str, text: &str) -> Result<(), ManagedError> {
        self.inner.send_line(name, text)
    }

    fn capture(&self, name: &str, lines: usize) -> Result<String, ManagedError> {
        self.inner.capture(name, lines)
    }

    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        self.inner.list_sessions()
    }

    fn get_pane_id(&self, name: &str) -> Option<String> {
        self.inner.get_pane_id(name)
    }

    fn pane_exists(&self, name: &str, pane_id: &str) -> bool {
        self.inner.pane_exists(name, pane_id)
    }

    fn pane_identity(&self, pane_id: &str) -> Result<PaneIdentity, ManagedError> {
        self.inner.pane_identity(pane_id)
    }

    fn send_line_to_pane(&self, _name: &str, _pane: &str, text: &str) -> Result<(), ManagedError> {
        if let Some(store) = self.store.lock().unwrap().as_ref()
            && store.try_write().is_err()
        {
            *self.sent_under_guard.lock().unwrap() = true;
        }
        let (hold, fail, read_only) = {
            let h = self.hooks.lock().unwrap();
            (h.hold_first, h.fail, h.read_only_after.clone())
        };
        if hold {
            self.rendezvous();
        }
        self.sends.lock().unwrap().push(text.to_owned());
        if let Some(dir) = read_only {
            set_mode(&dir, 0o555);
        }
        if fail {
            return Err(ManagedError::TmuxUnavailable(
                "fake: send-keys failed".into(),
            ));
        }
        Ok(())
    }
}

/// Set `dir`'s permission bits.
pub(crate) fn set_mode(dir: &Path, mode: u32) {
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// A record in `state` whose pane `%1` the fake proves is its own.
pub(crate) async fn seed_answerable(
    mgr: &SessionManager,
    root: &TempDir,
    state: ManagedSessionState,
) -> ManagedSessionId {
    let id = ManagedSessionId::new();
    seed_record(mgr, root, id, state, false).await;
    bind_pane(mgr, &id).await;
    id
}

/// A manager over a fresh [`AnswerDriver`], with one record in `state`.
struct Fixture {
    dir: TempDir,
    mgr: Arc<SessionManager>,
    driver: Arc<AnswerDriver>,
    id: ManagedSessionId,
}

async fn fixture(state: ManagedSessionState) -> Fixture {
    let dir = TempDir::new().unwrap();
    let driver = AnswerDriver::new();
    let mgr = Arc::new(
        SessionManager::new(dir.path(), driver.clone())
            .await
            .expect("manager"),
    );
    let id = seed_answerable(&mgr, &dir, state).await;
    *driver.store.lock().unwrap() = Some(mgr.store.clone());
    Fixture {
        dir,
        mgr,
        driver,
        id,
    }
}

impl Fixture {
    async fn record(&self) -> SessionRecord {
        self.mgr.get(&self.id).await.expect("record")
    }

    /// Set a pending decision; the id it was given, if any.
    async fn ask(&self, question: &str) -> Option<DecisionId> {
        self.mgr
            .set_pending_decision(&self.id, question, Some("merge"))
            .await
            .expect("set_pending_decision");
        self.record().await.pending_decision_id
    }

    async fn answer(
        &self,
        text: &str,
        claim: AnswerClaim,
    ) -> Result<Option<DecisionId>, ManagedError> {
        self.mgr.answer_decision(&self.id, text, &claim).await
    }
}

fn claim(by: &str, id: Option<DecisionId>) -> AnswerClaim {
    AnswerClaim {
        decision_id: id,
        answered_by: by.to_owned(),
    }
}

fn conflict_text(r: Result<Option<DecisionId>, ManagedError>) -> String {
    match r {
        Err(ManagedError::DecisionConflict(text)) => text,
        other => panic!("expected DecisionConflict, got {other:?}"),
    }
}

/// #9841: two answers racing one pending decision reach the pane once. The
/// first send is held until the second arrives (or 500 ms), so an answer
/// path that checks outside the store guard, or sends before it claims,
/// sends twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_answers_reach_the_pane_once() {
    let f = fixture(ManagedSessionState::Active).await;
    f.driver.hooks.lock().unwrap().hold_first = true;
    f.ask("merge or rebase?").await;

    let (m1, m2, id) = (Arc::clone(&f.mgr), Arc::clone(&f.mgr), f.id);
    let first = tokio::spawn(async move {
        m1.answer_decision(&id, "merge", &AnswerClaim::by("first"))
            .await
    });
    let second = tokio::spawn(async move {
        m2.answer_decision(&id, "rebase", &AnswerClaim::by("second"))
            .await
    });
    let (r1, r2) = (first.await.expect("join"), second.await.expect("join"));

    let sends = f.driver.sends();
    assert_eq!(sends.len(), 1, "the pane got {sends:?} ({r1:?} / {r2:?})");
    let (winner, refused) = match (r1, r2) {
        (Ok(_), refused @ Err(_)) => ("first", refused),
        (refused @ Err(_), Ok(_)) => ("second", refused),
        other => panic!("exactly one answer must win: {other:?}"),
    };
    let text = conflict_text(refused);
    assert!(
        text.contains(&format!("already answered by {winner} at ")),
        "the conflict must name the winner: {text}"
    );
}

/// #9841: an answer naming a decision already answered does not take the
/// next one; one naming no known decision is "not pending".
#[tokio::test]
async fn stale_decision_id_conflicts() {
    let f = fixture(ManagedSessionState::Active).await;
    let a = f.ask("decision A").await.expect(MINTED);
    let took = f.answer("yes", claim("alice", Some(a))).await;
    assert_eq!(took.expect("answer A"), Some(a));

    let b = f.ask("decision B").await.expect(MINTED);
    assert_ne!(a, b, "decision B must get its own id");
    let text = conflict_text(f.answer("yes", claim("bob", Some(a))).await);
    assert!(
        text.contains(&format!("decision {a} already answered by alice")),
        "{text}"
    );
    let text = conflict_text(f.answer("yes", claim("bob", Some(DecisionId::new()))).await);
    assert!(text.contains("is not pending"), "{text}");

    let record = f.record().await;
    assert_eq!(record.pending_decision.as_deref(), Some("decision B"));
    assert_eq!(record.pending_decision_id, Some(b));
    assert_eq!(f.driver.sends(), vec!["yes".to_string()]);
}

/// #9841: a repeat answer gets a conflict naming the first answerer and the
/// time, and nothing is typed. No send runs under the store guard.
#[tokio::test]
async fn repeat_answer_names_first_answerer_and_time() {
    let f = fixture(ManagedSessionState::Active).await;
    f.ask("merge or rebase?").await;
    f.answer("merge", AnswerClaim::by("alice"))
        .await
        .expect("first answer");
    let at = f
        .record()
        .await
        .last_answer
        .expect("the claim is recorded")
        .answered_at
        .to_rfc3339();

    let text = conflict_text(f.answer("rebase", AnswerClaim::by("bob")).await);
    assert!(
        text.contains(&format!("already answered by alice at {at}")),
        "{text}"
    );
    assert_eq!(f.driver.sends(), vec!["merge".to_string()]);
    assert!(
        !*f.driver.sent_under_guard.lock().unwrap(),
        "a pane send ran while the store guard was held"
    );
}

/// #9841 (D4): a send that fails after the claim closes the decision as
/// `DeliveryFailed`; it is not reopened, so a repeat is a conflict.
#[tokio::test]
async fn send_failure_after_claim_closes_decision_as_failed() {
    let f = fixture(ManagedSessionState::Active).await;
    f.driver.hooks.lock().unwrap().fail = true;
    let id = f.ask("merge or rebase?").await;

    let err = f.answer("merge", AnswerClaim::by("alice")).await;
    match &err {
        Err(ManagedError::DeliveryFailed(text)) => assert!(
            text.contains(&id.expect(MINTED).to_string())
                && text.contains("the decision is closed"),
            "{text}"
        ),
        other => panic!("expected DeliveryFailed, got {other:?}"),
    }
    let record = f.record().await;
    assert!(record.pending_decision.is_none(), "the decision reopened");
    assert_eq!(
        record.last_answer.map(|a| a.outcome),
        Some(AnswerOutcome::DeliveryFailed)
    );

    f.driver.hooks.lock().unwrap().fail = false;
    let text = conflict_text(f.answer("merge", AnswerClaim::by("bob")).await);
    assert!(text.contains("(delivery failed)"), "{text}");
    assert_eq!(
        f.driver.sends().len(),
        1,
        "the repeat must not retry the send"
    );
}

/// #9841: fail closed — a claim that cannot be persisted sends nothing and
/// leaves the decision pending, so a retry once the store recovers delivers.
#[tokio::test]
async fn claim_persist_failure_sends_nothing_and_keeps_the_decision() {
    let f = fixture(ManagedSessionState::Active).await;
    f.ask("merge or rebase?").await;

    set_mode(f.dir.path(), 0o555);
    let r = f.answer("merge", AnswerClaim::by("alice")).await;
    set_mode(f.dir.path(), 0o755);

    assert!(r.is_err(), "an unpersisted claim must refuse: {r:?}");
    assert!(
        f.driver.sends().is_empty(),
        "the pane got {:?}",
        f.driver.sends()
    );
    let record = f.record().await;
    assert!(record.pending_decision.is_some(), "the decision was lost");
    assert!(record.last_answer.is_none(), "an unpersisted claim stuck");

    f.answer("merge", AnswerClaim::by("alice"))
        .await
        .expect("the retry delivers");
    assert_eq!(f.driver.sends(), vec!["merge".to_string()]);
}

/// #9841: fail open — when the settle write fails after a delivered send,
/// the answer still reports success; the record keeps `Claimed`, which a
/// repeat reports as "delivery unconfirmed".
#[tokio::test]
async fn settle_failure_still_reports_the_delivered_answer() {
    let f = fixture(ManagedSessionState::Active).await;
    let id = f.ask("merge or rebase?").await;
    f.driver.hooks.lock().unwrap().read_only_after = Some(f.dir.path().to_path_buf());

    let r = f.answer("merge", AnswerClaim::by("alice")).await;
    set_mode(f.dir.path(), 0o755);
    f.driver.hooks.lock().unwrap().read_only_after = None;

    assert_eq!(r.expect("the pane got the answer"), id);
    assert_eq!(
        f.record().await.last_answer.map(|a| a.outcome),
        Some(AnswerOutcome::Claimed)
    );
    let text = conflict_text(f.answer("merge", AnswerClaim::by("bob")).await);
    assert!(text.contains("(delivery unconfirmed)"), "{text}");
}

/// #9841: a repeat answer is the conflict even when the pane is gone; the
/// pane refusal must not hide who answered.
#[tokio::test]
async fn repeat_answer_is_a_conflict_even_when_the_pane_is_gone() {
    let f = fixture(ManagedSessionState::Active).await;
    f.ask("merge or rebase?").await;
    f.answer("merge", AnswerClaim::by("alice"))
        .await
        .expect("first answer");
    *f.driver.inner.pane_exists_override.lock().unwrap() = Some(false);

    let text = conflict_text(f.answer("merge", AnswerClaim::by("bob")).await);
    assert!(text.contains("already answered by alice"), "{text}");
}

/// #9841: the pane is proven before the claim, so a refused pane leaves the
/// decision pending for a retry.
#[tokio::test]
async fn pane_refusal_leaves_decision_pending() {
    let f = fixture(ManagedSessionState::Active).await;
    f.ask("merge or rebase?").await;
    *f.driver.inner.pane_exists_override.lock().unwrap() = Some(false);

    let r = f.answer("merge", AnswerClaim::by("alice")).await;
    assert!(matches!(r, Err(ManagedError::PaneGone(..))), "{r:?}");
    let record = f.record().await;
    assert!(
        record.pending_decision.is_some(),
        "a pane refusal burned the claim"
    );
    assert!(record.last_answer.is_none());
    assert!(f.driver.sends().is_empty());
}

/// #9841: a decision set before decision ids existed is answerable once by an
/// answer that names no id; one that names an id finds it "not pending".
#[tokio::test]
async fn legacy_record_without_decision_id_loads_and_is_answerable() {
    let f = fixture(ManagedSessionState::Active).await;
    let mut legacy = f.record().await;
    legacy.pending_decision = Some("merge or rebase?".into());
    legacy.pending_decision_id = None;
    f.mgr
        .store
        .write()
        .await
        .upsert(legacy)
        .await
        .expect("seed");

    let text = conflict_text(
        f.answer("merge", claim("bob", Some(DecisionId::new())))
            .await,
    );
    assert!(text.contains("is not pending"), "{text}");
    assert_eq!(
        f.answer("merge", AnswerClaim::by("alice"))
            .await
            .expect("answer"),
        None
    );
    let text = conflict_text(f.answer("merge", AnswerClaim::by("bob")).await);
    assert!(text.contains("already answered by alice"), "{text}");
    assert_eq!(f.driver.sends(), vec!["merge".to_string()]);
}

/// #9841 (D3): a record written before the two decision fields existed loads
/// with both `None`, and a record without a decision serializes without them.
#[tokio::test]
async fn old_record_json_loads_without_decision_fields() {
    let f = fixture(ManagedSessionState::Active).await;
    let mut json = serde_json::to_value(f.record().await).expect("to json");
    let map = json.as_object_mut().expect("object");
    assert!(!map.contains_key("pending_decision_id"), "{map:?}");
    assert!(!map.contains_key("last_answer"), "{map:?}");
    map.insert("pending_decision".into(), "legacy question".into());

    let old: SessionRecord = serde_json::from_value(json).expect("a pre-#9841 record loads");
    assert_eq!(old.pending_decision.as_deref(), Some("legacy question"));
    assert!(old.pending_decision_id.is_none());
    assert!(old.last_answer.is_none());
}

/// #9841: every decision set gets a fresh id.
#[tokio::test]
async fn set_pending_decision_mints_a_new_id_each_time() {
    let f = fixture(ManagedSessionState::Active).await;
    let a = f.ask("first").await.expect(MINTED);
    let b = f.ask("second").await.expect(MINTED);
    assert_ne!(a, b);
}

/// #9841: the claim is in `sessions.json`, so a restarted daemon still
/// refuses the repeat.
#[tokio::test]
async fn claim_is_durable_across_store_reload() {
    let f = fixture(ManagedSessionState::Active).await;
    let id = f.ask("merge or rebase?").await;
    f.answer("merge", AnswerClaim::by("alice"))
        .await
        .expect("answer");

    let reloaded = SessionManager::new(f.dir.path(), f.driver.clone())
        .await
        .expect("reload");
    let record = reloaded.get(&f.id).await.expect("record");
    let last = record.last_answer.expect("the claim survived the reload");
    assert_eq!(
        (last.decision_id, last.outcome),
        (id, AnswerOutcome::Delivered)
    );
    let r = reloaded
        .answer_decision(&f.id, "merge", &AnswerClaim::by("bob"))
        .await;
    assert!(conflict_text(r).contains("already answered by alice"));
    assert_eq!(f.driver.sends().len(), 1);
}

/// #9841 (D1, D73 ruling: strict): with no decision pending and none
/// answered, an answer is refused; nothing is typed and the record is
/// unchanged.
#[tokio::test]
async fn answer_decision_without_a_pending_decision_is_refused() {
    let f = fixture(ManagedSessionState::Active).await;
    let before = serde_json::to_value(f.record().await).expect("json");
    for text in ["/approve", "/deny"] {
        let e = f
            .answer(text, AnswerClaim::by("slack"))
            .await
            .expect_err("an answer with nothing pending must be refused");
        assert!(
            e.to_string().starts_with("no pending decision on session"),
            "{e}"
        );
    }
    assert!(f.driver.sends().is_empty(), "typed: {:?}", f.driver.sends());
    assert_eq!(
        serde_json::to_value(f.record().await).expect("json"),
        before
    );
}

/// #9841 (D5): `answered_by` is cleaned and clipped, and falls back to the
/// transport; a malformed `decision_id` is refused with its value named.
#[test]
fn answered_by_is_clipped_and_defaults_to_the_transport() {
    let c = AnswerClaim::from_wire(None, Some("  ali\u{7}ce\n "), "http").expect("claim");
    assert_eq!(c.answered_by, "alice");
    assert_eq!(c.decision_id, None);
    let c = AnswerClaim::from_wire(None, Some(&"x".repeat(100)), "rpc").expect("claim");
    assert_eq!(c.answered_by.chars().count(), 64);
    let c = AnswerClaim::from_wire(None, Some("\u{1b}"), "rpc").expect("claim");
    assert_eq!(c.answered_by, "rpc");
    let id = DecisionId::new();
    let c = AnswerClaim::from_wire(Some(&id.to_string()), None, "http").expect("claim");
    assert_eq!((c.decision_id, c.answered_by.as_str()), (Some(id), "http"));
    let e = AnswerClaim::from_wire(Some("nope"), None, "http").expect_err("bad id");
    assert!(e.contains("\"nope\""), "{e}");
}
