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
//! - [`acquire`] — the bounded wait.
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
//! | shared `CARGO_TARGET_DIR` and no slot directory (no pool, seed failed, fingerprints not clearable) | REFUSED, exit 75 | `an_unusable_slot_directory_refuses_instead_of_sharing`, `a_shared_target_without_a_repo_identity_refuses` |
//! | pressure sysctl / PSI unreadable | pressure gate skipped; ceiling, leases and load still apply; warning | `unreadable_pressure_uses_the_ceiling_and_warns` |
//! | load average unreadable | load gate skipped; the rest applies | `an_unreadable_load_skips_only_the_load_gate` |
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
//! **A SIGKILLed holder.** The kernel drops its flock, so the slot reads free
//! at once; the build it spawned keeps running. The census counts that orphan
//! as a foreign group, and the next lease skips the slot while cargo's
//! `.cargo-lock` in its directory is held, so the orphan's directory is never
//! reseeded or invalidated under it.
//! Test: each submodule's suite, and `tests/tm_build_lease.rs` with real
//! processes.

pub mod acquire;
pub mod admission;
pub mod census;
pub mod config;
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
