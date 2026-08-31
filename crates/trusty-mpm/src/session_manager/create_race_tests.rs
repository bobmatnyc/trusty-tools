//! The create-path name race (issue #3707).
//!
//! Why: `tmux new-session -A` attaches to an existing session of the same name
//! instead of failing, so two creators that computed the same name both
//! returned success and two `SessionRecord`s were persisted against ONE
//! physical pane — aliasing, with no error at any step and every subsequent
//! `send-keys` typed into a terminal another session was driving. These tests
//! pin the fix: the managed create path creates WITHOUT `-A`, so a lost race
//! refuses, and the refusal is retried under a fresh name.
//! What: two fake drivers modelling the two halves of the race — one whose
//! listing is stale while tmux already holds the name, one that refuses every
//! name — driven through `SessionManager::create_with_reserved_name`.
//! Test: this file IS the test module.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use super::manager::{ManagedError, ManagedTmuxDriver, SessionManager};
use super::record::ManagedSessionId;

/// A driver that reproduces the race: the snapshot the name dedupe reads is
/// STALE, while tmux itself already holds the name.
///
/// Why: the aliasing bug lives entirely in the gap between those two views.
/// `list_sessions` is what `dedupe_session_name` consults a moment before the
/// create; the live set is what `tmux new-session` decides against. Modelling
/// them as one map would close the very window these tests open.
/// `create_session` succeeds on a name already live — that is what `-A` does,
/// and it is the behaviour under test rather than a shortcut.
/// What: `list_sessions` answers from `snapshot`; `session_exists_checked`
/// answers from `live`; `create_session` records the name and inserts it into
/// `live`, never failing.
/// Test: `two_creators_of_one_name_never_share_a_pane`.
struct RacingTmuxDriver {
    /// The stale live-session snapshot the name dedupe reads.
    snapshot: Mutex<Vec<String>>,
    /// The names tmux actually holds.
    live: Mutex<HashSet<String>>,
    /// Every name `create_session` was asked to create, in order.
    creates: Mutex<Vec<String>>,
}

impl RacingTmuxDriver {
    /// A driver whose tmux already holds `live` but whose listing shows
    /// `snapshot`.
    fn new(snapshot: &[&str], live: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            snapshot: Mutex::new(snapshot.iter().map(|s| (*s).to_string()).collect()),
            live: Mutex::new(live.iter().map(|s| (*s).to_string()).collect()),
            creates: Mutex::new(Vec::new()),
        })
    }
}

impl ManagedTmuxDriver for RacingTmuxDriver {
    fn create_session(&self, name: &str, _workdir: &str) -> Result<(), ManagedError> {
        self.creates.lock().unwrap().push(name.to_string());
        self.live.lock().unwrap().insert(name.to_string());
        Ok(())
    }

    fn session_exists_checked(&self, name: &str) -> Result<bool, ManagedError> {
        Ok(self.live.lock().unwrap().contains(name))
    }

    fn kill_session(&self, name: &str) -> Result<(), ManagedError> {
        self.live.lock().unwrap().remove(name);
        Ok(())
    }

    fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }

    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(self.snapshot.lock().unwrap().clone())
    }
}

/// A driver whose tmux refuses every name, so the retry has to terminate.
///
/// Why: the retry re-dedupes after each refusal; without a bound a server that
/// refuses everything turns one create into a spin.
/// What: `session_exists_checked` always says taken; `create_session` records
/// the call so the test can prove nothing was created.
/// Test: `create_gives_up_after_repeated_name_refusals`.
struct AlwaysTakenTmuxDriver {
    /// Every name `create_session` was asked to create — expected to stay empty.
    creates: Mutex<Vec<String>>,
}

impl ManagedTmuxDriver for AlwaysTakenTmuxDriver {
    fn create_session(&self, name: &str, _workdir: &str) -> Result<(), ManagedError> {
        self.creates.lock().unwrap().push(name.to_string());
        Ok(())
    }

    fn session_exists_checked(&self, _name: &str) -> Result<bool, ManagedError> {
        Ok(true)
    }

    fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }

    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(Vec::new())
    }
}

/// #3707: a creator that loses the name race gets its own pane, never a share
/// of the winner's.
///
/// What: the dedupe's snapshot is empty while tmux already holds
/// `tm-race-01`. Asserts the persisted record does NOT carry the taken name,
/// that exactly one pane was created under the record's own name, and that the
/// winner's pane was never touched.
/// Test: this function IS the test.
#[tokio::test]
async fn two_creators_of_one_name_never_share_a_pane() {
    let dir = TempDir::new().unwrap();
    let fake = RacingTmuxDriver::new(&[], &["tm-race-01"]);
    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    let record = mgr
        .create_with_reserved_name(
            ManagedSessionId::new(),
            "tm-race-01".into(),
            "loser".into(),
            Some(PathBuf::from("/tmp/wt-race")),
            None,
            None,
            None,
            Default::default(),
            false,
            false,
        )
        .await
        .expect("a lost name race must retry under a fresh name, not fail");

    assert_ne!(
        record.tmux_name, "tm-race-01",
        "the losing creator must not persist a record against the winner's pane"
    );
    let creates = fake.creates.lock().unwrap().clone();
    assert!(
        !creates.contains(&"tm-race-01".to_string()),
        "the winner's live session must never be re-created or attached: {creates:?}"
    );
    assert_eq!(
        creates,
        vec![record.tmux_name.clone()],
        "exactly one pane must be created, under the record's own name"
    );
}

/// #3707: sustained refusals end in a bounded error, not a spin.
///
/// Test: this function IS the test.
#[tokio::test]
async fn create_gives_up_after_repeated_name_refusals() {
    let dir = TempDir::new().unwrap();
    let fake = Arc::new(AlwaysTakenTmuxDriver {
        creates: Mutex::new(Vec::new()),
    });
    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    let err = mgr
        .create_with_reserved_name(
            ManagedSessionId::new(),
            "tm-wedged-01".into(),
            "loser".into(),
            Some(PathBuf::from("/tmp/wt-wedged")),
            None,
            None,
            None,
            Default::default(),
            false,
            false,
        )
        .await
        .expect_err("a name that can never be claimed must fail, not loop");

    assert!(
        matches!(err, ManagedError::NameCollision(_)),
        "a sustained refusal is a name collision, not a tmux outage: {err:?}"
    );
    assert!(
        fake.creates.lock().unwrap().is_empty(),
        "nothing may be created when every name is refused"
    );
}
