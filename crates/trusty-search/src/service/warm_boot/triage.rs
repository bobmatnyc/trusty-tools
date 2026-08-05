//! Root-existence triage and the salvage budget for warm-boot (#4846).
//!
//! Why: warm-boot used to hand every `indexes.toml` entry to the same restore
//! loop under the same per-index deadline. An entry whose `root_path` was
//! deleted took `restore_one_index` → `try_locate_moved_root` →
//! `scan_roots_for_colocated_indexes`, a depth-5 recursive `read_dir` +
//! `canonicalize` walk of every tracked root — **recomputed from scratch for
//! each dead entry**, even though the walk's inputs and its result are
//! identical for all of them within one boot. Measured on the reporting
//! machine's own registry (248 tracked roots, warm page cache): 9.5–10.5 s per
//! call. With 55 dead entries that is ~9 minutes of duplicated walking, and it
//! is what starved a live 70k-chunk index for the better part of an hour.
//!
//! What: [`triage_entries`] settles each entry with one `exists()` stat and
//! returns two SEPARATE vectors. The restore loop iterates `present` and has
//! no access to `missing`, so "live indexes go first" is a property of the
//! types rather than of loop ordering that a later edit could perturb.
//! [`SalvageBudget`] then caps the whole missing-root cohort's wall time
//! GLOBALLY and hands out [`SalvageGrant`]s — an unforgeable token (private
//! field, mintable only by the budget) that the relocation scan requires. A
//! spent budget therefore cannot pay for another walk, and because the present
//! cohort never consults the budget at all, exhaustion is structurally unable
//! to cost a live index anything.
//!
//! Nothing here deletes, deregisters, or rewrites anything. A missing root is
//! a reason to spend less time on an entry, never a reason to destroy it —
//! `index_status` returns 404 for an unloaded index whether it holds 0 chunks
//! or 70,180 (#4846 operator note), so a failed probe is not evidence about
//! the corpus.
//!
//! Test: `triage_*` and `salvage_budget_*` unit tests below.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::service::persistence::PersistedIndex;

/// Environment variable capping total warm-boot time spent on entries whose
/// `root_path` is missing, in seconds. `0` disables salvage entirely.
pub const SALVAGE_BUDGET_ENV: &str = "TRUSTY_WARMBOOT_SALVAGE_SECS";

/// Default global salvage budget: 30 seconds (#4846).
///
/// Why: the shared relocation scan measured 9.5–10.5 s on the reporting
/// machine's 248 tracked roots, so 30 s leaves room for a slower estate while
/// still bounding the cohort at a small constant instead of
/// `dead_entries × 10 s`. It is a ceiling on the whole cohort, not a per-entry
/// allowance — a per-entry allowance is precisely the shape that let 375 dead
/// entries consume an hour of boot.
const DEFAULT_SALVAGE_BUDGET_SECS: u64 = 30;

/// Warm-boot entries split by the cheapest evidence available about their root.
///
/// Why: a single `Vec<PersistedIndex>` cannot express "these are live and
/// these are not", so the restore loop had no way to spend its time
/// preferentially and no way to stop a dead entry from drawing on the same
/// deadline a live one needs. Two fields make that misuse unrepresentable
/// rather than merely discouraged.
/// What: `present` — `root_path` resolved; `missing` — it did not. Order
/// within each vector is the input order.
/// Test: `triage_splits_present_from_missing`, `triage_preserves_order`.
#[derive(Debug, Default)]
pub struct TriagedEntries {
    /// Entries whose `root_path` exists. These are the only ones the eager
    /// restore loop runs, and the only ones that consume the per-index restore
    /// deadline.
    pub present: Vec<PersistedIndex>,
    /// Entries whose `root_path` does not exist. Settled by a stat; eligible
    /// only for shared, budgeted salvage.
    pub missing: Vec<PersistedIndex>,
}

/// Split `entries` on root-path existence using one stat each (#4846).
///
/// Why: this is the cheap discriminator #4846 asks for. A path that does not
/// exist should cost a `stat`, not a 10-second filesystem walk, and the answer
/// is definitive for the entire class.
/// What: delegates to [`triage_entries_with`] with `Path::exists`.
/// Test: `triage_splits_present_from_missing`.
pub fn triage_entries(entries: Vec<PersistedIndex>) -> TriagedEntries {
    triage_entries_with(entries, |p| p.exists())
}

/// [`triage_entries`] with the existence predicate injected.
///
/// Why: lets the split be unit-tested without creating and deleting real
/// directories, and documents that the ONLY input to the decision is root-path
/// existence — no redb open, no lock, no probe.
/// What: partitions in place, preserving input order within each side.
/// Test: `triage_splits_present_from_missing`, `triage_preserves_order`.
pub fn triage_entries_with<F>(entries: Vec<PersistedIndex>, root_exists: F) -> TriagedEntries
where
    F: Fn(&Path) -> bool,
{
    let (present, missing) = entries.into_iter().partition(|e| root_exists(&e.root_path));
    TriagedEntries { present, missing }
}

/// Permission to spend warm-boot time on a missing-root entry (#4846).
///
/// Why: the relocation scan is the expensive operation this issue is about, so
/// "am I still allowed to pay for it?" must not be a condition a caller can
/// forget to check. Making the scan *require* a value that only
/// [`SalvageBudget::try_grant`] can produce turns the check into a compile-time
/// obligation. Mirrors the private-field token PR #4835 used to make an
/// invariant unforgeable.
/// What: a zero-sized token with a private field, so no code outside this
/// module can construct one.
/// Test: `salvage_budget_grants_until_exhausted`.
#[derive(Debug)]
pub struct SalvageGrant {
    /// Private so `SalvageGrant` cannot be constructed anywhere else.
    _mintable_only_by_budget: (),
}

/// Global wall-clock ceiling on warm-boot's missing-root cohort (#4846).
///
/// Why: the pre-fix budget was effectively per-index and unbounded in
/// aggregate — every entry got its own 10 s, so cost scaled with accumulated
/// registry cruft rather than with real index count. This is the opposite: one
/// ceiling for the whole cohort. The per-index deadline is kept where it is
/// correct (live entries, whose restore really is per-index work) and removed
/// where it was pathological.
/// What: holds an optional deadline. `None` means salvage is disabled — a
/// missing root then costs exactly its triage stat. Exhaustion leaves the
/// remaining entries registered and untouched; it never reaps, rewrites, or
/// deletes anything.
/// Test: `salvage_budget_grants_until_exhausted`,
/// `salvage_budget_disabled_never_grants`, `salvage_budget_secs_env_branches`.
#[derive(Debug)]
pub struct SalvageBudget {
    /// Instant after which no further grant is issued. `None` = disabled.
    deadline: Option<Instant>,
}

impl SalvageBudget {
    /// Build a budget from `TRUSTY_WARMBOOT_SALVAGE_SECS`.
    ///
    /// Why/What: mirrors the crate's other env knobs — `0` disables, a positive
    /// integer sets the ceiling, anything unparseable or unset falls back to
    /// [`DEFAULT_SALVAGE_BUDGET_SECS`].
    /// Test: `salvage_budget_secs_env_branches`.
    pub fn from_env() -> Self {
        Self::with_budget(salvage_budget_secs().map(Duration::from_secs))
    }

    /// Build a budget with an explicit ceiling (`None` disables salvage).
    ///
    /// Test: `salvage_budget_grants_until_exhausted`,
    /// `salvage_budget_disabled_never_grants`.
    pub fn with_budget(budget: Option<Duration>) -> Self {
        Self {
            deadline: budget.map(|d| Instant::now() + d),
        }
    }

    /// Mint a grant if the budget has time left.
    ///
    /// Why: the only way to obtain a [`SalvageGrant`]. Returning `Option`
    /// rather than a `bool` means the caller cannot proceed past an exhausted
    /// budget by ignoring a return value.
    /// What: `None` when salvage is disabled or the deadline has passed.
    /// Test: `salvage_budget_grants_until_exhausted`.
    pub fn try_grant(&self) -> Option<SalvageGrant> {
        match self.deadline {
            Some(deadline) if Instant::now() < deadline => Some(SalvageGrant {
                _mintable_only_by_budget: (),
            }),
            _ => None,
        }
    }
}

/// Resolve the salvage ceiling from the environment.
///
/// Why/What/Test: mirrors `orphan_reaper::reap_interval_secs` exactly — `None`
/// for `0`, otherwise a positive value or the default.
/// Test: `salvage_budget_secs_env_branches`.
pub fn salvage_budget_secs() -> Option<u64> {
    match std::env::var(SALVAGE_BUDGET_ENV) {
        Ok(v) => match v.trim().parse::<u64>() {
            Ok(0) => None,
            Ok(n) => Some(n),
            Err(_) => Some(DEFAULT_SALVAGE_BUDGET_SECS),
        },
        Err(_) => Some(DEFAULT_SALVAGE_BUDGET_SECS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(id: &str, root: &str) -> PersistedIndex {
        PersistedIndex {
            id: id.to_string(),
            root_path: PathBuf::from(root),
            colocated: true,
            ..Default::default()
        }
    }

    /// Why (#4846): the whole fix rests on settling a dead entry with a stat
    /// instead of a probe, so the split itself must be exact.
    /// What: three entries, one live root; assert each lands on the right side.
    /// Test: this test.
    #[test]
    fn triage_splits_present_from_missing() {
        let triaged = triage_entries_with(
            vec![
                entry("live", "/live/root"),
                entry("dead-a", "/dead/a"),
                entry("dead-b", "/dead/b"),
            ],
            |p| p == Path::new("/live/root"),
        );
        assert_eq!(
            triaged
                .present
                .iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>(),
            ["live"]
        );
        assert_eq!(
            triaged
                .missing
                .iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>(),
            ["dead-a", "dead-b"]
        );
    }

    /// Why: warm-boot's eager slice is already ordered by recency
    /// (`select_warmboot_entries`); triage must not reshuffle it, or the
    /// hottest live index could lose its place to a colder one.
    /// What: five live entries stay in input order.
    /// Test: this test.
    #[test]
    fn triage_preserves_order() {
        let entries: Vec<_> = (0..5).map(|i| entry(&format!("i{i}"), "/live")).collect();
        let triaged = triage_entries_with(entries, |_| true);
        assert_eq!(
            triaged
                .present
                .iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>(),
            ["i0", "i1", "i2", "i3", "i4"]
        );
        assert!(triaged.missing.is_empty());
    }

    /// Why (#4846 budget design): a budget with time left must grant, and one
    /// whose ceiling is zero must not — that is the exhaustion path, and it has
    /// to be reachable without waiting on a clock.
    /// What: a generous budget grants; a zero-duration budget does not.
    /// Test: this test.
    #[test]
    fn salvage_budget_grants_until_exhausted() {
        let generous = SalvageBudget::with_budget(Some(Duration::from_secs(60)));
        assert!(generous.try_grant().is_some());
        assert!(
            generous.try_grant().is_some(),
            "a budget with time left keeps granting"
        );

        let spent = SalvageBudget::with_budget(Some(Duration::ZERO));
        assert!(
            spent.try_grant().is_none(),
            "an exhausted budget must not mint a grant — that is what stops the \
             next relocation walk from running"
        );
    }

    /// Why: `TRUSTY_WARMBOOT_SALVAGE_SECS=0` must mean "never walk for a dead
    /// entry", the fastest-boot setting an operator can pick.
    /// Test: this test.
    #[test]
    fn salvage_budget_disabled_never_grants() {
        let disabled = SalvageBudget::with_budget(None);
        assert!(disabled.try_grant().is_none());
    }

    /// Why: the knob must honour `0` and fall back safely, mirroring
    /// `reap_interval_secs`.
    /// Test: this test.
    #[test]
    #[serial_test::serial]
    fn salvage_budget_secs_env_branches() {
        unsafe { std::env::set_var(SALVAGE_BUDGET_ENV, "0") };
        assert_eq!(salvage_budget_secs(), None);
        unsafe { std::env::set_var(SALVAGE_BUDGET_ENV, "5") };
        assert_eq!(salvage_budget_secs(), Some(5));
        unsafe { std::env::set_var(SALVAGE_BUDGET_ENV, "nope") };
        assert_eq!(salvage_budget_secs(), Some(DEFAULT_SALVAGE_BUDGET_SECS));
        unsafe { std::env::remove_var(SALVAGE_BUDGET_ENV) };
        assert_eq!(salvage_budget_secs(), Some(DEFAULT_SALVAGE_BUDGET_SECS));
    }
}
