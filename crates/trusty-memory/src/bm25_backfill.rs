//! Lossless BM25 backfill for a palace's existing drawers.
//!
//! Why: the BM25 lane indexes a drawer at write time, so every drawer written
//! before the lane was switched on is invisible to it — which is every drawer
//! on this host, since `TRUSTY_BM25_DAEMON=1` has never been set in any shipped
//! path. Turning the lane on without a backfill would produce a palace that
//! answers lexical queries from whatever it happened to see since the last
//! restart, and reports that as a normal empty result.
//!
//! Why NOT the existing write path: `tools::bm25::bm25_index_enqueue` writes
//! into a 256-slot bounded channel with `try_send` and DROPS on full. That is
//! a defensible trade for the write path — a dropped index op costs one stale
//! entry, the drawer itself is durable in redb, and `memory_remember` must not
//! wait on daemon RTT. It is not defensible for a backfill: the largest palace
//! on this host holds 1311 drawers, five times the queue, so a backfill routed
//! through it would silently drop roughly 80% of the corpus and leave the
//! palace answering from a fifth of its content — indistinguishable, from the
//! outside, from working fusion. So the write path is left exactly as it is
//! and backfill gets its own feeder: one document at a time, each awaiting the
//! daemon's ack, so the only backpressure mechanism in play is "wait for the
//! previous write to land". Nothing can be dropped because nothing is ever
//! offered to a full queue.
//!
//! Coverage is established by IDENTITY, never by counting. `stats.doc_count`
//! is a count over the daemon's whole corpus, and trusty-memory issues no BM25
//! `delete` on the forget path (#5053), so the corpus accumulates documents
//! for drawers the palace no longer has. Once those stale documents outnumber
//! the ones still missing, `doc_count >= drawer_count` is satisfied by a
//! corpus that shares no ids with the palace at all. Every coverage decision
//! here — the pre-flight skip, the post-run verdict, and
//! [`BackfillReport::fully_indexed`] — therefore goes through
//! `Bm25Client::missing_docs`, which answers about the exact set of drawer ids
//! being asked about. A coverage question that could not be asked reports
//! `None`, never `covered`.
//!
//! What: [`backfill_palace`] drives the feeder against a socket; [`palace_docs`]
//! extracts the `(drawer_id, text)` pairs; [`backfill_state_palace`] wires both
//! to an [`AppState`] and its spawn supervisor; [`spawn_startup_backfill`]
//! sweeps every palace that has drawers, serially, when the lane is enabled.
//! Idempotent throughout — the daemon's `upsert_document` is keyed by `doc_id`,
//! so a re-run overwrites rather than duplicating.
//!
//! Repair after a drop is handled by [`crate::bm25_repair`], which consumes the
//! dirty flags `bm25_index_enqueue` sets when it drops on a full queue.
//!
//! Fail-open: every failure mode degrades to a reported status, never an error
//! that propagates into a caller's request path and never an unbounded wait.
//! Daemon absent, spawn refused, RPC timeout, and per-document errors are all
//! counted and surfaced in the [`BackfillReport`].
//!
//! Test: `bm25_backfill_tests.rs` (unit) and `tests/bm25_backfill_e2e.rs`
//! (`#[ignore]`d, drives a real `trusty-bm25-daemon`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use trusty_common::bm25_client::{is_method_not_found, Bm25Client};
use trusty_common::memory_core::palace::Drawer;
use trusty_common::memory_core::retrieval::PalaceHandle;

use crate::AppState;

/// Per-document RPC deadline.
///
/// Why: the daemon coalesces writes on a 50 ms window and acks each one, so a
/// healthy round trip is sub-millisecond. Ten seconds is four orders of
/// magnitude of slack — long enough that a loaded host never trips it, short
/// enough that a wedged daemon costs one document's delay rather than stalling
/// the sweep behind it forever. A backfill that can hang is a backfill that can
/// hold a startup task open indefinitely.
/// What: 10 seconds, applied per `index` / `missing_docs` call.
/// Test: `backfill_reports_daemon_unavailable_when_socket_is_dead`.
const OP_TIMEOUT: Duration = Duration::from_secs(10);

/// Whole-palace time budget.
///
/// Why: the per-op timeout bounds one document; this bounds the run. 2300
/// documents across every palace on this host is single-digit MB of text and
/// completes in seconds, so a run still going after two minutes is not slow,
/// it is stuck — and reporting `Partial` with a count beats blocking.
/// What: 120 seconds. On expiry the feeder stops and reports what landed.
/// Test: covered by construction; the counters make a truncated run visible.
const PALACE_BUDGET: Duration = Duration::from_secs(120);

/// How many doc ids one `missing_docs` request carries.
///
/// Why: the wire framing is one newline-terminated JSON line per request, so
/// a 1311-id palace would otherwise build a single ~50 KB frame. Chunking
/// bounds the frame and the daemon's per-request allocation without changing
/// the answer — coverage is the union of the chunks' missing sets.
/// What: 256 ids per request.
/// Test: `coverage_probe_chunks_large_id_sets` (e2e).
const COVERAGE_CHUNK: usize = 256;

/// Environment opt-out for the startup sweep.
///
/// Why: an operator who wants the lane on but the backfill deferred (a large
/// cold palace on a busy host) needs a way to say so that does not also
/// disable the lane. Without it the only lever is `TRUSTY_BM25_DAEMON=0`,
/// which turns off the thing they were trying to keep.
/// What: `TRUSTY_BM25_NO_BACKFILL=1` skips the sweep. Explicit per-palace
/// calls to [`backfill_state_palace`] still work.
/// Test: `startup_backfill_respects_the_opt_out`.
pub const ENV_NO_BACKFILL: &str = "TRUSTY_BM25_NO_BACKFILL";

/// Outcome class of one palace's backfill.
///
/// Why: a caller (and an operator reading logs) needs to tell "nothing to do"
/// from "could not do it" from "did it, partially". Collapsing those into a
/// bool is how a partially-indexed palace comes to look finished.
/// What: five terminal states, all advisory — the load-bearing claim is
/// [`BackfillReport::fully_indexed`], which no status can satisfy on its own.
/// Test: `bm25_backfill_tests.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackfillStatus {
    /// The BM25 lane is switched off — nothing was attempted.
    Disabled,
    /// The daemon could not be reached or started. Recall degrades to
    /// vector-only; a later run can repair.
    DaemonUnavailable,
    /// The daemon was already verified to hold a document for every drawer.
    AlreadyIndexed,
    /// Every drawer was submitted and acked.
    Completed,
    /// Some drawers failed, the time budget expired, or coverage could not be
    /// verified. The index may be usable but is not known complete; a re-run
    /// repairs it.
    Partial,
}

/// What one backfill run did.
///
/// Why: the counters are the evidence that separates "the lane is on" from
/// "the lane has content". `missing_after` in particular is read back from the
/// daemon BY DRAWER ID rather than inferred from the submissions or from a
/// corpus count, so a run that acked 1311 documents into a daemon holding a
/// different 1311 reports the discrepancy instead of claiming success.
/// What: plain owned counters. `missing_after` is `None` when the post-run
/// coverage probe itself failed — which reads as "not covered", never as
/// "covered".
/// Test: `bm25_backfill_tests.rs`.
#[derive(Debug, Clone)]
pub struct BackfillReport {
    pub palace: String,
    pub status: BackfillStatus,
    /// Drawers the palace holds, including the blank ones.
    pub drawers_total: usize,
    /// Drawers with no indexable text, skipped without being submitted.
    pub skipped_empty: usize,
    /// Documents the daemon acked.
    pub indexed: usize,
    /// Documents that errored or timed out.
    pub failed: usize,
    /// How many of this palace's drawer ids the daemon still does NOT hold,
    /// asked by id after the run. `None` when the question could not be
    /// answered at all.
    pub missing_after: Option<usize>,
    /// `doc_count` the daemon reported after the run — observability only.
    /// Larger than `drawers_total - skipped_empty` means stale documents for
    /// drawers this palace no longer has (#5053). Never a coverage signal.
    pub final_doc_count: Option<usize>,
    pub elapsed_ms: u64,
}

impl BackfillReport {
    /// Terminal report for a run that never started.
    ///
    /// Why: a run that never started has, by construction, verified nothing —
    /// so `missing_after` starts as `None` and only the genuinely-empty case
    /// overrides it.
    /// What: zeroed counters with `missing_after: None`.
    /// Test: `short_circuit_reports_are_never_covered`.
    fn short_circuit(palace: &str, status: BackfillStatus, drawers_total: usize) -> Self {
        Self {
            palace: palace.to_string(),
            status,
            drawers_total,
            skipped_empty: 0,
            indexed: 0,
            failed: 0,
            missing_after: None,
            final_doc_count: None,
            elapsed_ms: 0,
        }
    }

    /// True when the daemon is known to hold a document for every drawer.
    ///
    /// Why: this is the question the whole module exists to answer, and it is
    /// the alarm the design rests on — so it must be a SET statement, not an
    /// arithmetic one. The previous version compared the daemon's `doc_count`
    /// against the palace's drawer count; because trusty-memory issues no BM25
    /// `delete`, stale documents accumulate and that comparison eventually
    /// returns `true` over a palace the daemon has never indexed.
    /// What: `true` only when a post-run probe asked the daemon about every one
    /// of this palace's drawer ids and it named none as missing. A probe that
    /// failed, timed out, or was never run leaves `None` and reports `false`.
    /// No status alone can satisfy it.
    /// Test: `fully_indexed_requires_a_verified_empty_missing_set`.
    pub fn fully_indexed(&self) -> bool {
        self.missing_after == Some(0)
    }

    /// Stale documents the daemon holds beyond this palace's indexable drawers.
    ///
    /// Why: this is the count whose growth broke the old predicate, and an
    /// operator should be able to see it climbing rather than discover it when
    /// coverage starts lying. Reported, never acted on.
    /// What: `final_doc_count` minus the indexable drawer count, floored at
    /// zero; `None` when the count was not read.
    /// Test: `stale_doc_estimate_is_reported_not_acted_on`.
    pub fn stale_doc_estimate(&self) -> Option<usize> {
        let indexable = self.drawers_total.saturating_sub(self.skipped_empty);
        self.final_doc_count.map(|n| n.saturating_sub(indexable))
    }
}

/// A palace's drawers, split into what is worth indexing and what is not.
///
/// Why: the two numbers a report needs — how many drawers exist and how many
/// carry no indexable text — can only be taken together, under one read of the
/// drawer lock. Deriving `skipped_empty` at a second call site is how the field
/// ended up permanently zero and the `drawers_total` doc ended up wrong.
/// What: `docs` is the `(doc_id, text)` pairs to submit; `skipped_empty` is how
/// many drawers were dropped for having no non-whitespace content.
/// Test: `docs_from_drawers_splits_blank_from_indexable`.
#[derive(Debug, Clone, Default)]
pub struct PalaceDocs {
    pub docs: Vec<(String, String)>,
    pub skipped_empty: usize,
}

impl PalaceDocs {
    /// Every drawer the palace holds, indexable or not.
    pub fn drawers_total(&self) -> usize {
        self.docs.len() + self.skipped_empty
    }

    /// Build from `(doc_id, text)` pairs that are already known indexable.
    ///
    /// Why: tests and callers driving the feeder directly have pairs, not a
    /// palace, and should not have to fabricate a drawer table to use it.
    /// What: takes the pairs verbatim with `skipped_empty: 0`.
    /// Test: used throughout `tests/bm25_backfill_e2e.rs`.
    pub fn from_pairs(docs: Vec<(String, String)>) -> Self {
        Self {
            docs,
            skipped_empty: 0,
        }
    }
}

/// Extract the `(doc_id, text)` pairs a palace should have indexed.
///
/// Why: the drawer table is behind a `parking_lot` lock, which must not be held
/// across an `.await`. Materialising the pairs up front costs one clone of
/// single-digit MB of text — the entire corpus across ~99 palaces is that
/// size — and removes the lock from the async path entirely.
/// What: one read of the lock, delegating the split to [`docs_from_drawers`].
/// Test: `docs_from_drawers_splits_blank_from_indexable`.
pub fn palace_docs(handle: &PalaceHandle) -> PalaceDocs {
    let drawers = handle.drawers.read();
    docs_from_drawers(&drawers)
}

/// Split a drawer slice into indexable pairs and a blank count.
///
/// Why: separated from the lock so the filter — the load-bearing part — can be
/// exercised directly rather than restated in a test.
/// What: clones `(id.to_string(), content)` for every drawer whose content has
/// non-whitespace text, and counts the rest. Empty drawers are omitted because
/// indexing zero tokens can never produce a hit, so submitting them would only
/// inflate the daemon's corpus.
/// Test: `docs_from_drawers_splits_blank_from_indexable`.
pub fn docs_from_drawers(drawers: &[Drawer]) -> PalaceDocs {
    let mut docs = Vec::with_capacity(drawers.len());
    let mut skipped_empty = 0usize;
    for d in drawers {
        if d.content.trim().is_empty() {
            skipped_empty += 1;
        } else {
            docs.push((d.id.to_string(), d.content.clone()));
        }
    }
    PalaceDocs {
        docs,
        skipped_empty,
    }
}

/// Outcome of asking the daemon which drawer ids it is missing.
///
/// Why: three answers, three different actions. "None missing" is the only one
/// that may skip work or claim coverage; the other two must not be collapsed
/// into it, and must not be collapsed into each other either — an outdated
/// daemon and an absent one send an operator after different problems.
/// What: `Missing(n)` carries a verified count; the other two carry no claim.
/// Test: `coverage_probe_classifies_the_three_failure_modes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Coverage {
    /// The daemon answered: this many of the ids asked about are absent.
    Missing(usize),
    /// The daemon answered `-32601` — it predates the `missing_docs` op.
    Unsupported,
    /// The daemon could not be reached, or did not answer in time.
    Unreachable,
}

/// Ask the daemon which of `ids` it does not hold, in bounded chunks.
///
/// Why: the coverage claim must never be inferred. This is the only place that
/// produces one, so every caller — pre-flight skip, post-run verdict — reaches
/// the same answer through the same failure classification.
/// What: chunks by [`COVERAGE_CHUNK`] under [`OP_TIMEOUT`] each and sums the
/// missing sets. Any chunk failing aborts with the corresponding non-answer;
/// a partial sum is not a coverage answer.
/// Test: `coverage_probe_classifies_the_three_failure_modes`.
async fn probe_coverage(client: &Bm25Client, palace: &str, ids: &[String]) -> Coverage {
    let mut missing = 0usize;
    for chunk in ids.chunks(COVERAGE_CHUNK) {
        match tokio::time::timeout(OP_TIMEOUT, client.missing_docs(chunk)).await {
            Ok(Ok(cov)) => missing += cov.missing.len(),
            Ok(Err(e)) if is_method_not_found(&e) => {
                tracing::error!(
                    palace = %palace,
                    "bm25 backfill: daemon does not implement `missing_docs` — it predates \
                     0.2.0. Coverage CANNOT be verified for this palace; upgrade the daemon. \
                     Reporting the palace as incomplete rather than guessing: {e:#}"
                );
                return Coverage::Unsupported;
            }
            Ok(Err(e)) => {
                tracing::warn!(palace = %palace, "bm25 backfill: coverage probe failed: {e:#}");
                return Coverage::Unreachable;
            }
            Err(_) => {
                tracing::warn!(palace = %palace, "bm25 backfill: coverage probe timed out");
                return Coverage::Unreachable;
            }
        }
    }
    Coverage::Missing(missing)
}

/// Feed a palace's documents to a BM25 daemon, losslessly.
///
/// Why: see the module doc — the live write path drops on a full queue, which
/// is wrong for a corpus five times the queue's depth. This feeder cannot drop
/// because it never offers work to a queue it has not been invited into: each
/// `index` call awaits the daemon's ack, and the daemon's own intake channel
/// applies real backpressure (`send().await`) behind that ack.
/// What: submits `docs` one at a time under [`OP_TIMEOUT`], stopping early if
/// [`PALACE_BUDGET`] expires. Skips the run only when a pre-flight probe named
/// zero missing drawer ids — a verified set statement, not a count comparison
/// — unless `force`. Probes again afterwards so the report's coverage claim is
/// the daemon's own answer about this palace's ids.
/// Failure handling is deliberately asymmetric: a failed pre-flight probe
/// proceeds with the full run (doing redundant work is safe; skipping work we
/// cannot prove is done is not), while a failed post-run probe leaves
/// `missing_after` as `None` so `fully_indexed` reports `false`.
/// Test: `tests/bm25_backfill_e2e.rs::backfill_indexes_every_drawer_without_drops`.
pub async fn backfill_palace(
    socket: &Path,
    palace: &str,
    palace_docs: PalaceDocs,
    force: bool,
) -> BackfillReport {
    let started = Instant::now();
    let client = Bm25Client::new(socket.to_path_buf());
    let PalaceDocs {
        docs,
        skipped_empty,
    } = palace_docs;
    let drawers_total = docs.len() + skipped_empty;
    let total = docs.len();

    if total == 0 {
        // Nothing indexable means nothing can be missing — the one case where
        // coverage is established without asking the daemon anything.
        let mut report =
            BackfillReport::short_circuit(palace, BackfillStatus::AlreadyIndexed, drawers_total);
        report.skipped_empty = skipped_empty;
        report.missing_after = Some(0);
        return report;
    }

    let ids: Vec<String> = docs.iter().map(|(id, _)| id.clone()).collect();

    // Pre-flight. A failure here is NOT a reason to skip — it is a reason to
    // do the work, because we cannot show the work is already done.
    if !force {
        match probe_coverage(&client, palace, &ids).await {
            Coverage::Missing(0) => {
                tracing::debug!(
                    palace = %palace,
                    drawers = total,
                    "bm25 backfill: every drawer id already present — skipping"
                );
                let mut report = BackfillReport::short_circuit(
                    palace,
                    BackfillStatus::AlreadyIndexed,
                    drawers_total,
                );
                report.skipped_empty = skipped_empty;
                report.missing_after = Some(0);
                report.final_doc_count = read_doc_count(&client, palace).await;
                report.elapsed_ms = started.elapsed().as_millis() as u64;
                log_stale_docs(&report);
                return report;
            }
            Coverage::Missing(n) => tracing::info!(
                palace = %palace,
                missing = n,
                drawers = total,
                "bm25 backfill: palace under-indexed — running"
            ),
            // Fall through and do the work. The post-run probe will hit the
            // same wall and leave `missing_after: None`, so the palace reports
            // as incomplete rather than as silently complete.
            Coverage::Unsupported => {}
            // The daemon is unreachable. Report it rather than spending the
            // whole budget discovering the same thing 1311 more times.
            Coverage::Unreachable => {
                let mut report = BackfillReport::short_circuit(
                    palace,
                    BackfillStatus::DaemonUnavailable,
                    drawers_total,
                );
                report.skipped_empty = skipped_empty;
                return report;
            }
        }
    }

    let deadline = started + PALACE_BUDGET;
    let mut indexed = 0usize;
    let mut failed = 0usize;
    let mut truncated = false;

    for (doc_id, text) in &docs {
        if Instant::now() >= deadline {
            tracing::warn!(
                palace = %palace,
                indexed,
                remaining = total - indexed - failed,
                "bm25 backfill: time budget expired — reporting partial coverage"
            );
            truncated = true;
            break;
        }
        match tokio::time::timeout(OP_TIMEOUT, client.index(doc_id, text)).await {
            Ok(Ok(())) => indexed += 1,
            Ok(Err(e)) => {
                failed += 1;
                tracing::warn!(palace = %palace, doc_id = %doc_id, "bm25 backfill index failed: {e:#}");
            }
            Err(_) => {
                failed += 1;
                tracing::warn!(palace = %palace, doc_id = %doc_id, "bm25 backfill index timed out");
            }
        }
    }

    // Read the coverage back from the daemon BY ID. Our own ack count is what
    // we believe happened; this is what the daemon says it holds.
    let missing_after = match probe_coverage(&client, palace, &ids).await {
        Coverage::Missing(n) => Some(n),
        Coverage::Unsupported | Coverage::Unreachable => None,
    };

    let status = if missing_after == Some(0) && failed == 0 && !truncated {
        BackfillStatus::Completed
    } else {
        BackfillStatus::Partial
    };
    let report = BackfillReport {
        palace: palace.to_string(),
        status,
        drawers_total,
        skipped_empty,
        indexed,
        failed,
        missing_after,
        final_doc_count: read_doc_count(&client, palace).await,
        elapsed_ms: started.elapsed().as_millis() as u64,
    };
    if !report.fully_indexed() {
        tracing::error!(
            palace = %palace,
            ?status,
            indexed,
            failed,
            missing_after = ?missing_after,
            "bm25 backfill did NOT establish coverage — this palace answers lexical \
             queries from a partial corpus"
        );
    } else {
        tracing::info!(
            palace = %palace,
            indexed,
            elapsed_ms = report.elapsed_ms,
            "bm25 backfill finished — coverage verified by drawer id"
        );
    }
    log_stale_docs(&report);
    report
}

/// Read the daemon's corpus size for the log line. Never a coverage signal.
async fn read_doc_count(client: &Bm25Client, palace: &str) -> Option<usize> {
    match tokio::time::timeout(OP_TIMEOUT, client.stats()).await {
        Ok(Ok(stats)) => Some(stats.doc_count),
        Ok(Err(e)) => {
            tracing::debug!(palace = %palace, "bm25 backfill: stats read failed: {e:#}");
            None
        }
        Err(_) => None,
    }
}

/// Surface documents the daemon holds for drawers the palace no longer has.
///
/// Why: this drift is the disease the old count-based predicate died of, and
/// it is invisible unless something says so out loud (#5053).
/// What: one `warn!` when the estimate is positive. Advisory only.
/// Test: `stale_doc_estimate_is_reported_not_acted_on`.
fn log_stale_docs(report: &BackfillReport) {
    if let Some(stale) = report.stale_doc_estimate().filter(|n| *n > 0) {
        tracing::warn!(
            palace = %report.palace,
            stale,
            doc_count = ?report.final_doc_count,
            "bm25 daemon holds documents for drawers this palace no longer has — \
             stale lexical hits are possible (see #5053)"
        );
    }
}

/// Resolve a palace's daemon socket, starting the daemon if needed.
///
/// Why: [`backfill_palace`] takes a socket rather than an `AppState` so it can
/// be tested against a bare daemon. This is the adapter that turns "a palace
/// id" into "a socket that is being served", and it is also the point at which
/// the supervisor's daemon cap applies — a sweep across many palaces spawns
/// them one at a time and the cap reaps the ones that fall out of the window.
/// What: `None` when the lane is off or the supervisor could not start a
/// daemon. Uses the per-palace socket the supervisor returns, NOT
/// `state.bm25_client`, which is bound to the default palace's socket only.
/// Test: `backfill_state_palace_is_disabled_without_a_client`.
async fn socket_for_palace(state: &AppState, palace: &str) -> Option<PathBuf> {
    state.bm25_client.as_ref()?;
    let supervisor = state.bm25_supervisor.as_ref()?;
    let data_dir = state.data_root.join(palace).join("bm25");
    match supervisor.ensure_running(palace, &data_dir).await {
        Ok(socket) => Some(socket),
        Err(e) => {
            tracing::warn!(palace = %palace, "bm25 backfill: could not start daemon: {e:#}");
            None
        }
    }
}

/// Backfill one palace through the daemon's spawn supervisor.
///
/// Why: the entry point callers actually use. Keeping the lane check here
/// means every caller degrades identically when the lane is off, instead of
/// each remembering to test `bm25_client.is_some()` first.
/// What: returns [`BackfillStatus::Disabled`] when the lane is off and
/// [`BackfillStatus::DaemonUnavailable`] when the daemon will not start —
/// neither is an error, because neither should fail a caller's request, and
/// neither reports coverage.
/// Test: `backfill_state_palace_is_disabled_without_a_client`.
pub async fn backfill_state_palace(
    state: &AppState,
    handle: &PalaceHandle,
    palace: &str,
    force: bool,
) -> BackfillReport {
    if state.bm25_client.is_none() {
        return BackfillReport::short_circuit(palace, BackfillStatus::Disabled, 0);
    }
    let docs = palace_docs(handle);
    let drawers_total = docs.drawers_total();
    let Some(socket) = socket_for_palace(state, palace).await else {
        let mut report =
            BackfillReport::short_circuit(palace, BackfillStatus::DaemonUnavailable, drawers_total);
        report.skipped_empty = docs.skipped_empty;
        return report;
    };
    backfill_palace(&socket, palace, docs, force).await
}

/// Whether the startup sweep is switched off by [`ENV_NO_BACKFILL`].
///
/// Why: the guard has to be reachable from a test without spawning the sweep,
/// otherwise the test restates the comparison instead of exercising it — which
/// is how it came to pass against a deleted implementation.
/// What: exact `"1"`, matching every other trusty-* flag; anything else, and an
/// unset var, leave the sweep enabled.
/// Test: `startup_backfill_respects_the_opt_out`.
pub fn startup_backfill_opted_out() -> bool {
    std::env::var(ENV_NO_BACKFILL).as_deref() == Ok("1")
}

/// What one startup sweep considered and what it could not verify.
///
/// Why: the sweep's own log line is a coverage claim, and a claim built from
/// counters that never saw a palace is the same fail-open one layer out. This
/// carries `enumerated` — palaces found ON DISK — so "all coverage verified"
/// is a statement about the whole corpus rather than about whatever subset the
/// sweep happened to look at.
/// What: `enumerated` is `Some(n)` palaces found on disk, or `None` when the
/// enumeration itself failed; `swept` is those with drawers that were actually
/// backfilled; `incomplete` is those left without verified coverage;
/// `unopenable` is those that could not be hydrated at all.
///
/// `enumerated` is an `Option` for the same reason `BackfillReport::
/// missing_after` is: a sweep that could not enumerate has zero incomplete
/// palaces only because it examined none, and a plain `usize` would let
/// `all_verified()` read `true` off exactly that. Encoding "did not ask" in the
/// type makes the fail-open unrepresentable rather than merely avoided.
/// Test: `startup_sweep_enumerates_every_palace_on_disk`,
/// `a_sweep_that_cannot_enumerate_verifies_nothing`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepOutcome {
    pub enumerated: Option<usize>,
    pub swept: usize,
    pub incomplete: usize,
    pub unopenable: usize,
}

impl SweepOutcome {
    /// True only when the whole corpus was enumerated, every palace in it was
    /// examined, and every one came back with verified coverage.
    ///
    /// Test: `a_sweep_that_cannot_enumerate_verifies_nothing`.
    pub fn all_verified(&self) -> bool {
        self.enumerated.is_some() && self.incomplete == 0 && self.unopenable == 0
    }
}

/// Every palace id present under `data_root`.
///
/// Why not `PalaceStore::list_palaces`: it returns `Ok` while silently dropping
/// any palace whose `palace.json` fails to decode, and any `read_dir` entry
/// that errors. The sweep would read that `Ok` as a complete enumeration, so an
/// undecodable palace would be absent from the count, absent from the repair
/// queue, and the sweep would still log `all coverage verified` — the same
/// fail-open as the LRU enumeration, one layer further out. This enumerates
/// ids, not metadata, so a palace with unreadable metadata is still SEEN; the
/// sweep then fails to open it and records it as unopenable.
///
/// An errored directory entry fails the whole enumeration rather than
/// shrinking it, because a partial list is indistinguishable from a short one
/// and the caller can only tell "verified everything" from "verified what I
/// happened to see" if the difference reaches it.
///
/// What: returns the name of every immediate subdirectory holding a
/// `palace.json`.
/// Test: `startup_sweep_counts_an_undecodable_palace_instead_of_skipping_it`,
/// `a_sweep_that_cannot_enumerate_verifies_nothing`.
fn palace_ids_on_disk(data_root: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(data_root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.join("palace.json").is_file() {
            continue;
        }
        match path.file_name().and_then(|n| n.to_str()) {
            Some(name) => ids.push(name.to_string()),
            None => anyhow::bail!(
                "palace directory name is not valid UTF-8: {}",
                path.display()
            ),
        }
    }
    Ok(ids)
}

/// Sweep every palace ON DISK that has drawers, serially.
///
/// Why the disk and not the registry: `registry.list()` snapshots the LRU key
/// set of currently-OPEN handles, capped at `DEFAULT_MAX_OPEN_PALACES` (64).
/// This host holds ~99 palaces, so at least 35 would never be probed, never be
/// marked dirty, and the sweep would then report `all coverage verified` — the
/// same fail-open the coverage predicate itself had, relocated from "a palace
/// it examined" to "a palace it never examines". `service/helpers.rs` (#4637)
/// already settled this for the recall fan-out: answering from cache-resident
/// palaces only "would silently drop ~98.9% of the corpus, which is a
/// correctness regression, not an optimisation". Same reasoning, same fix.
/// Holding the opened `Arc` for the palace's whole backfill also removes the
/// mid-sweep-eviction hole — the idle ticker is armed before this runs, and a
/// borrowed LRU entry could vanish underneath it.
/// What: enumerates palace ids straight off the data root and hydrates each
/// with `open_palace` on the blocking pool (serial, so cold opens do not thrash
/// the 64-slot LRU). Every palace that cannot be enumerated, cannot be opened,
/// or cannot be verified is reported and queued for repair. A failed
/// enumeration verifies NOTHING and says so — it never reports a clean sweep.
/// Test: `startup_sweep_enumerates_every_palace_on_disk`,
/// `startup_sweep_marks_unopenable_palaces_instead_of_skipping_them`,
/// `startup_sweep_counts_an_undecodable_palace_instead_of_skipping_it`,
/// `a_sweep_that_cannot_enumerate_verifies_nothing`.
pub async fn run_startup_sweep(state: &AppState) -> SweepOutcome {
    let started = Instant::now();
    let root = state.data_root.clone();
    let listed = tokio::task::spawn_blocking(move || palace_ids_on_disk(&root)).await;

    let palaces = match listed {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            tracing::error!(
                "bm25 backfill: could not enumerate palaces on disk — the startup sweep \
                 verified NOTHING and no palace was queued for repair: {e:#}"
            );
            return SweepOutcome::default();
        }
        Err(e) => {
            tracing::error!(
                "bm25 backfill: palace enumeration task failed — the startup sweep \
                 verified NOTHING: {e}"
            );
            return SweepOutcome::default();
        }
    };

    let mut out = SweepOutcome {
        enumerated: Some(palaces.len()),
        ..Default::default()
    };

    for palace in palaces {
        let id = trusty_common::memory_core::palace::PalaceId::new(palace.clone());
        let registry = std::sync::Arc::clone(&state.registry);
        let root = state.data_root.clone();
        let opened = tokio::task::spawn_blocking(move || registry.open_palace(&root, &id)).await;
        let handle = match opened {
            Ok(Ok(h)) => h,
            Ok(Err(e)) => {
                // A palace we cannot open is a palace we cannot verify. Queue
                // it rather than skipping it into the "all verified" tally.
                tracing::error!(palace = %palace, "bm25 backfill: could not open palace: {e:#}");
                out.unopenable += 1;
                crate::bm25_repair::mark_dirty(state, &palace);
                continue;
            }
            Err(e) => {
                tracing::error!(palace = %palace, "bm25 backfill: open task failed: {e}");
                out.unopenable += 1;
                crate::bm25_repair::mark_dirty(state, &palace);
                continue;
            }
        };

        // A palace whose served drawer table is empty has no id that could be
        // missing from the index, so it is covered — skipping it is not a gap.
        // The justification is `handle.drawers` specifically, NOT "the palace
        // has no data": `open_with_intent` falls back to an empty table when
        // `load_drawers` fails, and `load_drawers` itself skips undecodable
        // rows, so drawers CAN be absent here while sitting in redb. That stays
        // correct only because `handle.drawers` is what every lane serves from,
        // vector included — coverage is defined relative to what the palace
        // serves, not to what is on disk behind it. Change where the lanes read
        // from and this skip stops being safe.
        if handle.drawers.read().is_empty() {
            continue;
        }

        let report = backfill_state_palace(state, &handle, &palace, false).await;
        out.swept += 1;
        if !report.fully_indexed() {
            out.incomplete += 1;
            crate::bm25_repair::mark_dirty(state, &palace);
        }
    }

    if out.all_verified() {
        tracing::info!(
            enumerated = ?out.enumerated,
            swept = out.swept,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "bm25 backfill: startup sweep complete, all coverage verified"
        );
    } else {
        tracing::error!(
            enumerated = ?out.enumerated,
            swept = out.swept,
            incomplete = out.incomplete,
            unopenable = out.unopenable,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "bm25 backfill: startup sweep left palaces without verified coverage — \
             queued for repair"
        );
    }
    out
}

/// Start the startup sweep on a background task.
///
/// Why: a daemon restart is the only moment at which the whole corpus is known
/// to be reachable and nothing is waiting on it. The work itself is
/// [`run_startup_sweep`], kept awaitable so its enumeration can be tested
/// without racing a spawned task.
/// What: returns immediately. No-op when the lane is off or [`ENV_NO_BACKFILL`]
/// is set — which, until the lane's default is flipped, is every deployment.
/// Test: `startup_backfill_respects_the_opt_out`.
pub fn spawn_startup_backfill(state: &AppState) {
    if state.bm25_client.is_none() {
        tracing::debug!("bm25 backfill: lane disabled — skipping startup sweep");
        return;
    }
    if startup_backfill_opted_out() {
        tracing::info!("bm25 backfill: {ENV_NO_BACKFILL}=1 — skipping startup sweep");
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        run_startup_sweep(&state).await;
    });
}

#[cfg(test)]
#[path = "bm25_backfill_tests.rs"]
mod tests;
