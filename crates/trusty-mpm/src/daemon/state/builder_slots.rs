//! Machine-wide builder-slot leases, claimed and released on [`DaemonState`]
//! (#6892).
//!
//! Why: the daemon is the only process on the machine that sees every session's
//! delegations, so it is the only one that can answer "how many builders are
//! running HERE" — the question a per-session rule structurally cannot ask. It
//! is also the only place the answer and the claim can be made indivisible,
//! which is what stops two dispatches issued in one PM turn from both seeing a
//! free slot and both taking it. Same shape as
//! [`DaemonState::claim_shared_tree_dispatch`](crate::daemon::state::DaemonState::claim_shared_tree_dispatch),
//! and for the same reason (#5324).
//!
//! What the lease IS: the delegation record the tracker would have written
//! anyway. There is no second kind of state and no separate expiry to clean up
//! — a builder holds a slot for exactly as long as its delegation is live, and
//! [`builder_lease`] is the predicate that decides when that stops being true.
//!
//! **A DENIED dispatch releases too, and that release is this module's own
//! (#6892 critic round).** The three signals below all describe an agent that
//! ran. A dispatch the cap refuses never runs at all, and yet the guard's
//! preceding shared-tree or worktree-grant claim has already recorded it as
//! `Running` — those claim BY recording. Nothing downstream would ever close
//! that record, so the refusal closes it inside the same critical section.
//! #8012: the route passes
//! [`delegation_tracker::release_denied_dispatch`](crate::daemon::services::delegation_tracker::release_denied_dispatch)
//! as that closure, because the record can also be absent — a dispatch that
//! declared its own isolation is never recorded by the shared-tree claim, and
//! the tracker's independent hook can land after the deny. That release
//! tombstones an absent record; [`DaemonState::release_denied_builder_dispatch`],
//! this module's own primitive, closes only one that already exists.
//!
//! **Three independent releases, whichever fires first.** A `SubagentStop` or
//! the staleness sweep moves the record out of
//! [`DelegationStatus::is_live`](crate::core::agent::DelegationStatus::is_live);
//! the dispatching session's PID being confirmed dead releases it without
//! waiting for any signal from the agent; and
//! [`BUILDER_LEASE_TTL_SECS`] releases it regardless of both. All three exist
//! because each covers a hole the others leave — see each variant of
//! [`BuilderLease`].
//! Test: the `#[cfg(test)]` suite below.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::agent::Delegation;
use crate::core::builder_slot_pool::{SeedKind, SlotPool, SlotReservation};
use crate::core::dispatch_isolation::agent_is_builder;
use crate::core::session::SessionId;

use super::core::DaemonState;

/// How long a builder may hold a slot before the lease is released regardless
/// of every other signal.
///
/// Why 45 minutes, and why not
/// [`RUNNING_STALE_AFTER_SECS`](super::sessions::RUNNING_STALE_AFTER_SECS):
/// that constant is six hours and is calibrated for a VANISHED SUBAGENT — an
/// agent that may legitimately still be running a long CI wait. This clock
/// measures something else, a build. A `cargo` run that has held one of a
/// handful of machine-wide slots for three quarters of an hour is either
/// finished and unreported or wedged, and in both cases holding the slot for
/// another five hours starves every other session on the machine of the thing
/// this cap exists to ration. 45 minutes sits above the longest ordinary
/// workspace build observed here (~17 minutes for a full CI leg) with room to
/// spare, and well below the six-hour window.
///
/// Releasing early is survivable in a way holding late is not: the worst case is
/// an admitted (N+1)th builder on a machine sized for N, which is the state
/// every machine was in before this cap existed. The worst case of holding is a
/// machine that admits nothing.
/// What: seconds, measured from `started_at` (falling back to `created_at`).
/// Test: `a_lease_is_released_at_the_ttl_when_the_pid_is_inconclusive`,
/// `a_lease_inside_the_ttl_is_still_held`.
pub const BUILDER_LEASE_TTL_SECS: i64 = 45 * 60;

/// One builder currently holding a slot, as the deny message names it.
///
/// Why: a deny that cannot say WHICH builders are running reads as arbitrary and
/// gets retried identically. Every field here is already on the
/// [`Delegation`] record — no new state, and nothing to keep in sync.
/// What: the agent name, the session that dispatched it, and how long it has
/// been running. `elapsed_secs` is what makes a wedged holder visible: a lease
/// at 44 minutes tells the reader something a name alone does not.
/// Test: `builder_holders_report_the_agent_session_and_elapsed_time`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuilderHolder {
    /// The builder agent's name.
    pub agent: String,
    /// The session that dispatched it.
    pub session: SessionId,
    /// How long it has been running, in seconds.
    pub elapsed_secs: i64,
}

/// Whether one builder-class delegation still holds its slot, and if not, why.
///
/// Why: the three releases must be distinguishable, not merely summed to a
/// boolean. `tm doctor` warns on exactly one of them —
/// [`Self::ReleasedByTtl`] — because a lease that only the TTL could end is a
/// lease whose owner never reported and whose PID could not be checked, which
/// is a signal about the harness rather than about the build.
/// What: [`Self::Held`] carries the elapsed seconds the holder is reported
/// with. The three released variants are ordered by the strength of the
/// evidence behind them, and [`builder_lease`] returns the first that applies.
/// Test: `a_terminal_delegation_holds_nothing`,
/// `a_lease_is_released_when_the_owner_pid_is_dead`,
/// `a_lease_is_released_at_the_ttl_when_the_pid_is_inconclusive`,
/// `a_lease_inside_the_ttl_is_still_held`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuilderLease {
    /// The builder is running and the slot is taken. Carries elapsed seconds.
    Held(i64),
    /// The delegation reached a terminal or stale status — the ordinary end.
    ReleasedByStatus,
    /// The dispatching session's process is confirmed gone. This is the fast
    /// path: a killed PM takes its agents with it and none of them emits a
    /// `SubagentStop`, so without this the slots stay held until the TTL.
    ReleasedByDeadOwner,
    /// Neither of the above could answer and the lease outlived
    /// [`BUILDER_LEASE_TTL_SECS`]. This is the backstop, and the one `tm
    /// doctor` reports: reaching it means nothing else was able to end the
    /// lease.
    ReleasedByTtl,
}

impl BuilderLease {
    /// Is the slot still taken?
    ///
    /// Test: `a_lease_inside_the_ttl_is_still_held`.
    #[must_use]
    pub fn is_held(self) -> bool {
        matches!(self, Self::Held(_))
    }
}

/// Classify one delegation's builder lease.
///
/// Why: pure — it takes the owner's liveness as an argument rather than probing
/// for it — so all four outcomes are assertable without killing a process or
/// waiting 45 minutes, and the I/O lives in
/// [`DaemonState::builder_slot_holders`] next door.
/// What: in order — a non-live status releases; a `Some(false)` owner releases;
/// an elapsed time at or past [`BUILDER_LEASE_TTL_SECS`] releases; anything else
/// is [`BuilderLease::Held`] with that elapsed time. `owner_alive` is `None`
/// when the owning session records no PID, which is INCONCLUSIVE rather than
/// dead: a session the daemon never learned a PID for must not have its builds
/// released on a guess, so it falls through to the TTL. Elapsed is measured from
/// `started_at`, falling back to `created_at` for a record that never learned
/// one, and is clamped at zero so a clock skew cannot manufacture a lease that
/// is instantly past its TTL.
/// Test: `a_terminal_delegation_holds_nothing`,
/// `a_lease_is_released_when_the_owner_pid_is_dead`,
/// `a_lease_is_released_at_the_ttl_when_the_pid_is_inconclusive`,
/// `a_lease_inside_the_ttl_is_still_held`,
/// `an_unknown_owner_pid_does_not_release_the_lease`.
#[must_use]
pub fn builder_lease(
    delegation: &Delegation,
    owner_alive: Option<bool>,
    now: chrono::DateTime<chrono::Utc>,
) -> BuilderLease {
    if !delegation.status.is_live() {
        return BuilderLease::ReleasedByStatus;
    }
    if owner_alive == Some(false) {
        return BuilderLease::ReleasedByDeadOwner;
    }
    let started = delegation.started_at.unwrap_or(delegation.created_at);
    let elapsed = (now - started).num_seconds().max(0);
    if elapsed >= BUILDER_LEASE_TTL_SECS {
        return BuilderLease::ReleasedByTtl;
    }
    BuilderLease::Held(elapsed)
}

/// What the daemon knows about this machine's builder slots right now.
///
/// Why: `tm doctor` needs two lists, not one — who is holding, and which leases
/// only the TTL could have ended. Folding them into a single count would lose
/// exactly the signal the Warn row exists to surface.
/// What: `holders` are the live leases the cap is decided from; `expired` are
/// builder delegations whose records still look live but whose lease has passed
/// [`BUILDER_LEASE_TTL_SECS`]. `cap` is the machine's effective cap at the
/// moment of the read.
/// Test: `census_separates_holders_from_expired_leases`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuilderSlotCensus {
    /// Builders currently holding a slot.
    pub holders: Vec<BuilderHolder>,
    /// Leases past the TTL that no other signal has ended.
    pub expired: Vec<BuilderHolder>,
    /// The machine's effective `builders.max_concurrent`.
    pub cap: u32,
}

/// What one pool-aware claim resolved to (#8261).
///
/// Why: the claim now answers three things, not two — who holds slots, whether
/// this dispatch took one, and WHICH DIRECTORY it took. A third tuple element
/// would be unreadable at the call site and trips `clippy::type_complexity`.
/// What: `slot_dir`/`slot_seed` are `Some` only when `claimed` is true AND the
/// pool reserved a SEEDED slot; a refused reservation leaves both `None` and
/// `claimed` false, and an unseeded one admits with `seed_index` set and
/// `slot_notice` saying why no directory came with the admission.
/// Test: `an_admitted_builder_records_the_slot_directory_it_was_given`,
/// `a_slot_the_pool_cannot_provide_is_refused_not_admitted_unthrottled`,
/// `an_unseeded_slot_admits_with_no_directory_and_seeds_nothing_inline`.
#[derive(Debug, Default)]
pub struct BuilderSlotGrant {
    /// Builders already holding a slot, excluding this dispatch's own record.
    pub holders: Vec<BuilderHolder>,
    /// Whether this call took a slot.
    pub claimed: bool,
    /// The private `CARGO_TARGET_DIR` this claim was given.
    pub slot_dir: Option<PathBuf>,
    /// How that directory came to exist, rendered.
    pub slot_seed: Option<String>,
    /// Why an admitted builder got NO private directory (#8261 critic round).
    ///
    /// Why: admitting with no directory and no signal leaves the engineer
    /// building in the shared directory believing it was given a slot, which is
    /// indistinguishable from the contention this issue exists to end.
    pub slot_notice: Option<String>,
    /// Why the pool refused this claim outright, when it did.
    ///
    /// Why: a pool refusal is NOT a full machine, and a deny that says "raise
    /// `max_concurrent`" for an unwritable directory sends the reader to the
    /// wrong repair.
    pub slot_refused: Option<String>,
    /// The slot index whose directory still needs [`SlotPool::seed`].
    ///
    /// Why: the seed clones a target directory measured at 207 GB, which cannot
    /// run under the claim mutex — the caller runs it after answering.
    pub seed_index: Option<u32>,
    /// Trash trees of invalidated fingerprints the caller deletes with
    /// [`SlotPool::purge_invalidated`] after answering (#8794).
    ///
    /// Why: a handover moves the previous holder's builds aside under this
    /// mutex; deleting their files there outran the hook's 2-second budget.
    pub purge: Vec<PathBuf>,
}

/// Why an admitted builder got no slot index at all (#8261 critic round).
const NO_INDEX_NOTICE: &str = "no builder slot index could be assigned to this dispatch — its \
     delegation record was not found, so no private cargo target directory was reserved.";

/// Why an admitted builder got no slot: none of the free ones is seeded (#8794).
///
/// What: names the slot being seeded by index only — its directory is not this
/// dispatch's to build in — and the last seed failure on record, if any.
/// Test: `two_admissions_racing_for_an_unseeded_slot_get_no_slot_and_one_seed`.
fn unseeded_notice(index: u32, in_flight: bool, last_failure: Option<String>) -> String {
    let seeding = if in_flight {
        "is already being seeded"
    } else {
        "is being seeded now, off the claim path"
    };
    let failure = last_failure
        .map(|why| format!(" Its previous seed did not complete: {why}."))
        .unwrap_or_default();
    format!(
        "no seeded builder slot was free, so this dispatch was admitted WITHOUT a private cargo \
         target directory and builds in the shared one. An unseeded slot is never handed out \
         (#8794): slot-{index} {seeding}, and a later builder gets it once the seed completes. \
         Do not point CARGO_TARGET_DIR at it.{failure}"
    )
}

/// Why an admitted builder got no slot: no free seeded slot could be handed
/// over, and every index below the ceiling is taken or seeded (#8794).
/// Test: `a_failed_handover_is_not_reseeded_past_the_ceiling`.
fn ceiling_notice(ceiling: u32) -> String {
    format!(
        "no builder slot could be handed to this dispatch and every slot below the ceiling of \
         {ceiling} is held or already seeded, so no new slot is seeded and this dispatch builds \
         in the shared target directory. The daemon log names why each seeded slot was not \
         handed over."
    )
}

/// Longest-running first, then by agent name.
///
/// Why: stable output so a deny message and a doctor row read the same way
/// twice in a row; a `DashMap` scan has no inherent order. #8819: shared with
/// the restored-lease holders, which are merged into the same list.
pub(super) fn sort_holders(rows: &mut [BuilderHolder]) {
    rows.sort_by(|a, b| {
        b.elapsed_secs
            .cmp(&a.elapsed_secs)
            .then_with(|| a.agent.cmp(&b.agent))
    });
}

impl DaemonState {
    /// Every builder currently holding one of this machine's slots.
    ///
    /// Why: deliberately NOT scoped to a session or a directory. The cap is a
    /// property of the machine, so the population is every session's
    /// delegations — the same widening ADR-0048 made for the shared-tree guard
    /// and for the same reason, one step further: there the key was the
    /// directory, here there is no key at all.
    /// What: scans the delegation map for builder-class agents
    /// ([`agent_is_builder`]) whose [`builder_lease`] is
    /// [`BuilderLease::Held`], resolving each owner's liveness from its
    /// session's recorded PID. `exclude_tool_use_id` drops the caller's own
    /// in-flight dispatch: the daemon's `matcher: "*"` hook and the guard's POST
    /// race on the SAME dispatch, and without the exclusion the very first
    /// builder on an idle machine could find itself and be denied.
    /// Test: `builder_holders_report_the_agent_session_and_elapsed_time`,
    /// `a_builder_claim_excludes_the_callers_own_dispatch`,
    /// `non_builder_delegations_hold_no_slot`.
    #[must_use]
    pub fn builder_slot_holders(&self, exclude_tool_use_id: Option<&str>) -> Vec<BuilderHolder> {
        self.builder_leases(exclude_tool_use_id, BuilderLease::is_held)
    }

    /// This machine's builder-slot census, for `tm doctor` (#6892).
    ///
    /// Why: one read answers both of doctor's questions, so the row can never
    /// describe a holder set and an expiry set sampled a moment apart.
    /// What: [`Self::builder_slot_holders`] plus every lease
    /// [`BuilderLease::ReleasedByTtl`] left it, under `cap`. Read-only — it
    /// reaps nothing, because a doctor row that mutated state would make the
    /// diagnosis change the thing diagnosed.
    /// Test: `census_separates_holders_from_expired_leases`.
    #[must_use]
    pub fn builder_slot_census(&self, cap: u32) -> BuilderSlotCensus {
        self.builder_slot_census_with_pool_root(cap, None)
    }

    /// [`Self::builder_slot_census`], counting the restored leases under
    /// `pool_root` among the holders (#8819).
    /// Test: `the_census_counts_restored_leases_once_and_skips_stale_ones_8819`.
    #[must_use]
    pub fn builder_slot_census_with_pool_root(
        &self,
        cap: u32,
        pool_root: Option<&Path>,
    ) -> BuilderSlotCensus {
        BuilderSlotCensus {
            holders: self.builder_slot_holders_with_pool_root(None, pool_root),
            expired: self.builder_leases(None, |lease| lease == BuilderLease::ReleasedByTtl),
            cap,
        }
    }

    /// How many builders this machine has room for right now (#8261).
    ///
    /// Why: the daemon is the process that counts, so it is the process that
    /// must derive N — a `tm` older or newer than the daemon would otherwise
    /// argue for a number the live leases were not admitted under. Same
    /// reasoning that put
    /// [`resolve_max_concurrent`](crate::core::builders::resolve_max_concurrent)
    /// here in #6892, extended from a static cap to a measured one.
    /// What: samples this host's readings, reads the operator's `[builders]`
    /// section for the ceiling and the two limits, counts the current holders,
    /// and runs the pure formula against the daemon's own quiet window. `held`
    /// is passed so a drop in N can never revoke a granted lease.
    ///
    /// `ceiling` is supplied by the caller rather than read here, for the same
    /// testability reason [`builder_slot_op`](crate::daemon::builder_slot_routes::builder_slot_op)
    /// takes its cap: that same resolver reads the operator's real
    /// `~/.trusty-mpm`, and a test driving this would otherwise depend on the
    /// machine it runs on.
    /// Test: `the_daemon_resolves_capacity_against_its_own_quiet_window`.
    #[must_use]
    pub fn builder_capacity(
        &self,
        config: &crate::core::builders::BuildersConfig,
        ceiling: u32,
        exclude_tool_use_id: Option<&str>,
    ) -> crate::core::builder_capacity::Capacity {
        self.builder_capacity_with_pool_root(config, ceiling, exclude_tool_use_id, None)
    }

    /// [`Self::builder_capacity`], counting the restored leases under
    /// `pool_root` as held (#8819).
    /// Test: `the_census_counts_restored_leases_once_and_skips_stale_ones_8819`.
    #[must_use]
    pub fn builder_capacity_with_pool_root(
        &self,
        config: &crate::core::builders::BuildersConfig,
        ceiling: u32,
        exclude_tool_use_id: Option<&str>,
        pool_root: Option<&Path>,
    ) -> crate::core::builder_capacity::Capacity {
        let holders = self.builder_slot_holders_with_pool_root(exclude_tool_use_id, pool_root);
        let held = u32::try_from(holders.len()).unwrap_or(u32::MAX);
        let readings = crate::core::builder_capacity::sample_capacity_readings();
        let mut quiet = self.builder_quiet_window.lock();
        crate::core::builder_capacity::resolve_capacity(
            &readings,
            config,
            ceiling,
            held,
            &mut quiet,
            chrono::Utc::now(),
        )
    }

    /// Answer "who holds a builder slot" and claim one, in one step (#6892).
    ///
    /// Why: asking and acting are two steps, and two dispatches issued in ONE PM
    /// turn — the framework's own documented pattern for parallel work — can
    /// both ask before either is recorded, both see a free slot, and both be
    /// admitted. That is the whole failure this cap exists to prevent, so the
    /// answer and the claim are one operation. `delegations` is a `DashMap`: it
    /// makes each entry atomic, never a scan-then-insert pair.
    ///
    /// What: under
    /// [`builder_claim`](DaemonState::builder_claim_guard)'s mutex, computes
    /// the holder list and, when `eligible` says this dispatch is itself a
    /// builder AND the holders are strictly under `cap`, runs `record` before
    /// releasing. Returns the holders the caller decides on plus whether the
    /// slot was claimed. A second caller arriving concurrently blocks until the
    /// first has recorded, so it sees the fuller list — exactly `cap` dispatches
    /// are admitted however many arrive at once.
    ///
    /// `record` is a closure rather than an inlined write so this method keeps
    /// no opinion about what a delegation record looks like: its only caller
    /// passes the delegation tracker's own `PreToolUse` observer, so the claim
    /// IS the record that tracker would have written milliseconds later, with
    /// the same lifecycle and the same staleness sweep.
    ///
    /// **A refused claim runs `release` instead, and that is not symmetry for
    /// its own sake (#6892 critic round).** By the time this is asked, the
    /// guard's preceding shared-tree or worktree-grant call has ALREADY recorded
    /// a `Running` delegation for this dispatch — both of those claim by
    /// recording on an empty answer. Denying here means the tool never runs, so
    /// no `SubagentStop` will ever close that record and no process will die: it
    /// stays live for the six hours of
    /// [`RUNNING_STALE_AFTER_SECS`](super::sessions::RUNNING_STALE_AFTER_SECS),
    /// occupying both the checkout (so #4480 refuses the re-issue this cap's own
    /// deny message recommends) and a builder slot (so the machine is one
    /// permanently short). Releasing it inside this same critical section is
    /// what makes the deny leave no trace.
    ///
    /// Neither closure may take THIS lock again — it is not reentrant — and
    /// neither may await. Both MAY take
    /// [`dispatch_record_guard`](DaemonState::dispatch_record_guard), and both
    /// passed today do: that is the documented lock order, matching the
    /// shared-tree claim's. This mutex is never taken inside that one, so the
    /// two orders cannot cross.
    /// Test: `builder_cap_admits_up_to_the_cap_and_denies_the_rest`,
    /// `builder_cap_admits_exactly_one_of_two_simultaneous_claims`,
    /// `a_refused_builder_claim_records_nothing`,
    /// `a_non_builder_dispatch_claims_no_slot`,
    /// `a_denied_builder_releases_the_record_the_dispatch_just_claimed`.
    pub fn claim_builder_slot<C: FnOnce(&Self), R: FnOnce(&Self)>(
        &self,
        cap: u32,
        exclude_tool_use_id: Option<&str>,
        eligible: bool,
        record: C,
        release: R,
    ) -> (Vec<BuilderHolder>, bool) {
        let grant = self.claim_builder_slot_with_pool(
            cap,
            exclude_tool_use_id,
            eligible,
            None,
            record,
            release,
        );
        (grant.holders, grant.claimed)
    }

    /// [`Self::claim_builder_slot`], additionally providing the slot's directory
    /// from `pool` (#8261).
    ///
    /// Why: a slot index is not usable by an engineer — the directory is. Giving
    /// the pool its own entry point rather than a sixth parameter on
    /// [`Self::claim_builder_slot`] keeps every existing caller and test on the
    /// index-only contract, which is the same `_with_*` idiom
    /// [`builder_slot_op_with_capacity`](crate::daemon::builder_slot_routes::builder_slot_op_with_capacity)
    /// already uses next door.
    ///
    /// What: as [`Self::claim_builder_slot`], then — still inside the claim
    /// mutex, against the same holder set admission was decided from —
    /// [`Self::grant_pool_slot`] hands over a free SEEDED slot and records it on
    /// the delegation, or grants none (#8794).
    ///
    /// **Only the BOUNDED half of the pool runs here (#8261 critic round).**
    /// `reserve_path` is a stat and one `create_dir_all`; the clone that used to
    /// run on this line took minutes against a 207 GB target directory, under
    /// the claim mutex, while the hook's 2-second claim budget expired — so the
    /// dispatch was denied while the daemon went on holding a `Running` lease
    /// for the 45-minute TTL and every other admission blocked behind the mutex.
    /// With no seeded slot free, the builder is admitted with NO slot, a notice,
    /// and [`BuilderSlotGrant::seed_index`] set for the caller to seed afterwards.
    ///
    /// **A claim whose dispatch was already released is not admitted (#8794).**
    /// The hook's release for a claim it gave up on can land while this claim
    /// still waits on the mutex; `record` then finds that tombstone, and the
    /// claim answers `claimed: false` without handing over a slot.
    ///
    /// **A slot the pool cannot RESERVE is NO slot.** `reserve_path` failing
    /// runs `release` and returns `claimed: false` with
    /// [`BuilderSlotGrant::slot_refused`] set; it never admits a builder that
    /// would then fall back to the shared target directory, which is the
    /// clobbering [`crate::core::builder_slot_pool`] exists to end (see
    /// [`SlotPoolError`](crate::core::builder_slot_pool::SlotPoolError), and the
    /// design's §F "fail closed to no slot"). A `pool` of `None` keeps the
    /// pre-#8261 behaviour exactly.
    ///
    /// Test: `an_admitted_builder_records_the_slot_directory_it_was_given`,
    /// `a_slot_the_pool_cannot_provide_is_refused_not_admitted_unthrottled`,
    /// `an_unseeded_slot_admits_with_no_directory_and_seeds_nothing_inline`,
    /// `two_admissions_racing_for_an_unseeded_slot_get_no_slot_and_one_seed`.
    // The five claim inputs plus the pool plus `self`. Splitting them into a
    // struct would hide which of them the claim mutex protects.
    #[allow(clippy::too_many_arguments)]
    pub fn claim_builder_slot_with_pool<C: FnOnce(&Self), R: FnOnce(&Self)>(
        &self,
        cap: u32,
        exclude_tool_use_id: Option<&str>,
        eligible: bool,
        pool: Option<&SlotPool>,
        record: C,
        release: R,
    ) -> BuilderSlotGrant {
        let pool_root = pool.map(SlotPool::root);
        self.claim_builder_slot_with_pool_root(
            cap,
            exclude_tool_use_id,
            eligible,
            pool,
            pool_root,
            record,
            release,
        )
    }

    /// [`Self::claim_builder_slot_with_pool`], counting the restored leases
    /// under `pool_root` whether or not this dispatch has a pool (#8819).
    ///
    /// Why (#8819 critic): a dispatch from a checkout with no GitHub origin has
    /// no pool, yet the machine-wide cap must still count every restored lease,
    /// or a restarted daemon admits it past the cap.
    /// Test: `a_claim_with_no_pool_counts_restored_leases_8819`.
    #[allow(clippy::too_many_arguments)]
    pub fn claim_builder_slot_with_pool_root<C: FnOnce(&Self), R: FnOnce(&Self)>(
        &self,
        cap: u32,
        exclude_tool_use_id: Option<&str>,
        eligible: bool,
        pool: Option<&SlotPool>,
        pool_root: Option<&Path>,
        record: C,
        release: R,
    ) -> BuilderSlotGrant {
        let _claim = self.builder_claim_guard();
        // #8819: a restarted daemon also counts the leases it restored from disk.
        let holders = self.builder_slot_holders_with_pool_root(exclude_tool_use_id, pool_root);
        let admitted = eligible && u32::try_from(holders.len()).unwrap_or(u32::MAX) < cap;
        let mut grant = BuilderSlotGrant {
            holders,
            claimed: admitted,
            ..BuilderSlotGrant::default()
        };
        if admitted {
            record(self);
            // #8794: the hook gave up on this claim and its release landed
            // first, so `record` found the tombstone. Nothing will run: skip the
            // handover rather than hold this mutex for one.
            if self.builder_claim_released(exclude_tool_use_id) {
                grant.claimed = false;
                return grant;
            }
            // #8261: the slot is chosen INSIDE this critical section, against
            // the same holder set admission was decided from. Choosing it after
            // the lock releases would let two admitted builders pick the same
            // index and share a directory — the exact clobbering the pool
            // exists to end.
            match pool {
                Some(pool) => self.grant_pool_slot(pool, exclude_tool_use_id, &mut grant),
                None => self.assign_builder_slot(exclude_tool_use_id),
            }
            if !grant.claimed {
                release(self);
            }
        } else if eligible {
            // #6892: only an ELIGIBLE refusal is a deny. An ineligible payload
            // was never going to be denied by this guard, so nothing it may have
            // recorded is this rule's to undo.
            release(self);
        }
        grant
    }

    /// Give one admitted builder a SEEDED pool slot, or none (#8794).
    ///
    /// Why: a builder admitted into a slot whose seed had not run could start
    /// cargo there while the seed replaced the directory under it, in a profile
    /// directory the seed's lock check could not see yet. So an unseeded slot is
    /// never handed out.
    /// What: tries each free seeded slot, lowest first, through
    /// [`SlotPool::hand_over`] — which refuses a slot a live cargo build holds
    /// and invalidates another holder's builds — and records the first one it
    /// hands over. With none, the builder is admitted with NO slot and a notice,
    /// and this call claims the seed of the lowest free unseeded slot below
    /// [`SlotPool::ceiling`] for the caller to run off this path — none at all
    /// once every index below it is taken or seeded (#8794). The claim is keyed
    /// by the slot's path and taken under this mutex, so a seed already in
    /// flight is never spawned twice (#8261 critic round 2). A slot whose parent
    /// cannot be made refuses the claim. Every step is bounded: stats, a
    /// `create_dir_all`, and the handover's one `rename` per invalidated package
    /// directory. The trash trees of every slot tried go to
    /// [`BuilderSlotGrant::purge`] for the caller to delete after answering.
    /// #8819: a slot whose on-disk lease a previous daemon granted is skipped
    /// while that lease is live or unverifiable, and every handover records a
    /// lease before the slot is handed out.
    /// Test: `two_admissions_racing_for_an_unseeded_slot_get_no_slot_and_one_seed`,
    /// `a_held_slot_survives_a_daemon_restart_8819`,
    /// `an_admitted_builder_records_the_slot_directory_it_was_given`,
    /// `an_unseeded_slot_admits_with_no_directory_and_seeds_nothing_inline`,
    /// `a_second_claim_on_a_slot_invalidates_the_first_holders_build`,
    /// `a_slot_the_pool_cannot_provide_is_refused_not_admitted_unthrottled`,
    /// `a_failed_handover_is_not_reseeded_past_the_ceiling`.
    fn grant_pool_slot(
        &self,
        pool: &SlotPool,
        tool_use_id: Option<&str>,
        grant: &mut BuilderSlotGrant,
    ) {
        let Some(holder) = tool_use_id else {
            grant.slot_notice = Some(NO_INDEX_NOTICE.to_string());
            return;
        };
        let taken = self.taken_builder_slots(holder);
        let mut seeded = BTreeSet::new();
        for index in pool.existing_indexes() {
            if taken.contains(&index) || !pool.is_seeded(index) {
                continue;
            }
            seeded.insert(index);
            // #8819: a lease granted before a restart is not in `taken`; its
            // file is the evidence, and one that cannot be verified keeps it.
            let lost = self.lost_lease_verdict(pool, index);
            if lost.blocks() {
                tracing::info!("seeded builder slot {index} is still leased: {lost:?}");
                continue;
            }
            let handed = pool.hand_over(index, holder);
            // #8794: listed here, under the mutex every handover runs under, so
            // no listed tree is still being filled; deleted after the answer.
            grant.purge.extend(pool.invalidated_trees(index));
            let path = match handed {
                Ok(path) => path,
                Err(err) => {
                    tracing::warn!("seeded builder slot {index} not handed out: {err}");
                    continue;
                }
            };
            // #8819: a lease that is not on disk is lost at the next restart.
            if let Err(err) = pool.record_lease(index, &self.slot_lease_for(holder)) {
                tracing::warn!("builder slot {index} not handed out, lease unrecorded: {err}");
                continue;
            }
            if !self.stamp_builder_slot(holder, index) {
                pool.clear_lease(index);
                grant.slot_notice = Some(NO_INDEX_NOTICE.to_string());
                return;
            }
            let rendered = format!("{:?}", SeedKind::AlreadySeeded);
            self.record_builder_slot_dir(tool_use_id, &path, &rendered);
            grant.slot_dir = Some(path);
            grant.slot_seed = Some(rendered);
            return;
        }
        // #8794: bounded by the ceiling, so hand-overs that keep failing cannot
        // clone a new slot on every admission.
        let Some(target) = (0..pool.ceiling()).find(|i| !taken.contains(i) && !seeded.contains(i))
        else {
            grant.slot_notice = Some(ceiling_notice(pool.ceiling()));
            return;
        };
        match pool.reserve_path(target) {
            Ok(SlotReservation::Seeding(path)) => {
                let in_flight = !self.builder_seeding.lock().insert(path);
                if !in_flight {
                    grant.seed_index = Some(target);
                }
                grant.slot_notice = Some(unseeded_notice(
                    target,
                    in_flight,
                    pool.last_seed_failure(target),
                ));
            }
            // A seed published this slot after the scan above; the next claim gets it.
            Ok(SlotReservation::Ready(_)) => {
                grant.slot_notice = Some(unseeded_notice(target, true, None));
            }
            Err(err) => {
                tracing::warn!(
                    "builder slot {target} could not be reserved, so no slot is granted: {err}"
                );
                grant.claimed = false;
                grant.slot_refused = Some(err.to_string());
            }
        }
    }

    /// Release the seed claim on the slot at `slot`, whatever the seed's
    /// outcome (#8261).
    ///
    /// Why: the claim taken in [`Self::grant_pool_slot`] suppresses every later
    /// spawn for that slot, so a seed that ended without releasing would leave
    /// the slot permanently unseedable — a slot nobody can warm, admitting
    /// every future builder into the shared directory. The blocking task calls
    /// this on BOTH exits for that reason.
    /// Test: `a_second_reservation_does_not_spawn_a_second_seed`.
    pub fn finish_builder_seed(&self, slot: &Path) {
        self.builder_seeding.lock().remove(slot);
    }

    /// Has this dispatch's record already been released (#8794)?
    ///
    /// Why: a release that reaches the daemon before its claim leaves a
    /// terminal record the claim's `record` does not overwrite.
    /// Test: `a_release_that_beats_its_claim_leaves_no_lease_8794`.
    fn builder_claim_released(&self, tool_use_id: Option<&str>) -> bool {
        tool_use_id.is_some_and(|id| {
            self.delegations.iter().any(|entry| {
                entry.value().tool_use_id.as_deref() == Some(id)
                    && entry.value().status.is_terminal()
            })
        })
    }

    /// Record the directory [`SlotPool::hand_over`] provided onto the lease.
    ///
    /// Test: `an_admitted_builder_records_the_slot_directory_it_was_given`.
    fn record_builder_slot_dir(&self, tool_use_id: Option<&str>, path: &Path, seed: &str) {
        let Some(tool_use_id) = tool_use_id else {
            return;
        };
        let _record = self.dispatch_record_guard();
        for mut entry in self.delegations.iter_mut() {
            if entry.value().tool_use_id.as_deref() == Some(tool_use_id) {
                entry.value_mut().builder_slot_dir = Some(path.to_path_buf());
                entry.value_mut().builder_slot_seed = Some(seed.to_string());
                break;
            }
        }
    }

    /// Close out the delegation record a denied builder dispatch just created
    /// (#6892 critic round).
    ///
    /// Why: see [`Self::claim_builder_slot`]. The record is written by the
    /// shared-tree claim or the worktree grant that runs immediately before the
    /// cap is asked, and a `PreToolUse` deny means nothing downstream will ever
    /// close it.
    /// What: marks the delegation carrying `tool_use_id`
    /// [`DelegationStatus::Cancelled`](crate::core::agent::DelegationStatus::Cancelled)
    /// and stamps `ended_at`, under the dispatch-record lock so it cannot race
    /// the tracker's own writer.
    ///
    /// It RETAINS the record rather than removing it, deliberately. The
    /// tracker's `matcher: "*"` hook fires on the same dispatch and may land
    /// after this, and `delegation_tracker::on_dispatch_locked` returns early on
    /// a `tool_use_id` it already knows — whatever its status. Keeping a
    /// cancelled record is therefore what stops that hook from re-creating a
    /// live one; deleting it would leave the hole open. `Cancelled`, not
    /// `Completed`: nothing ran.
    /// Returns `false` when there is no such record — the ordinary case when the
    /// guard's own preceding claim declined to record, and the reason #8012 moved
    /// the route's refusal onto
    /// [`delegation_tracker::release_denied_dispatch`](crate::daemon::services::delegation_tracker::release_denied_dispatch):
    /// writing nothing there left the late writer free to record a live
    /// delegation for a dispatch that never ran. This stays the state-level
    /// primitive, for a caller holding a `tool_use_id` and no payload.
    /// Test: `releasing_an_unknown_dispatch_is_a_no_op`.
    pub fn release_denied_builder_dispatch(
        &self,
        session: SessionId,
        tool_use_id: Option<&str>,
    ) -> bool {
        let Some(tool_use_id) = tool_use_id else {
            return false;
        };
        let _record = self.dispatch_record_guard();
        let Some(id) =
            self.find_delegation(session, |d| d.tool_use_id.as_deref() == Some(tool_use_id))
        else {
            return false;
        };
        self.terminate_delegation(id, crate::core::agent::DelegationStatus::Cancelled)
    }

    /// Give the just-recorded builder the lowest free slot index, when the claim
    /// has no pool (#8261).
    ///
    /// Why: LOWEST free, not next-highest, so a machine that rarely reaches its
    /// ceiling keeps reusing the same few directories — which is what makes
    /// them warm. Growing the pool monotonically would leave every slot cold
    /// exactly when the ceiling is finally reached.
    /// What: collects the indices live leases already hold, plus those a
    /// stop-released builder may still use (#8548), excluding this dispatch's
    /// own record; takes the first index not among them, and stamps
    /// it on the record carrying `tool_use_id`. Caller must hold
    /// [`builder_claim_guard`](DaemonState::builder_claim_guard) — the whole
    /// point is that the read and the write are one step.
    ///
    /// A record that cannot be found is not an error: the claim's `record`
    /// closure may legitimately have written nothing (see
    /// [`Self::release_denied_builder_dispatch`] for the same case), and a
    /// dispatch with no slot index simply gets no pool directory, which is the
    /// fail-closed direction.
    /// Test: `an_admitted_builder_is_assigned_the_lowest_free_slot`,
    /// `a_released_slot_index_is_reassigned_to_the_next_builder`,
    /// `a_resumed_user_stopped_builder_keeps_its_slot_index_8548`.
    fn assign_builder_slot(&self, tool_use_id: Option<&str>) {
        let Some(tool_use_id) = tool_use_id else {
            return;
        };
        let taken = self.taken_builder_slots(tool_use_id);
        if let Some(index) = (0u32..).find(|i| !taken.contains(i)) {
            self.stamp_builder_slot(tool_use_id, index);
        }
    }

    /// The slot indices live builder leases hold, plus those a stop-released
    /// builder may still be resumed into (#8548), excluding `tool_use_id`'s own.
    ///
    /// Test: `an_admitted_builder_is_assigned_the_lowest_free_slot`,
    /// `a_resumed_user_stopped_builder_keeps_its_slot_index_8548`.
    fn taken_builder_slots(&self, tool_use_id: &str) -> BTreeSet<u32> {
        let now = chrono::Utc::now();
        self.delegations
            .iter()
            .filter_map(|entry| {
                let d = entry.value();
                if d.tool_use_id.as_deref() == Some(tool_use_id) || !agent_is_builder(&d.agent) {
                    return None;
                }
                let owner = self.session_owner_alive(d.session);
                // #8548: a stopped builder may be resumed into its directory, so
                // its index stays taken until the TTL although its capacity is free.
                (builder_lease(d, owner, now).is_held()
                    || super::builder_slot_release::stop_quarantine_holds(d, owner, now))
                .then_some(d.builder_slot)
                .flatten()
            })
            .collect()
    }

    /// Stamp slot `index` on the record carrying `tool_use_id`; `false` when
    /// there is no such record.
    ///
    /// Test: `an_admitted_builder_is_assigned_the_lowest_free_slot`.
    fn stamp_builder_slot(&self, tool_use_id: &str, index: u32) -> bool {
        // The documented lock order: this mutex is taken INSIDE the builder
        // claim, never the other way round — same order the claim's own
        // `record` and `release` closures use.
        let _record = self.dispatch_record_guard();
        for mut entry in self.delegations.iter_mut() {
            if entry.value().tool_use_id.as_deref() == Some(tool_use_id) {
                entry.value_mut().builder_slot = Some(index);
                return true;
            }
        }
        false
    }

    /// The one scan every builder-slot query runs.
    ///
    /// Why: two copies of this filter would drift, and a drift here is a cap
    /// that counts a finished build or misses a running one. `keep` is the only
    /// axis the callers disagree on.
    /// Test: as the two public wrappers.
    fn builder_leases(
        &self,
        exclude_tool_use_id: Option<&str>,
        keep: impl Fn(BuilderLease) -> bool,
    ) -> Vec<BuilderHolder> {
        let now = chrono::Utc::now();
        let mut rows: Vec<BuilderHolder> = self
            .delegations
            .iter()
            .filter_map(|entry| {
                let d = entry.value();
                if !agent_is_builder(&d.agent) {
                    return None;
                }
                if exclude_tool_use_id.is_some() && d.tool_use_id.as_deref() == exclude_tool_use_id
                {
                    return None;
                }
                let lease = builder_lease(d, self.session_owner_alive(d.session), now);
                if !keep(lease) {
                    return None;
                }
                let started = d.started_at.unwrap_or(d.created_at);
                Some(BuilderHolder {
                    agent: d.agent.clone(),
                    session: d.session,
                    elapsed_secs: (now - started).num_seconds().max(0),
                })
            })
            .collect();
        sort_holders(&mut rows);
        rows
    }

    /// Is the process that dispatched this delegation still running?
    ///
    /// Why: a killed PM takes every subagent it dispatched with it, and none of
    /// them emits a `SubagentStop` — so without a PID check those slots stay
    /// held for the full TTL on a machine that is doing nothing. This is the
    /// same evidence the #6497 dead-session reaper acts on, read here rather
    /// than waited for, because the reaper runs on the housekeeping loop and a
    /// dispatch cannot wait for a loop tick.
    /// What: `Some(false)` only when the session records a PID and
    /// [`is_process_alive`](crate::core::process::is_process_alive) says it is
    /// gone. `None` — not `Some(true)` — when the session is unknown or records
    /// no PID: absence of a PID is undeterminable, not evidence of death
    /// (ADR-0045), and treating it as either would be a guess in a place where
    /// guessing wrong releases a running build's slot.
    /// Test: `a_lease_is_released_when_the_owner_pid_is_dead`,
    /// `an_unknown_owner_pid_does_not_release_the_lease`.
    fn session_owner_alive(&self, session: SessionId) -> Option<bool> {
        let pid = self.session(session)?.pid?;
        Some(crate::core::process::is_process_alive(pid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent::{Delegation, DelegationStatus, ModelTier};
    use crate::core::session::{ControlModel, Session, SessionStatus};

    /// A registered session whose PID field the caller sets.
    fn session_with_pid(state: &DaemonState, pid: Option<u32>) -> SessionId {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut s = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
        s.tmux_name = format!("tmpm-builder-test-{n}");
        s.status = SessionStatus::Active;
        s.pid = pid;
        let id = s.id;
        state.register_session(s);
        id
    }

    /// One running delegation of `agent`, started `age_secs` ago.
    fn running(session: SessionId, agent: &str, age_secs: i64) -> Delegation {
        let mut d = Delegation::new(session, None, agent, ModelTier::Sonnet, "build it");
        d.status = DelegationStatus::Running;
        d.started_at = Some(chrono::Utc::now() - chrono::Duration::seconds(age_secs));
        d
    }

    // ---- the pure lease predicate ---------------------------------------

    #[test]
    fn a_terminal_delegation_holds_nothing() {
        let mut d = running(SessionId::new(), "rust-engineer", 10);
        for status in [
            DelegationStatus::Completed,
            DelegationStatus::Failed,
            DelegationStatus::Cancelled,
            DelegationStatus::Stale,
        ] {
            d.status = status;
            assert_eq!(
                builder_lease(&d, Some(true), chrono::Utc::now()),
                BuilderLease::ReleasedByStatus,
                "{status:?} must release the slot"
            );
        }
    }

    /// Criterion 3, as the predicate sees it. A TTL-only implementation returns
    /// `Held` here and fails this case.
    #[test]
    fn a_lease_is_released_when_the_owner_pid_is_dead() {
        let d = running(SessionId::new(), "rust-engineer", 30);
        assert_eq!(
            builder_lease(&d, Some(false), chrono::Utc::now()),
            BuilderLease::ReleasedByDeadOwner,
            "a dead dispatcher releases its builders' slots long before the TTL"
        );
    }

    /// Criterion 4. The PID check answers nothing and the status never moves —
    /// the TTL is the only thing left, and it must still fire.
    #[test]
    fn a_lease_is_released_at_the_ttl_when_the_pid_is_inconclusive() {
        let d = running(
            SessionId::new(),
            "rust-engineer",
            BUILDER_LEASE_TTL_SECS + 1,
        );
        assert_eq!(
            builder_lease(&d, None, chrono::Utc::now()),
            BuilderLease::ReleasedByTtl
        );
        // And exactly at the boundary, not one second later.
        let d = running(SessionId::new(), "rust-engineer", BUILDER_LEASE_TTL_SECS);
        assert_eq!(
            builder_lease(&d, None, chrono::Utc::now()),
            BuilderLease::ReleasedByTtl
        );
    }

    #[test]
    fn a_lease_inside_the_ttl_is_still_held() {
        let d = running(
            SessionId::new(),
            "rust-engineer",
            BUILDER_LEASE_TTL_SECS - 60,
        );
        let lease = builder_lease(&d, Some(true), chrono::Utc::now());
        assert!(lease.is_held(), "{lease:?}");
        assert!(matches!(lease, BuilderLease::Held(e) if e >= BUILDER_LEASE_TTL_SECS - 61));
    }

    #[test]
    fn an_unknown_owner_pid_does_not_release_the_lease() {
        // Absence of a PID is undeterminable, not death — releasing on it would
        // free a running build's slot on no evidence at all.
        let d = running(SessionId::new(), "rust-engineer", 30);
        assert!(builder_lease(&d, None, chrono::Utc::now()).is_held());
    }

    // ---- the state-level scan and claim ---------------------------------

    #[test]
    fn builder_holders_report_the_agent_session_and_elapsed_time() {
        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));
        state.upsert_delegation(running(session, "rust-engineer", 120));

        let holders = state.builder_slot_holders(None);
        assert_eq!(holders.len(), 1);
        assert_eq!(holders[0].agent, "rust-engineer");
        assert_eq!(holders[0].session, session);
        assert!(holders[0].elapsed_secs >= 120, "{:?}", holders[0]);
    }

    /// Criterion 7 at the counting layer: a non-builder dispatch is not merely
    /// admitted, it is INVISIBLE — it never appears as a holder, so it can never
    /// deny anyone else either.
    #[test]
    fn non_builder_delegations_hold_no_slot() {
        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));
        for agent in [
            "research",
            "ticketing",
            "qa",
            "documentation",
            "version-control",
        ] {
            state.upsert_delegation(running(session, agent, 60));
        }
        assert!(state.builder_slot_holders(None).is_empty());
    }

    /// Criterion 8 at the counting layer: `local-ops` declares `role: ops` and
    /// must still occupy a slot.
    #[test]
    fn a_local_ops_delegation_holds_a_builder_slot() {
        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));
        state.upsert_delegation(running(session, "local-ops", 5));
        assert_eq!(state.builder_slot_holders(None).len(), 1);
    }

    /// Criterion 3 end to end through the real PID probe. `u32::MAX` is above
    /// every real PID, so the owner is provably gone without killing anything.
    #[test]
    fn a_dead_sessions_builders_stop_holding_slots() {
        let state = DaemonState::new();
        let dead = session_with_pid(&state, Some(u32::MAX));
        let alive = session_with_pid(&state, Some(std::process::id()));
        state.upsert_delegation(running(dead, "rust-engineer", 30));
        state.upsert_delegation(running(alive, "python-engineer", 30));

        let holders = state.builder_slot_holders(None);
        assert_eq!(holders.len(), 1, "{holders:?}");
        assert_eq!(holders[0].agent, "python-engineer");
    }

    /// Criterion 1. Two sessions, cap 2, four engineer dispatches: the first two
    /// are admitted whichever session they came from, and the third and fourth
    /// are refused with both holders named.
    #[test]
    fn builder_cap_admits_up_to_the_cap_and_denies_the_rest() {
        let state = DaemonState::new();
        let a = session_with_pid(&state, Some(std::process::id()));
        let b = session_with_pid(&state, Some(std::process::id()));

        let mut admitted = 0;
        let mut last_holders = Vec::new();
        for (session, agent) in [
            (a, "rust-engineer"),
            (b, "python-engineer"),
            (a, "react-engineer"),
            (b, "local-ops"),
        ] {
            let (holders, claimed) = state.claim_builder_slot(
                2,
                None,
                true,
                |s| s.upsert_delegation(running(session, agent, 1)),
                |_| {},
            );
            last_holders = holders;
            if claimed {
                admitted += 1;
            }
        }
        assert_eq!(admitted, 2, "the cap is machine-wide, not per session");
        assert_eq!(
            last_holders.len(),
            2,
            "the deny must name both actual holders: {last_holders:?}"
        );
        let names: Vec<&str> = last_holders.iter().map(|h| h.agent.as_str()).collect();
        assert!(names.contains(&"rust-engineer"), "{names:?}");
        assert!(names.contains(&"python-engineer"), "{names:?}");
    }

    /// Criterion 2. One free slot, two claims arriving at once — the claim is
    /// atomic, so exactly one is admitted. A check-then-decide implementation
    /// admits both here.
    #[test]
    fn builder_cap_admits_exactly_one_of_two_simultaneous_claims() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let state = Arc::new(DaemonState::new());
        let session = session_with_pid(&state, Some(std::process::id()));
        let admitted = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(std::sync::Barrier::new(2));

        let handles: Vec<_> = ["rust-engineer", "python-engineer"]
            .into_iter()
            .map(|agent| {
                let state = Arc::clone(&state);
                let admitted = Arc::clone(&admitted);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let (_, claimed) = state.claim_builder_slot(
                        1,
                        None,
                        true,
                        |s| s.upsert_delegation(running(session, agent, 0)),
                        |_| {},
                    );
                    if claimed {
                        admitted.fetch_add(1, Ordering::Relaxed);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread");
        }
        assert_eq!(
            admitted.load(Ordering::Relaxed),
            1,
            "one free slot must admit exactly one of two simultaneous dispatches"
        );
        assert_eq!(state.builder_slot_holders(None).len(), 1);
    }

    /// #8261: two admitted builders must never be handed the same slot index,
    /// or they share a directory and clobber each other — the whole failure the
    /// pool exists to end.
    #[test]
    fn an_admitted_builder_is_assigned_the_lowest_free_slot() {
        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));

        for (n, expected) in [("toolu_A", 0), ("toolu_B", 1), ("toolu_C", 2)] {
            let mut d = running(session, "rust-engineer", 1);
            d.tool_use_id = Some(n.to_string());
            let (_, claimed) = state.claim_builder_slot(
                4,
                Some(n),
                true,
                |s| s.upsert_delegation(d.clone()),
                |_| {},
            );
            assert!(claimed, "{n} must be admitted under a ceiling of 4");
            let got = state
                .delegations
                .iter()
                .find(|e| e.value().tool_use_id.as_deref() == Some(n))
                .and_then(|e| e.value().builder_slot);
            assert_eq!(got, Some(expected), "{n} took the wrong slot");
        }
    }

    /// A pool rooted at `root`, for a fixed test identity, whose checkout (a
    /// sibling of `root`) names a path package — a handover needs one (#8794).
    fn test_pool(root: std::path::PathBuf) -> SlotPool {
        test_pool_with_ceiling(root, 4)
    }

    /// [`test_pool`] seeding at most `ceiling` slots.
    fn test_pool_with_ceiling(root: std::path::PathBuf, ceiling: u32) -> SlotPool {
        let checkout = crate::core::builder_slot_pool::test_support::write_checkout(
            &root.with_file_name("checkout"),
        );
        SlotPool::new(
            root,
            trusty_common::github_path::GithubPath {
                owner: "acme".to_string(),
                repo: "widgets".to_string(),
            },
            ceiling,
        )
        .with_checkout(checkout)
    }

    /// #8261: an admitted builder is handed a DIRECTORY, not just an index — the
    /// index alone is not something an engineer can put on a cargo command.
    #[test]
    fn an_admitted_builder_records_the_slot_directory_it_was_given() {
        let root = tempfile::tempdir().expect("tempdir");
        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));
        let pool = test_pool(root.path().join("pool"));
        // A slot the pool has already seeded — the only state a claim may hand
        // out, since seeding cannot run on the claim path (#8261 critic round).
        pool.seed(0, None).expect("a seeded slot 0");

        let mut d = running(session, "rust-engineer", 1);
        d.tool_use_id = Some("toolu_A".to_string());
        let grant = state.claim_builder_slot_with_pool(
            4,
            Some("toolu_A"),
            true,
            Some(&pool),
            |s| s.upsert_delegation(d.clone()),
            |_| {},
        );

        assert!(grant.claimed, "a quiet machine with a usable pool admits");
        let dir = grant
            .slot_dir
            .expect("an admitted builder gets a directory");
        assert!(dir.is_dir(), "the slot directory must exist: {dir:?}");
        assert!(
            dir.ends_with("slot-0"),
            "the first builder takes slot-0: {dir:?}"
        );
        let recorded = state
            .delegations
            .iter()
            .find(|e| e.value().tool_use_id.as_deref() == Some("toolu_A"))
            .map(|e| e.value().builder_slot_dir.clone());
        assert_eq!(
            recorded,
            Some(Some(dir)),
            "the lease must carry the directory, or nothing can report it"
        );
    }

    /// #8261 critic round, CRITICAL: the claim path may not seed.
    ///
    /// `SlotPool::acquire_path` used to run here, inside the claim mutex, and
    /// `cp -c -R` of a 207 GB shared target directory cannot finish inside the
    /// hook's 2-second claim budget — so the first dispatch per slot was denied
    /// on a timeout while the daemon held a `Running` lease for the 45-minute
    /// TTL and every other admission blocked on the mutex. An unseeded slot must
    /// therefore admit WITHOUT a directory and leave the seed to the caller.
    #[test]
    fn an_unseeded_slot_admits_with_no_directory_and_seeds_nothing_inline() {
        let root = tempfile::tempdir().expect("tempdir");
        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));
        let pool = test_pool(root.path().join("pool"));

        let mut d = running(session, "rust-engineer", 1);
        d.tool_use_id = Some("toolu_A".to_string());
        let grant = state.claim_builder_slot_with_pool(
            4,
            Some("toolu_A"),
            true,
            Some(&pool),
            |s| s.upsert_delegation(d.clone()),
            |_| {},
        );

        assert!(
            grant.claimed,
            "an unseeded slot admits — it is the SEED that is deferred, not the dispatch"
        );
        assert!(
            grant.slot_dir.is_none(),
            "a slot whose seed has not run is not handed out: {:?}",
            grant.slot_dir
        );
        assert_eq!(
            grant.seed_index,
            Some(0),
            "the caller is told which slot to seed off the claim path"
        );
        assert!(
            grant.slot_notice.is_some(),
            "admitting with no private directory must say so"
        );
        assert!(
            !pool.slot_path(0).exists(),
            "the reservation creates the parent only, never the slot itself"
        );
    }

    /// #8794 item 1: two admissions race for one unseeded slot. Neither is
    /// handed the slot, or its index, before it is seeded, and exactly one seed
    /// is claimed. At 598183228 the first admission held the unseeded slot-0
    /// and the second was sent to seed slot-1.
    #[test]
    fn two_admissions_racing_for_an_unseeded_slot_get_no_slot_and_one_seed() {
        use std::sync::Arc;
        let root = tempfile::tempdir().expect("tempdir");
        let state = Arc::new(DaemonState::new());
        let session = session_with_pid(&state, Some(std::process::id()));
        let pool = test_pool(root.path().join("pool"));
        let barrier = Arc::new(std::sync::Barrier::new(2));

        let handles: Vec<_> = ["toolu_A", "toolu_B"]
            .into_iter()
            .map(|id| {
                let (state, pool) = (Arc::clone(&state), pool.clone());
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mut d = running(session, "rust-engineer", 1);
                    d.tool_use_id = Some(id.to_string());
                    barrier.wait();
                    state.claim_builder_slot_with_pool(
                        4,
                        Some(id),
                        true,
                        Some(&pool),
                        move |s| s.upsert_delegation(d),
                        |_| {},
                    )
                })
            })
            .collect();
        let grants: Vec<BuilderSlotGrant> = handles
            .into_iter()
            .map(|h| h.join().expect("thread"))
            .collect();

        for grant in &grants {
            assert!(grant.claimed, "both fit under the cap: {grant:?}");
            assert!(grant.slot_dir.is_none(), "no directory before the seed");
        }
        let held: Vec<u32> = state
            .delegations
            .iter()
            .filter_map(|e| e.value().builder_slot)
            .collect();
        assert!(
            held.is_empty(),
            "no admission holds an unseeded slot: {held:?}"
        );
        let seeds: Vec<u32> = grants.iter().filter_map(|g| g.seed_index).collect();
        assert_eq!(seeds, vec![0], "exactly one seed, of the one free slot");

        // Once seeded, the slot is handed to the next admission.
        pool.seed(0, None).expect("the seed runs");
        let mut c = running(session, "rust-engineer", 1);
        c.tool_use_id = Some("toolu_C".to_string());
        let third = state.claim_builder_slot_with_pool(
            4,
            Some("toolu_C"),
            true,
            Some(&pool),
            move |s| s.upsert_delegation(c),
            |_| {},
        );
        assert_eq!(third.slot_dir, Some(pool.slot_path(0)));
    }

    /// #8794 item 2: slot-0 is claimed by A, then by B. B's claim invalidates
    /// the build A left for the repo's own package.
    #[test]
    fn a_second_claim_on_a_slot_invalidates_the_first_holders_build() {
        let root = tempfile::tempdir().expect("tempdir");
        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));
        let checkout = root.path().join("checkout");
        std::fs::create_dir_all(&checkout).expect("checkout");
        std::fs::write(
            checkout.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"widgets-core\"\nversion = \"0.1.0\"\n",
        )
        .expect("Cargo.lock");
        let pool = test_pool(root.path().join("pool")).with_checkout(checkout);
        pool.seed(0, None).expect("a seeded slot 0");

        let mut a = running(session, "rust-engineer", 1);
        a.tool_use_id = Some("toolu_A".to_string());
        let a_id = a.id;
        let first = state.claim_builder_slot_with_pool(
            4,
            Some("toolu_A"),
            true,
            Some(&pool),
            move |s| s.upsert_delegation(a),
            |_| {},
        );
        let slot = first.slot_dir.expect("A is handed slot 0");
        let built = slot
            .join("debug")
            .join(".fingerprint")
            .join("widgets-core-0123456789abcdef");
        std::fs::create_dir_all(&built).expect("A's build");
        state.terminate_delegation(a_id, DelegationStatus::Completed);

        let mut b = running(session, "rust-engineer", 1);
        b.tool_use_id = Some("toolu_B".to_string());
        let second = state.claim_builder_slot_with_pool(
            4,
            Some("toolu_B"),
            true,
            Some(&pool),
            move |s| s.upsert_delegation(b),
            |_| {},
        );
        assert_eq!(second.slot_dir, Some(slot), "B is handed the same slot");
        assert!(!built.exists(), "A's build must not be served to B");
        // #8794 critic round 3: the delete is handed to the caller.
        assert_eq!(second.purge.len(), 1, "{:?}", second.purge);
        assert_eq!(SlotPool::purge_invalidated(&second.purge), 1);
    }

    /// Seed slot 0 of `pool` and hand it to `toolu_A` outside any lease, so the
    /// slot is free, seeded, and names A; returns its `served:` line.
    fn slot_zero_served_to_a(pool: &SlotPool) -> String {
        pool.seed(0, None).expect("a seeded slot 0");
        let slot = pool.hand_over(0, "toolu_A").expect("A is handed slot 0");
        // #8819: a lease-less handover inside the TTL is unverifiable and keeps
        // its slot, so backdate it past the TTL to leave the slot free.
        std::fs::File::options()
            .write(true)
            .open(slot.join(crate::core::builder_slot_pool::SEED_MARKER))
            .and_then(|marker| {
                marker.set_modified(
                    std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 60 * 60),
                )
            })
            .expect("backdate the marker");
        served_line(&slot)
    }

    /// The `served:` line of `slot`'s marker.
    fn served_line(slot: &Path) -> String {
        std::fs::read_to_string(slot.join(crate::core::builder_slot_pool::SEED_MARKER))
            .expect("marker")
            .lines()
            .find(|line| line.starts_with("served: "))
            .unwrap_or_default()
            .to_string()
    }

    /// Claim a slot from `pool` for a new running builder `id`.
    fn claim_for(
        state: &DaemonState,
        session: SessionId,
        id: &str,
        pool: &SlotPool,
    ) -> BuilderSlotGrant {
        let mut d = running(session, "rust-engineer", 1);
        d.tool_use_id = Some(id.to_string());
        state.claim_builder_slot_with_pool(
            4,
            Some(id),
            true,
            Some(pool),
            move |s| s.upsert_delegation(d),
            |_| {},
        )
    }

    /// #8794 critic round 3: `hand_over` fails (its checkout has no
    /// `Cargo.lock`), so B gets no directory, the marker still names A, and the
    /// failed slot is not the seed target. At c7cf951a4 B was handed slot 0.
    #[test]
    fn a_handover_error_grants_no_slot_and_keeps_the_marker() {
        let root = tempfile::tempdir().expect("tempdir");
        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));
        let pool = test_pool(root.path().join("pool"));
        let before = slot_zero_served_to_a(&pool);
        let no_lock = pool.clone().with_checkout(root.path().join("no-lock"));

        let grant = claim_for(&state, session, "toolu_B", &no_lock);

        assert!(grant.claimed, "the admission itself stands: {grant:?}");
        assert_eq!(grant.slot_dir, None, "a failed handover hands out nothing");
        assert_eq!(
            served_line(&pool.slot_path(0)),
            before,
            "the marker keeps A"
        );
        assert_eq!(grant.seed_index, Some(1), "slot 0 is not re-seeded");
    }

    /// #8794 critic round 3: with the only seeded slot refusing its handover (a
    /// live build holds it), no seed starts at or past the pool's ceiling. At
    /// c7cf951a4 every such admission seeded the next index, unbounded.
    #[test]
    fn a_failed_handover_is_not_reseeded_past_the_ceiling() {
        let root = tempfile::tempdir().expect("tempdir");
        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));
        let pool = test_pool_with_ceiling(root.path().join("pool"), 1);
        let before = slot_zero_served_to_a(&pool);
        let debug = pool.slot_path(0).join("debug");
        std::fs::create_dir_all(&debug).expect("profile");
        let lock = std::fs::File::create(debug.join(".cargo-lock")).expect("lock");
        lock.lock().expect("a live build holds slot 0");

        let grant = claim_for(&state, session, "toolu_B", &pool);
        assert_eq!(grant.slot_dir, None, "{grant:?}");
        assert_eq!(grant.seed_index, None, "the ceiling of 1 is reached");
        assert!(
            grant
                .slot_notice
                .as_deref()
                .is_some_and(|n| n.contains("ceiling")),
            "{:?}",
            grant.slot_notice
        );
        assert_eq!(
            served_line(&pool.slot_path(0)),
            before,
            "the marker keeps A"
        );

        let wider = test_pool_with_ceiling(root.path().join("pool"), 2);
        let grant = claim_for(&state, session, "toolu_C", &wider);
        assert_eq!(grant.seed_index, Some(1), "below the ceiling, one seed");
    }

    /// #8261 Fail-Open Check: a slot the pool cannot provide is NO slot.
    ///
    /// Falling back to the shared target directory is the clobbering
    /// `core::builder_slot_pool` exists to end (`SlotPoolError`'s own contract:
    /// "the caller must then grant NO slot"), and the design's §F says an
    /// unprovidable slot fails CLOSED. So a failed `reserve_path` must refuse the
    /// claim, not admit an unthrottled builder pointed at the shared directory.
    /// This FAILS before `claim_builder_slot_with_pool` existed, because the
    /// claim then ignored the pool entirely and always answered `claimed: true`.
    #[test]
    fn a_slot_the_pool_cannot_provide_is_refused_not_admitted_unthrottled() {
        let root = tempfile::tempdir().expect("tempdir");
        // A regular FILE where the pool root must be a directory, so
        // `create_dir_all` under it cannot succeed for any slot.
        let blocker = root.path().join("not-a-directory");
        std::fs::write(&blocker, b"#8261").expect("write blocker");

        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));
        let pool = test_pool(blocker);

        let mut d = running(session, "rust-engineer", 1);
        d.tool_use_id = Some("toolu_A".to_string());
        let mut released = false;
        let grant = state.claim_builder_slot_with_pool(
            4,
            Some("toolu_A"),
            true,
            Some(&pool),
            |s| s.upsert_delegation(d.clone()),
            |_| released = true,
        );

        assert!(
            !grant.claimed,
            "an unprovidable slot must refuse the claim, never admit unthrottled"
        );
        assert!(
            grant.slot_dir.is_none(),
            "a refused claim carries no directory"
        );
        assert!(
            released,
            "the refusal must release the record the guard already wrote"
        );
        assert!(
            grant
                .slot_refused
                .as_deref()
                .is_some_and(|d| d.contains("not-a-directory")),
            "a pool refusal names the path it could not make: {:?}",
            grant.slot_refused
        );
        let index = state
            .delegations
            .iter()
            .find(|e| e.value().tool_use_id.as_deref() == Some("toolu_A"))
            .and_then(|e| e.value().builder_slot);
        assert_eq!(
            index, None,
            "the index must be cleared, or it counts against the next builder"
        );
    }

    /// #8261: a released slot's INDEX returns to the pool, so a machine that
    /// rarely reaches its ceiling keeps reusing the same warm directories
    /// rather than growing monotonically into cold ones.
    #[test]
    fn a_released_slot_index_is_reassigned_to_the_next_builder() {
        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));

        let mut first = running(session, "rust-engineer", 1);
        first.tool_use_id = Some("toolu_1".to_string());
        let first_id = first.id;
        state.claim_builder_slot(
            2,
            Some("toolu_1"),
            true,
            |s| s.upsert_delegation(first.clone()),
            |_| {},
        );

        // The holder ends: its lease — and therefore its index — is free again.
        state.terminate_delegation(first_id, DelegationStatus::Completed);

        let mut second = running(session, "rust-engineer", 1);
        second.tool_use_id = Some("toolu_2".to_string());
        let (_, claimed) = state.claim_builder_slot(
            2,
            Some("toolu_2"),
            true,
            |s| s.upsert_delegation(second.clone()),
            |_| {},
        );
        assert!(claimed);
        let got = state
            .delegations
            .iter()
            .find(|e| e.value().tool_use_id.as_deref() == Some("toolu_2"))
            .and_then(|e| e.value().builder_slot);
        assert_eq!(
            got,
            Some(0),
            "the freed index is reused, not skipped for a cold one"
        );
    }

    #[test]
    fn a_refused_builder_claim_records_nothing() {
        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));
        state.upsert_delegation(running(session, "rust-engineer", 10));

        let mut recorded = false;
        let mut released = false;
        let (holders, claimed) =
            state.claim_builder_slot(1, None, true, |_| recorded = true, |_| released = true);
        assert_eq!(holders.len(), 1, "the deny must name the holder");
        assert!(!claimed);
        assert!(
            !recorded,
            "nothing may be written when the claim is refused"
        );
        // #6892 critic round: and the record the preceding claim wrote is undone.
        assert!(released, "a refused eligible claim must release");
    }

    /// Criterion 7 at the claim layer. `eligible = false` is the daemon's own
    /// re-derivation for a non-builder dispatch: no slot is taken even on an
    /// idle machine.
    #[test]
    fn a_non_builder_dispatch_claims_no_slot() {
        let state = DaemonState::new();
        let mut recorded = false;
        let mut released = false;
        let (holders, claimed) =
            state.claim_builder_slot(4, None, false, |_| recorded = true, |_| released = true);
        assert!(holders.is_empty());
        assert!(!claimed);
        assert!(!recorded);
        // An INELIGIBLE dispatch was never going to be denied by this guard, so
        // nothing it may have recorded is this rule's to undo.
        assert!(!released, "an ineligible payload must not be released");
    }

    #[test]
    fn a_builder_claim_excludes_the_callers_own_dispatch() {
        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));
        // What the daemon's own `matcher: "*"` hook writes when it wins the race
        // with the guard's POST for the same dispatch.
        let mut mine = running(session, "rust-engineer", 0);
        mine.tool_use_id = Some("toolu_MINE".to_string());
        state.upsert_delegation(mine);

        let (holders, claimed) =
            state.claim_builder_slot(1, Some("toolu_MINE"), true, |_| {}, |_| {});
        assert!(
            holders.is_empty() && claimed,
            "a dispatch must never be denied by its own record: {holders:?}"
        );
    }

    /// The release is keyed by `tool_use_id`, so a payload without one — or a
    /// dispatch whose preceding claim recorded nothing — is a no-op rather than
    /// a scan that guesses at which record to close.
    #[test]
    fn releasing_an_unknown_dispatch_is_a_no_op() {
        let state = DaemonState::new();
        let session = session_with_pid(&state, Some(std::process::id()));
        state.upsert_delegation(running(session, "rust-engineer", 10));

        assert!(!state.release_denied_builder_dispatch(session, None));
        assert!(!state.release_denied_builder_dispatch(session, Some("toolu_NOT_HERE")));
        assert_eq!(
            state.builder_slot_holders(None).len(),
            1,
            "a no-op release must not touch the live holder"
        );
    }

    #[test]
    fn census_separates_holders_from_expired_leases() {
        let state = DaemonState::new();
        let session = session_with_pid(&state, None);
        state.upsert_delegation(running(session, "rust-engineer", 60));
        state.upsert_delegation(running(
            session,
            "python-engineer",
            BUILDER_LEASE_TTL_SECS + 60,
        ));

        let census = state.builder_slot_census(3);
        assert_eq!(census.cap, 3);
        assert_eq!(census.holders.len(), 1);
        assert_eq!(census.holders[0].agent, "rust-engineer");
        assert_eq!(census.expired.len(), 1);
        assert_eq!(census.expired[0].agent, "python-engineer");
    }
}
