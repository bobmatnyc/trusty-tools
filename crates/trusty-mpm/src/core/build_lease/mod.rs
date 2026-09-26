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
//! What:
//! - [`slots`] — `~/.trusty-mpm/build-slots/slot-K.lock`, the flock, the holder
//!   records, the admission lock.
//! - [`config`] — the lease keys of `[builders]`, kept out of the published
//!   `BuildersConfig` shape.
//! - [`admission`] — the pure decision: memory pressure, load, ceiling, leases,
//!   foreign builds.
//! - [`census`] — foreign compiler groups (the #8297 process census) and the
//!   live readings.
//! - [`acquire`] — the bounded wait.
//! - [`target_dir`] — the slot's private `CARGO_TARGET_DIR`.
//! - [`stale_guard`] — clears a slot's workspace fingerprints when its next
//!   holder builds a different worktree, so cargo cannot serve one worktree's
//!   build as another's "Fresh" one.
//!
//! **Fail-open table.** A signal that cannot be read must neither refuse every
//! build forever nor admit unbounded builds (owner ruling 2026-09-21: a failed
//! reading falls back to the fixed ceiling, never unlimited admission; an
//! unsafe target never admits). Every arm below is BOUNDED — by the ceiling,
//! the lease count or the census — or refuses. Each row names its direction
//! and its test:
//!
//! | Failure | Direction | Test |
//! |---|---|---|
//! | `~/.trusty-mpm/build-slots` uncreatable | per-user temp dir instead; if that fails too, CENSUS-BOUNDED unleased run: admitted only while fewer than `ceiling` builds run without a lease, else wait and exit 75 | `the_temp_fallback_is_used_when_home_is_unwritable`, `an_uncreatable_slot_dir_is_bounded_by_the_census` |
//! | one slot file broken (open / `flock` error) | that index is skipped and the candidate range widened past it; the ceiling still binds | `a_broken_lowest_slot_is_skipped` |
//! | `admission.lock` unopenable, or no slot file lockable | census-bounded unleased run, as the first row | `an_unopenable_admission_lock_is_bounded`, `unusable_slot_files_fall_back_to_the_census_bound` |
//! | lease impossible AND census unreadable | FAILS CLOSED: nothing bounds the build, so it waits and exits 75 naming both faults | `no_lease_and_no_census_admits_nothing` |
//! | slot directory unusable while the inherited `CARGO_TARGET_DIR` is the shared one | FAILS CLOSED: the lease is released and the build does not run, rather than build in the directory concurrent worktrees overwrite | `an_unusable_slot_directory_refuses_instead_of_sharing` |
//! | pressure sysctl / PSI unreadable | pressure gate skipped; ceiling, leases and load still apply; warning | `unreadable_pressure_uses_the_ceiling_and_warns` |
//! | load average unreadable | load gate skipped; the rest applies | `an_unreadable_load_skips_only_the_load_gate` |
//! | process census fails (lease held) | foreign builds not subtracted; ceiling and leases apply | `an_unreadable_census_counts_leases_only` |
//! | invalid `[builders]` value | that key's default, pressure gate included | `an_invalid_config_still_gates_on_pressure` |
//! | slot's workspace package list unreadable on a checkout change | every fingerprint in the slot is cleared — a cold build, never a stale one | `an_unreadable_package_list_clears_every_fingerprint` |
//! | daemon down | decision is local and unaffected; only the daemon's log line is lost, noted on stderr | `a_lease_runs_when_the_daemon_is_down` |
//!
//! `tm doctor`'s `builder_cap` row FAILS on a broken slot file, an unopenable
//! `admission.lock` or an uncreatable slot directory, so a degraded lease is
//! never silent.
//!
//! **A SIGKILLed holder.** The kernel drops its flock, so the slot is free at
//! once; the build it spawned keeps running with no lease. The census counts
//! that orphan as a foreign group — its ancestry no longer passes through any
//! live holder — so it reduces `n_effective` for leased builds and fills the
//! ceiling for unleased ones until it exits.
//!
//! Why census-bounded rather than refusing when no lease can be taken: a
//! refusal there stops every build on the host until an operator repairs a
//! permission. Unleased builds are visible only to the census, which also
//! counts other `tm build-lease` processes that started earlier, so two started
//! together cannot both see an empty machine. Why the ceiling rather than a
//! refusal when a reading fails: the ceiling is #6892's fixed cap, a bound the
//! operator chose, never unlimited.
//! Test: each submodule's suite, and `tests/tm_build_lease.rs` with real
//! processes.

pub mod acquire;
pub mod admission;
pub mod census;
pub mod config;
pub mod slots;
pub mod stale_guard;
pub mod target_dir;

/// The exit code `tm build-lease` returns when no slot freed in time (#8261).
///
/// Why: `EX_TEMPFAIL`, the code `tm wait` already uses for "re-issue this
/// command later" — which is the right instruction here too.
pub const EXIT_LEASE_TIMEOUT: i32 = 75;
