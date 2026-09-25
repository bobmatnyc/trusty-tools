//! Tests for the migrate step's store pre-check (#8563, Refs #8236).
//!
//! Every plist is a temp file under a temp `$HOME`, every store is a
//! `MemoryKeyStore` or a fake wrapping one, and every value is synthetic. No
//! test reads `~/Library/LaunchAgents`, the real store, or the real `.env*`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use trusty_common::credentials::{KeyStore, KeyStoreError, MemoryKeyStore};

use super::*;

/// The synthetic plist value.
const PLIST_VALUE: &str = "sk-test-not-real";

/// A synthetic, DIFFERENT value an operator already stored (a rotated key).
const STORED_VALUE: &str = "sk-test-rotated-not-real";

/// Write `com.trusty.<label>.plist` holding `OPENROUTER_API_KEY = value`.
fn plist_at(home: &Path, label: &str, value: &str) -> PathBuf {
    let dir = home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("com.trusty.{label}.plist"));
    let body = format!(
        "<plist version=\"1.0\">\n<dict>\n  \
         <key>Label</key>\n  <string>com.trusty.{label}</string>\n  \
         <key>EnvironmentVariables</key>\n  <dict>\n    \
         <key>OPENROUTER_API_KEY</key>\n    <string>{value}</string>\n    \
         <key>RUST_LOG</key>\n    <string>info</string>\n  \
         </dict>\n</dict>\n</plist>\n"
    );
    std::fs::write(&path, body).expect("write plist");
    path
}

/// A temp `$HOME` holding one trusty plist with `PLIST_VALUE`.
fn home_with_plist() -> (tempfile::TempDir, PathBuf) {
    let home = tempfile::tempdir().expect("tempdir");
    let path = plist_at(home.path(), "mpm", PLIST_VALUE);
    (home, path)
}

/// A store that counts writes and can fail every read or every write.
struct CountingStore {
    /// Where accepted writes land.
    inner: MemoryKeyStore,
    /// How many times `set` was called.
    sets: AtomicUsize,
    /// When true, every `try_get` fails.
    fail_reads: bool,
    /// When true, every `set` fails (after counting).
    fail_writes: bool,
}

impl CountingStore {
    fn new(fail_reads: bool) -> Self {
        Self {
            inner: MemoryKeyStore::new(),
            sets: AtomicUsize::new(0),
            fail_reads,
            fail_writes: false,
        }
    }

    /// `FileKeyStore` over a corrupt `credentials.toml`: `try_get` reports the
    /// key absent, and `set` fails because it parses the file first.
    fn corrupt_file() -> Self {
        Self {
            fail_writes: true,
            ..Self::new(false)
        }
    }

    fn sets(&self) -> usize {
        self.sets.load(Ordering::SeqCst)
    }
}

impl KeyStore for CountingStore {
    fn get(&self, provider: &str) -> Option<String> {
        self.try_get(provider).ok().flatten()
    }
    fn try_get(&self, provider: &str) -> Result<Option<String>, KeyStoreError> {
        if self.fail_reads {
            return Err(KeyStoreError::HomeUnavailable);
        }
        Ok(self.inner.get(provider))
    }
    fn set(&self, provider: &str, value: &str) -> Result<(), KeyStoreError> {
        self.sets.fetch_add(1, Ordering::SeqCst);
        if self.fail_writes {
            return Err(KeyStoreError::Io {
                path: PathBuf::from("corrupt-store.toml"),
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, "corrupt"),
            });
        }
        self.inner.set(provider, value)
    }
    fn unset(&self, provider: &str) -> Result<(), KeyStoreError> {
        self.inner.unset(provider)
    }
    fn list(&self) -> Vec<String> {
        self.inner.list()
    }
}

/// Assert no rendered step carries either synthetic value or their prefix.
fn assert_no_value(steps: &[RepairStep]) {
    let rendered = format!("{steps:?}");
    for value in [PLIST_VALUE, STORED_VALUE, "sk-test"] {
        assert!(!rendered.contains(value), "value leaked: {rendered}");
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("read plist")
}

/// Why (#8563): an operator who already stored a NEW rotated key must not have
/// it overwritten by the OLD plist value. The key is refused in both modes, the
/// store keeps its value, and the plist entry is not stripped.
/// Test: this test.
#[test]
fn a_different_store_value_is_never_overwritten() {
    let (home, path) = home_with_plist();
    let before = read(&path);
    let store = Arc::new(CountingStore::new(false));
    store
        .inner
        .set("openrouter", STORED_VALUE)
        .expect("seed store");

    for mode in [RepairMode::DryRun, RepairMode::Apply] {
        let steps = repair_with_store(home.path(), mode, store.clone());
        assert_eq!(steps.len(), 1, "{steps:?}");
        let rendered = format!("{:?}", steps[0]);
        assert!(
            rendered.contains("the store already holds a different openrouter credential"),
            "{mode:?} must say why the key is left: {rendered}"
        );
        assert!(
            !matches!(
                steps[0].status,
                StepStatus::Planned | StepStatus::Applied { .. }
            ),
            "{mode:?} must not plan or report a strip: {rendered}"
        );
        assert_no_value(&steps);
    }

    assert_eq!(store.get("openrouter").as_deref(), Some(STORED_VALUE));
    assert_eq!(store.sets(), 0, "the store must not be written");
    assert_eq!(read(&path), before, "the plist key must not be stripped");
}

/// Why (#8563): a store that already holds the SAME value has nothing to gain
/// from a write; the key counts as imported and is stripped without one.
/// Test: this test.
#[test]
fn an_equal_store_value_is_imported_without_a_write() {
    let (home, path) = home_with_plist();
    let store = Arc::new(CountingStore::new(false));
    store
        .inner
        .set("openrouter", PLIST_VALUE)
        .expect("seed store");

    let steps = repair_with_store(home.path(), RepairMode::Apply, store.clone());

    assert_eq!(steps[0].status, StepStatus::Applied { backup: None });
    assert_eq!(store.sets(), 0, "an equal value must not be rewritten");
    assert_eq!(store.get("openrouter").as_deref(), Some(PLIST_VALUE));
    assert!(!read(&path).contains("OPENROUTER_API_KEY"));
    assert_no_value(&steps);
}

/// Why (#8563): a store that cannot be read before the write cannot prove the
/// write is safe. Fail closed: no write, no strip, in either mode.
/// Test: this test.
#[test]
fn a_store_read_error_before_the_write_fails_closed() {
    let (home, path) = home_with_plist();
    let before = read(&path);
    let store = Arc::new(CountingStore::new(true));

    for mode in [RepairMode::DryRun, RepairMode::Apply] {
        let steps = repair_with_store(home.path(), mode, store.clone());
        let rendered = format!("{:?}", steps[0]);
        assert!(
            !matches!(
                steps[0].status,
                StepStatus::Planned | StepStatus::Applied { .. }
            ),
            "{mode:?} must not plan or report a strip: {rendered}"
        );
        assert!(
            rendered.contains("could not be read before the write"),
            "{mode:?}: {rendered}"
        );
        assert_no_value(&steps);
    }

    assert_eq!(store.sets(), 0, "no write after a failed pre-check");
    assert_eq!(read(&path), before, "no strip after a failed pre-check");
}

/// Why (#8563): the running daemon keeps the removed credential in its
/// environment until launchd reloads the unit, and `kickstart -k` does not
/// reload it. The applied step has to say which commands do.
/// Test: this test.
#[test]
fn the_applied_step_names_the_reload_commands() {
    let (home, path) = home_with_plist();
    let steps = repair_with_store(
        home.path(),
        RepairMode::Apply,
        Arc::new(MemoryKeyStore::new()),
    );

    assert_eq!(steps[0].status, StepStatus::Applied { backup: None });
    let what = &steps[0].what;
    assert!(
        what.contains("launchctl bootout gui/$(id -u)/com.trusty.mpm"),
        "{what}"
    );
    assert!(
        what.contains(&format!(
            "launchctl bootstrap gui/$(id -u) {}",
            path.display()
        )),
        "{what}"
    );
    assert!(
        what.contains("`launchctl kickstart -k` does NOT reload"),
        "{what}"
    );
    assert_no_value(&steps);
}

/// Why (#8563): `FileKeyStore::try_get` reports an unreadable or corrupt
/// credentials file as absent, so the pre-check says "write". The apply's
/// `set` then fails on the same file, and the key must stay in the plist.
/// Test: this test.
#[test]
fn a_corrupt_file_store_refuses_the_write_and_keeps_the_key() {
    let (home, path) = home_with_plist();
    let before = read(&path);
    let store = Arc::new(CountingStore::corrupt_file());

    let plan = repair_with_store(home.path(), RepairMode::DryRun, store.clone());
    assert_eq!(plan[0].status, StepStatus::Planned, "{plan:?}");
    assert!(
        plan[0]
            .what
            .contains("a planned write can still be refused"),
        "the plan must warn that the write may be refused: {}",
        plan[0].what
    );
    assert_eq!(store.sets(), 0, "the dry run never writes");

    let steps = repair_with_store(home.path(), RepairMode::Apply, store.clone());
    let rendered = format!("{:?}", steps[0]);
    assert!(
        matches!(steps[0].status, StepStatus::Failed(_)),
        "{rendered}"
    );
    assert!(
        rendered.contains("the store refused the write"),
        "{rendered}"
    );
    assert_eq!(store.sets(), 1, "the write was attempted once");
    assert_eq!(read(&path), before, "a key that was not stored stays");
    assert_no_value(&plan);
    assert_no_value(&steps);
}

/// Why (#8563): the apply writes the first plist's value, then refuses the
/// second plist's different value. The dry run has to plan the same thing,
/// not check both against the store as it stood before the run.
/// Test: this test.
#[test]
fn the_plan_refuses_a_second_plist_with_a_different_value() {
    let home = tempfile::tempdir().expect("tempdir");
    plist_at(home.path(), "a", PLIST_VALUE);
    let second = plist_at(home.path(), "b", STORED_VALUE);
    let store = Arc::new(CountingStore::new(false));

    let plan = repair_with_store(home.path(), RepairMode::DryRun, store.clone());
    assert_eq!(plan.len(), 2, "{plan:?}");
    assert_eq!(plan[0].status, StepStatus::Planned, "{plan:?}");
    let rendered = format!("{:?}", plan[1]);
    assert!(
        matches!(plan[1].status, StepStatus::Refused(_)),
        "the plan must refuse the second plist: {rendered}"
    );
    assert!(
        rendered.contains("the store already holds a different openrouter credential"),
        "{rendered}"
    );
    assert_eq!(store.sets(), 0, "the dry run never writes");
    assert_no_value(&plan);

    // The apply does what the plan said: first imported, second left in place.
    let before = read(&second);
    let steps = repair_with_store(home.path(), RepairMode::Apply, store.clone());
    assert_eq!(steps[0].status, StepStatus::Applied { backup: None });
    assert!(
        matches!(steps[1].status, StepStatus::Failed(_)),
        "{steps:?}"
    );
    assert_eq!(read(&second), before);
    assert_eq!(store.get("openrouter").as_deref(), Some(PLIST_VALUE));
    assert_no_value(&steps);
}

/// Why (#8563): a second plist holding the value the first one will write
/// counts as imported with no write of its own, in the plan and the apply.
/// Test: this test.
#[test]
fn the_plan_imports_a_second_plist_with_an_equal_value() {
    let home = tempfile::tempdir().expect("tempdir");
    plist_at(home.path(), "a", PLIST_VALUE);
    plist_at(home.path(), "b", PLIST_VALUE);
    let store = Arc::new(CountingStore::new(false));

    let plan = repair_with_store(home.path(), RepairMode::DryRun, store.clone());
    assert_eq!(plan.len(), 2, "{plan:?}");
    assert!(
        plan.iter().all(|s| s.status == StepStatus::Planned),
        "{plan:?}"
    );
    assert!(plan[0].what.contains("a planned write"), "{}", plan[0].what);
    assert!(
        !plan[1].what.contains("a planned write"),
        "the second plist needs no write: {}",
        plan[1].what
    );
    assert_no_value(&plan);

    let steps = repair_with_store(home.path(), RepairMode::Apply, store.clone());
    assert!(
        steps
            .iter()
            .all(|s| s.status == StepStatus::Applied { backup: None }),
        "{steps:?}"
    );
    assert_eq!(store.sets(), 1, "only the first plist writes");
    assert_no_value(&steps);
}
