//! Post-install launchd bootstrap for the shared daemon members (#2556).
//!
//! Why: `tctl install` placed binaries on PATH but only turnkeyed ONE daemon
//! end-to-end (trusty-mpm, via [`super::plist_bootstrap`]). The shared daemons
//! (search/memory/analyze — and, after #2557, review/console) each own their
//! launchd plist behind a `<binary> service install` subcommand, but `tctl
//! install` never invoked any of them. So a fresh-machine `tctl install`
//! followed by `tctl start` failed for those members with "no such plist"
//! (`lifecycle.rs` bootstraps a plist path nothing ever wrote). This module
//! wires each daemon member's own `service install` into the install flow so
//! the plist exists before `tctl start`/`tctl up` runs.
//!
//! What: [`bootstrap_member_service`] shells out to `<binary> service install`
//! (reusing the daemon's own audited launchd logic rather than duplicating
//! plist XML in the installer) after the binary lands. It is FAIL-SOFT
//! (mirroring [`super::plist_bootstrap`]): a failure logs and returns a
//! [`BootstrapAction::Failed`] rather than aborting the install. It is
//! IDEMPOTENT and NON-CLOBBERING: if a launchd plist already exists for the
//! member it is left untouched (skip-with-message) — re-running install neither
//! overwrites a possibly operator-customised plist nor needlessly restarts a
//! healthy daemon. An opt-out ([`NO_SERVICE_ENV`] / the `--no-service` flag)
//! disables the whole step.
//!
//! 🔴 (#3836 HIGH fix) [`bootstrap_one`] does NOT trust a clean
//! `service install` exit code as proof the agent is actually loaded. #3832
//! found that trusty-memory's `service install` used to write the plist
//! WITHOUT loading it — a fresh install reported "installed and bootstrapped"
//! while `launchctl list` never showed the label at all. Because `tctl
//! install` downloads a component's LATEST PUBLISHED release binary
//! (`download::release`), a source-level fix to one component's `service
//! install` does not protect a demo running an OLDER already-published
//! binary, and does not guard against the next component that ships the same
//! bug. So after `run_service_install` returns `Ok`, [`bootstrap_one`] now
//! independently checks whether launchd actually loaded the label
//! ([`ServiceEnv::is_loaded`]) and, if not, force-bootstraps the
//! already-written plist directly ([`ServiceEnv::bootstrap_fallback`]) —
//! reported as [`BootstrapAction::InstalledByFallback`] rather than a silent
//! `Installed`, so the narration is honest about what actually happened.
//!
//! 🔴 (#3841 root-cause fix, demo-critical) 0.4.8 shipped the #3836 check
//! ONLY on the fresh-install branch (no plist yet). A machine already
//! carrying a plist-present-but-unloaded state — e.g. left behind by an
//! EARLIER pre-0.4.8 run that hit #3832 — hits `bootstrap_one`'s
//! ALREADY-INSTALLED skip branch instead, which used to return `Skipped`
//! unconditionally without EVER checking `is_loaded`, so the defensive
//! postcondition never ran for exactly the machines it exists to repair.
//! [`bootstrap_one`] now runs the same `is_loaded` → `bootstrap_fallback`
//! postcondition on THAT branch too (reported as
//! [`BootstrapAction::LoadedByFallback`]), still without ever re-running
//! `service install` over an existing plist (non-clobbering, unchanged). The
//! post-install verify tail (`verify_tail`) additionally attempts the same
//! fallback at its own layer for a `NotLoaded` member — belt-and-braces, so a
//! future skip-path regression still self-heals at verify time.
//!
//! 🔴 (#4470) Neither of the two branches above may issue its bootstrap until
//! [`super::port_guard::guard_bootstrap`] clears the member's port.
//! `launchctl bootstrap` exits 0 even when a foreign, unsupervised process
//! already owns that port, so an ungated bootstrap reports success while that
//! process keeps serving the port — possibly from an older binary (#4230). The
//! guard fails CLOSED and the refusal is reported as
//! [`BootstrapAction::RefusedForeignPort`], with nothing attempted and nothing
//! changed.
//!
//! Testability: every side effect is behind the [`ServiceEnv`] seam so the
//! decision logic is unit-tested against an in-memory fake — the tests NEVER
//! touch `~/Library/LaunchAgents` or invoke `launchctl` against the live
//! daemons on the host (mirrors trusty-mpm's `FakeNoopTmuxDriver` pattern).
//!
//! Test: `service_bootstrap_tests.rs` covers the member predicate, the opt-out
//! truth table, every [`bootstrap_one`] branch — including the #3836
//! defensive-fallback branches, the #3841 already-installed-path branches, and
//! the #4470 port-guard refusals — (via `FakeServiceEnv`), and the
//! [`start_plan`] relaxation used by `lifecycle.rs`.

/// Environment-variable opt-out for the post-install service bootstrap.
///
/// Why: automation / CI and operators who manage launchd themselves need a way
/// to keep `tctl install` from touching launchd at all, without a CLI flag on
/// every call site.
/// What: when set to any value, [`bootstrap_enabled`] returns `false`.
/// Test: `bootstrap_enabled_respects_env`.
pub const NO_SERVICE_ENV: &str = "TCTL_NO_SERVICE_BOOTSTRAP";

/// One member's service-bootstrap outcome.
///
/// Why: the install flow narrates a per-member line and must distinguish "we
/// installed the service", "we intentionally skipped", and "it failed but we
/// carried on" — a typed result keeps that reporting honest and testable.
/// What: `Skipped` carries the human reason; `Installed` means `service
/// install` ran clean AND launchd confirmed the label loaded;
/// `InstalledByFallback` (#3836) means `service install` exited clean but
/// launchd did NOT have the label loaded, so the installer force-bootstrapped
/// the plist directly; `LoadedByFallback` (#3841 — the already-installed
/// skip-path gap) means a plist ALREADY EXISTED on disk (so `service install`
/// was never re-run at all) but launchd did not have the label loaded, so the
/// installer force-bootstrapped the existing plist directly;
/// `RefusedForeignPort` (#4470) means a process launchd does not supervise
/// already holds the member's port, so no bootstrap was attempted at all;
/// `Failed` carries the (non-fatal) error text.
/// Test: `bootstrap_one_*` tests assert each variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BootstrapAction {
    /// Not attempted — opt-out, not a service member, or plist already
    /// present AND already loaded.
    Skipped(String),
    /// `<binary> service install` ran successfully AND launchd loaded it.
    Installed,
    /// `<binary> service install` exited 0 but launchd never loaded the
    /// label; the installer force-bootstrapped the plist directly instead
    /// (#3836 — the #3832 defense-in-depth: protects the demo even against a
    /// component binary whose own `service install` doesn't actually load
    /// the agent, e.g. an older already-published release).
    InstalledByFallback,
    /// The plist already existed on disk (so `service install` was NOT
    /// re-run — non-clobbering) but launchd did not have the label loaded;
    /// the installer force-bootstrapped the existing plist directly (#3841 —
    /// the same #3832 defense, extended to the already-installed/re-run
    /// path, which previously skipped the postcondition check entirely).
    LoadedByFallback,
    /// #4470: a process launchd does not supervise already holds this
    /// member's port, so the bootstrap was REFUSED rather than issued. The
    /// payload is the operator-facing explanation from
    /// [`super::port_guard::guard_bootstrap`], naming the offending pid and
    /// how to clear it. Distinct from `Failed`: nothing was attempted and
    /// nothing was changed.
    RefusedForeignPort(String),
    /// Shell-out failed; install continues (fail-soft) but this is surfaced.
    Failed(String),
}

impl BootstrapAction {
    /// Whether this outcome must fail the install report (#2566, #4470).
    ///
    /// Why: `install_all` folds this into `service_ok`, which
    /// `InstallReport::build` folds into `all_ok`, which drives the process
    /// exit code and the `--json` payload. #2566 established the rule that a
    /// genuine bootstrap failure must never be reported as success. #4470's
    /// first round then added [`BootstrapAction::RefusedForeignPort`] and did
    /// NOT add it to the call site's inline `matches!`, so a refusal — the
    /// guard working exactly as designed, with the daemon consequently not
    /// running — set `service_ok: true` and `tctl install` exited 0 reporting
    /// `all_ok: true`. That inverts the entire point of the guard: it turned a
    /// detected orphan into a silent success. Making this a METHOD on the enum
    /// rather than an inline match at the call site means a new variant is
    /// classified once, here, next to its definition.
    ///
    /// What: `true` for [`BootstrapAction::Failed`] (a genuine failure) and
    /// [`BootstrapAction::RefusedForeignPort`] (the daemon is NOT going to be
    /// running — the install did not achieve what it claims). `false` for the
    /// three success variants and for `Skipped` (an intentional no-op).
    ///
    /// Test: `bootstrap_action_failure_classification_is_exhaustive`,
    /// `refused_foreign_port_drives_all_ok_false_and_a_nonzero_exit_code`.
    pub fn is_failure(&self) -> bool {
        match self {
            BootstrapAction::Failed(_) | BootstrapAction::RefusedForeignPort(_) => true,
            BootstrapAction::Installed
            | BootstrapAction::InstalledByFallback
            | BootstrapAction::LoadedByFallback
            | BootstrapAction::Skipped(_) => false,
        }
    }

    /// A one-line human summary for the installer narration.
    ///
    /// Why: `install.rs` renders one line per member; centralising the phrasing
    /// keeps the human + reasoning in one place.
    /// What: maps each variant to a concise message including `binary`.
    /// Test: `note_mentions_binary`.
    pub fn note(&self, binary: &str) -> String {
        match self {
            BootstrapAction::Installed => {
                format!("{binary}: launchd service installed and bootstrapped")
            }
            BootstrapAction::InstalledByFallback => format!(
                "{binary}: launchd service installed; bootstrapped by installer \
                 (component binary did not load its service)"
            ),
            BootstrapAction::LoadedByFallback => format!(
                "{binary}: launchd plist already present but was not loaded; \
                 bootstrapped by installer"
            ),
            BootstrapAction::Skipped(reason) => {
                format!("{binary}: service bootstrap skipped ({reason})")
            }
            BootstrapAction::RefusedForeignPort(reason) => {
                format!("{binary}: service bootstrap REFUSED — {reason}")
            }
            BootstrapAction::Failed(err) => {
                format!("{binary}: service bootstrap failed (non-fatal): {err}")
            }
        }
    }
}

/// Whether a member ships a `<binary> service install` subcommand.
///
/// Why: only the launchd-managed shared daemons expose `service install`.
/// trusty-mpm is process-managed (its own supervisor plist is handled by
/// [`super::plist_bootstrap`]) and `tga` is not a daemon at all — attempting a
/// service bootstrap for either would shell out to a subcommand that does not
/// exist.
///
/// #6290: trusty-review left this list. It has no daemon — reviews run per
/// invocation — so it ships no `service` subcommand at all, and shelling out to
/// one would be the same "subcommand that does not exist" failure the list
/// exists to prevent. What it needs instead is EVICTION, which is
/// [`member_has_retired_service`]'s question.
///
/// What: `true` for search/memory/analyze/console; `false` otherwise.
/// Test: `service_members_recognised`, `non_service_members_excluded`,
/// `retired_review_has_no_service_install`.
pub fn member_has_service_install(binary: &str) -> bool {
    matches!(
        binary,
        "trusty-search" | "trusty-memory" | "trusty-analyze" | "trusty-console"
    )
}

/// Whether a member has a launchd unit an upgrade must boot out (#6290).
///
/// Why: a retired daemon's unit does not disappear when the binary stops
/// shipping the subcommand it launches. `com.trusty.review` is loaded with
/// `KeepAlive::Always` on every host that installed trusty-review before this
/// change, respawning a `serve` subcommand the new binary does not have. The
/// install path is the only pass that visits every member on an operator's
/// machine, so it is where the eviction belongs.
/// What: delegates to the canonical registry — a member is retired iff
/// `trusty_common::launchd_labels::RETIRED_SERVICES` names it. Restating the
/// member list here is what #4919 spent four issues undoing.
/// Test: `retired_review_has_no_service_install`.
pub fn member_has_retired_service(binary: &str) -> bool {
    trusty_common::launchd_labels::retired_service_for_member(binary).is_some()
}

/// Whether the post-install service-bootstrap step should run.
///
/// Why: the operator can opt out via the `--no-service` install flag or the
/// [`NO_SERVICE_ENV`] environment variable; keeping the decision a pure
/// function makes it testable without mutating the process environment.
/// What: `true` unless the flag is set OR the env opt-out is present.
/// Test: `bootstrap_enabled_truth_table`, `bootstrap_enabled_respects_env`.
pub fn bootstrap_enabled(no_service_flag: bool, env_opt_out: bool) -> bool {
    !no_service_flag && !env_opt_out
}

/// Side-effecting operations the bootstrap needs, behind a seam for testing.
///
/// Why: the decision logic (skip vs install, idempotency, member filtering,
/// and the #3836 defensive-fallback check) must be unit-tested WITHOUT
/// writing to `~/Library/LaunchAgents` or running `launchctl` against the
/// host's live daemons. Abstracting the four effects — "does a plist already
/// exist?", "run `service install`", "did launchd actually load it?", and
/// "force-bootstrap the plist directly" — lets a fake drive the logic
/// hermetically (the `FakeNoopTmuxDriver` pattern).
/// What: `plist_present` reports whether the member's LaunchAgent plist
/// exists; `run_service_install` shells out to `<binary> service install`;
/// `is_loaded` (#3836) reports whether launchd currently has the label
/// loaded at all; `bootstrap_fallback` (#3836) force-bootstraps the
/// already-written plist directly via `launchctl`, bypassing the component
/// binary's own (possibly broken) load step; `port_guard` (#4470) answers
/// "may we bootstrap this member at all, or is a foreign process already on
/// its port?" — behind the same seam so the refusal path is unit-testable
/// without an actual squatter binding an actual port.
/// Test: exercised via `RealServiceEnv` (production) and `FakeServiceEnv` (unit
/// tests, `bootstrap_one_*`).
pub trait ServiceEnv {
    /// Does a launchd plist already exist on disk for this member?
    fn plist_present(&self, binary: &str) -> bool;
    /// Run `<binary> service install` (expected to write the plist and
    /// bootstrap it — but see [`bootstrap_one`]'s #3836 postcondition check,
    /// which does not simply trust that expectation).
    ///
    /// `exe_path` (#4964 Phase 0.2) is the CONCRETE path of the binary the
    /// caller just installed, when it knows one. It must be spawned in
    /// preference to a `PATH` lookup of `binary`: the spawned process bakes its
    /// own `std::env::current_exe()` into the plist's `ProgramArguments[0]`, so
    /// resolving a stale earlier-on-`PATH` copy here writes a plist that
    /// launchd's `KeepAlive` then respawns forever. `None` means "no concrete
    /// path known" (the `tctl start` path, which installs no binary) and keeps
    /// the `PATH` lookup.
    fn run_service_install(
        &self,
        binary: &str,
        exe_path: Option<&std::path::Path>,
    ) -> anyhow::Result<()>;
    /// Does launchd currently have this member's label loaded at all
    /// (#3836)?
    fn is_loaded(&self, binary: &str) -> bool;
    /// Force-bootstrap the member's already-written plist directly via
    /// `launchctl`, independent of the component binary's own load logic
    /// (#3836 defensive fallback).
    fn bootstrap_fallback(&self, binary: &str) -> anyhow::Result<()>;
    /// #4470: may this member be bootstrapped, or does a process launchd does
    /// not supervise already hold its port? `Err` carries the operator-facing
    /// refusal.
    fn port_guard(&self, binary: &str) -> Result<(), String>;

    /// #6290: boot out and delete every launchd unit this member has RETIRED,
    /// returning the labels that were actually there.
    ///
    /// Best-effort, like [`trusty_common::launchd_activate::LaunchdConfig::evict_legacy`]
    /// it delegates to: a unit that is absent, unloaded, or refuses to unload
    /// must never fail an install, because nothing is being installed for this
    /// member in the first place. An empty return means there was nothing to
    /// clear, which is the steady state on every host after the first pass.
    fn evict_retired(&self, binary: &str) -> Vec<String>;
}

/// Decide and (via the seam) execute the service bootstrap for one member.
///
/// Why: this is the whole per-member policy — filter non-service members, honour
/// idempotency / non-clobber by never re-running `service install` over an
/// existing plist, and (#3836, extended by #3841) independently VERIFY the
/// postcondition `service install` is supposed to establish (the label is
/// loaded) rather than trusting its exit code alone — #3832 was exactly a
/// component binary whose `service install` exited 0 without ever loading the
/// agent.
///
/// 🔴 (#3841 root-cause fix) The #3836 postcondition check originally lived
/// ONLY on the fresh-install branch below (`plist_present == false`). On a
/// machine already carrying a plist-present-but-never-loaded state — e.g. an
/// EARLIER `tctl install` run that hit exactly the #3832 bug before 0.4.8
/// shipped — a re-run's `plist_present(binary)` is `true`, so the OLD code
/// returned `Skipped` immediately and the postcondition never ran at all.
/// That is precisely the demo-critical reproduction: 0.4.8's defensive
/// fallback existed, but the exact machines it was meant to repair (damaged
/// by an older run) took the one code path that never reached it. The
/// already-present branch now runs the SAME `is_loaded` check the fresh
/// branch does, and force-bootstraps via [`ServiceEnv::bootstrap_fallback`]
/// (never re-running `service install` — the non-clobber guarantee is
/// unchanged) when the label is not loaded.
///
/// What: returns [`BootstrapAction::Skipped`] when the member has no `service
/// install` subcommand. When a plist is already present: if
/// [`ServiceEnv::is_loaded`], returns `Skipped` (unchanged, non-clobbering
/// behaviour); otherwise force-bootstraps the existing plist via
/// [`ServiceEnv::bootstrap_fallback`] and returns
/// [`BootstrapAction::LoadedByFallback`] (or `Failed` if the fallback itself
/// fails) WITHOUT ever calling `run_service_install`. Otherwise (no plist
/// yet) runs `run_service_install`; on success, checks
/// [`ServiceEnv::is_loaded`] — if loaded, returns
/// [`BootstrapAction::Installed`]; if NOT loaded, calls
/// [`ServiceEnv::bootstrap_fallback`] and returns
/// [`BootstrapAction::InstalledByFallback`] on success or
/// [`BootstrapAction::Failed`] (with BOTH failure reasons folded in) if the
/// fallback also fails. A `run_service_install` failure is
/// [`BootstrapAction::Failed`] directly (the fallback is only for a
/// misleadingly-successful exit code, not a genuine failure).
/// Test: `bootstrap_one_skips_non_service_member`,
/// `bootstrap_one_skips_when_plist_present_and_loaded`,
/// `bootstrap_one_installs_when_absent`, `bootstrap_one_reports_failure`,
/// `bootstrap_one_falls_back_when_not_loaded`,
/// `bootstrap_one_installed_directly_when_loaded`,
/// `bootstrap_one_reports_failure_when_fallback_also_fails`,
/// `bootstrap_one_loads_via_fallback_when_plist_present_but_not_loaded`
/// (THE #3841 miss, pinned against current main),
/// `bootstrap_one_reports_failure_when_present_but_fallback_fails`,
/// `bootstrap_one_refuses_when_foreign_process_holds_port` and
/// `bootstrap_one_refuses_existing_plist_when_foreign_process_holds_port`
/// (THE #4470 refusals — each asserts NOTHING was installed or bootstrapped).
///
/// 🔴 (#4470) Both bootstrap-issuing branches are gated by
/// [`ServiceEnv::port_guard`] first. `launchctl bootstrap` exits 0 even when a
/// foreign process already owns the daemon's port, so an ungated bootstrap
/// reports success while that process keeps serving the port — the #4230
/// orphan. The gate sits immediately before each side effect, never after, so
/// a refusal cannot leave launchd or the filesystem half-changed.
pub fn bootstrap_one(
    env: &dyn ServiceEnv,
    binary: &str,
    exe_path: Option<&std::path::Path>,
) -> BootstrapAction {
    // #6290: a retired member is visited to CLEAR its unit, not to install one.
    // This runs before the `member_has_service_install` skip because that skip
    // returns without touching launchd, which is exactly the state that leaves
    // `com.trusty.review` respawning a subcommand the binary no longer has.
    if member_has_retired_service(binary) {
        let evicted = env.evict_retired(binary);
        return BootstrapAction::Skipped(if evicted.is_empty() {
            format!("{binary} has no launchd service (retired, #6290)")
        } else {
            format!(
                "{binary}'s launchd service is retired (#6290) — evicted {}",
                evicted.join(", ")
            )
        });
    }
    if !member_has_service_install(binary) {
        return BootstrapAction::Skipped(format!("{binary} has no `service install` subcommand"));
    }
    if env.plist_present(binary) {
        // #3841: a plist already existing on disk is NOT proof launchd has
        // it loaded — this is the exact gap the skip path left open. Never
        // re-run `service install` over an existing plist (non-clobbering,
        // unchanged), but DO independently verify the load postcondition and
        // repair it in place when it doesn't hold.
        if env.is_loaded(binary) {
            return BootstrapAction::Skipped(
                "launchd plist already present and loaded — left untouched (re-run \
                 `service install` to refresh)"
                    .to_string(),
            );
        }
        // #4470: the fallback below IS a `launchctl bootstrap`, which exits 0
        // even when a foreign process already holds the port — check first,
        // and refuse rather than issue a bootstrap whose success would be a
        // lie. Placed AFTER the already-loaded skip so a healthy member is
        // never gated on a port probe, and BEFORE the bootstrap so a refusal
        // leaves launchd exactly as it found it.
        if let Err(reason) = env.port_guard(binary) {
            return BootstrapAction::RefusedForeignPort(reason);
        }
        return match env.bootstrap_fallback(binary) {
            Ok(()) => BootstrapAction::LoadedByFallback,
            Err(e) => BootstrapAction::Failed(format!(
                "launchd plist already present for {binary} but not loaded, and the \
                 installer's fallback `launchctl bootstrap` failed: {e}"
            )),
        };
    }
    // #4470: `<binary> service install` writes the plist AND bootstraps it, so
    // the same foreign-port refusal must gate it — before it runs, so a refusal
    // never leaves a half-installed member behind.
    if let Err(reason) = env.port_guard(binary) {
        return BootstrapAction::RefusedForeignPort(reason);
    }
    if let Err(e) = env.run_service_install(binary, exe_path) {
        return BootstrapAction::Failed(e.to_string());
    }
    // #3836 HIGH fix: a clean exit is not proof launchd actually loaded the
    // label (#3832's exact failure mode) — verify independently, and
    // force-bootstrap directly if the component binary's own load step
    // didn't take.
    if env.is_loaded(binary) {
        return BootstrapAction::Installed;
    }
    match env.bootstrap_fallback(binary) {
        Ok(()) => BootstrapAction::InstalledByFallback,
        Err(e) => BootstrapAction::Failed(format!(
            "`{binary} service install` exited 0 but launchd never loaded the service, \
             and the installer's fallback `launchctl bootstrap` also failed: {e}"
        )),
    }
}

/// Bootstrap a member's launchd service after install (fail-soft, macOS-only).
///
/// Why: the install flow calls this once per successfully-installed daemon
/// member. launchd is macOS-only, so on other platforms it is a no-op skip
/// (matching [`super::plist_bootstrap`]'s non-macOS behaviour).
/// What: on macOS delegates to [`bootstrap_one`] with the production
/// [`RealServiceEnv`]; elsewhere returns a `Skipped` no-op. `exe_path` (#4964
/// Phase 0.2) is the concrete path of the binary the caller just installed —
/// see [`ServiceEnv::run_service_install`] for why passing it matters.
/// Test: the pure policy is covered by `bootstrap_one_*`; this thin cfg wrapper
/// is side-effecting (real `launchctl`) and never invoked in the test suite.
pub fn bootstrap_member_service(
    binary: &str,
    exe_path: Option<&std::path::Path>,
) -> BootstrapAction {
    #[cfg(target_os = "macos")]
    {
        bootstrap_one(&RealServiceEnv, binary, exe_path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (binary, exe_path);
        BootstrapAction::Skipped("launchd is macOS-only".to_string())
    }
}

/// How `tctl start` should bring up a launchd member, given plist presence.
///
/// Why: `lifecycle.rs`'s launchd Start assumed the plist already existed and
/// `launchctl bootstrap`-ed it — which hard-failed on a fresh machine where
/// nothing wrote the plist. Relaxing that to "bootstrap if the plist exists,
/// otherwise run `service install` (which writes AND bootstraps)" is the whole
/// fix; encoding it as a pure enum keeps the relaxation testable.
/// What: [`StartPlan::Bootstrap`] when the plist is present; otherwise
/// [`StartPlan::ServiceInstall`].
/// Test: `start_plan_maps_presence`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartPlan {
    /// Plist exists — `launchctl bootstrap` it (existing behaviour).
    Bootstrap,
    /// Plist absent — run `<binary> service install` to write + bootstrap it.
    ServiceInstall,
}

/// Choose the `tctl start` plan for a launchd member from plist presence.
///
/// Why: see [`StartPlan`] — one pure decision the side-effecting caller acts on.
/// What: `present → Bootstrap`, `absent → ServiceInstall`.
/// Test: `start_plan_maps_presence`.
pub fn start_plan(plist_present: bool) -> StartPlan {
    if plist_present {
        StartPlan::Bootstrap
    } else {
        StartPlan::ServiceInstall
    }
}

/// Production [`ServiceEnv`]: real plist path check + real `service install`.
///
/// Why: the live install/start path needs the actual filesystem + subprocess
/// behaviour; isolating it in one type keeps every other function pure.
/// What: `plist_present` checks
/// `~/Library/LaunchAgents/<plist_label_for(binary)>.plist`;
/// `run_service_install` spawns `<binary> service install` and maps a non-zero
/// exit to an error; `is_loaded` (#3836) checks `launchctl list <label>` via
/// [`super::verify_launchd_state::is_label_loaded`] — the same `launchctl
/// list` primitive `verify_tail`'s down-state classification is built on;
/// `bootstrap_fallback` (#3836) force-bootstraps the already-written plist
/// directly, reusing the same minimal-`LaunchdConfig` (label only — the other
/// fields are inert for `bootstrap`/`bootout`) pattern `lifecycle.rs`'s
/// `launchd_control` uses.
/// Test: side-effecting; never constructed in the test suite (the fake is used).
pub struct RealServiceEnv;

impl ServiceEnv for RealServiceEnv {
    fn plist_present(&self, binary: &str) -> bool {
        super::plist_label::plist_path_for(binary)
            .map(|p| p.exists())
            .unwrap_or(false)
    }

    fn run_service_install(
        &self,
        binary: &str,
        exe_path: Option<&std::path::Path>,
    ) -> anyhow::Result<()> {
        let target = service_install_target(binary, exe_path)?;
        let mut cmd = std::process::Command::new(&target);
        cmd.args(["service", "install"]);
        run_captured(cmd, &format!("`{} service install`", target.display()))
    }

    fn is_loaded(&self, binary: &str) -> bool {
        let label = super::plist_label::plist_label_for(binary);
        super::verify_launchd_state::is_label_loaded(&label)
    }

    // #4470: the one shared implementation of the foreign-port check; the
    // `tctl start`/`restart` path in `lifecycle.rs` calls the same function.
    fn port_guard(&self, binary: &str) -> Result<(), String> {
        super::port_guard::guard_bootstrap(binary)
    }

    // #6290: both of a retired member's labels — the canonical one and every
    // legacy alias — are booted out and their plists deleted, through the same
    // `evict_legacy` primitive an install uses for a renamed unit. Deleting the
    // plist is what stops the next `launchctl bootstrap` resurrecting it.
    fn evict_retired(&self, binary: &str) -> Vec<String> {
        #[cfg(target_os = "macos")]
        {
            use trusty_common::launchd::{KeepAlive, LaunchdConfig};

            let labels = trusty_common::launchd_labels::retired_labels_for_member(binary);
            if labels.is_empty() {
                return Vec::new();
            }
            // Only `label` matters to `bootout` / `plist_path`, and
            // `evict_legacy` overwrites it per alias — the rest are inert, the
            // same minimal-config pattern `bootstrap_fallback` above uses.
            let cfg = LaunchdConfig {
                label: labels[0].to_owned(),
                exe_path: std::path::PathBuf::from(binary),
                args: Vec::new(),
                log_dir: std::path::PathBuf::from("/tmp"),
                keep_alive: KeepAlive::Always,
                throttle_interval: 0,
                env_vars: Vec::new(),
                fd_limit: None,
                working_directory: None,
            };
            cfg.evict_legacy(&labels)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = binary;
            Vec::new()
        }
    }

    fn bootstrap_fallback(&self, binary: &str) -> anyhow::Result<()> {
        // `trusty_common::launchd` is itself `#[cfg(target_os = "macos")]`
        // (it shells out to the real `launchctl` binary, which only exists
        // on macOS) — this whole `RealServiceEnv` impl block is NOT
        // platform-gated (its other methods are plain `std::process::Command`
        // calls that degrade gracefully cross-platform), so referencing that
        // module unconditionally fails to compile on Linux CI (the
        // `Clippy`/`MSRV`/`Test` jobs). Gate this one body instead of the
        // whole impl, mirroring `bootstrap_member_service`'s existing
        // platform split just below.
        #[cfg(target_os = "macos")]
        {
            use trusty_common::launchd::{KeepAlive, LaunchdConfig};

            // Only `label` matters for `bootstrap` (it derives the plist path
            // from it and the plist must already exist on disk — written by
            // the `service install` that just ran); the rest are inert
            // because we never render/write a plist here, mirroring
            // `lifecycle.rs`'s `launchd_control`.
            let cfg = LaunchdConfig {
                label: super::plist_label::plist_label_for(binary),
                exe_path: std::path::PathBuf::from(binary),
                args: Vec::new(),
                log_dir: std::path::PathBuf::from("/tmp"),
                keep_alive: KeepAlive::Always,
                throttle_interval: 0,
                env_vars: Vec::new(),
                fd_limit: None,
                working_directory: None,
            };
            cfg.bootstrap()
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = binary;
            anyhow::bail!("launchd is macOS-only")
        }
    }
}

/// Which executable a `service install` should actually spawn.
///
/// Why (#4964 Phase 0.2): `<binary> service install` bakes the SPAWNED
/// PROCESS's own `std::env::current_exe()` into the launchd plist's
/// `ProgramArguments[0]`, and launchd's `KeepAlive` then respawns exactly that
/// path at every boot, forever — nothing rewrites the plist unless `service
/// install` runs again, and re-running it reproduces the same resolution. So
/// resolving a bare NAME through `$PATH` here is not a cosmetic detail: on a
/// machine where a stale copy sits earlier on `PATH` than the directory the
/// install just wrote to, `tctl install` places a new binary and then persists
/// the OLD one into launchd. Preferring a caller-supplied concrete path removes
/// the name-resolution step a stale binary can win.
///
/// What: returns `exe_path` verbatim when the caller supplies one. Otherwise
/// falls back to `which::which(binary)` — the pre-#4964 behaviour, still
/// correct for `tctl start`, which installs nothing and genuinely has no
/// concrete path to offer. Errors when neither yields a path.
///
/// Test: `service_install_target_prefers_the_concrete_path_over_path_lookup`,
/// `service_install_target_falls_back_to_path_lookup`,
/// `service_install_target_errors_when_nothing_resolves`.
fn service_install_target(
    binary: &str,
    exe_path: Option<&std::path::Path>,
) -> anyhow::Result<std::path::PathBuf> {
    if let Some(p) = exe_path {
        return Ok(p.to_owned());
    }
    which::which(binary).map_err(|_| anyhow::anyhow!("{binary} is not on PATH"))
}

/// Run `cmd` to completion with its stdout/stderr CAPTURED, never inherited.
///
/// Why (#3830 demo-critical fix): this is called from inside `install_all`'s
/// per-component loop while an interactive `LiveChecklist` may still be
/// actively steady-ticking OTHER (not-yet-terminal) rows on a background
/// thread. `std::process::Command::status()` inherits the parent's
/// stdout/stderr by default — a child writing straight to that same terminal
/// fd races `indicatif`'s redraw and desyncs its "how many lines did I just
/// draw" bookkeeping, which is exactly what produced #3830's duplicate,
/// interleaved-line corruption (reproduced and confirmed via a PTY capture
/// replayed through a VT100 emulator; fixed by switching this one call from
/// `.status()` to `.output()`). Extracted as a free function over an already-
/// configured `Command` (rather than inlined in `run_service_install`) so the
/// capture behavior itself — not just the plist-bootstrap policy — is directly
/// unit-testable without touching `launchctl` or PATH.
/// What: Runs `cmd` via `.output()`; `Ok(())` on exit 0. On a non-zero exit,
/// folds the captured stderr (trimmed) into the returned error so the
/// diagnostic is never silently lost — it just never reaches the live
/// terminal directly. `label` is the human-readable command description used
/// in the error text (e.g. `` `trusty-search service install` ``).
/// Test: `run_captured_never_leaks_to_parent_stdio` (the #3830 regression
/// proof — redirects the REAL stdout fd and asserts a noisy successful
/// child's bytes never land there; code-critic on PR #3834 caught that an
/// earlier version of this test asserted only `is_ok()`, which still passed
/// against a reverted `.status()`-based implementation), and
/// `run_captured_folds_stderr_into_error` (an error's message contains the
/// child's exact stderr text, which is only possible if that stderr was
/// CAPTURED via `.output()`; `.status()`, the pre-fix call, gives no access
/// to it at all).
fn run_captured(mut cmd: std::process::Command, label: &str) -> anyhow::Result<()> {
    let output = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("spawn {label}: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        if stderr.is_empty() {
            anyhow::bail!("{label} exited with {}", output.status)
        } else {
            anyhow::bail!("{label} exited with {}: {stderr}", output.status)
        }
    }
}

#[cfg(test)]
#[path = "service_bootstrap_tests.rs"]
mod tests;
