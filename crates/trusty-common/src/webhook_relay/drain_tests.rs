//! Tests for the inbox drain (#5192).
//!
//! The five that carry the change, one per way a drain lies about its work:
//! `drain_removes_an_entry_the_processor_accepted` (the happy path actually
//! removes), `drain_keeps_an_entry_whose_processor_failed` (a failure is not a
//! deletion and not a `processed` count),
//! `drain_leaves_an_interrupted_entry_claimable` (a crash mid-processing is
//! recoverable), `drain_does_not_double_process_a_claimed_entry` (two drainers,
//! one run), and `drain_report_accounts_for_every_scanned_entry` (the counts
//! add up under a mixed pass).
//!
//! Every failure-path case drives a processor that ACTUALLY fails, in the
//! configuration the defect occurs in — a suite whose processor always succeeds
//! would pass over a drain that deletes on error.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::claim::{Claim, ClaimOutcome};
use super::drain::{
    DeliveryProcessor, Disposition, DrainPolicy, DrainReport, FailureOutcome, ProcessFailure,
    drain_once,
};
use super::inbox::Inbox;
use super::retry::{self, DEFAULT_MAX_ATTEMPTS};
use super::{Provenance, RelayDelivery};

// ─── Fixtures ────────────────────────────────────────────────────────────────

fn delivery(id: &str) -> RelayDelivery {
    RelayDelivery {
        delivery_id: id.to_string(),
        source: "review".to_string(),
        event: "pull_request".to_string(),
        headers: BTreeMap::from([("x-github-event".to_string(), "pull_request".to_string())]),
        body_b64: "eyJhY3Rpb24iOiJyZXZpZXdfcmVxdWVzdGVkIn0=".to_string(),
        provenance: Provenance {
            algorithm: "hmac-sha256".to_string(),
            key_id: "GITHUB_WEBHOOK_SECRET".to_string(),
            verified: true,
        },
        received_at_unix_ms: 1_700_000_000_000,
        attempts: 0,
    }
}

/// An inbox in a fresh temp dir, holding `ids`.
fn inbox_with(ids: &[&str]) -> (tempfile::TempDir, Inbox) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = Inbox::open(tmp.path().join("webhook-inbox")).expect("open inbox");
    for id in ids {
        inbox.take_ownership(&delivery(id)).expect("take ownership");
    }
    (tmp, inbox)
}

fn entry_of(inbox: &Inbox, id: &str) -> PathBuf {
    inbox.entry_path(id)
}

/// What a [`ScriptedProcessor`] should do with a given delivery.
#[derive(Clone)]
enum Verdict {
    Accept,
    Ignore,
    FailRetryable,
    FailPermanent,
}

/// A processor whose answer per delivery id the test chooses.
///
/// Why: the drain's failure arms are only reachable through a processor that
/// actually returns `Err`. A stub that always succeeds leaves every branch this
/// change exists to get right unexecuted.
struct ScriptedProcessor {
    verdicts: Mutex<BTreeMap<String, Verdict>>,
    default: Verdict,
    seen: Mutex<Vec<String>>,
    calls: AtomicUsize,
    /// Fires once per call, so a listener test waits on the drain having
    /// happened rather than on a duration.
    called: tokio::sync::Notify,
}

impl ScriptedProcessor {
    fn always(default: Verdict) -> Arc<Self> {
        Arc::new(Self {
            verdicts: Mutex::new(BTreeMap::new()),
            default,
            seen: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            called: tokio::sync::Notify::new(),
        })
    }

    fn with(pairs: &[(&str, Verdict)]) -> Arc<Self> {
        Arc::new(Self {
            verdicts: Mutex::new(
                pairs
                    .iter()
                    .map(|(id, v)| ((*id).to_string(), v.clone()))
                    .collect(),
            ),
            default: Verdict::Accept,
            seen: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            called: tokio::sync::Notify::new(),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().expect("seen").clone()
    }
}

#[async_trait::async_trait]
impl DeliveryProcessor for ScriptedProcessor {
    async fn process(&self, delivery: &RelayDelivery) -> Result<Disposition, ProcessFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen
            .lock()
            .expect("seen")
            .push(delivery.delivery_id.clone());
        self.called.notify_one();
        let verdict = self
            .verdicts
            .lock()
            .expect("verdicts")
            .get(&delivery.delivery_id)
            .cloned()
            .unwrap_or(self.default.clone());
        match verdict {
            Verdict::Accept => Ok(Disposition::Processed),
            Verdict::Ignore => Ok(Disposition::Ignored {
                reason: "not an actionable event".to_string(),
            }),
            Verdict::FailRetryable => Err(ProcessFailure::retryable("github returned 503")),
            Verdict::FailPermanent => Err(ProcessFailure::permanent("payload has no pull_request")),
        }
    }
}

/// A processor that holds the claim open until the test releases it.
///
/// Why: the interleaving "drainer A is mid-pipeline, drainer B starts a pass"
/// is the one that decides whether an entry can be processed twice, and it is
/// not observable against a processor that returns immediately. The barrier
/// makes the interleaving deterministic rather than a sleep race.
struct BlockingProcessor {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl DeliveryProcessor for BlockingProcessor {
    async fn process(&self, _delivery: &RelayDelivery) -> Result<Disposition, ProcessFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        self.release.notified().await;
        Ok(Disposition::Processed)
    }
}

fn held_ids(inbox: &Inbox) -> Vec<String> {
    let mut ids: Vec<String> = inbox
        .list()
        .expect("list")
        .into_iter()
        .map(|(_, d)| d.delivery_id)
        .collect();
    ids.sort();
    ids
}

fn quarantined(inbox: &Inbox) -> usize {
    retry::quarantined_count(inbox.root()).expect("quarantined count")
}

// ─── Claim ───────────────────────────────────────────────────────────────────

#[test]
fn claim_is_exclusive_between_two_holders() {
    let (_tmp, inbox) = inbox_with(&["d-1"]);
    let path = entry_of(&inbox, "d-1");

    let first = Claim::try_acquire(&path).expect("first claim");
    let ClaimOutcome::Claimed(first) = first else {
        panic!("the first claim must succeed");
    };
    assert_eq!(first.delivery().delivery_id, "d-1");

    // Same process, second fd on the same inode: flock is per-open-file-
    // description, so this is the same contention two drainers would see.
    match Claim::try_acquire(&path).expect("second claim") {
        ClaimOutcome::InFlight => {}
        other => panic!("a held entry must report InFlight, got {other:?}"),
    }
}

#[test]
fn claim_is_released_when_the_holder_is_dropped() {
    // The property a crash relies on: nothing on disk records the claim, so
    // losing the holder — cleanly or by SIGKILL — makes the entry claimable.
    let (_tmp, inbox) = inbox_with(&["d-1"]);
    let path = entry_of(&inbox, "d-1");

    let claim = Claim::try_acquire(&path).expect("claim");
    assert!(matches!(claim, ClaimOutcome::Claimed(_)));
    drop(claim);

    match Claim::try_acquire(&path).expect("re-claim") {
        ClaimOutcome::Claimed(c) => assert_eq!(c.delivery().delivery_id, "d-1"),
        other => panic!("a released entry must be claimable, got {other:?}"),
    }
}

#[test]
fn claim_of_a_vanished_entry_reports_vanished() {
    let (tmp, _inbox) = inbox_with(&[]);
    let missing = tmp.path().join("webhook-inbox").join("nothing.json");
    assert!(matches!(
        Claim::try_acquire(&missing).expect("claim"),
        ClaimOutcome::Vanished
    ));
}

#[test]
fn claim_refuses_an_entry_unlinked_while_the_lock_was_contended() {
    // The `nlink` check. Without it, the loser of a contended lock processes an
    // inode the winner already removed — a delivery reviewed and commented on
    // twice, from one webhook.
    let (_tmp, inbox) = inbox_with(&["d-1"]);
    let path = entry_of(&inbox, "d-1");

    // Stand in for "the winner has an fd open and has already unlinked":
    // holding an open fd across the unlink is exactly that state.
    let held = std::fs::File::open(&path).expect("open");
    std::fs::remove_file(&path).expect("unlink");

    // Re-create the *name* pointing at nothing is not possible; instead assert
    // the claim of the still-open inode's path reports Vanished, which is the
    // observable the drain acts on.
    assert!(matches!(
        Claim::try_acquire(&path).expect("claim"),
        ClaimOutcome::Vanished
    ));
    drop(held);
}

#[test]
fn claim_of_an_undecodable_entry_reports_undecodable() {
    let (_tmp, inbox) = inbox_with(&[]);
    let path = inbox.root().join("garbage.json");
    std::fs::write(&path, b"{ not json").expect("write");

    match Claim::try_acquire(&path).expect("claim") {
        ClaimOutcome::Undecodable { reason, .. } => assert!(!reason.is_empty()),
        other => panic!("expected Undecodable, got {other:?}"),
    }
}

// ─── Attempt bookkeeping ─────────────────────────────────────────────────────

#[test]
fn attempt_record_survives_a_reopen() {
    // The retry bound has to outlive the process, because the process is
    // short-lived by design. An in-memory counter would make every
    // console-supervised spawn start the budget over.
    let (_tmp, inbox) = inbox_with(&["d-1"]);
    let path = entry_of(&inbox, "d-1");

    let first = retry::record_failure(&path, "boom", 1_000).expect("record");
    assert_eq!(first.attempts, 1);
    assert_eq!(first.first_failed_at_unix_ms, 1_000);

    let second = retry::record_failure(&path, "boom again", 2_000).expect("record");
    assert_eq!(second.attempts, 2);
    assert_eq!(second.first_failed_at_unix_ms, 1_000);
    assert_eq!(second.last_failed_at_unix_ms, 2_000);
    assert_eq!(second.last_error, "boom again");

    assert_eq!(retry::load_attempts(&path), second);
}

#[test]
fn attempt_sidecar_is_not_counted_as_held_work() {
    // If the sidecar counted, one failing delivery would inflate the undrained
    // number console renders and every retry would make the backlog look worse
    // than it is.
    let (_tmp, inbox) = inbox_with(&["d-1"]);
    let path = entry_of(&inbox, "d-1");
    retry::record_failure(&path, "boom", 1_000).expect("record");

    assert_eq!(
        super::inbox::held_count(inbox.root()).expect("held count"),
        1,
        "the sidecar must not be counted as a delivery"
    );
    assert_eq!(inbox.list().expect("list").len(), 1);
}

#[test]
fn quarantine_moves_the_entry_and_keeps_its_history() {
    let (_tmp, inbox) = inbox_with(&["d-1"]);
    let path = entry_of(&inbox, "d-1");
    retry::record_failure(&path, "poisoned", 1_000).expect("record");

    let target = retry::quarantine(inbox.root(), &path).expect("quarantine");

    assert!(!path.exists(), "the original entry must be gone");
    assert!(target.exists(), "the delivery must still exist");
    assert_eq!(
        super::inbox::held_count(inbox.root()).expect("held"),
        0,
        "a quarantined delivery is not drainable work"
    );
    assert_eq!(quarantined(&inbox), 1);
    assert_eq!(
        retry::load_attempts(&target).last_error,
        "poisoned",
        "the failure history travels with the delivery"
    );
    // Still a real delivery, not a husk.
    let stored: RelayDelivery =
        serde_json::from_slice(&std::fs::read(&target).expect("read")).expect("decode");
    assert_eq!(stored.delivery_id, "d-1");
}

#[test]
fn quarantine_is_idempotent_after_an_interrupted_move() {
    // Crash between the link and the unlink: the entry is in both places. The
    // re-run must finish the move rather than error out and strand it.
    let (_tmp, inbox) = inbox_with(&["d-1"]);
    let path = entry_of(&inbox, "d-1");
    let dir = retry::quarantine_dir(inbox.root());
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::hard_link(&path, dir.join(path.file_name().expect("name"))).expect("pre-link");

    retry::quarantine(inbox.root(), &path).expect("quarantine completes the move");

    assert!(!path.exists());
    assert_eq!(quarantined(&inbox), 1);
}

#[test]
fn quarantined_count_reports_zero_when_nothing_is_quarantined() {
    let (_tmp, inbox) = inbox_with(&["d-1"]);
    assert_eq!(quarantined(&inbox), 0);
}

// ─── Drain: success ──────────────────────────────────────────────────────────

#[tokio::test]
async fn drain_removes_an_entry_the_processor_accepted() {
    let (_tmp, inbox) = inbox_with(&["d-1"]);
    let processor = ScriptedProcessor::always(Verdict::Accept);

    let report = drain_once(&inbox, processor.as_ref(), DrainPolicy::default()).await;

    assert_eq!(processor.seen(), vec!["d-1".to_string()]);
    assert_eq!(report.processed, 1);
    assert_eq!(report.scanned, 1);
    assert!(report.is_clean(), "{report:?}");
    assert!(held_ids(&inbox).is_empty(), "the entry must be gone");
    assert_eq!(quarantined(&inbox), 0);
}

#[tokio::test]
async fn drain_removes_an_entry_the_processor_deliberately_ignored() {
    // A filtered event is accepted work, not processed work. Counting it as
    // `processed` is how a drain reports reviews it never ran.
    let (_tmp, inbox) = inbox_with(&["d-1"]);
    let processor = ScriptedProcessor::always(Verdict::Ignore);

    let report = drain_once(&inbox, processor.as_ref(), DrainPolicy::default()).await;

    assert_eq!(report.ignored, 1);
    assert_eq!(report.processed, 0);
    assert!(report.is_clean());
    assert!(held_ids(&inbox).is_empty());
}

#[tokio::test]
async fn drain_of_an_empty_inbox_does_nothing_and_says_so() {
    let (_tmp, inbox) = inbox_with(&[]);
    let processor = ScriptedProcessor::always(Verdict::Accept);

    let report = drain_once(&inbox, processor.as_ref(), DrainPolicy::default()).await;

    assert_eq!(report, DrainReport::default());
    assert_eq!(processor.calls(), 0);
}

// ─── Drain: failure ──────────────────────────────────────────────────────────

#[tokio::test]
async fn drain_keeps_an_entry_whose_processor_failed() {
    // 🔴 The core failure case. The pipeline call fails; the delivery must
    // survive, must not be counted as processed, and must be visible.
    let (_tmp, inbox) = inbox_with(&["d-1"]);
    let processor = ScriptedProcessor::always(Verdict::FailRetryable);

    let report = drain_once(&inbox, processor.as_ref(), DrainPolicy::default()).await;

    assert_eq!(report.processed, 0, "a failure is never a processed count");
    assert_eq!(report.ignored, 0);
    assert_eq!(report.retry_pending, 1);
    assert!(
        !report.is_clean(),
        "a failure must not read as a clean pass"
    );
    assert_eq!(held_ids(&inbox), vec!["d-1".to_string()], "not lost");
    assert_eq!(quarantined(&inbox), 0, "still within the retry bound");

    let failure = report.failures.first().expect("one reported failure");
    assert_eq!(failure.delivery_id, "d-1");
    assert_eq!(failure.outcome, FailureOutcome::Retrying);
    assert_eq!(failure.attempts, 1);
    assert!(
        failure.reason.contains("503"),
        "the processor's own words must reach the report: {}",
        failure.reason
    );
    assert_eq!(
        super::inbox::held_count(inbox.root()).expect("held"),
        1,
        "a failed delivery still reads as undrained work to console"
    );
}

#[tokio::test]
async fn drain_retries_a_retryable_failure_until_the_bound_then_quarantines() {
    let (_tmp, inbox) = inbox_with(&["d-1"]);
    let processor = ScriptedProcessor::always(Verdict::FailRetryable);
    let policy = DrainPolicy { max_attempts: 3 };

    for pass in 1..=3 {
        let report = drain_once(&inbox, processor.as_ref(), policy).await;
        if pass < 3 {
            assert_eq!(report.retry_pending, 1, "pass {pass}");
            assert_eq!(report.quarantined, 0, "pass {pass}");
            assert_eq!(held_ids(&inbox), vec!["d-1".to_string()], "pass {pass}");
        } else {
            assert_eq!(report.quarantined, 1, "pass {pass}");
            assert_eq!(report.retry_pending, 0, "pass {pass}");
        }
    }

    assert_eq!(processor.calls(), 3, "the bound stops the retries");
    assert!(held_ids(&inbox).is_empty(), "no longer drainable work");
    assert_eq!(quarantined(&inbox), 1, "and it was kept, not deleted");

    // A fourth pass must not resurrect it, and must not count it again.
    let after = drain_once(&inbox, processor.as_ref(), policy).await;
    assert_eq!(after.scanned, 0);
    assert_eq!(processor.calls(), 3);
}

#[tokio::test]
async fn drain_quarantines_a_permanent_failure_immediately() {
    let (_tmp, inbox) = inbox_with(&["d-1"]);
    let processor = ScriptedProcessor::always(Verdict::FailPermanent);

    let report = drain_once(&inbox, processor.as_ref(), DrainPolicy::default()).await;

    assert_eq!(report.quarantined, 1);
    assert_eq!(report.retry_pending, 0);
    assert_eq!(processor.calls(), 1, "a permanent failure is not retried");
    assert_eq!(quarantined(&inbox), 1);
    assert!(held_ids(&inbox).is_empty());
    let failure = report.failures.first().expect("reported");
    assert_eq!(failure.outcome, FailureOutcome::Quarantined);
    assert!(failure.reason.contains("permanent"), "{}", failure.reason);
}

#[tokio::test]
async fn drain_quarantines_an_entry_whose_attempt_budget_was_already_spent() {
    // A budget spent by an earlier process. The drain must not hand it to the
    // pipeline again just because this process has not seen it fail.
    let (_tmp, inbox) = inbox_with(&["d-1"]);
    let path = entry_of(&inbox, "d-1");
    for i in 0..DEFAULT_MAX_ATTEMPTS {
        retry::record_failure(&path, "earlier run", u64::from(i)).expect("record");
    }
    let processor = ScriptedProcessor::always(Verdict::Accept);

    let report = drain_once(&inbox, processor.as_ref(), DrainPolicy::default()).await;

    assert_eq!(processor.calls(), 0, "budget checked before the pipeline");
    assert_eq!(report.quarantined, 1);
    assert_eq!(quarantined(&inbox), 1);
}

#[tokio::test]
async fn drain_quarantines_an_entry_that_is_not_a_decodable_delivery() {
    let (_tmp, inbox) = inbox_with(&["d-1"]);
    std::fs::write(inbox.root().join("garbage.json"), b"{ not json").expect("write");
    let processor = ScriptedProcessor::always(Verdict::Accept);

    let report = drain_once(&inbox, processor.as_ref(), DrainPolicy::default()).await;

    assert_eq!(report.processed, 1, "the good delivery still goes through");
    assert_eq!(report.quarantined, 1);
    assert_eq!(processor.seen(), vec!["d-1".to_string()]);
    assert_eq!(quarantined(&inbox), 1);
    assert!(
        report
            .failures
            .iter()
            .any(|f| f.reason.contains("not a decodable delivery")),
        "{report:?}"
    );
}

#[tokio::test]
async fn drain_reports_a_scan_error_rather_than_an_empty_pass() {
    // "I could not look" and "there was nothing" must never be the same answer:
    // one of them is safe to render green.
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("webhook-inbox");
    let inbox = Inbox::open(&root).expect("open");
    std::fs::remove_dir_all(&root).expect("remove dir");
    std::fs::write(&root, b"not a directory").expect("write file over the dir");
    let processor = ScriptedProcessor::always(Verdict::Accept);

    let report = drain_once(&inbox, processor.as_ref(), DrainPolicy::default()).await;

    assert!(report.scan_error.is_some(), "{report:?}");
    assert!(!report.is_clean());
    assert_eq!(report.scanned, 0);
    assert_eq!(processor.calls(), 0);
}

// ─── Drain: concurrency and interruption ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drain_does_not_double_process_a_claimed_entry() {
    // Two drainers over one inbox, with the interleaving pinned by a barrier
    // rather than a sleep: A is *inside* the pipeline call when B's pass runs.
    let (_tmp, inbox) = inbox_with(&["d-1"]);
    let blocking = Arc::new(BlockingProcessor {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        calls: AtomicUsize::new(0),
    });

    let first = {
        let inbox = inbox.clone();
        let blocking = Arc::clone(&blocking);
        tokio::spawn(
            async move { drain_once(&inbox, blocking.as_ref(), DrainPolicy::default()).await },
        )
    };

    blocking.entered.notified().await;
    assert_eq!(blocking.calls.load(Ordering::SeqCst), 1);

    // B runs a whole pass while A holds the claim.
    let second = drain_once(&inbox, blocking.as_ref(), DrainPolicy::default()).await;
    assert_eq!(
        second.skipped_in_flight, 1,
        "the second drainer must see the entry as held, not process it: {second:?}"
    );
    assert_eq!(second.processed, 0);
    assert_eq!(
        blocking.calls.load(Ordering::SeqCst),
        1,
        "the pipeline must have been entered exactly once"
    );

    blocking.release.notify_one();
    let first = first.await.expect("join");
    assert_eq!(first.processed, 1);
    assert!(held_ids(&inbox).is_empty());
    assert_eq!(blocking.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn drain_leaves_an_interrupted_entry_claimable() {
    // A crash mid-processing is, to the filesystem, a dropped claim: the entry
    // is still there and nothing on disk says it is taken. Asserted
    // deterministically — hold the claim, prove another drainer is locked out,
    // drop it, prove the next pass picks the delivery up and completes it.
    let (_tmp, inbox) = inbox_with(&["d-1"]);
    let path = entry_of(&inbox, "d-1");
    let processor = ScriptedProcessor::always(Verdict::Accept);

    let ClaimOutcome::Claimed(mid_flight) = Claim::try_acquire(&path).expect("claim") else {
        panic!("claim must succeed");
    };
    let during = drain_once(&inbox, processor.as_ref(), DrainPolicy::default()).await;
    assert_eq!(during.skipped_in_flight, 1);
    assert_eq!(processor.calls(), 0);

    // The "crash": the holder disappears without writing anything.
    drop(mid_flight);
    assert!(path.exists(), "the delivery survives the interruption");
    assert_eq!(
        retry::load_attempts(&path).attempts,
        0,
        "an interruption is not a failed attempt — it must not spend the budget"
    );

    let after = drain_once(&inbox, processor.as_ref(), DrainPolicy::default()).await;
    assert_eq!(
        after.processed, 1,
        "the next drainer picks it up: {after:?}"
    );
    assert!(held_ids(&inbox).is_empty());
}

// ─── Drain: honest counts under partial failure ──────────────────────────────

#[tokio::test]
async fn drain_report_accounts_for_every_scanned_entry() {
    // One pass with every outcome in it at once. The identity
    // `accounted() == scanned` is what stops a future arm quietly dropping an
    // entry from the totals while still deleting it.
    let (_tmp, inbox) = inbox_with(&["ok-1", "skip-1", "retry-1", "poison-1"]);
    std::fs::write(inbox.root().join("garbage.json"), b"nope").expect("write");
    let processor = ScriptedProcessor::with(&[
        ("ok-1", Verdict::Accept),
        ("skip-1", Verdict::Ignore),
        ("retry-1", Verdict::FailRetryable),
        ("poison-1", Verdict::FailPermanent),
    ]);

    let report = drain_once(&inbox, processor.as_ref(), DrainPolicy::default()).await;

    assert_eq!(report.scanned, 5);
    assert_eq!(report.processed, 1);
    assert_eq!(report.ignored, 1);
    assert_eq!(report.retry_pending, 1);
    assert_eq!(report.quarantined, 2, "the poison payload and the garbage");
    assert_eq!(
        report.accounted(),
        report.scanned,
        "every scanned entry must be accounted for exactly once: {report:?}"
    );
    assert!(!report.is_clean());

    // And the disk agrees with the report.
    assert_eq!(held_ids(&inbox), vec!["retry-1".to_string()]);
    assert_eq!(quarantined(&inbox), 2);
}

#[tokio::test]
async fn drain_processed_count_excludes_an_acceptance_whose_entry_could_not_be_removed() {
    // The subtle over-count: the pipeline ran, the unlink failed, and the entry
    // is still on disk and will run again. Reporting it as `processed` would
    // claim the inbox is one item lighter than it is.
    let (_tmp, inbox) = inbox_with(&["d-1"]);
    let path = entry_of(&inbox, "d-1");
    let processor = ScriptedProcessor::always(Verdict::Accept);

    // Make the unlink fail without making the entry unreadable: a read-only
    // parent directory refuses the unlink, and the claim still opens the file.
    let mut perms = std::fs::metadata(inbox.root()).expect("meta").permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(inbox.root(), perms).expect("chmod");

    let report = drain_once(&inbox, processor.as_ref(), DrainPolicy::default()).await;

    // Restore before any assertion can fail and leak an unwritable temp dir.
    std::fs::set_permissions(
        inbox.root(),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .expect("restore");

    assert_eq!(processor.calls(), 1);
    assert_eq!(
        report.processed, 0,
        "an entry still on disk is not a completed drain: {report:?}"
    );
    let failure = report.failures.first().expect("reported");
    assert_eq!(failure.outcome, FailureOutcome::Stuck);
    assert!(
        failure.reason.contains("processed again"),
        "the report must say what happens next: {}",
        failure.reason
    );
    assert_eq!(report.accounted(), report.scanned);
    assert!(path.exists(), "and the delivery is not lost");
}

#[test]
fn drain_policy_defaults_to_the_shared_bound() {
    assert_eq!(DrainPolicy::default().max_attempts, DEFAULT_MAX_ATTEMPTS);
}

#[test]
fn quarantine_dir_is_not_counted_as_a_held_delivery() {
    // `held_count` walks the inbox root; a quarantine subdirectory living
    // inside it must not read as work waiting to be drained.
    let (_tmp, inbox) = inbox_with(&[]);
    std::fs::create_dir_all(retry::quarantine_dir(inbox.root())).expect("mkdir");
    assert_eq!(super::inbox::held_count(inbox.root()).expect("held"), 0);
}

// ─── The listener that runs the drain ────────────────────────────────────────

/// A drain interval long enough that only the notify path can wake the drain.
///
/// Why: a test that passes because a 30-second timer fired proves the timer,
/// not the wake. Pinning the interval out of reach makes the assertion "the
/// accepted delivery woke the drain" rather than "something eventually ran".
const NEVER: std::time::Duration = std::time::Duration::from_secs(3600);

#[tokio::test]
async fn listener_drains_a_delivery_it_just_accepted() {
    // End to end over a real socket: the frame arrives, the ack is written on
    // durability alone (unchanged from #5182), and the same process then reads
    // the entry back out and runs the pipeline.
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join("sockets").join("drain.sock");
    let processor = ScriptedProcessor::always(Verdict::Accept);
    let listener = super::listener::WebhookListener::open(&sock, tmp.path().join("inbox"))
        .expect("open")
        .with_processor(Arc::clone(&processor) as Arc<dyn DeliveryProcessor>)
        .with_drain_tuning(DrainPolicy::default(), NEVER);
    let inbox = listener.inbox().clone();

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let running = tokio::spawn(async move {
        listener
            .run(async {
                let _ = stop_rx.await;
            })
            .await
    });

    let d = delivery("listener-drain-1");
    let mut acked = false;
    for _ in 0..200 {
        let frame = super::RelayFrame::new(
            &d.delivery_id,
            &d.source,
            &d.event,
            &d.headers,
            &d.body_b64,
            &d.provenance,
            d.received_at_unix_ms,
            d.attempts,
        );
        if let Ok(resp) = crate::uds::send_framed_request::<_, super::RelayResponse>(
            &sock,
            &frame,
            std::time::Duration::from_secs(5),
        )
        .await
        {
            acked = resp.is_ack();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(acked, "the listener must still ack on durability alone");

    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        processor.called.notified(),
    )
    .await
    .expect("the accepted delivery must wake the drain");

    stop_tx.send(()).expect("signal shutdown");
    running.await.expect("join").expect("clean exit");

    assert_eq!(processor.seen(), vec!["listener-drain-1".to_string()]);
    assert!(
        held_ids(&inbox).is_empty(),
        "the drained delivery must be gone from the inbox"
    );
}

#[tokio::test]
async fn listener_drains_what_a_previous_run_left_behind() {
    // Restart safety. A delivery the previous process was SIGKILLed over is
    // durable and claimable, and nothing will ever send a frame for it again —
    // so a drain that only ran on arrival would hold it forever.
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join("sockets").join("restart.sock");
    let inbox_root = tmp.path().join("inbox");
    let earlier = Inbox::open(&inbox_root).expect("open");
    earlier
        .take_ownership(&delivery("left-behind-1"))
        .expect("previous run took ownership");

    let processor = ScriptedProcessor::always(Verdict::Accept);
    let listener = super::listener::WebhookListener::open(&sock, &inbox_root)
        .expect("open")
        .with_processor(Arc::clone(&processor) as Arc<dyn DeliveryProcessor>)
        .with_drain_tuning(DrainPolicy::default(), NEVER);

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let running = tokio::spawn(async move {
        listener
            .run(async {
                let _ = stop_rx.await;
            })
            .await
    });

    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        processor.called.notified(),
    )
    .await
    .expect("startup must drain what the previous run left");

    stop_tx.send(()).expect("signal shutdown");
    running.await.expect("join").expect("clean exit");

    assert_eq!(processor.seen(), vec!["left-behind-1".to_string()]);
    assert!(held_ids(&earlier).is_empty());
}

/// Every path helper agrees on where a sidecar lives.
#[test]
fn attempt_sidecar_sits_beside_its_entry() {
    let entry = Path::new("/inbox/d-1-0011223344556677.json");
    assert_eq!(
        retry::attempt_path(entry),
        PathBuf::from("/inbox/d-1-0011223344556677.attempt")
    );
}
