//! Bounded-timeout configuration for memory operations (issue #906).
//!
//! Why: The remember/recall path has several `await` points that can hang
//! indefinitely — most importantly the CoreML cold-compile inside
//! `FastEmbedder::new()` (30-120 s on Apple Silicon) and the per-call
//! `embed_batch` invocation. Without explicit bounds a single stuck embedder
//! blocks every concurrent memory operation in the process forever. This
//! module centralises the three timeout thresholds and their env-var overrides
//! so callers get a single import and defaults can be tuned per deployment.
//!
//! What: Exports per-leg `std::time::Duration` functions —
//! `embedder_init_timeout()`, `embed_batch_timeout()`, `write_lock_timeout()`,
//! `open_queue_timeout()` — plus a `lock_with_timeout` async helper that
//! applies a lock timeout at every call site uniformly. Issue #4002 adds
//! `write_op_budget()` and [`OpBudget`], the joint ceiling that stops two
//! sequential per-leg waits from composing into their sum.
//!
//! Test: `embedder_init_timeout_default`, `embed_batch_timeout_default`,
//! `write_lock_timeout_default`, `write_op_budget_default`,
//! `budget_clamps_a_leg_to_what_is_left`,
//! `an_exhausted_budget_leaves_a_later_leg_nothing`,
//! `parse_secs_with_falls_back_on_bad_value`,
//! `parse_secs_with_reads_custom_value`, `parse_secs_with_uses_default_when_absent`
//! (unit tests at the bottom of this file; run with
//! `cargo test -p trusty-common --features memory-core`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, MutexGuard};

/// Default ceiling for `FastEmbedder::new()` cold init.
///
/// Why: CoreML graph compilation on Apple Silicon can take 30-120 s on first
/// run; 180 s gives ample headroom without risking an indefinite hang.
const DEFAULT_EMBEDDER_INIT_SECS: u64 = 180;

/// Default ceiling for a single `embed_batch` call.
///
/// Why: Normal batches take 10-30 ms; even worst-case single-item batches
/// should complete well under 10 s. 30 s gives a 100x safety margin.
const DEFAULT_EMBED_BATCH_SECS: u64 = 30;

/// Default ceiling for an interactive caller's wait on a dream permit (#7106).
///
/// Why: the idle dream loop may wait forever — nothing is watching it. A user
/// who invoked `palace_dream` is watching, and one semantic-consolidation call
/// alone is bounded at 120 s per palace, so two of them ahead in the queue can
/// hold every slot for minutes. 30 s is long enough to absorb a cycle that is
/// nearly finished and short enough that the caller gets an answer.
const DEFAULT_DREAM_PERMIT_WAIT_SECS: u64 = 30;

/// Default ceiling for per-palace write-mutex acquisition.
///
/// Why: The mutex is held only during the embed+upsert+persist pipeline
/// (< 1 s normally). A long queue of writers could push acquisition time
/// above 1 s; 60 s is conservative without risking an indefinite cascade.
const DEFAULT_WRITE_LOCK_SECS: u64 = 60;

/// Default ceiling for the per-palace *open-queue* mutex in
/// `PalaceRegistry::open_palace` (issue #3992).
///
/// Why: distinct from [`DEFAULT_WRITE_LOCK_SECS`] on purpose — that one
/// bounds waiting for an *already-open* palace's write mutex (held for a
/// sub-second embed+upsert+persist pipeline), while this one bounds waiting
/// to become the next caller allowed to attempt *opening* a palace's redb
/// files under sustained `OpenIntent::Writer` lock contention. Each queued
/// opener's own attempt is separately bounded to ~1.55 s
/// (`concurrent_open::WRITER_RETRY_ATTEMPTS` x `WRITER_RETRY_SLEEP_MS`), but
/// a failed Writer open is never cached, so under persistent contention every
/// new caller repeats that ~1.55 s dance — 60 s (the same order of magnitude
/// as the write-mutex bound, chosen for the same reason: generous for a
/// legitimate burst, still finite) caps roughly 30+ such attempts before
/// giving up with a clear error rather than hanging indefinitely.
const DEFAULT_OPEN_QUEUE_SECS: u64 = 60;

/// Total wall-clock ceiling for one memory write operation (issue #4002).
///
/// Why: a write acquires the per-palace write mutex
/// ([`DEFAULT_WRITE_LOCK_SECS`]) and then the per-palace open queue
/// ([`DEFAULT_OPEN_QUEUE_SECS`]) in sequence. Each leg is bounded on its own,
/// but nothing bounded their sum, so a caller that exhausted both waited
/// 60 s + ~63 s before an error surfaced. 60 s is `max` of the two legs, not
/// their sum: it keeps the operator-visible SLA equal to the larger single
/// leg, which is what the two per-leg defaults already imply.
const DEFAULT_WRITE_OP_BUDGET_SECS: u64 = 60;

/// The budget only fixes anything while it is strictly smaller than the sum of
/// the legs it governs. Raising it to 120 s would restore the composed ceiling
/// issue #4002 removed while every runtime test kept passing, so the
/// relationship is a build failure rather than a test failure.
const _: () = assert!(
    DEFAULT_WRITE_OP_BUDGET_SECS < DEFAULT_WRITE_LOCK_SECS + DEFAULT_OPEN_QUEUE_SECS,
    "the joint write budget must be smaller than the additive worst case it replaces (#4002)"
);

/// Total wall-clock ceiling for the embed+upsert+persist pipeline that runs
/// while the per-palace write mutex is held (issue #6366).
///
/// Why: [`DEFAULT_WRITE_OP_BUDGET_SECS`] bounds only the two ACQUISITION legs.
/// Once a writer holds the mutex, the pipeline it runs had no ceiling of any
/// kind, so a slow commit held the mutex for as long as it took and every other
/// writer on that palace waited behind it. Three `memory_note` calls were
/// aborted client-side after 1800 s while the daemon stayed healthy and the
/// writes landed later. 240 s is the operator-visible ceiling on one write's
/// critical section: long enough that no legitimate write can trip it, short
/// enough that a stall surfaces as a named error rather than a client abort.
const DEFAULT_WRITE_PIPELINE_SECS: u64 = 240;

/// The pipeline ceiling must clear the two embedder legs it contains, or a cold
/// CoreML compile on an otherwise-healthy host would trip it and turn a slow
/// first write into a failed one. Both legs run inside the critical section
/// this budget governs, so the relationship is a build failure rather than a
/// test failure.
const _: () = assert!(
    DEFAULT_WRITE_PIPELINE_SECS > DEFAULT_EMBEDDER_INIT_SECS + DEFAULT_EMBED_BATCH_SECS,
    "the write-pipeline ceiling must exceed the embedder legs it contains (#6366)"
);

/// The ceiling governs the critical section the acquisition budget stops at. A
/// ceiling at or below that budget would mean a write which waited its full
/// turn for the mutex could never then be allowed to run — a different, and
/// wrong, policy.
const _: () = assert!(
    DEFAULT_WRITE_PIPELINE_SECS > DEFAULT_WRITE_OP_BUDGET_SECS,
    "the write-pipeline ceiling must exceed the acquisition budget it follows (#6366)"
);

/// Default ceiling on how long one palace redb write transaction may hold the
/// write lock before it is rolled back (#8749).
///
/// Why: redb admits one writer per file, so a transaction that stalls holds off
/// every other writer — including after #6366's pipeline ceiling has already
/// given up on it. A KG batch is at most 64 ops and normally commits in
/// milliseconds, so 30 s is orders of magnitude above a healthy transaction.
/// The clock starts when redb grants the lock, so waiting for it never counts.
const DEFAULT_WRITE_TXN_SECS: u64 = 30;

/// A deadline at or above the pipeline floor would let a commit that a
/// timed-out pipeline abandoned keep the lock longer than any ceiling the
/// pipeline can be configured to; [`cap_write_txn_deadline`] enforces the same
/// relation at runtime for an overridden value.
const _: () = assert!(
    DEFAULT_WRITE_TXN_SECS < WRITE_PIPELINE_FLOOR_SECS,
    "the write-transaction deadline must sit inside the pipeline ceiling (#8749)"
);

/// A writer queued on the palace write mutex behind a stalled transaction must
/// outlast that transaction's deadline, or it times out before the lock frees.
/// This covers the defaults; [`cap_write_txn_deadline`] enforces it at runtime
/// for an overridden deadline or lock wait.
const _: () = assert!(
    DEFAULT_WRITE_TXN_SECS < DEFAULT_WRITE_LOCK_SECS,
    "the write-transaction deadline must be shorter than the write-lock wait (#8749)"
);

/// Elapsed pipeline duration above which a COMPLETED write is logged as slow.
///
/// Why: issue #6366 found nothing in `stderr.log` for the whole 1800 s window —
/// the stall was completely silent, so the only evidence was client-side. A
/// write that finishes but takes seconds under the mutex is the leading
/// indicator of the stall, and naming it costs one `warn!` per slow write.
/// 5 s is two orders of magnitude above the "< 1 s normally" the write-mutex
/// docs assume, so an ordinary write never logs.
const SLOW_WRITE_WARN_SECS: u64 = 5;

/// A warn threshold at or above the ceiling could never fire — the write would
/// have failed first — which would silently retire the diagnostic.
const _: () = assert!(
    SLOW_WRITE_WARN_SECS < DEFAULT_WRITE_PIPELINE_SECS,
    "the slow-write warning must be reachable below the pipeline ceiling (#6366)"
);

/// Return the `FastEmbedder::new()` init timeout.
///
/// Why: Overridable via `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS` so operators on
/// slow CI machines or cold CUDA hosts can extend the ceiling without
/// recompiling.
/// What: Reads the env var; falls back to `DEFAULT_EMBEDDER_INIT_SECS` (180).
/// Test: `embedder_init_timeout_default`, `parse_secs_with_reads_custom_value`.
pub fn embedder_init_timeout() -> Duration {
    parse_secs_env(
        "TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS",
        DEFAULT_EMBEDDER_INIT_SECS,
    )
}

/// Return the per-call `embed_batch` timeout.
///
/// Why: Overridable via `TRUSTY_EMBED_BATCH_TIMEOUT_SECS` so high-throughput
/// or GPU-backed deployments can tune the ceiling.
/// What: Reads the env var; falls back to `DEFAULT_EMBED_BATCH_SECS` (30).
/// Test: `embed_batch_timeout_default`.
pub fn embed_batch_timeout() -> Duration {
    parse_secs_env("TRUSTY_EMBED_BATCH_TIMEOUT_SECS", DEFAULT_EMBED_BATCH_SECS)
}

/// Return how long an interactive dream call waits for a concurrency permit.
///
/// Why (#7106): the process-wide dream bound makes the on-demand
/// `palace_dream` / `dream_consolidate_room` tools queue behind the idle
/// loops. An unbounded wait there turns a user-invoked call into a silent hang
/// for as long as the cycles ahead of it take. This is the ceiling that turns
/// that hang into an error the caller can act on. The idle loop does NOT use
/// it — nothing is waiting on the idle loop, so it queues indefinitely.
/// What: reads `TRUSTY_DREAM_PERMIT_WAIT_SECS`, falling back to
/// [`DEFAULT_DREAM_PERMIT_WAIT_SECS`] (30) when absent or malformed.
/// Test: `dream_permit_wait_timeout_default`,
/// `dream::concurrency_tests::an_interactive_dream_errors_when_the_dreamer_is_busy`.
pub fn dream_permit_wait_timeout() -> Duration {
    parse_secs_env(
        "TRUSTY_DREAM_PERMIT_WAIT_SECS",
        DEFAULT_DREAM_PERMIT_WAIT_SECS,
    )
}

/// Return the per-palace write-lock acquisition timeout.
///
/// Why: Overridable via `TRUSTY_WRITE_LOCK_TIMEOUT_SECS` to accommodate
/// unusually deep write queues on write-heavy deployments.
/// What: Reads the env var; falls back to `DEFAULT_WRITE_LOCK_SECS` (60).
/// Test: `write_lock_timeout_default`.
pub fn write_lock_timeout() -> Duration {
    parse_secs_env("TRUSTY_WRITE_LOCK_TIMEOUT_SECS", DEFAULT_WRITE_LOCK_SECS)
}

/// Return the per-palace open-queue acquisition timeout (issue #3992).
///
/// Why: Overridable via `TRUSTY_OPEN_QUEUE_TIMEOUT_SECS`, independently of
/// [`write_lock_timeout`] — see [`DEFAULT_OPEN_QUEUE_SECS`] for why the two
/// are deliberately separate knobs. Operators whose palace files sit on slow
/// or contended storage (network mounts, many concurrent sessions hammering
/// one freshly-evicted palace) can raise this without also loosening the
/// unrelated write-mutex bound.
/// What: Reads the env var; falls back to `DEFAULT_OPEN_QUEUE_SECS` (60).
/// Test: `open_queue_timeout_default`.
pub fn open_queue_timeout() -> Duration {
    parse_secs_env("TRUSTY_OPEN_QUEUE_TIMEOUT_SECS", DEFAULT_OPEN_QUEUE_SECS)
}

/// Return the total budget one memory write operation may spend waiting
/// (issue #4002).
///
/// Why: Overridable via `TRUSTY_WRITE_OP_BUDGET_SECS`. This is the number an
/// operator reasons about — "how long before `memory_remember` gives up" —
/// whereas [`write_lock_timeout`] and [`open_queue_timeout`] cap individual
/// legs inside it.
/// What: Reads the env var; falls back to `DEFAULT_WRITE_OP_BUDGET_SECS` (60).
/// Test: `write_op_budget_default`; the default's relationship to the legs it
/// replaces is a `const` assertion beside `DEFAULT_WRITE_OP_BUDGET_SECS`.
pub fn write_op_budget() -> Duration {
    parse_secs_env("TRUSTY_WRITE_OP_BUDGET_SECS", DEFAULT_WRITE_OP_BUDGET_SECS)
}

/// Return the ceiling on one write's embed+upsert+persist pipeline (#6366).
///
/// Why: Overridable via `TRUSTY_WRITE_PIPELINE_TIMEOUT_SECS`. This is the
/// number an operator reasons about when a palace's writes go slow — "how long
/// may one write hold this palace's write mutex" — as distinct from
/// [`write_op_budget`], which bounds only the waits BEFORE the mutex is held.
/// What: Reads the env var; falls back to `DEFAULT_WRITE_PIPELINE_SECS` (240).
/// A clamped value warns once per process (#8749: this now runs on every
/// write transaction, via [`write_txn_deadline`]).
/// Test: `write_pipeline_timeout_default`; its relationship to the embedder
/// legs it contains is a `const` assertion beside `DEFAULT_WRITE_PIPELINE_SECS`.
pub fn write_pipeline_timeout() -> Duration {
    let configured = parse_secs_env(
        "TRUSTY_WRITE_PIPELINE_TIMEOUT_SECS",
        DEFAULT_WRITE_PIPELINE_SECS,
    );
    let floored = floor_write_pipeline(configured);
    // #8749: once per process, not once per write transaction.
    if floored != configured && first_warning(&PIPELINE_FLOOR_WARNED) {
        tracing::warn!(
            configured_secs = configured.as_secs(),
            floor_secs = WRITE_PIPELINE_FLOOR_SECS,
            applied_secs = floored.as_secs(),
            "#6366: TRUSTY_WRITE_PIPELINE_TIMEOUT_SECS is below the embedder \
             legs the write pipeline contains, so every cold write would fail \
             the moment the embedder initialises. Clamped to the compiled-in \
             default; raise the value above the floor to set it deliberately"
        );
    }
    floored
}

/// The lowest write-pipeline ceiling that can still contain the embedder legs.
///
/// Why: the `const` assertion beside [`DEFAULT_WRITE_PIPELINE_SECS`] protects
/// only the compiled-in default. `TRUSTY_WRITE_PIPELINE_TIMEOUT_SECS` bypasses
/// it entirely, so `=0` (or any value under the legs' sum) silently failed
/// every write on every palace with no signal but the per-write error.
const WRITE_PIPELINE_FLOOR_SECS: u64 = DEFAULT_EMBEDDER_INIT_SECS + DEFAULT_EMBED_BATCH_SECS;

/// The floor is a lower bound the default must itself satisfy, or clamping
/// would raise a correctly-configured value.
const _: () = assert!(
    DEFAULT_WRITE_PIPELINE_SECS > WRITE_PIPELINE_FLOOR_SECS,
    "the compiled-in default must clear the floor it is clamped to (#6366)"
);

/// Clamp a configured write-pipeline ceiling up to the embedder legs it must
/// contain (#6366).
///
/// Why: split out as a pure function so the clamp is testable without env
/// mutation, matching how [`parse_secs_with`] separates parsing from lookup.
/// What: returns `configured` when it strictly exceeds
/// [`WRITE_PIPELINE_FLOOR_SECS`]; otherwise returns
/// [`DEFAULT_WRITE_PIPELINE_SECS`] — the compiled-in value the `const`
/// assertions already prove sound, rather than the bare floor.
/// Test: `a_zero_pipeline_override_is_clamped_to_the_default`,
/// `an_override_below_the_embedder_legs_is_clamped`,
/// `an_override_above_the_floor_is_honoured`.
pub fn floor_write_pipeline(configured: Duration) -> Duration {
    if configured > Duration::from_secs(WRITE_PIPELINE_FLOOR_SECS) {
        configured
    } else {
        Duration::from_secs(DEFAULT_WRITE_PIPELINE_SECS)
    }
}

/// Return the deadline for one palace redb write transaction (#8749).
///
/// Why: Overridable via `TRUSTY_WRITE_TXN_DEADLINE_SECS` for a host whose
/// commits are legitimately slow. The override is checked against the live
/// pipeline ceiling and write-lock wait (#8749 review): a deadline that
/// outlives either lets a stalled commit hold the lock past every waiter.
/// What: Reads the env var, then [`cap_write_txn_deadline`] against
/// [`write_pipeline_timeout`] and [`write_lock_timeout`]. Runs on every write
/// transaction, so a corrected value is logged at warn once per process.
/// Test: `write_txn_deadline_default`,
/// `a_txn_deadline_outside_the_pipeline_ceiling_falls_back_to_the_default`,
/// `a_txn_deadline_at_or_above_the_write_lock_wait_falls_back_to_the_default`.
pub fn write_txn_deadline() -> Duration {
    let configured = parse_secs_env("TRUSTY_WRITE_TXN_DEADLINE_SECS", DEFAULT_WRITE_TXN_SECS);
    let pipeline = write_pipeline_timeout();
    let lock_wait = write_lock_timeout();
    let applied = cap_write_txn_deadline(configured, pipeline, lock_wait);
    if applied != configured && first_warning(&TXN_DEADLINE_WARNED) {
        tracing::warn!(
            configured_secs = configured.as_secs(),
            pipeline_secs = pipeline.as_secs(),
            lock_wait_secs = lock_wait.as_secs(),
            applied_secs = applied.as_secs(),
            "#8749: TRUSTY_WRITE_TXN_DEADLINE_SECS must be above zero and below \
             both the write-pipeline ceiling and the write-lock wait; using the \
             compiled-in default"
        );
    }
    applied
}

/// Keep a configured write-transaction deadline inside
/// `(0, min(pipeline, lock_wait))` (#8749).
///
/// Why: `0` would roll back every write on every palace (the #6366
/// zero-override hazard). A value at or above the pipeline ceiling lets a
/// commit the pipeline abandoned keep the lock longer than the ceiling, and
/// one at or above the write-lock wait times out every writer queued behind a
/// stalled transaction before the deadline frees the lock.
/// What: returns `configured` when it is non-zero and below both bounds,
/// otherwise [`DEFAULT_WRITE_TXN_SECS`]. The `const` assertions prove the
/// default sits below every pipeline ceiling and the default lock wait; an
/// operator who lowers the lock wait below the default still gets the
/// default, with the warning naming both values.
/// Test: `a_txn_deadline_outside_the_pipeline_ceiling_falls_back_to_the_default`,
/// `a_txn_deadline_at_or_above_the_write_lock_wait_falls_back_to_the_default`.
pub fn cap_write_txn_deadline(
    configured: Duration,
    pipeline: Duration,
    lock_wait: Duration,
) -> Duration {
    if configured.is_zero() || configured >= pipeline.min(lock_wait) {
        Duration::from_secs(DEFAULT_WRITE_TXN_SECS)
    } else {
        configured
    }
}

/// Set once [`write_pipeline_timeout`] has warned about a clamped value.
static PIPELINE_FLOOR_WARNED: AtomicBool = AtomicBool::new(false);
/// Set once [`write_txn_deadline`] has warned about a corrected value.
static TXN_DEADLINE_WARNED: AtomicBool = AtomicBool::new(false);

/// `true` the first time it is called for `flag`, `false` after (#8749).
///
/// Why: both readers run on every write transaction, and the environment they
/// read does not change within a process, so one warning carries the whole
/// signal and a per-write repeat only floods `stderr.log`.
fn first_warning(flag: &AtomicBool) -> bool {
    !flag.swap(true, Ordering::Relaxed)
}

/// Return the elapsed time above which a completed write is logged as slow.
///
/// Why: Overridable via `TRUSTY_SLOW_WRITE_WARN_SECS` so an operator chasing a
/// stall can lower it without recompiling, or raise it on a host where writes
/// are legitimately slow and the warning is only noise.
/// What: Reads the env var; falls back to `SLOW_WRITE_WARN_SECS` (5).
/// Test: `slow_write_warn_threshold_default`.
pub fn slow_write_warn_threshold() -> Duration {
    parse_secs_env("TRUSTY_SLOW_WRITE_WARN_SECS", SLOW_WRITE_WARN_SECS)
}

/// One operation's remaining share of a joint wait budget (issue #4002).
///
/// Why: `memory_remember` waits for the per-palace write mutex, then waits
/// again to enter the per-palace open queue. Both waits were bounded, but each
/// by its own full timeout, so the effective ceiling was their SUM (60 s + 60 s
/// with stock defaults) rather than either configured bound. An `OpBudget`
/// makes the later leg spend what the earlier leg left instead of starting a
/// fresh window: the caller stamps one at the top of the operation and clamps
/// every subsequent wait through [`OpBudget::leg`].
/// What: stores the start instant and the total. [`OpBudget::remaining`]
/// saturates at zero, so an overrun can only shrink a later leg, never extend
/// one; [`OpBudget::leg`] returns `min(configured_leg, remaining)`, so the
/// budget is a ceiling on top of each per-leg timeout and never raises one.
/// Storing `total` rather than a precomputed deadline keeps a large configured
/// value from overflowing `Instant`.
/// Test: `budget_clamps_a_leg_to_what_is_left`,
/// `an_exhausted_budget_leaves_a_later_leg_nothing`,
/// `budget_never_extends_a_shorter_leg`.
#[derive(Debug, Clone, Copy)]
pub struct OpBudget {
    started: Instant,
    total: Duration,
}

impl OpBudget {
    /// Stamp a budget of `total` starting now.
    #[must_use]
    pub fn start(total: Duration) -> Self {
        Self {
            started: Instant::now(),
            total,
        }
    }

    /// Stamp a budget of [`write_op_budget`] starting now.
    #[must_use]
    pub fn start_default() -> Self {
        Self::start(write_op_budget())
    }

    /// Time left in this budget, saturating at zero once it is spent.
    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.total.saturating_sub(self.started.elapsed())
    }

    /// Clamp one leg's configured timeout to what the budget has left.
    ///
    /// Why: this is the whole fix for issue #4002 — it is what stops two
    /// independently-bounded waits from composing into their sum.
    /// What: `min(configured, remaining())`. Returns `Duration::ZERO` once the
    /// budget is spent, which makes the leg a non-blocking attempt rather than
    /// an unconditional failure: an uncontended acquisition still succeeds.
    /// Test: `budget_clamps_a_leg_to_what_is_left`,
    /// `an_exhausted_budget_leaves_a_later_leg_nothing`.
    #[must_use]
    pub fn leg(&self, configured: Duration) -> Duration {
        configured.min(self.remaining())
    }
}

/// Acquire a `tokio::sync::Mutex` with a bounded timeout, returning an error
/// on expiry.
///
/// Why: The write-lock acquisition pattern (get timeout, call
/// `tokio::time::timeout`, map the elapsed error to a formatted message) was
/// duplicated at four call sites (retrieval remember + forget paths and
/// tools.rs memory_remember + memory_note handlers). A single helper
/// eliminates the duplication and guarantees a consistent error message shape
/// (issue #906).
/// What: Calls `tokio::time::timeout(duration, mutex.lock())`. On success
/// returns the `MutexGuard`. On expiry returns [`WriteTimeout::LockWait`]
/// (#8749: typed, so a caller can tell a timed-out wait from a data error),
/// whose message names the palace label and the configured duration.
/// Test: `write_lock_timeout_returns_error_when_held` in
/// `memory_core::retrieval::timeout_tests` exercises this path end-to-end.
pub async fn lock_with_timeout<'a>(
    mutex: &'a Arc<Mutex<()>>,
    duration: Duration,
    label: &str,
) -> anyhow::Result<MutexGuard<'a, ()>> {
    tokio::time::timeout(duration, mutex.lock())
        .await
        .map_err(|_| {
            anyhow::Error::new(WriteTimeout::LockWait {
                palace: label.to_string(),
                waited: duration,
            })
        })
}

/// A palace write that gave up rather than wait without bound (#8749).
///
/// Why: a writer queued behind a stalled one must fail with an error a caller
/// can recognise — "timed out, retry" — not an opaque string. Wrapped in
/// `anyhow`, so callers that only read the message see the same text as
/// before, and `downcast_ref::<WriteTimeout>()` names the case.
/// What: `LockWait` — the per-palace write mutex was not granted in time, so
/// nothing ran. `PipelineBudget` — the write's critical section ran out of
/// budget; work not yet dispatched never runs, and a commit already
/// dispatched lands whole or rolls back at its transaction deadline.
/// Test: `write_lock_timeout_returns_error_when_held`,
/// `a_writer_behind_a_stalled_commit_times_out_with_a_typed_error`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WriteTimeout {
    /// Waiting for the palace write mutex ran out of time.
    #[error(
        "palace '{palace}' write-lock acquisition timed out after {waited:?} \
         (issue #906); a previous writer may be stuck — retry or increase \
         TRUSTY_WRITE_LOCK_TIMEOUT_SECS"
    )]
    LockWait { palace: String, waited: Duration },
    /// The write pipeline ran past its ceiling.
    #[error(
        "palace '{}' write pipeline exceeded its {:?} budget after {:?} \
         (issue #6366); the palace write mutex has been released so other \
         writers proceed. kg.redb is {} bytes — a large store makes commits \
         slower; raise TRUSTY_WRITE_PIPELINE_TIMEOUT_SECS if writes on this \
         palace are legitimately this slow",
        .palace,
        .budget,
        .elapsed,
        .kg_redb_bytes.map_or_else(|| "unknown".to_string(), |b| b.to_string())
    )]
    PipelineBudget {
        palace: String,
        budget: Duration,
        elapsed: Duration,
        kg_redb_bytes: Option<u64>,
    },
}

/// Pure parser: return a `Duration` from a lookup-provided optional string.
///
/// Why: Separating the env-lookup side-effect from the parse logic makes the
/// core behaviour testable without any `unsafe` env mutation. The public
/// `parse_secs_env` and `embedder_init_timeout` / `embed_batch_timeout` /
/// `write_lock_timeout` functions delegate here and are themselves trivially
/// correct once this function is verified.
/// What: Calls `lookup(key)` to get an optional `String`. If present, tries
/// `u64` parse; on success returns `Duration::from_secs(parsed)`. Falls back
/// to `Duration::from_secs(default_secs)` when the key is absent or the value
/// is non-numeric.
/// Test: `parse_secs_with_falls_back_on_bad_value`,
///       `parse_secs_with_reads_custom_value`,
///       `parse_secs_with_uses_default_when_absent`.
pub fn parse_secs_with(
    lookup: impl Fn(&str) -> Option<String>,
    key: &str,
    default_secs: u64,
) -> Duration {
    lookup(key)
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(default_secs))
}

/// Parse a duration from `$name` (seconds), returning `default_secs` on
/// missing or malformed values.
///
/// Why: Centralising the parse keeps each public function a one-liner and
/// ensures consistent fallback semantics across all three timeouts. Delegates
/// to `parse_secs_with` with the real env lookup so the pure logic is tested
/// separately (no env mutation in pure-logic tests).
/// What: Calls `parse_secs_with` with `std::env::var` as the lookup.
/// Test: Public-function default tests verify the end-to-end path; pure-logic
///       tests cover `parse_secs_with` directly without env mutations.
fn parse_secs_env(name: &str, default_secs: u64) -> Duration {
    parse_secs_with(|k| std::env::var(k).ok(), name, default_secs)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // Pure-logic tests — exercise `parse_secs_with` with an injected lookup.
    // These tests never touch process env: no `unsafe`, no races under the
    // multi-threaded test harness.
    // -------------------------------------------------------------------------

    /// Why: Guard that `parse_secs_with` returns the default when the lookup
    /// returns `None` (key absent).
    /// What: Pass a lookup that always returns `None`, assert default returned.
    /// Test: itself.
    #[test]
    fn parse_secs_with_uses_default_when_absent() {
        let t = parse_secs_with(|_| None, "ANY_KEY", DEFAULT_EMBEDDER_INIT_SECS);
        assert_eq!(t, Duration::from_secs(DEFAULT_EMBEDDER_INIT_SECS));
    }

    /// Why: Guard that a non-numeric value falls back to the default rather
    /// than panicking.
    /// What: Pass a lookup returning `Some("notanumber")`, assert default.
    /// Test: itself.
    #[test]
    fn parse_secs_with_falls_back_on_bad_value() {
        let t = parse_secs_with(
            |_| Some("notanumber".to_string()),
            "ANY_KEY",
            DEFAULT_EMBEDDER_INIT_SECS,
        );
        assert_eq!(t, Duration::from_secs(DEFAULT_EMBEDDER_INIT_SECS));
    }

    /// Why: Guard that a valid numeric value is respected.
    /// What: Pass a lookup returning `Some("5")`, assert 5 s returned.
    /// Test: itself.
    #[test]
    fn parse_secs_with_reads_custom_value() {
        let t = parse_secs_with(
            |_| Some("5".to_string()),
            "ANY_KEY",
            DEFAULT_EMBED_BATCH_SECS,
        );
        assert_eq!(t, Duration::from_secs(5));
    }

    // -------------------------------------------------------------------------
    // Default-value tests — exercise the public timeout functions to ensure
    // the defaults are what the module documentation promises. These tests
    // rely on the env vars being absent at test time; they are serialised
    // behind a process-wide mutex to avoid interleaving with any other test
    // that sets those vars (e.g. the `#[ignore]` timeout-fire tests).
    // -------------------------------------------------------------------------

    /// Serialises tests that read the real env so they cannot interleave.
    ///
    /// #5937: the crate-wide [`crate::data_dir::ENV_LOCK`], so the writers in
    /// `retrieval/timeout_tests.rs` exclude these readers.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::data_dir::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Why: Guard that the default is 180 s when the env var is absent.
    /// What: Hold the env mutex, verify the var is unset (or unset it), call
    /// `embedder_init_timeout()`, assert 180 s.
    /// Test: itself.
    #[test]
    fn embedder_init_timeout_default() {
        let _guard = env_lock();
        // Clear the var so we're testing the absent-key code path.
        // SAFETY: we hold `env_lock()` which serialises all env-touching
        // tests in this module; no other thread mutates this var while
        // the mutex is held.
        unsafe { std::env::remove_var("TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS") };
        let t = embedder_init_timeout();
        assert_eq!(t, Duration::from_secs(DEFAULT_EMBEDDER_INIT_SECS));
    }

    /// Why: Guard that the default is 30 s when the env var is absent.
    /// What: Hold the env mutex, unset the var, call `embed_batch_timeout()`,
    /// assert 30 s.
    /// Test: itself.
    #[test]
    fn embed_batch_timeout_default() {
        let _guard = env_lock();
        unsafe { std::env::remove_var("TRUSTY_EMBED_BATCH_TIMEOUT_SECS") };
        let t = embed_batch_timeout();
        assert_eq!(t, Duration::from_secs(DEFAULT_EMBED_BATCH_SECS));
    }

    /// Why: Guard that the default is 30 s when the env var is absent — an
    /// interactive dream call must never inherit an unbounded wait (#7106).
    /// What: Exercises `parse_secs_with` with an injected lookup rather than
    /// mutating the process environment, because a dream test in this same
    /// binary SETS `TRUSTY_DREAM_PERMIT_WAIT_SECS` under its own lock; a
    /// remove-then-read here would race it.
    /// Test: itself.
    #[test]
    fn dream_permit_wait_timeout_default() {
        let t = parse_secs_with(
            |_| None,
            "TRUSTY_DREAM_PERMIT_WAIT_SECS",
            DEFAULT_DREAM_PERMIT_WAIT_SECS,
        );
        assert_eq!(t, Duration::from_secs(DEFAULT_DREAM_PERMIT_WAIT_SECS));
        assert_eq!(DEFAULT_DREAM_PERMIT_WAIT_SECS, 30);
    }

    /// Why: Guard that the default is 60 s when the env var is absent.
    /// What: Hold the env mutex, unset the var, call `write_lock_timeout()`,
    /// assert 60 s.
    /// Test: itself.
    #[test]
    fn write_lock_timeout_default() {
        let _guard = env_lock();
        unsafe { std::env::remove_var("TRUSTY_WRITE_LOCK_TIMEOUT_SECS") };
        let t = write_lock_timeout();
        assert_eq!(t, Duration::from_secs(DEFAULT_WRITE_LOCK_SECS));
    }

    /// Why (issue #3992): Guard that the default is 60 s when the env var is
    /// absent, and that it is a distinct knob from `write_lock_timeout`.
    /// What: Hold the env mutex, unset the var, call `open_queue_timeout()`,
    /// assert 60 s.
    /// Test: itself.
    #[test]
    fn open_queue_timeout_default() {
        let _guard = env_lock();
        unsafe { std::env::remove_var("TRUSTY_OPEN_QUEUE_TIMEOUT_SECS") };
        let t = open_queue_timeout();
        assert_eq!(t, Duration::from_secs(DEFAULT_OPEN_QUEUE_SECS));
    }

    /// Why (issue #4002): Guard that the default total budget is 60 s when the
    /// env var is absent.
    /// What: Hold the env mutex, unset the var, call `write_op_budget()`.
    /// Test: itself.
    #[test]
    fn write_op_budget_default() {
        let _guard = env_lock();
        unsafe { std::env::remove_var("TRUSTY_WRITE_OP_BUDGET_SECS") };
        let t = write_op_budget();
        assert_eq!(t, Duration::from_secs(DEFAULT_WRITE_OP_BUDGET_SECS));
    }

    /// Why (issue #6366): Guard that the pipeline ceiling defaults to 240 s
    /// when the env var is absent.
    /// What: Hold the env mutex, unset the var, call `write_pipeline_timeout()`.
    /// Test: itself.
    #[test]
    fn write_pipeline_timeout_default() {
        let _guard = env_lock();
        unsafe { std::env::remove_var("TRUSTY_WRITE_PIPELINE_TIMEOUT_SECS") };
        let t = write_pipeline_timeout();
        assert_eq!(t, Duration::from_secs(DEFAULT_WRITE_PIPELINE_SECS));
    }

    /// Why (#8749): the transaction deadline must default to the documented
    /// value, and a `0` override must not roll back every write.
    /// What: Hold the env mutex, check unset and `0`.
    /// Test: itself.
    #[test]
    fn write_txn_deadline_default() {
        let _guard = env_lock();
        unsafe { std::env::remove_var("TRUSTY_WRITE_TXN_DEADLINE_SECS") };
        assert_eq!(
            write_txn_deadline(),
            Duration::from_secs(DEFAULT_WRITE_TXN_SECS)
        );
        unsafe { std::env::set_var("TRUSTY_WRITE_TXN_DEADLINE_SECS", "0") };
        let zero = write_txn_deadline();
        unsafe { std::env::remove_var("TRUSTY_WRITE_TXN_DEADLINE_SECS") };
        assert_eq!(zero, Duration::from_secs(DEFAULT_WRITE_TXN_SECS));
    }

    /// Why (#8749 review): a deadline at or above the pipeline ceiling lets a
    /// commit the pipeline abandoned hold the lock past every caller; the
    /// `const` assertion covers only the compiled-in default.
    /// What: pure check of [`cap_write_txn_deadline`] — zero and at/above the
    /// ceiling fall back to the default; a value inside is honoured.
    /// Test: itself.
    #[test]
    fn a_txn_deadline_outside_the_pipeline_ceiling_falls_back_to_the_default() {
        let default = Duration::from_secs(DEFAULT_WRITE_TXN_SECS);
        let pipeline = Duration::from_secs(DEFAULT_WRITE_PIPELINE_SECS);
        // A lock wait above the pipeline, so only the pipeline bound is tested.
        let lock_wait = pipeline * 4;
        for configured in [Duration::ZERO, pipeline, pipeline * 2] {
            assert_eq!(
                cap_write_txn_deadline(configured, pipeline, lock_wait),
                default
            );
        }
        let inside = Duration::from_secs(90);
        assert_eq!(cap_write_txn_deadline(inside, pipeline, lock_wait), inside);
    }

    /// Why (#8749 review): a deadline at or above the write-lock wait times
    /// out every writer queued behind a stalled transaction before the
    /// deadline frees the lock — the relation the `const` assertion states for
    /// the defaults, which an override bypassed.
    /// What: through the env, end to end: a 90 s deadline (inside the 240 s
    /// pipeline, above the 60 s lock wait) falls back to the default, and so
    /// does a 25 s deadline under a 20 s lock wait; 45 s under 60 s is kept.
    /// Test: itself.
    #[test]
    fn a_txn_deadline_at_or_above_the_write_lock_wait_falls_back_to_the_default() {
        const TXN: &str = "TRUSTY_WRITE_TXN_DEADLINE_SECS";
        const LOCK: &str = "TRUSTY_WRITE_LOCK_TIMEOUT_SECS";
        let _guard = env_lock();
        unsafe { std::env::remove_var("TRUSTY_WRITE_PIPELINE_TIMEOUT_SECS") };
        let default = Duration::from_secs(DEFAULT_WRITE_TXN_SECS);
        let mut seen = Vec::new();
        for (txn, lock) in [("90", None), ("25", Some("20")), ("45", None)] {
            unsafe { std::env::set_var(TXN, txn) };
            match lock {
                Some(l) => unsafe { std::env::set_var(LOCK, l) },
                None => unsafe { std::env::remove_var(LOCK) },
            }
            seen.push(write_txn_deadline());
        }
        unsafe { std::env::remove_var(TXN) };
        unsafe { std::env::remove_var(LOCK) };
        assert_eq!(
            seen,
            [default, default, Duration::from_secs(45)],
            "#8749: the deadline must stay below the write-lock wait"
        );
    }

    /// Why (issue #6366): `TRUSTY_WRITE_PIPELINE_TIMEOUT_SECS=0` parsed to
    /// `Duration::ZERO` and was honoured, so `remember_within`'s zero-budget
    /// rejection failed EVERY write on EVERY palace with nothing at startup
    /// saying why. The `const` assertion beside `DEFAULT_WRITE_PIPELINE_SECS`
    /// never saw the override.
    /// What: clamps zero up to the compiled-in default.
    /// Test: itself.
    #[test]
    fn a_zero_pipeline_override_is_clamped_to_the_default() {
        assert_eq!(
            floor_write_pipeline(Duration::ZERO),
            Duration::from_secs(DEFAULT_WRITE_PIPELINE_SECS)
        );
    }

    /// Why (issue #6366): a value below the embedder legs is the less obvious
    /// half of the same defect — a cold CoreML compile alone can take 180 s, so
    /// any ceiling under the legs' sum fails cold writes on a healthy host.
    /// What: a value just under the floor clamps; the floor itself clamps too,
    /// mirroring the strict `>` the const assertions use.
    /// Test: itself.
    #[test]
    fn an_override_below_the_embedder_legs_is_clamped() {
        let default = Duration::from_secs(DEFAULT_WRITE_PIPELINE_SECS);
        assert_eq!(floor_write_pipeline(Duration::from_secs(1)), default);
        assert_eq!(
            floor_write_pipeline(Duration::from_secs(WRITE_PIPELINE_FLOOR_SECS - 1)),
            default
        );
        assert_eq!(
            floor_write_pipeline(Duration::from_secs(WRITE_PIPELINE_FLOOR_SECS)),
            default
        );
    }

    /// Why (issue #6366): the clamp must not quietly overrule an operator who
    /// raised or lowered the ceiling deliberately but legitimately — otherwise
    /// the knob the error message advertises would not work.
    /// What: values above the floor pass through untouched, in both directions
    /// relative to the default.
    /// Test: itself.
    #[test]
    fn an_override_above_the_floor_is_honoured() {
        for secs in [WRITE_PIPELINE_FLOOR_SECS + 1, 600, 3600] {
            assert_eq!(
                floor_write_pipeline(Duration::from_secs(secs)),
                Duration::from_secs(secs),
                "a {secs}s ceiling clears the floor and must be honoured"
            );
        }
    }

    /// Why (issue #6366): Guard that the slow-write warning defaults to 5 s.
    /// What: Hold the env mutex, unset the var, call
    /// `slow_write_warn_threshold()`.
    /// Test: itself.
    #[test]
    fn slow_write_warn_threshold_default() {
        let _guard = env_lock();
        unsafe { std::env::remove_var("TRUSTY_SLOW_WRITE_WARN_SECS") };
        let t = slow_write_warn_threshold();
        assert_eq!(t, Duration::from_secs(SLOW_WRITE_WARN_SECS));
    }

    // -------------------------------------------------------------------------
    // `OpBudget` (issue #4002). No sleeps: an exhausted budget is constructed
    // with a zero total, whose `remaining()` is exactly zero on any machine
    // because `saturating_sub` can only move it down.
    // -------------------------------------------------------------------------

    /// Why (issue #4002): the second leg must spend what the first left, not
    /// open a fresh window of its own.
    /// What: stamp a 5 s budget, assert a 60 s configured leg is clamped to at
    /// most the 5 s total.
    /// Test: itself.
    #[test]
    fn budget_clamps_a_leg_to_what_is_left() {
        let budget = OpBudget::start(Duration::from_secs(5));
        let leg = budget.leg(Duration::from_secs(60));
        assert!(
            leg <= Duration::from_secs(5),
            "a 60s leg under a 5s budget must be clamped to the budget; got {leg:?}"
        );
    }

    /// Why (issue #4002): once the first leg has spent the whole budget the
    /// second must not start another 60 s wait — that composition is the defect.
    /// What: stamp a zero budget (exact, no clock dependence) and assert both
    /// `remaining()` and a 60 s clamped leg are zero.
    /// Test: itself.
    #[test]
    fn an_exhausted_budget_leaves_a_later_leg_nothing() {
        let budget = OpBudget::start(Duration::ZERO);
        assert_eq!(budget.remaining(), Duration::ZERO);
        assert_eq!(budget.leg(Duration::from_secs(60)), Duration::ZERO);
    }

    /// Why (issue #4002): the budget is a ceiling, never a floor. A fix that
    /// returned `remaining()` unconditionally would let a 1 ms leg wait the
    /// whole budget.
    /// What: stamp a 60 s budget, assert a 1 ms configured leg passes through.
    /// Test: itself.
    #[test]
    fn budget_never_extends_a_shorter_leg() {
        let budget = OpBudget::start(Duration::from_secs(60));
        assert_eq!(
            budget.leg(Duration::from_millis(1)),
            Duration::from_millis(1)
        );
    }
}
