//! The build-command lease: machine-wide admission at the build, not the
//! dispatch (#8261 increment two, absorbing #8297; owner ruling 2026-09-24,
//! option D).
//!
//! Why: the #6892 cap admitted DISPATCHES by agent type. A typescript-engineer
//! or a CI-polling local-ops was refused a slot it never used, while a `cargo`
//! run by any other agent, session or terminal was not counted at all, and a
//! 45-minute lease TTL let a 201-minute cold compile lose its slot directory to
//! the next builder. The thing that loads the machine is the build command, so
//! that is where the slot is taken: the `PreToolUse` hook rewrites a heavy
//! build (`cargo build/test/clippy/check/doc/install/run/bench` by default,
//! `builders.heavy_build_commands`) to `tm build-lease -- <cmd>`, and the
//! lease holds a kernel `flock` for the build's whole life.
//!
//! **Slot relay** (#8261 round 5). The slot reaches the agent that runs the
//! build through that agent's own Bash call, never through the PM: the hook's
//! `updatedInput` rewrites the subagent's command, and `tm build-lease` sets
//! the slot's `CARGO_TARGET_DIR` in the build's environment and names it on
//! stderr. A dispatch-time channel cannot work: the slot is taken per build,
//! after the dispatch, and a dispatch hook's `additionalContext` reaches only
//! the PM. Test: `a_subagents_leased_build_runs_in_its_slot_directory`.
//!
//! What:
//! - [`slots`] — the slot files, the flock, the holder records, the admission
//!   lock.
//! - [`store`] — where the slot files live: `~/.trusty-mpm/build-slots`, else
//!   one fixed per-uid path, never `$TMPDIR`.
//! - [`config`] — the lease keys of `[builders]`, kept out of the published
//!   `BuildersConfig` shape.
//! - [`admission`] — the pure decision: memory pressure, load, ceiling, leases,
//!   foreign builds. The anti-starvation floor is load-only (see that module).
//! - [`census`] — foreign compiler groups (the #8297 process census) and the
//!   live readings.
//! - [`census_detail`] — the census attributed per group (pgid, leader,
//!   driver, parent chain) for `tm build-lease --census`.
//! - [`acquire`] — the bounded wait.
//! - [`evict`] — disk-budget eviction of the slot pool's `slot-N`
//!   directories, skipping every slot in use (#8451).
//! - [`orphan`] — a build still running after its holder was SIGKILLed.
//! - [`target_dir`] — the slot's private `CARGO_TARGET_DIR`.
//! - [`stale_guard`] — clears a slot's workspace fingerprints when its next
//!   holder builds a different worktree, and detects a slot directory an
//!   orphaned build still uses.
//!
//! **Failure table.** "Allow up to the cap" (owner ruling, #8261 round 3): a
//! lease that cannot be taken runs the build UNLEASED only while the census
//! counts fewer than `ceiling` builds without a lease, and the run is reported
//! DEGRADED naming the store, the OS error and the repair. With no census
//! either, the build is refused. A signal that cannot be READ degrades to the
//! ceiling, never to unlimited admission.
//!
//! | Failure | Direction | Test |
//! |---|---|---|
//! | `~/.trusty-mpm/build-slots` unusable | the fixed per-uid store `/tmp/trusty-mpm-build-slots-<uid>` (owner and mode checked); `tm doctor` WARNS | `a_broken_home_falls_back_to_the_fixed_per_uid_store`, `two_sessions_with_different_tmpdirs_share_one_store` |
//! | both stores unusable | census-bounded UNLEASED run, reported degraded | `an_unusable_store_runs_unleased_up_to_the_cap` |
//! | one slot file broken | that index is skipped and the candidate range widened past it; the ceiling still binds | `a_broken_lowest_slot_is_skipped` |
//! | `admission.lock` unopenable, or no slot file lockable | census-bounded UNLEASED run: admitted only while fewer than `ceiling` builds run without a lease, else wait and exit 75; reported degraded | `unleased_is_admitted_while_the_count_is_below_the_ceiling`, `unleased_is_refused_when_the_count_reaches_the_ceiling`, `an_admission_lock_directory_runs_unleased_up_to_the_cap`, `unleased_is_refused_once_the_census_reaches_the_cap` |
//! | lease impossible AND census unreadable | FAILS CLOSED: nothing bounds the build, so it waits and exits 75 naming both faults | `no_lease_and_no_census_admits_nothing` |
//! | an unleased run whose inherited `CARGO_TARGET_DIR` is shared | REFUSED, exit 75: only a leased slot can replace the shared directory | `a_shared_dir_without_a_pool_is_refused` |
//! | a free slot whose directory an orphaned build still uses (`.cargo-lock` held) | that slot is skipped; its fingerprints are untouched | `a_busy_orphan_slot_is_not_reused` |
//! | a free slot whose dead holder's build still runs (a `cargo test` run holds no `.cargo-lock`) | counted as held, never taken, until that build exits — by pid and start window only, so an exec-replaced build (`cargo nextest` runs as `cargo-nextest`) stays held | `a_sigkilled_holders_live_test_run_keeps_its_slot`, `an_orphaned_build_keeps_its_slot`, `an_exec_replaced_cargo_subcommand_keeps_its_slot` |
//! | a free slot whose leftover record is corrupt, or whose build's pid or start time cannot be read | BROKEN: skipped, never taken, record kept; `tm doctor` FAILS | `a_corrupt_record_in_a_free_slot_is_broken`, `a_corrupt_record_slot_is_skipped_not_taken`, `doctor_fails_on_a_corrupt_slot_record`, `an_unparseable_started_at_is_broken`, `an_uncheckable_pid_is_broken`, `a_failed_pid_check_is_unknown`, `an_unreadable_start_time_is_unknown` |
//! | shared `CARGO_TARGET_DIR` and no slot directory (no pool, seed failed, fingerprints not clearable) | REFUSED, exit 75 | `an_unusable_slot_directory_refuses_instead_of_sharing`, `a_shared_target_without_a_repo_identity_refuses` |
//! | pressure sysctl / PSI unreadable | pressure gate skipped; ceiling, leases and load still apply; warning | `unreadable_pressure_uses_the_ceiling_and_warns` |
//! | load average unreadable | load gate skipped; the rest applies | `an_unreadable_load_skips_only_the_load_gate` |
//! | census at or over the ceiling | no floor: every leased build waits, then exits 75 (`// #8261: owner ruling (a) roll out together`) | `foreign_builds_can_reduce_the_slots_to_zero` |
//! | process census fails | foreign builds not subtracted; ceiling and leases apply | `an_unreadable_census_counts_leases_only` |
//! | invalid `[builders]` value | that key's default, pressure gate included | `an_invalid_config_still_gates_on_pressure` |
//! | slot's workspace package list unreadable on a checkout change | every fingerprint in the slot is cleared — a cold build, never a stale one | `an_unreadable_package_list_clears_every_fingerprint` |
//! | daemon down | decision is local and unaffected; only the daemon's log line is lost, noted on stderr | `a_lease_runs_when_the_daemon_is_down` |
//!
//! `tm doctor`'s `builder_cap` row FAILS, naming an UNKNOWN lease state with the
//! store, the error and the repair, on a broken slot file, an unopenable
//! `admission.lock` or no usable store, and WARNS while the fallback store is in
//! use, so a degraded lease is never silent.
//!
//! **A SIGKILLed holder.** The kernel drops its flock, and the build it
//! spawned keeps running. The record the holder left in
//! the slot file keeps the slot counted as held, and never taken, while that
//! build is alive ([`orphan`], #8261): cargo releases `.cargo-lock` while test
//! binaries run, so that lock alone left a live `cargo test` run's slot free.
//! The process name is not compared, because cargo, the rustup proxy and
//! `sh -c` exec into another image; a record that cannot be read or checked
//! makes the slot broken, never free (#8736). The census excludes the orphan's compilers, which its
//! record already counts, and the next lease still skips a slot whose
//! `.cargo-lock` is held, so the orphan's directory is never reseeded under it.
//! Test: each submodule's suite, and `tests/tm_build_lease.rs` with real
//! processes.

pub mod acquire;
pub mod admission;
pub mod census;
pub mod census_detail;
pub mod config;
// #8451: the daemon's percent-of-volume sweep over the slot pool.
pub mod evict;
pub mod orphan;
pub mod slots;
pub mod stale_guard;
pub mod store;
pub mod target_dir;

/// The exit code `tm build-lease` returns when it does not run the build:
/// no slot freed in time, or no lease can be taken (#8261).
///
/// Why: `EX_TEMPFAIL`, the code `tm wait` already uses for "re-issue this
/// command later" — which is the right instruction here too.
pub const EXIT_LEASE_TIMEOUT: i32 = 75;
