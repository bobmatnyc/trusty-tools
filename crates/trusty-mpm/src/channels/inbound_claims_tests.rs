//! Tests for the inbound event claim log (#9923 Q3-Q5).
//!
//! Why: the log is a Fail-Open Check site; each test goes red if an arm
//! that must refuse (a second opener, an append error, a log a correct store
//! never writes, an unsealed file, a failed prune) is changed to serve.
//! What: real files under a hermetic temp dir; faults through the store's
//! own seam.
//! Test: this is the test module.

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use chrono::{TimeDelta, Utc};

use super::{
    Claim, ClaimError, ClaimFault, ClaimHold, ClaimStore, RETENTION, claimed_never_done, prune_path,
};
use crate::channels::question_table::{QuestionTableError, lock_path, torn_path};

/// A temp dir and the log path inside a not-yet-created `channels/`.
struct Fixture {
    _tmp: tempfile::TempDir,
    path: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = crate::test_support::hermetic_temp_dir();
    let path = tmp.path().join("channels").join("inbound_claims.jsonl");
    Fixture { _tmp: tmp, path }
}

/// A claim line at `at`, as the store writes it.
fn claimed_line(id: &str, at: chrono::DateTime<Utc>) -> String {
    format!(
        "{{\"v\":1,\"kind\":\"claimed\",\"event_id\":\"{id}\",\"at\":\"{}\"}}\n",
        at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    )
}

fn write_log(path: &std::path::Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    std::fs::write(path, text).expect("write log");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("0600");
}

/// 🔴 #9923 Q3: a claim is durable, a second is a duplicate, and both hold
/// across a reopen from disk.
#[test]
fn claim_is_durable_and_a_reclaim_is_a_duplicate_after_reopen() {
    let fx = fixture();
    let store = ClaimStore::open_at(&fx.path).expect("open");
    assert_eq!(store.claim("Ev1").expect("claim"), Claim::Fresh);
    assert_eq!(store.claim("Ev1").expect("claim"), Claim::Duplicate);
    drop(store);
    let text = std::fs::read_to_string(&fx.path).expect("log");
    assert_eq!(text.matches("\"claimed\"").count(), 1, "{text}");
    let store = ClaimStore::open_at(&fx.path).expect("reopen");
    assert_eq!(store.claim("Ev1").expect("claim"), Claim::Duplicate);
    assert_eq!(store.claim("Ev2").expect("claim"), Claim::Fresh);
}

/// 🔴 #9923 Q3: a second store on the path is refused while the first lives.
#[test]
fn second_claim_store_on_same_path_is_refused_while_first_lives() {
    let fx = fixture();
    let first = ClaimStore::open_at(&fx.path).expect("open");
    let second = ClaimStore::open_at(&fx.path);
    assert!(
        matches!(
            second,
            Err(ClaimError::Store(QuestionTableError::Locked { .. }))
        ),
        "{second:?}"
    );
    drop(first);
    ClaimStore::open_at(&fx.path).expect("the lock is free once dropped");
}

/// 🔴 #9923 (plan item 6): an append error refuses the claim and poisons
/// the store; the hold's reopen replays what reached the disk.
#[test]
fn claim_append_failure_poisons_until_reopen() {
    let fx = fixture();
    let store =
        ClaimStore::open_with_fault(&fx.path, Utc::now(), ClaimFault::Append).expect("open");
    assert!(store.claim("Ev1").is_err(), "the append failed");
    assert!(store.is_poisoned());
    assert!(matches!(store.claim("Ev2"), Err(ClaimError::Poisoned)));
    drop(store);
    let hold = ClaimHold::default();
    let store = hold.get(&fx.path).expect("reopen");
    assert_eq!(
        store.claim("Ev1").expect("claim"),
        Claim::Fresh,
        "never written"
    );
}

/// 🔴 #9923 Q3: a poisoned store is replaced by its hold's reopen.
#[test]
fn poisoned_claim_store_is_reopened_by_its_hold() {
    let fx = fixture();
    let hold = ClaimHold::default();
    let store = hold.get(&fx.path).expect("open");
    assert_eq!(store.claim("Ev1").expect("claim"), Claim::Fresh);
    // A real append failure poisons it and frees its flock.
    store.state.lock().expect("state").fault = Some(ClaimFault::Append);
    assert!(store.claim("Ev2").is_err(), "the append failed");
    assert!(store.is_poisoned());
    let again = hold.get(&fx.path).expect("reopened");
    assert!(!std::sync::Arc::ptr_eq(&store, &again), "a new store");
    assert_eq!(again.claim("Ev1").expect("claim"), Claim::Duplicate);
}

/// 🔴 #9923 Q3: a log a correct store never writes fails the open.
#[test]
fn corrupt_claim_line_fails_the_open() {
    let fx = fixture();
    let now = Utc::now();
    for text in [
        format!("not json\n{}", claimed_line("Ev1", now)),
        format!("{}{}", claimed_line("Ev1", now), claimed_line("Ev1", now)),
        "{\"v\":1,\"kind\":\"done\",\"event_id\":\"Ev9\",\"at\":\"x\",\"outcome\":\"ok\"}\n".into(),
        "{\"v\":2,\"kind\":\"claimed\",\"event_id\":\"Ev1\",\"at\":\"x\"}\n".into(),
        claimed_line("Ev 1", now),
    ] {
        write_log(&fx.path, &text);
        let opened = ClaimStore::open_at(&fx.path);
        assert!(
            matches!(
                opened,
                Err(ClaimError::Store(QuestionTableError::Corrupt { .. }))
            ),
            "{text:?}: {opened:?}"
        );
    }
}

/// 🔴 #9923: a torn final line is moved to `.torn` and never applies.
#[test]
fn torn_final_claim_line_is_quarantined_and_never_applies() {
    let fx = fixture();
    let now = Utc::now();
    write_log(
        &fx.path,
        &format!(
            "{}{{\"v\":1,\"kind\":\"claimed\",\"ev",
            claimed_line("Ev1", now)
        ),
    );
    let store = ClaimStore::open_at(&fx.path).expect("open");
    assert_eq!(store.claim("Ev1").expect("claim"), Claim::Duplicate);
    let torn = std::fs::read_to_string(torn_path(&fx.path)).expect("torn");
    assert!(torn.contains("\"ev"), "{torn}");
}

/// 🔴 #9923 Q3: an unsealed log, lock, torn or prune file refuses the open.
#[test]
fn unsealed_claim_log_refuses_to_open() {
    let fx = fixture();
    drop(ClaimStore::open_at(&fx.path).expect("create"));
    std::fs::set_permissions(&fx.path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let opened = ClaimStore::open_at(&fx.path);
    assert!(
        matches!(
            opened,
            Err(ClaimError::Store(QuestionTableError::Unsealed { .. }))
        ),
        "{opened:?}"
    );
    std::fs::set_permissions(&fx.path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    for planted in [lock_path(&fx.path), prune_path(&fx.path)] {
        let _ = std::fs::remove_file(&planted);
        std::os::unix::fs::symlink("/etc/hosts", &planted).expect("symlink");
        let opened = ClaimStore::open_at(&fx.path);
        assert!(
            matches!(
                opened,
                Err(ClaimError::Store(QuestionTableError::Unsealed { .. }))
            ),
            "{}: {opened:?}",
            planted.display()
        );
        std::fs::remove_file(&planted).expect("unplant");
    }
}

/// 🔴 #9923 Q5: an open prunes claims past retention and keeps the rest.
#[test]
fn prune_drops_only_expired_claims() {
    let fx = fixture();
    let now = Utc::now();
    let old = now - RETENTION - TimeDelta::hours(1);
    let young = now - RETENTION + TimeDelta::hours(1);
    write_log(
        &fx.path,
        &format!(
            "{}{}",
            claimed_line("EvOld", old),
            claimed_line("EvYoung", young)
        ),
    );
    let store = ClaimStore::open_at_time(&fx.path, now).expect("open");
    assert_eq!(store.claim("EvYoung").expect("claim"), Claim::Duplicate);
    assert_eq!(store.claim("EvOld").expect("claim"), Claim::Fresh, "pruned");
    let mode = std::fs::metadata(&fx.path)
        .expect("meta")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    assert!(!prune_path(&fx.path).exists(), "no prune file left");
}

/// 🔴 #9923 Q5 (Fail-Open Check): a failed prune fails the open and
/// deletes no unexpired claim.
#[test]
fn prune_failure_fails_the_open_and_deletes_no_unexpired_claim() {
    let fx = fixture();
    let now = Utc::now();
    let old = now - RETENTION - TimeDelta::hours(1);
    let text = format!(
        "{}{}",
        claimed_line("EvOld", old),
        claimed_line("EvYoung", now)
    );
    write_log(&fx.path, &text);
    let opened = ClaimStore::open_with_fault(&fx.path, now, ClaimFault::Prune);
    assert!(opened.is_err(), "a failed prune fails the open");
    assert_eq!(
        std::fs::read_to_string(&fx.path).expect("log"),
        text,
        "untouched"
    );
    assert!(!prune_path(&fx.path).exists(), "no prune file left");
    let store = ClaimStore::open_at_time(&fx.path, now).expect("open");
    assert_eq!(store.claim("EvYoung").expect("claim"), Claim::Duplicate);
}

/// 🔴 #9923 Q4: a claim with no `done` is listed for `tm doctor`; a
/// settled one and an in-flight one are not.
#[test]
fn claimed_never_done_is_listed_for_doctor() {
    let fx = fixture();
    let store = ClaimStore::open_at(&fx.path).expect("open");
    store.claim("EvLost").expect("claim");
    store.claim("EvDone").expect("claim");
    store.settle("EvDone", "delivered").expect("settle");
    store
        .settle("EvDone", "delivered")
        .expect("a second settle writes nothing");
    assert!(matches!(
        store.settle("EvNever", "x"),
        Err(ClaimError::InvalidEventId)
    ));
    let later = Utc::now() + TimeDelta::minutes(5);
    let listed = claimed_never_done(&fx.path, later, TimeDelta::minutes(2)).expect("read");
    assert_eq!(listed, ["EvLost"]);
    let now = claimed_never_done(&fx.path, Utc::now(), TimeDelta::minutes(2)).expect("read");
    assert!(now.is_empty(), "an in-flight claim is not listed: {now:?}");
    let text = std::fs::read_to_string(&fx.path).expect("log");
    assert_eq!(text.matches("\"done\"").count(), 1, "{text}");
}

/// 🔴 #9923: an id that is not a plain Slack id is refused and never written.
#[test]
fn invalid_event_id_is_refused_unwritten() {
    let fx = fixture();
    let store = ClaimStore::open_at(&fx.path).expect("open");
    for id in ["", "Ev 1", "Ev\n1", "../Ev", &"E".repeat(65)] {
        assert!(
            matches!(store.claim(id), Err(ClaimError::InvalidEventId)),
            "{id:?}"
        );
    }
    assert_eq!(std::fs::read_to_string(&fx.path).expect("log"), "");
}
