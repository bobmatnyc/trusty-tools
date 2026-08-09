//! Reading the inbox back out and driving the pipeline (#5192, ADR-0034 §5).
//!
//! Why: #5182 made the receiver take durable ownership of a delivery before
//! acknowledging it, and acknowledging is what licenses `trusty-console` to
//! delete its only copy. That closed the loss window and opened a different
//! one: the delivery became the receiver's problem and nothing on the receiver
//! read it back. A webhook reached a file and stopped — no review, no analysis,
//! no comment, and a spool that reported empty because every delivery had been
//! successfully accepted by a service that then did nothing with it.
//!
//! What: [`drain_once`] is one full pass over the inbox. It claims each entry
//! exclusively ([`super::Claim`]), hands the delivery to a
//! [`DeliveryProcessor`], and removes the entry ONLY after the processor said
//! it accepted the work. Failures are counted durably and bounded
//! ([`super::retry`]); an entry out of retries is quarantined, never deleted.
//!
//! 🔴 **The four rules this module exists to hold, and where each one lives.**
//!
//! 1. *Nothing is removed before the pipeline accepted it.* The only unlink of
//!    a live delivery is [`super::retry::remove_processed`], reachable from one
//!    arm of [`drain_once`]'s match — the arm behind `Ok(_)` from the processor.
//! 2. *A failure is visible.* Every non-success lands in
//!    [`DrainReport::failures`] with the delivery id and the processor's own
//!    words, and [`DrainReport::log_summary`] raises it at `error!`. There is no
//!    swallowed `Err` and no arm that logs at `debug!` and moves on.
//! 3. *Counts are true under failure.* [`DrainReport::processed`] counts
//!    entries the processor accepted AND that are gone from the inbox. An
//!    acceptance whose removal failed is [`FailureOutcome::Stuck`], counted
//!    nowhere else — the count under-reports success rather than inventing it.
//! 4. *A stuck inbox is not healthy.* Nothing here deletes work to make a
//!    number go down, so [`super::held_count`] and
//!    [`super::quarantined_count`] — which is what `trusty-console` renders —
//!    keep reporting the backlog for exactly as long as it exists.
//!
//! An interrupted pass is safe by construction: the claim is an `flock` the
//! kernel releases on process death, so a delivery a SIGKILLed drainer was
//! mid-way through is claimable by the next one. It may therefore be processed
//! twice, which is the at-least-once contract [`super::RelayParams::attempts`]
//! already states — processors deduplicate on `delivery_id`.
//!
//! Test: `tests.rs` — `drain_*`.

use std::path::{Path, PathBuf};

use super::RelayDelivery;
use super::claim::{Claim, ClaimOutcome};
use super::inbox::{Inbox, InboxError};
use super::retry::{self, DEFAULT_MAX_ATTEMPTS};

/// What a processor did with a delivery it accepted responsibility for.
///
/// Both variants remove the entry, and they are separate so a count of real
/// work is never inflated by deliveries the pipeline deliberately skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// The pipeline ran for this delivery.
    Processed,
    /// The pipeline examined it and there is deliberately nothing to do — the
    /// wrong event type, a filtered action, a PR already reviewed at this SHA.
    Ignored {
        /// Why, for the log. Not an error.
        reason: String,
    },
}

/// Why a processor could not accept a delivery.
///
/// Why: `retryable` is the only thing standing between a transient GitHub 5xx
/// and a poisoned payload, and getting it wrong in either direction is a
/// defect — a permanent error retried forever pins the drain, and a transient
/// error quarantined on the first failure needs a human for something that
/// would have healed itself.
/// Test: `drain_retries_a_retryable_failure`,
/// `drain_quarantines_a_permanent_failure_immediately`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessFailure {
    /// What went wrong, in the processor's own words. Stored durably and
    /// reported.
    pub reason: String,
    /// Whether another attempt could plausibly succeed.
    pub retryable: bool,
}

impl ProcessFailure {
    /// A failure another attempt might survive.
    pub fn retryable(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retryable: true,
        }
    }

    /// A failure no number of attempts will fix. Quarantined on the spot.
    pub fn permanent(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retryable: false,
        }
    }
}

/// Whatever turns a held delivery into the work it represents.
///
/// Why: `trusty-review` runs its review pipeline and `trusty-analyze` fetches,
/// analyses and comments. Neither can live here, and the ordering rule that
/// makes the drain safe must not be reimplemented twice — so the crates supply
/// the pipeline and this module supplies the ordering, exactly as `DeliverySink`
/// splits the receive half.
///
/// What: async, because both pipelines are network-bound. Returning `Ok` MUST
/// mean the work has been done or has been deliberately declined — it is what
/// licenses the entry's removal, and there is no second chance after it.
///
/// Test: `drain_removes_an_entry_the_processor_accepted`,
/// `drain_keeps_an_entry_whose_processor_failed`.
#[async_trait::async_trait]
pub trait DeliveryProcessor: Send + Sync + 'static {
    /// Do the work for one delivery, or say why it could not be done.
    async fn process(&self, delivery: &RelayDelivery) -> Result<Disposition, ProcessFailure>;
}

/// How hard the drain tries before giving up on one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrainPolicy {
    /// Failures after which an entry is quarantined rather than retried.
    pub max_attempts: u32,
}

impl Default for DrainPolicy {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
        }
    }
}

/// What became of one entry that did not succeed.
///
/// Test: `drain_report_accounts_for_every_scanned_entry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureOutcome {
    /// Left in the inbox; the next pass will try again.
    Retrying,
    /// Moved to `<inbox>/quarantine/`. Needs a human.
    Quarantined,
    /// Neither retried cleanly nor quarantined — the drain could not act on it
    /// at all. Includes the case where the pipeline accepted the delivery and
    /// the entry could not then be removed.
    Stuck,
}

/// One entry's failure, as reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainFailure {
    /// Delivery id, or a placeholder when the entry could not be decoded.
    pub delivery_id: String,
    /// Entry the failure is about.
    pub path: PathBuf,
    /// Failures recorded against it so far, this one included.
    pub attempts: u32,
    /// What went wrong.
    pub reason: String,
    /// Where that left the entry.
    pub outcome: FailureOutcome,
}

/// The result of one pass, in numbers that hold under failure.
///
/// 🔴 Every field is a disjoint count of entries seen in this pass, and
/// [`DrainReport::accounted`] must equal [`DrainReport::scanned`]. That
/// identity is asserted by a test precisely so a future arm that forgets to
/// count something cannot silently shrink the totals — the shape of defect
/// where a drain reports `0 dropped` while dropping work.
///
/// Test: `drain_report_accounts_for_every_scanned_entry`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrainReport {
    /// Entries present when the pass started.
    pub scanned: usize,
    /// Accepted by the processor and removed from the inbox.
    pub processed: usize,
    /// Deliberately declined by the processor and removed.
    pub ignored: usize,
    /// Failed, still within the retry bound, still held.
    pub retry_pending: usize,
    /// Moved to quarantine this pass.
    pub quarantined: usize,
    /// Held by another drainer.
    pub skipped_in_flight: usize,
    /// Gone before this pass could claim them — drained by a concurrent pass.
    pub vanished: usize,
    /// Every non-success, with the delivery id and the reason.
    pub failures: Vec<DrainFailure>,
    /// Why the inbox itself could not be listed, if it could not.
    pub scan_error: Option<String>,
}

impl DrainReport {
    /// Entries this pass reached a verdict on. Must equal [`Self::scanned`].
    pub fn accounted(&self) -> usize {
        self.processed
            + self.ignored
            + self.retry_pending
            + self.quarantined
            + self.skipped_in_flight
            + self.vanished
            + self
                .failures
                .iter()
                .filter(|f| f.outcome == FailureOutcome::Stuck)
                .count()
    }

    /// True only when nothing needs an operator's attention.
    ///
    /// `skipped_in_flight` is deliberately not a fault: it means another
    /// drainer has the entry, which is the concurrency control working.
    pub fn is_clean(&self) -> bool {
        self.scan_error.is_none() && self.failures.is_empty() && self.quarantined == 0
    }

    /// Emit the pass at a level that matches what it found.
    ///
    /// Why: rule 2 in the module docs. A drain that reports its failures at
    /// `debug!` has the same operational value as one that swallows them.
    /// Test: not asserted directly; the counts it prints are.
    pub fn log_summary(&self, source: &str) {
        if let Some(e) = &self.scan_error {
            tracing::error!(source, error = %e, "webhook inbox could not be listed; nothing was drained");
            return;
        }
        if self.is_clean() {
            if self.processed > 0 || self.ignored > 0 {
                tracing::info!(
                    source,
                    processed = self.processed,
                    ignored = self.ignored,
                    skipped_in_flight = self.skipped_in_flight,
                    "drained the webhook inbox"
                );
            }
            return;
        }
        for failure in &self.failures {
            tracing::error!(
                source,
                delivery_id = %failure.delivery_id,
                path = %failure.path.display(),
                attempts = failure.attempts,
                outcome = ?failure.outcome,
                reason = %failure.reason,
                "webhook delivery was not processed"
            );
        }
        tracing::error!(
            source,
            scanned = self.scanned,
            processed = self.processed,
            retry_pending = self.retry_pending,
            quarantined = self.quarantined,
            "webhook inbox drain finished with unprocessed deliveries"
        );
    }

    /// Record a quarantine, or the failure to quarantine.
    fn quarantine(&mut self, root: &Path, path: &Path, id: &str, attempts: u32, reason: &str) {
        match retry::quarantine(root, path) {
            Ok(target) => {
                self.quarantined += 1;
                self.failures.push(DrainFailure {
                    delivery_id: id.to_string(),
                    path: target,
                    attempts,
                    reason: reason.to_string(),
                    outcome: FailureOutcome::Quarantined,
                });
            }
            Err(e) => {
                self.failures.push(DrainFailure {
                    delivery_id: id.to_string(),
                    path: path.to_path_buf(),
                    attempts,
                    reason: format!("{reason}; and quarantining it failed: {e}"),
                    outcome: FailureOutcome::Stuck,
                });
            }
        }
    }

    /// Record an accepted delivery, or the failure to remove it afterwards.
    fn accept(&mut self, path: &Path, id: &str, disposition: &Disposition) {
        match retry::remove_processed(path) {
            Ok(()) => match disposition {
                Disposition::Processed => self.processed += 1,
                Disposition::Ignored { reason } => {
                    self.ignored += 1;
                    tracing::info!(delivery_id = %id, reason = %reason, "webhook delivery needed no work");
                }
            },
            // The work happened; the bookkeeping did not. Counting this as
            // `processed` would report an entry gone that is still on disk and
            // will be processed again, so it is reported as what it is.
            Err(e) => self.failures.push(DrainFailure {
                delivery_id: id.to_string(),
                path: path.to_path_buf(),
                attempts: 0,
                reason: format!(
                    "the pipeline accepted this delivery but its inbox entry could not be \
                     removed, so it will be processed again: {e}"
                ),
                outcome: FailureOutcome::Stuck,
            }),
        }
    }
}

/// Drain every claimable entry in `inbox` through `processor`, once.
///
/// Why: one bounded pass rather than an internal loop, so the caller owns the
/// schedule and a test owns the clock. See the module docs for the four rules
/// this upholds.
///
/// What: lists the inbox oldest-first by mtime (order is a courtesy, not a
/// correctness requirement — every entry is independent), then for each entry
/// claims, checks the retry budget, processes, and disposes. An entry another
/// drainer holds is skipped immediately rather than waited on.
///
/// Blocking filesystem work runs inline. Each operation is a small local-file
/// syscall against a `0700` directory, and the caller is a dedicated drain task
/// in a short-lived process — not a shared runtime worker serving connections.
///
/// Never returns `Err`: a pass that could not even list the inbox reports
/// [`DrainReport::scan_error`], because a drain that returns early with an
/// error and no report is a drain whose failure has no counts attached to it.
///
/// Test: `drain_removes_an_entry_the_processor_accepted`,
/// `drain_keeps_an_entry_whose_processor_failed`,
/// `drain_quarantines_an_entry_that_is_out_of_retries`,
/// `drain_quarantines_a_permanent_failure_immediately`,
/// `drain_does_not_double_process_a_claimed_entry`,
/// `drain_report_accounts_for_every_scanned_entry`,
/// `drain_reports_a_scan_error_rather_than_an_empty_pass`.
pub async fn drain_once(
    inbox: &Inbox,
    processor: &dyn DeliveryProcessor,
    policy: DrainPolicy,
) -> DrainReport {
    let root = inbox.root().to_path_buf();
    let mut report = DrainReport::default();

    let entries = match candidate_entries(&root) {
        Ok(entries) => entries,
        Err(e) => {
            report.scan_error = Some(format!("{e}"));
            return report;
        }
    };
    report.scanned = entries.len();

    for path in entries {
        let claim = match Claim::try_acquire(&path) {
            Ok(ClaimOutcome::Claimed(claim)) => claim,
            Ok(ClaimOutcome::InFlight) => {
                report.skipped_in_flight += 1;
                continue;
            }
            Ok(ClaimOutcome::Vanished) => {
                report.vanished += 1;
                continue;
            }
            // No processor can ever accept these, so retrying is a loop with no
            // exit. Quarantine keeps the bytes for an operator to look at.
            Ok(ClaimOutcome::Undecodable { path, reason }) => {
                report.quarantine(
                    &root,
                    &path,
                    "<undecodable>",
                    0,
                    &format!("inbox entry is not a decodable delivery: {reason}"),
                );
                continue;
            }
            Err(e) => {
                report.failures.push(DrainFailure {
                    delivery_id: "<unclaimed>".to_string(),
                    path,
                    attempts: 0,
                    reason: format!("{e}"),
                    outcome: FailureOutcome::Stuck,
                });
                continue;
            }
        };

        let id = claim.delivery().delivery_id.clone();
        let already = retry::load_attempts(&path);
        if already.attempts >= policy.max_attempts {
            report.quarantine(
                &root,
                &path,
                &id,
                already.attempts,
                &format!(
                    "out of retries after {} failed attempts; last error: {}",
                    already.attempts, already.last_error
                ),
            );
            continue;
        }

        match processor.process(claim.delivery()).await {
            Ok(disposition) => report.accept(&path, &id, &disposition),
            Err(failure) => {
                let record = match retry::record_failure(&path, &failure.reason, now_unix_ms()) {
                    Ok(record) => record,
                    // The bound cannot be enforced, and an unbounded retry is
                    // the failure this whole path exists to avoid.
                    Err(e) => {
                        report.quarantine(
                            &root,
                            &path,
                            &id,
                            0,
                            &format!(
                                "{}; and the attempt record could not be written, so the retry \
                                 bound cannot be enforced: {e}",
                                failure.reason
                            ),
                        );
                        continue;
                    }
                };

                if !failure.retryable || record.attempts >= policy.max_attempts {
                    let why = if failure.retryable {
                        format!(
                            "{} (attempt {} of {})",
                            failure.reason, record.attempts, policy.max_attempts
                        )
                    } else {
                        format!("{} (permanent; not retried)", failure.reason)
                    };
                    report.quarantine(&root, &path, &id, record.attempts, &why);
                } else {
                    report.retry_pending += 1;
                    report.failures.push(DrainFailure {
                        delivery_id: id,
                        path: path.clone(),
                        attempts: record.attempts,
                        reason: failure.reason,
                        outcome: FailureOutcome::Retrying,
                    });
                }
            }
        }
    }

    report
}

/// Every `*.json` directly in `root`, oldest mtime first.
///
/// Deliberately does NOT decode: a decode here would race a concurrent drainer
/// and would have to guess what an undecodable file means. Both questions
/// belong to [`Claim::try_acquire`], under the lock.
fn candidate_entries(root: &Path) -> Result<Vec<PathBuf>, InboxError> {
    let read = match std::fs::read_dir(root) {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(InboxError::Read {
                path: root.to_path_buf(),
                source,
            });
        }
    };
    let mut entries: Vec<(std::time::SystemTime, PathBuf)> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .map(|p| {
            let mtime = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (mtime, p)
        })
        .collect();
    entries.sort();
    Ok(entries.into_iter().map(|(_, p)| p).collect())
}

/// Wall clock in milliseconds, or 0 if the clock is before the epoch.
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
