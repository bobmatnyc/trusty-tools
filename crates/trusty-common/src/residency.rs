//! Active-project residency: the wire contract and the freshness state
//! machine every consumer wraps a pulled set in (#7087 slice 1a).
//!
//! Why: trusty-memory and trusty-search each run a recency-only LRU with no
//! notion of "the operator still has this project open" — a palace or index
//! for a live session ages out exactly like one nobody has touched in weeks,
//! and a restart burst across several active projects can evict all of them
//! at once (#6836). trusty-mpm already knows which projects are active: a
//! managed session with persisted state in `{Active, Provisioning}` whose
//! `tmux_name` still resolves in a live `tmux list-sessions`. Pulling that
//! set and pinning it is cheaper and more honest than teaching two unrelated
//! eviction policies to each re-derive "active" on their own.
//!
//! What: [`ActiveProjectSet`] / [`ActiveProject`] are the wire shape a
//! producer (trusty-mpm, slice 1b) publishes and every consumer (trusty-memory,
//! trusty-search; slices 2 and 3) decodes. [`ResidencySnapshot`] is the
//! injected-clock state machine a consumer's pull ticker drives:
//! [`crate::residency::ResidencySnapshot::observe`] on a successful pull,
//! [`crate::residency::ResidencySnapshot::on_pull_failure`] on a failed one,
//! [`crate::residency::ResidencySnapshot::pinned`] to read the currently-trusted set (or `None`
//! when it has gone stale or was never fetched). Every time parameter is a
//! caller-supplied `std::time::Instant` — this module never calls
//! `Instant::now` itself — so a test drives the fresh → stale → restored
//! sequence deterministically instead of racing a real clock.
//!
//! Unconditional (no feature gate): both `mpm_rpc`'s client and a future
//! producer need these types, and neither should have to opt in to `uds` just
//! to share a struct definition.
//!
//! Test: `residency_snapshot_is_not_fresh_before_the_first_observe`,
//! `residency_snapshot_stays_pinned_across_a_pull_failure_while_fresh`,
//! `residency_snapshot_clears_once_stale`,
//! `residency_snapshot_observe_restores_after_staleness`,
//! `residency_pull_secs_*`, `residency_stale_secs_*`,
//! `residency_grace_secs_*`, `residency_enabled_*`.

// #7087

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Wire schema version for [`ActiveProjectSet`].
///
/// Why a version field at all: a producer and a consumer are two different
/// binaries that upgrade independently, and `#[serde(default)]` on every field
/// already lets an old consumer read a newer, wider payload — this is the
/// escape hatch for the day a change is not additive and a consumer needs to
/// refuse rather than silently misread it. Nothing in slice 1a enforces it;
/// that is a decision for whichever slice first needs to.
pub const ACTIVE_PROJECT_SET_SCHEMA: u32 = 1;

/// Every project trusty-mpm currently considers active, as of one pull.
///
/// `#[non_exhaustive]`: a producer or consumer built against a future minor
/// version must not have its build broken by a field this crate adds. Combined
/// with `#[derive(Default)]`, an external crate still constructs one with
/// `ActiveProjectSet { generation, projects, ..Default::default() }`.
///
/// Test: `fetch_active_projects_at_round_trips_a_set` (in [`crate::mpm_rpc`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ActiveProjectSet {
    /// [`ACTIVE_PROJECT_SET_SCHEMA`] the producer published under.
    #[serde(default)]
    pub schema: u32,
    /// Monotonically increasing counter the producer bumps on every session
    /// start, stop, adopt, or decommission. Not currently consumed by anything
    /// in this crate — a future consumer uses it to detect a set it has
    /// already applied and skip redundant pin/evict work.
    #[serde(default)]
    pub generation: u64,
    /// Unix timestamp (seconds) the producer published this set at.
    #[serde(default)]
    pub published_at_unix: u64,
    /// The active projects themselves, in no particular order.
    #[serde(default)]
    pub projects: Vec<ActiveProject>,
}

/// One project trusty-mpm considers active.
///
/// Test: `fetch_active_projects_at_round_trips_a_set` (in [`crate::mpm_rpc`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ActiveProject {
    /// The project's working directory: `workspace_path` if the managed
    /// session set one, else the session's `cwd`.
    #[serde(default)]
    pub root: PathBuf,
    /// The trusty-memory palace slug this root resolves to, when the producer
    /// could derive one. `None` for a root with no palace yet (a fresh
    /// checkout nobody has opened trusty-memory against).
    #[serde(default)]
    pub palace_id: Option<String>,
    /// The trusty-search index id(s) this root resolves to. Usually one; two
    /// when `root` is a git worktree, in which case the base checkout's index
    /// id is included alongside the worktree's own.
    #[serde(default)]
    pub index_ids: Vec<String>,
    /// Every managed session id currently keeping this project active.
    #[serde(default)]
    pub session_ids: Vec<String>,
    /// Unix timestamp (seconds) of the most recent activity across
    /// `session_ids`, when the producer tracks one.
    #[serde(default)]
    pub last_activity_unix: Option<u64>,
}

/// The freshness state machine a consumer's pull ticker drives.
///
/// Why a type rather than the raw `Option<ActiveProjectSet>` a ticker might
/// otherwise hold directly: "pin this set" and "the set is old enough that
/// pinning it would be wrong" are two different questions, and a consumer that
/// answered them ad hoc at each call site is exactly the drift this shared
/// type exists to prevent (trusty-memory's registry and trusty-search's
/// residency sweep, slices 2 and 3, both need the same answer). Every method
/// takes `now` as an [`Instant`] the caller supplies — this type never reads
/// the clock itself — so a test can walk fresh → stale → restored without a
/// real sleep.
///
/// What "stale" means: no successful [`observe`] within `stale_after` of
/// `now`. A pull that has never succeeded is stale by definition, and
/// [`on_pull_failure`] eagerly drops the held set once staleness is detected —
/// not because [`pinned`] cannot compute the same answer live (it does,
/// independently), but so a consumer that never calls [`pinned`] does not keep
/// holding an arbitrarily old set in memory.
///
/// [`observe`]: crate::residency::ResidencySnapshot::observe
/// [`on_pull_failure`]: crate::residency::ResidencySnapshot::on_pull_failure
/// [`pinned`]: crate::residency::ResidencySnapshot::pinned
///
/// Test: `residency_snapshot_is_not_fresh_before_the_first_observe`,
/// `residency_snapshot_stays_pinned_across_a_pull_failure_while_fresh`,
/// `residency_snapshot_clears_once_stale`,
/// `residency_snapshot_observe_restores_after_staleness`.
#[derive(Debug, Clone)]
pub struct ResidencySnapshot {
    stale_after: Duration,
    set: Option<ActiveProjectSet>,
    last_success: Option<Instant>,
}

impl ResidencySnapshot {
    /// A snapshot with nothing observed yet.
    ///
    /// `stale_after` is normally [`residency_stale_secs`] turned into a
    /// [`Duration`] by the caller; passed explicitly here rather than read
    /// from the environment so this type stays clock- and env-free.
    pub fn new(stale_after: Duration) -> Self {
        Self {
            stale_after,
            set: None,
            last_success: None,
        }
    }

    /// Record a successful pull.
    ///
    /// What: replaces the held set unconditionally and marks `now` as the last
    /// success, which is what [`is_fresh`](Self::is_fresh) measures forward
    /// from.
    pub fn observe(&mut self, set: ActiveProjectSet, now: Instant) {
        self.set = Some(set);
        self.last_success = Some(now);
    }

    /// Record a failed pull.
    ///
    /// What: leaves the held set untouched while it is still fresh — a
    /// transient failure must not evict a project the last successful pull
    /// said was active. Once [`is_fresh`](Self::is_fresh) says otherwise, the
    /// set is dropped rather than left to answer `None` from
    /// [`pinned`](Self::pinned) while still occupying memory.
    pub fn on_pull_failure(&mut self, now: Instant) {
        if !self.is_fresh(now) {
            self.set = None;
        }
    }

    /// Has a pull succeeded within `stale_after` of `now`?
    ///
    /// `false` when nothing has ever been observed — staleness and "never
    /// fetched" are deliberately the same answer, per the module doc.
    pub fn is_fresh(&self, now: Instant) -> bool {
        self.last_success
            .is_some_and(|last| now.saturating_duration_since(last) < self.stale_after)
    }

    /// The currently-trusted set, or `None` when it is stale or was never
    /// fetched.
    ///
    /// A live computation from [`is_fresh`](Self::is_fresh) rather than a
    /// flag [`on_pull_failure`](Self::on_pull_failure) sets — so a caller that
    /// only ever calls [`observe`](Self::observe) (never
    /// [`on_pull_failure`](Self::on_pull_failure)) still gets a correct answer
    /// once enough time passes with no successful pull.
    pub fn pinned(&self, now: Instant) -> Option<&ActiveProjectSet> {
        if self.is_fresh(now) {
            self.set.as_ref()
        } else {
            None
        }
    }
}

/// Default pull interval, in seconds, when [`RESIDENCY_PULL_SECS_ENV`] is
/// unset or unparsable.
pub const DEFAULT_RESIDENCY_PULL_SECS: u64 = 30;

/// Default staleness window, in seconds, when [`RESIDENCY_STALE_SECS_ENV`] is
/// unset or unparsable.
pub const DEFAULT_RESIDENCY_STALE_SECS: u64 = 600;

/// Default eviction grace period, in seconds, when
/// [`RESIDENCY_GRACE_SECS_ENV`] is unset or unparsable.
pub const DEFAULT_RESIDENCY_GRACE_SECS: u64 = 120;

/// Environment variable naming how often a consumer's pull ticker fires.
pub const RESIDENCY_PULL_SECS_ENV: &str = "TRUSTY_RESIDENCY_PULL_SECS";

/// Environment variable naming [`ResidencySnapshot`]'s staleness window.
pub const RESIDENCY_STALE_SECS_ENV: &str = "TRUSTY_RESIDENCY_STALE_SECS";

/// Environment variable naming how long a non-active resident handle idles
/// before a consumer parks or evicts it.
pub const RESIDENCY_GRACE_SECS_ENV: &str = "TRUSTY_RESIDENCY_GRACE_SECS";

/// Environment variable that disables residency pinning outright when set to
/// `off`.
pub const RESIDENCY_ENABLED_ENV: &str = "TRUSTY_RESIDENCY";

/// Parse [`RESIDENCY_PULL_SECS_ENV`]'s raw value.
///
/// Takes the already-read `Option<&str>` rather than reading the environment
/// itself, so a test exercises the parsing rule directly with a literal
/// instead of mutating process-global state. Default
/// [`DEFAULT_RESIDENCY_PULL_SECS`] on absence, empty, or anything that does
/// not parse as a non-negative integer.
///
/// Test: `residency_pull_secs_defaults_on_absence`,
/// `residency_pull_secs_defaults_on_garbage`,
/// `residency_pull_secs_reads_a_valid_value`.
pub fn residency_pull_secs(raw: Option<&str>) -> u64 {
    parse_secs(raw, DEFAULT_RESIDENCY_PULL_SECS)
}

/// Parse [`RESIDENCY_STALE_SECS_ENV`]'s raw value.
///
/// Same contract as [`residency_pull_secs`]; defaults to
/// [`DEFAULT_RESIDENCY_STALE_SECS`].
///
/// Test: `residency_stale_secs_defaults_on_absence`,
/// `residency_stale_secs_defaults_on_garbage`,
/// `residency_stale_secs_reads_a_valid_value`.
pub fn residency_stale_secs(raw: Option<&str>) -> u64 {
    parse_secs(raw, DEFAULT_RESIDENCY_STALE_SECS)
}

/// Parse [`RESIDENCY_GRACE_SECS_ENV`]'s raw value.
///
/// Same contract as [`residency_pull_secs`]; defaults to
/// [`DEFAULT_RESIDENCY_GRACE_SECS`].
///
/// Test: `residency_grace_secs_defaults_on_absence`,
/// `residency_grace_secs_defaults_on_garbage`,
/// `residency_grace_secs_reads_a_valid_value`.
pub fn residency_grace_secs(raw: Option<&str>) -> u64 {
    parse_secs(raw, DEFAULT_RESIDENCY_GRACE_SECS)
}

/// Parse [`RESIDENCY_ENABLED_ENV`]'s raw value.
///
/// `true` unless the trimmed value case-insensitively equals `off` — anything
/// else, including garbage, unset, and empty, leaves residency pinning on. A
/// typo in this variable must never silently disable pinning.
///
/// Test: `residency_enabled_is_true_by_default`,
/// `residency_enabled_is_false_for_off`,
/// `residency_enabled_ignores_case_and_whitespace`,
/// `residency_enabled_stays_true_for_garbage`.
pub fn residency_enabled(raw: Option<&str>) -> bool {
    !matches!(raw.map(str::trim), Some(v) if v.eq_ignore_ascii_case("off"))
}

/// Shared parsing rule behind the three `residency_*_secs` functions: a
/// trimmed, non-empty, valid `u64` wins; anything else falls back to
/// `default`.
fn parse_secs(raw: Option<&str>, default: u64) -> u64 {
    match raw.map(str::trim) {
        Some(trimmed) if !trimmed.is_empty() => trimmed.parse::<u64>().unwrap_or(default),
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_set(generation: u64) -> ActiveProjectSet {
        ActiveProjectSet {
            schema: ACTIVE_PROJECT_SET_SCHEMA,
            generation,
            published_at_unix: 1_700_000_000,
            projects: vec![ActiveProject {
                root: PathBuf::from("/repo"),
                palace_id: Some("palace-1".to_string()),
                ..Default::default()
            }],
        }
    }

    /// Why: "stale" and "never fetched" are deliberately the same answer —
    /// see the module doc — and this is the base case nothing else builds on.
    /// Test: itself.
    #[test]
    fn residency_snapshot_is_not_fresh_before_the_first_observe() {
        let snapshot = ResidencySnapshot::new(Duration::from_secs(10));
        let now = Instant::now();
        assert!(!snapshot.is_fresh(now));
        assert_eq!(snapshot.pinned(now), None);
    }

    /// Why: a transient pull failure must not evict a project the last
    /// successful pull said was active — that is the whole point of pinning
    /// rather than re-deriving "active" on every tick.
    /// Test: itself.
    #[test]
    fn residency_snapshot_stays_pinned_across_a_pull_failure_while_fresh() {
        let mut snapshot = ResidencySnapshot::new(Duration::from_secs(10));
        let t0 = Instant::now();
        let set = sample_set(1);
        snapshot.observe(set.clone(), t0);

        let t1 = t0 + Duration::from_secs(5);
        snapshot.on_pull_failure(t1);

        assert_eq!(snapshot.pinned(t1), Some(&set));
    }

    /// Why: today's recency-only eviction is the only evictor once the
    /// producer has been unreachable long enough — an unreachable producer
    /// must never be read as "an empty active set" (that would evict
    /// everything at once instead of falling back to today's behavior).
    /// Test: itself.
    #[test]
    fn residency_snapshot_clears_once_stale() {
        let mut snapshot = ResidencySnapshot::new(Duration::from_secs(10));
        let t0 = Instant::now();
        snapshot.observe(sample_set(1), t0);

        let t1 = t0 + Duration::from_secs(20);
        snapshot.on_pull_failure(t1);

        assert!(!snapshot.is_fresh(t1));
        assert_eq!(snapshot.pinned(t1), None);
    }

    /// Why: a producer that comes back after an outage must be trusted again
    /// immediately, not held to the staleness window of the pull that never
    /// happened.
    /// Test: itself.
    #[test]
    fn residency_snapshot_observe_restores_after_staleness() {
        let mut snapshot = ResidencySnapshot::new(Duration::from_secs(10));
        let t0 = Instant::now();
        snapshot.observe(sample_set(1), t0);

        let t1 = t0 + Duration::from_secs(20);
        snapshot.on_pull_failure(t1);
        assert_eq!(snapshot.pinned(t1), None);

        let t2 = t0 + Duration::from_secs(21);
        let restored = sample_set(2);
        snapshot.observe(restored.clone(), t2);

        assert_eq!(snapshot.pinned(t2), Some(&restored));
    }

    #[test]
    fn residency_pull_secs_defaults_on_absence() {
        assert_eq!(residency_pull_secs(None), DEFAULT_RESIDENCY_PULL_SECS);
    }

    #[test]
    fn residency_pull_secs_defaults_on_garbage() {
        assert_eq!(
            residency_pull_secs(Some("not-a-number")),
            DEFAULT_RESIDENCY_PULL_SECS
        );
        assert_eq!(residency_pull_secs(Some("")), DEFAULT_RESIDENCY_PULL_SECS);
        assert_eq!(residency_pull_secs(Some("  ")), DEFAULT_RESIDENCY_PULL_SECS);
        assert_eq!(residency_pull_secs(Some("-5")), DEFAULT_RESIDENCY_PULL_SECS);
    }

    #[test]
    fn residency_pull_secs_reads_a_valid_value() {
        assert_eq!(residency_pull_secs(Some("45")), 45);
        assert_eq!(residency_pull_secs(Some(" 45 ")), 45);
    }

    #[test]
    fn residency_stale_secs_defaults_on_absence() {
        assert_eq!(residency_stale_secs(None), DEFAULT_RESIDENCY_STALE_SECS);
    }

    #[test]
    fn residency_stale_secs_defaults_on_garbage() {
        assert_eq!(
            residency_stale_secs(Some("nope")),
            DEFAULT_RESIDENCY_STALE_SECS
        );
    }

    #[test]
    fn residency_stale_secs_reads_a_valid_value() {
        assert_eq!(residency_stale_secs(Some("900")), 900);
    }

    #[test]
    fn residency_grace_secs_defaults_on_absence() {
        assert_eq!(residency_grace_secs(None), DEFAULT_RESIDENCY_GRACE_SECS);
    }

    #[test]
    fn residency_grace_secs_defaults_on_garbage() {
        assert_eq!(
            residency_grace_secs(Some("off")),
            DEFAULT_RESIDENCY_GRACE_SECS
        );
    }

    #[test]
    fn residency_grace_secs_reads_a_valid_value() {
        assert_eq!(residency_grace_secs(Some("60")), 60);
    }

    #[test]
    fn residency_enabled_is_true_by_default() {
        assert!(residency_enabled(None));
    }

    #[test]
    fn residency_enabled_is_false_for_off() {
        assert!(!residency_enabled(Some("off")));
    }

    #[test]
    fn residency_enabled_ignores_case_and_whitespace() {
        assert!(!residency_enabled(Some("  OFF  ")));
        assert!(!residency_enabled(Some("Off")));
    }

    #[test]
    fn residency_enabled_stays_true_for_garbage() {
        assert!(residency_enabled(Some("nah")));
        assert!(residency_enabled(Some("")));
        assert!(residency_enabled(Some("0")));
    }
}
