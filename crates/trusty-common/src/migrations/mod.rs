//! Reusable schema-migration kernel for the trusty-* ecosystem (issue #179).
//!
//! Why: every long-lived trusty-* store ships its own ad-hoc "is this schema
//! version N or N-1?" branch. `trusty-search` migrated `chunks.json → redb`,
//! `trusty-memory` lazy-upgrades legacy drawer rows, the vector-store lane
//! drains `.usearch` files behind a `usearch-migrate` feature, and the palace
//! store stamps a `schema_version` field that nothing ever reads. The code is
//! correct in isolation but drifts subtly across crates and the next migration
//! starts from scratch every time. This module is the single shared kernel:
//! a `SchemaVersion` stamp, a `Migration` trait, and a `MigrationRunner` that
//! applies pending steps in order and writes the stamp after each.
//!
//! What: pure-data API gated behind the `migrations` feature flag. No new
//! crate-level dependencies — uses `anyhow` and `serde` which the workspace
//! already requires. Helper modules cover the two common stamp formats
//! (`file_stamp` for a JSON sidecar; `redb_stamp` is documentation-only
//! because redb is not a trusty-common dependency).
//!
//! Test: `cargo test -p trusty-common --features migrations` exercises the
//! runner ordering, crash-resumption semantics, and the file-stamp round-trip.

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub mod file_stamp;
pub mod redb_stamp;

/// A u32 schema version stamp.
///
/// Why: every persisted store needs an unambiguous "what schema is this on?"
/// answer that survives a process restart. A monotonically-increasing `u32` is
/// the smallest workable representation — it round-trips through JSON,
/// postcard, and redb without surprises, and the explicit `UNVERSIONED` zero
/// sentinel lets a brand-new store self-identify as "pre-migration" without
/// requiring callers to special-case `None`.
/// What: a newtype around `u32`. `0` is reserved as [`Self::UNVERSIONED`] —
/// every real schema starts at version 1. Comparison is the underlying
/// integer order, so a `MigrationRunner` can ask "is the on-disk version
/// strictly less than the target?" with the natural `<` operator.
/// Test: `schema_version_ordering` confirms `UNVERSIONED < SchemaVersion(1)
/// < SchemaVersion(2)`; round-trip through serde is covered by the
/// `file_stamp_roundtrip` test in `file_stamp::tests`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct SchemaVersion(pub u32);

impl SchemaVersion {
    /// Sentinel for stores that have never recorded a schema version — they
    /// were created before the migration kernel existed (or were just
    /// initialised and haven't run their first migration yet).
    ///
    /// Why: callers want to distinguish "fresh store, run every migration"
    /// from "version 1, only run migrations 2 onwards" without resorting to
    /// `Option<SchemaVersion>`. Reserving 0 as a sentinel keeps the type
    /// `Copy` and makes the "no stamp on disk → default to UNVERSIONED"
    /// fallback trivial.
    /// What: `SchemaVersion(0)`. By convention, no [`Migration`] should
    /// declare `from_version() == UNVERSIONED.next()` of itself — the very
    /// first migration step in any store runs from `UNVERSIONED` to
    /// `SchemaVersion(1)`.
    /// Test: `schema_version_ordering` plus `runner_applies_pending_steps`
    /// (which starts at `UNVERSIONED` and walks through two steps).
    pub const UNVERSIONED: Self = Self(0);

    /// Return the version that immediately follows this one.
    ///
    /// Why: the [`MigrationRunner`] writes the stamp as
    /// `step.from_version().next()` after each successful migration. Lifting
    /// the `+ 1` into a named method keeps that hot path readable and the
    /// invariant ("each step advances exactly one major version") visible in
    /// the type's API.
    /// What: returns `SchemaVersion(self.0 + 1)`. Saturates at `u32::MAX`
    /// (`saturating_add(1)`) so a malformed schema version on disk can never
    /// trigger arithmetic-overflow undefined behaviour or panic — instead the
    /// next runner pass observes `current >= target` and exits cleanly.
    /// Test: covered by the runner tests, which assert the stamp written
    /// after step `n` equals `SchemaVersion(n + 1)`.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl std::fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// A single ordered migration step that moves a store from `from_version`
/// to `from_version.next()`.
///
/// Why: every migration the trusty-* ecosystem has shipped fits the same
/// shape — "given a store that is currently on version N, do the work to
/// make it version N+1." Encoding that shape as a trait gives the
/// [`MigrationRunner`] one composable abstraction to register and dispatch
/// against, and forces new migrations to declare their starting version
/// explicitly (no more "this one runs first" comments). The generic `S`
/// parameter is whatever store handle the migration body operates on
/// (a `CodeIndexer`, a `PalaceStore`, a `redb::Database`, …) so the kernel
/// is store-agnostic.
/// What: three methods. `from_version` is the version this step expects to
/// see on disk before it runs (the runner skips a step whose
/// `from_version` is `< current`). `label` is a short human-readable
/// identifier for logs (`"chunks.json → index.redb"`, `"drop legacy
/// drawers table"`). `apply` performs the actual migration — it owns the
/// store reference and returns `Result<()>` so failures surface to the
/// runner, which then leaves the stamp at the previous version so a retry
/// can resume.
/// Test: covered by `runner_applies_pending_steps`,
/// `runner_skips_already_applied`, and `runner_is_incremental_on_crash`
/// in this module's tests.
pub trait Migration<S>: Send + Sync {
    /// Schema version this step expects on disk before it runs.
    ///
    /// Why: the runner uses this as the dispatch key. A step whose
    /// `from_version()` is strictly less than the current on-disk version
    /// is treated as already-applied and skipped; otherwise the runner
    /// fires `apply(store)` and writes the stamp at `from_version.next()`.
    /// What: a [`SchemaVersion`]. The very first migration of any store
    /// returns [`SchemaVersion::UNVERSIONED`].
    ///
    /// `clippy::wrong_self_convention` would prefer `from_*` to take no
    /// `&self`, but this method is the trait's central accessor — it asks
    /// "what version does *this* step expect?" so taking `&self` is the
    /// correct shape for a `Box<dyn Migration<S>>` dispatch table.
    #[allow(clippy::wrong_self_convention)]
    fn from_version(&self) -> SchemaVersion;

    /// Short human-readable label used in `tracing` logs.
    ///
    /// Why: operators reading daemon logs need to see *what* the runner is
    /// doing without grepping source. A static `&'static str` keeps the
    /// trait object-safe without forcing the migration body to materialise
    /// a `String` on every call.
    /// What: a `&'static str`. Should be terse — "JSON corpus → redb",
    /// "drop community tables".
    fn label(&self) -> &'static str;

    /// Apply the migration to `store`.
    ///
    /// Why: this is where the actual data movement / schema rewrite
    /// happens. The runner guarantees `apply` is called exactly once per
    /// step per `run()` invocation, in increasing version order, and only
    /// when the on-disk version permits it (so the migration body never
    /// has to defend against "am I already applied?" itself).
    /// What: receives `&S` (the store handle is shared by reference so the
    /// caller can keep using the store afterwards). Returns
    /// `Result<()>` — an `Err` aborts the runner, leaving the on-disk
    /// stamp at the previous version so a later `run()` resumes from the
    /// same step.
    fn apply(&self, store: &S) -> Result<()>;
}

/// Applies pending [`Migration`] steps in order, writing a [`SchemaVersion`]
/// stamp after each successful application.
///
/// Why: the trusty-* ecosystem has at least four open-coded migration loops
/// today, each subtly different (some forget to stamp after success, some
/// stamp before running so a crash mid-step appears completed). Centralising
/// the loop into a runner that owns the ordering, the skip-if-already-applied
/// logic, and the stamp-after-success contract removes that drift entirely.
/// The closure-based `write_stamp` callback keeps the runner storage-agnostic
/// — JSON sidecar, redb metadata table, in-memory test stub all share one
/// runner.
/// What: holds the registered steps as `Vec<Box<dyn Migration<S>>>` so each
/// runner instance can mix migrations against the same store type. `run()`
/// sorts by `from_version`, dispatches each pending step (one whose
/// `from_version >= current`), calls `write_stamp(step.from_version.next())`
/// after a successful `apply`, and returns the final reached version. A
/// failing step short-circuits the loop without rewriting the stamp.
/// Test: see the module-level `tests` block.
pub struct MigrationRunner<S> {
    steps: Vec<Box<dyn Migration<S>>>,
}

impl<S> MigrationRunner<S> {
    /// Build a runner from a list of migration steps.
    ///
    /// Why: the call site already owns the migration registry (it is the
    /// only place that knows which migrations exist for that particular
    /// store), so constructing the runner with the registry inline keeps
    /// the configuration co-located with the dispatch.
    /// What: stores the steps verbatim, sorted by `from_version` so the
    /// dispatch loop can walk them in order without re-sorting per call.
    /// Duplicate `from_version` values are permitted but the second is
    /// effectively dead code (the first applies, advances the stamp, the
    /// second's `from_version < current` and is skipped).
    /// Test: ordering is asserted by `runner_applies_pending_steps`.
    pub fn new(mut steps: Vec<Box<dyn Migration<S>>>) -> Self {
        steps.sort_by_key(|s| s.from_version());
        Self { steps }
    }

    /// The schema version this runner advances the store to when every
    /// registered step has been applied.
    ///
    /// Why: callers want to know "is this store fully up to date?" without
    /// having to peek into the step list. Exposing the target version as a
    /// method also makes the runner's intent self-documenting in logs:
    /// `"migrations: target=vN, current=vM"`.
    /// What: returns the last step's `from_version.next()`, or
    /// [`SchemaVersion::UNVERSIONED`] if no steps are registered (which is
    /// not a typical configuration — a runner with zero steps is a no-op).
    /// Test: covered by `runner_target_version_matches_last_step`.
    pub fn target_version(&self) -> SchemaVersion {
        self.steps
            .last()
            .map(|s| s.from_version().next())
            .unwrap_or(SchemaVersion::UNVERSIONED)
    }

    /// Run all pending migrations, returning the final reached version.
    ///
    /// Why: the runner contract is "advance the store from `current` to
    /// `target_version()`, stamping after each successful step so a crash
    /// is recoverable." Returning the reached version lets the caller log
    /// (and assert in tests) where the migration ended up — equal to
    /// `target_version()` on full success, equal to a step's
    /// `from_version()` if that step failed.
    /// What: walks the (already sorted) step list, skipping any step whose
    /// `from_version()` is strictly less than `current`. For each
    /// applicable step, calls `apply(store)`; on success calls
    /// `write_stamp(step.from_version().next())`; on failure returns the
    /// error without rewriting the stamp. A `write_stamp` failure is
    /// treated as a migration failure (the data was migrated but we can no
    /// longer record that it was migrated, which would invite a re-run on
    /// the next boot — explicit failure is safer than silent inconsistency).
    /// Test: `runner_applies_pending_steps`, `runner_skips_already_applied`,
    /// `runner_is_incremental_on_crash`, and
    /// `runner_propagates_write_stamp_failure`.
    pub fn run(
        &self,
        store: &S,
        current: SchemaVersion,
        write_stamp: impl Fn(SchemaVersion) -> Result<()>,
    ) -> Result<SchemaVersion> {
        let mut reached = current;
        for step in &self.steps {
            if step.from_version() < reached {
                tracing::debug!(
                    "migrations: skipping '{}' ({} < {})",
                    step.label(),
                    step.from_version(),
                    reached
                );
                continue;
            }
            tracing::info!(
                "migrations: applying '{}' ({} → {})",
                step.label(),
                step.from_version(),
                step.from_version().next()
            );
            step.apply(store)?;
            let next = step.from_version().next();
            write_stamp(next)?;
            reached = next;
        }
        Ok(reached)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    /// In-memory test store: tracks which steps fired and the last stamp
    /// the runner wrote.
    #[derive(Default)]
    struct TestStore {
        step_0_ran: AtomicBool,
        step_1_ran: AtomicBool,
        step_2_ran: AtomicBool,
        last_stamp: AtomicU32,
    }

    impl TestStore {
        fn stamp(&self) -> SchemaVersion {
            SchemaVersion(self.last_stamp.load(Ordering::SeqCst))
        }
        fn write_stamp(&self) -> impl Fn(SchemaVersion) -> Result<()> + '_ {
            move |v: SchemaVersion| {
                self.last_stamp.store(v.0, Ordering::SeqCst);
                Ok(())
            }
        }
    }

    struct Step0;
    impl Migration<TestStore> for Step0 {
        fn from_version(&self) -> SchemaVersion {
            SchemaVersion::UNVERSIONED
        }
        fn label(&self) -> &'static str {
            "step 0 → 1"
        }
        fn apply(&self, store: &TestStore) -> Result<()> {
            store.step_0_ran.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    struct Step1;
    impl Migration<TestStore> for Step1 {
        fn from_version(&self) -> SchemaVersion {
            SchemaVersion(1)
        }
        fn label(&self) -> &'static str {
            "step 1 → 2"
        }
        fn apply(&self, store: &TestStore) -> Result<()> {
            store.step_1_ran.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    /// A step that always fails — exercises the crash-resumption invariant.
    struct FailingStep2;
    impl Migration<TestStore> for FailingStep2 {
        fn from_version(&self) -> SchemaVersion {
            SchemaVersion(2)
        }
        fn label(&self) -> &'static str {
            "step 2 → 3 (failing)"
        }
        fn apply(&self, store: &TestStore) -> Result<()> {
            store.step_2_ran.store(true, Ordering::SeqCst);
            Err(anyhow::anyhow!("simulated migration failure"))
        }
    }

    #[test]
    fn schema_version_ordering() {
        // Why: the runner relies on `<` to decide which steps are pending.
        // What: confirm UNVERSIONED is strictly less than v1, v1 < v2, and
        // equality round-trips.
        assert!(SchemaVersion::UNVERSIONED < SchemaVersion(1));
        assert!(SchemaVersion(1) < SchemaVersion(2));
        assert_eq!(SchemaVersion::UNVERSIONED, SchemaVersion(0));
        assert_eq!(SchemaVersion(5).next(), SchemaVersion(6));
        // `next()` saturates so a corrupted-on-disk MAX never panics.
        assert_eq!(SchemaVersion(u32::MAX).next(), SchemaVersion(u32::MAX));
    }

    #[test]
    fn runner_applies_pending_steps() {
        // Why: baseline correctness — start at UNVERSIONED, two steps, both
        // should fire in order and the final stamp should equal the target.
        let store = TestStore::default();
        let runner: MigrationRunner<TestStore> =
            MigrationRunner::new(vec![Box::new(Step0), Box::new(Step1)]);
        let reached = runner
            .run(&store, SchemaVersion::UNVERSIONED, store.write_stamp())
            .expect("two-step migration succeeds");

        assert!(store.step_0_ran.load(Ordering::SeqCst));
        assert!(store.step_1_ran.load(Ordering::SeqCst));
        assert_eq!(reached, SchemaVersion(2));
        assert_eq!(store.stamp(), SchemaVersion(2));
        assert_eq!(runner.target_version(), SchemaVersion(2));
    }

    #[test]
    fn runner_skips_already_applied() {
        // Why: starting at v1 means step 0 → 1 is already applied; only step
        // 1 → 2 should fire.
        let store = TestStore::default();
        let runner: MigrationRunner<TestStore> =
            MigrationRunner::new(vec![Box::new(Step0), Box::new(Step1)]);
        let reached = runner
            .run(&store, SchemaVersion(1), store.write_stamp())
            .expect("resume-from-v1 succeeds");

        assert!(
            !store.step_0_ran.load(Ordering::SeqCst),
            "step 0 should be skipped when current >= v1"
        );
        assert!(store.step_1_ran.load(Ordering::SeqCst));
        assert_eq!(reached, SchemaVersion(2));
        assert_eq!(store.stamp(), SchemaVersion(2));
    }

    #[test]
    fn runner_is_incremental_on_crash() {
        // Why: the central crash-safety invariant — if step 2 → 3 fails, the
        // stamp written for step 1 → 2 must persist (so a retry resumes at v2,
        // not at the beginning).
        let store = TestStore::default();
        let runner: MigrationRunner<TestStore> = MigrationRunner::new(vec![
            Box::new(Step0),
            Box::new(Step1),
            Box::new(FailingStep2),
        ]);
        let err = runner
            .run(&store, SchemaVersion::UNVERSIONED, store.write_stamp())
            .expect_err("FailingStep2 should fail");
        assert!(
            err.to_string().contains("simulated migration failure"),
            "unexpected error message: {err}"
        );
        assert!(store.step_0_ran.load(Ordering::SeqCst));
        assert!(store.step_1_ran.load(Ordering::SeqCst));
        assert!(store.step_2_ran.load(Ordering::SeqCst));
        // Stamp is at v2 because step 1 → 2 ran and stamped, then step 2 → 3
        // fired but errored *after* writing nothing.
        assert_eq!(store.stamp(), SchemaVersion(2));
    }

    #[test]
    fn runner_propagates_write_stamp_failure() {
        // Why: a write_stamp failure must surface to the caller — silently
        // proceeding would leave an unrecorded migration.
        let store = TestStore::default();
        let runner: MigrationRunner<TestStore> = MigrationRunner::new(vec![Box::new(Step0)]);
        let err = runner
            .run(&store, SchemaVersion::UNVERSIONED, |_v| {
                Err(anyhow::anyhow!("stamp write failed"))
            })
            .expect_err("write_stamp failure should propagate");
        assert!(err.to_string().contains("stamp write failed"));
        // The migration body ran, but the runner correctly reported failure.
        assert!(store.step_0_ran.load(Ordering::SeqCst));
    }

    #[test]
    fn runner_target_version_matches_last_step() {
        // Why: documented contract — target_version() is the stamp the runner
        // writes after the final step.
        let runner_empty: MigrationRunner<TestStore> = MigrationRunner::new(vec![]);
        assert_eq!(runner_empty.target_version(), SchemaVersion::UNVERSIONED);

        let runner_two: MigrationRunner<TestStore> =
            MigrationRunner::new(vec![Box::new(Step0), Box::new(Step1)]);
        assert_eq!(runner_two.target_version(), SchemaVersion(2));
    }

    #[test]
    fn runner_handles_out_of_order_step_registration() {
        // Why: the constructor must sort the steps so callers can register
        // them in any order — useful when many small migration modules each
        // contribute one step to a shared `Vec`.
        let store = TestStore::default();
        let runner: MigrationRunner<TestStore> =
            MigrationRunner::new(vec![Box::new(Step1), Box::new(Step0)]);
        runner
            .run(&store, SchemaVersion::UNVERSIONED, store.write_stamp())
            .expect("out-of-order registration runs in version order");
        assert!(store.step_0_ran.load(Ordering::SeqCst));
        assert!(store.step_1_ran.load(Ordering::SeqCst));
        assert_eq!(store.stamp(), SchemaVersion(2));
    }
}
