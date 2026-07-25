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
//! Testability: every side effect is behind the [`ServiceEnv`] seam so the
//! decision logic is unit-tested against an in-memory fake — the tests NEVER
//! touch `~/Library/LaunchAgents` or invoke `launchctl` against the live
//! daemons on the host (mirrors trusty-mpm's `FakeNoopTmuxDriver` pattern).
//!
//! Test: `tests` covers the member predicate, the opt-out truth table, every
//! [`bootstrap_one`] branch — including the #3836 defensive-fallback branches
//! and the #3841 already-installed-path branches — (via `FakeServiceEnv`),
//! and the [`start_plan`] relaxation used by `lifecycle.rs`.

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
/// installer force-bootstrapped the existing plist directly; `Failed` carries
/// the (non-fatal) error text.
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
    /// Shell-out failed; install continues (fail-soft) but this is surfaced.
    Failed(String),
}

impl BootstrapAction {
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
/// What: `true` for search/memory/analyze/review/console; `false` otherwise.
/// Test: `service_members_recognised`, `non_service_members_excluded`.
pub fn member_has_service_install(binary: &str) -> bool {
    matches!(
        binary,
        "trusty-search" | "trusty-memory" | "trusty-analyze" | "trusty-review" | "trusty-console"
    )
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
/// binary's own (possibly broken) load step.
/// Test: exercised via `RealServiceEnv` (production) and `FakeServiceEnv` (unit
/// tests, `bootstrap_one_*`).
pub trait ServiceEnv {
    /// Does a launchd plist already exist on disk for this member?
    fn plist_present(&self, binary: &str) -> bool;
    /// Run `<binary> service install` (expected to write the plist and
    /// bootstrap it — but see [`bootstrap_one`]'s #3836 postcondition check,
    /// which does not simply trust that expectation).
    fn run_service_install(&self, binary: &str) -> anyhow::Result<()>;
    /// Does launchd currently have this member's label loaded at all
    /// (#3836)?
    fn is_loaded(&self, binary: &str) -> bool;
    /// Force-bootstrap the member's already-written plist directly via
    /// `launchctl`, independent of the component binary's own load logic
    /// (#3836 defensive fallback).
    fn bootstrap_fallback(&self, binary: &str) -> anyhow::Result<()>;
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
/// `bootstrap_one_reports_failure_when_present_but_fallback_fails`.
pub fn bootstrap_one(env: &dyn ServiceEnv, binary: &str) -> BootstrapAction {
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
        return match env.bootstrap_fallback(binary) {
            Ok(()) => BootstrapAction::LoadedByFallback,
            Err(e) => BootstrapAction::Failed(format!(
                "launchd plist already present for {binary} but not loaded, and the \
                 installer's fallback `launchctl bootstrap` failed: {e}"
            )),
        };
    }
    if let Err(e) = env.run_service_install(binary) {
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
/// [`RealServiceEnv`]; elsewhere returns a `Skipped` no-op.
/// Test: the pure policy is covered by `bootstrap_one_*`; this thin cfg wrapper
/// is side-effecting (real `launchctl`) and never invoked in the test suite.
pub fn bootstrap_member_service(binary: &str) -> BootstrapAction {
    #[cfg(target_os = "macos")]
    {
        bootstrap_one(&RealServiceEnv, binary)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = binary;
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

    fn run_service_install(&self, binary: &str) -> anyhow::Result<()> {
        if which::which(binary).is_err() {
            anyhow::bail!("{binary} is not on PATH");
        }
        let mut cmd = std::process::Command::new(binary);
        cmd.args(["service", "install"]);
        run_captured(cmd, &format!("`{binary} service install`"))
    }

    fn is_loaded(&self, binary: &str) -> bool {
        let label = super::plist_label::plist_label_for(binary);
        super::verify_launchd_state::is_label_loaded(&label)
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
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// In-memory [`ServiceEnv`] fake — records `service install` /
    /// `bootstrap_fallback` calls and simulates plist presence + launchd load
    /// state, so tests never touch launchd or the real
    /// `~/Library/LaunchAgents`.
    ///
    /// Why the `loaded: true` default (#3836): the pre-existing
    /// `bootstrap_one_*` tests below construct this via `new(present, fail)`
    /// and assert the OLD `Installed` outcome — defaulting `loaded` to `true`
    /// (the honest, common case: `service install` DID load the agent) keeps
    /// those tests passing unchanged and expresses the #3832 failure mode
    /// (loaded: false) as an opt-in builder instead.
    struct FakeServiceEnv {
        present: bool,
        fail: bool,
        loaded: bool,
        fail_fallback: bool,
        installed: RefCell<Vec<String>>,
        fallback_calls: RefCell<Vec<String>>,
    }

    impl FakeServiceEnv {
        fn new(present: bool, fail: bool) -> Self {
            Self {
                present,
                fail,
                loaded: true,
                fail_fallback: false,
                installed: RefCell::new(Vec::new()),
                fallback_calls: RefCell::new(Vec::new()),
            }
        }

        /// Builder (#3836): simulate `service install` exiting 0 WITHOUT
        /// launchd ever loading the label — #3832's exact failure mode.
        fn not_loaded(mut self) -> Self {
            self.loaded = false;
            self
        }

        /// Builder (#3836): simulate the installer's own fallback
        /// `launchctl bootstrap` also failing.
        fn failing_fallback(mut self) -> Self {
            self.fail_fallback = true;
            self
        }
    }

    impl ServiceEnv for FakeServiceEnv {
        fn plist_present(&self, _binary: &str) -> bool {
            self.present
        }
        fn run_service_install(&self, binary: &str) -> anyhow::Result<()> {
            self.installed.borrow_mut().push(binary.to_string());
            if self.fail {
                anyhow::bail!("simulated failure");
            }
            Ok(())
        }
        fn is_loaded(&self, _binary: &str) -> bool {
            self.loaded
        }
        fn bootstrap_fallback(&self, binary: &str) -> anyhow::Result<()> {
            self.fallback_calls.borrow_mut().push(binary.to_string());
            if self.fail_fallback {
                anyhow::bail!("simulated fallback failure");
            }
            Ok(())
        }
    }

    /// Why: the member predicate gates which daemons get a service bootstrap;
    /// every launchd-managed shared daemon must be recognised.
    /// What: asserts the five service members return `true`.
    /// Test: this is the test.
    #[test]
    fn service_members_recognised() {
        for b in [
            "trusty-search",
            "trusty-memory",
            "trusty-analyze",
            "trusty-review",
            "trusty-console",
        ] {
            assert!(
                member_has_service_install(b),
                "{b} should be a service member"
            );
        }
    }

    /// Why: process-managed / non-daemon members must NOT be shelled out to a
    /// non-existent `service install` subcommand.
    /// What: asserts trusty-mpm and tga are excluded.
    /// Test: this is the test.
    #[test]
    fn non_service_members_excluded() {
        assert!(!member_has_service_install("trusty-mpm"));
        assert!(!member_has_service_install("tga"));
        assert!(!member_has_service_install("nope"));
    }

    /// Why: both opt-out paths must disable the step; neither set must enable it.
    /// What: exercises the four-way truth table.
    /// Test: this is the test.
    #[test]
    fn bootstrap_enabled_truth_table() {
        assert!(bootstrap_enabled(false, false));
        assert!(!bootstrap_enabled(true, false));
        assert!(!bootstrap_enabled(false, true));
        assert!(!bootstrap_enabled(true, true));
    }

    /// Why: the env opt-out is the automation escape hatch; pin that a present
    /// env value disables the step (flag unset).
    /// What: `bootstrap_enabled(false, true)` is `false`.
    /// Test: this is the test.
    #[test]
    fn bootstrap_enabled_respects_env() {
        assert!(!bootstrap_enabled(false, true));
        assert!(bootstrap_enabled(false, false));
    }

    /// Why: a non-service member must be skipped without any install attempt.
    /// What: asserts `Skipped` and that no install was recorded.
    /// Test: this is the test.
    #[test]
    fn bootstrap_one_skips_non_service_member() {
        let env = FakeServiceEnv::new(false, false);
        let action = bootstrap_one(&env, "trusty-mpm");
        assert!(matches!(action, BootstrapAction::Skipped(_)));
        assert!(env.installed.borrow().is_empty());
    }

    /// Why: idempotency + non-clobber — an existing, ALREADY LOADED plist
    /// must be left untouched with NO `service install` call and NO fallback
    /// bootstrap (no needless restart, no overwrite).
    /// What: with `present = true` and `loaded` at its default (`true`),
    /// asserts `Skipped`, no recorded install, and no recorded fallback call.
    /// Test: this is the test.
    #[test]
    fn bootstrap_one_skips_when_plist_present_and_loaded() {
        let env = FakeServiceEnv::new(true, false);
        let action = bootstrap_one(&env, "trusty-search");
        assert!(matches!(action, BootstrapAction::Skipped(_)));
        assert!(
            env.installed.borrow().is_empty(),
            "must not re-install over an existing plist"
        );
        assert!(
            env.fallback_calls.borrow().is_empty(),
            "must not force-bootstrap an already-loaded label"
        );
    }

    /// Why: THE #3841 root-cause fix — a plist already present on disk is NOT
    /// proof launchd has it loaded (a prior run can leave exactly this state,
    /// #3832's failure signature). Before the fix, `bootstrap_one` returned
    /// `Skipped` here WITHOUT ever checking `is_loaded`, so the #3836
    /// defensive postcondition never ran for exactly the damaged machines it
    /// exists to repair. This test fails against the pre-fix code (it always
    /// short-circuited to `Skipped` the instant `plist_present` was `true`).
    /// What: with `present = true` AND `loaded = false` (via `.not_loaded()`),
    /// asserts `LoadedByFallback`, exactly one recorded `bootstrap_fallback`
    /// call, and NO `service install` call (non-clobbering — the plist itself
    /// is never rewritten, only loaded).
    /// Test: this is the test.
    #[test]
    fn bootstrap_one_loads_via_fallback_when_plist_present_but_not_loaded() {
        let env = FakeServiceEnv::new(true, false).not_loaded();
        let action = bootstrap_one(&env, "trusty-memory");
        assert_eq!(action, BootstrapAction::LoadedByFallback);
        assert_eq!(env.fallback_calls.borrow().as_slice(), ["trusty-memory"]);
        assert!(
            env.installed.borrow().is_empty(),
            "an already-present plist must never be re-installed, only loaded"
        );
    }

    /// Why: even on the already-installed path, a fallback failure must be
    /// surfaced loudly (`Failed`) with a message naming BOTH the plist-not-
    /// loaded state and the fallback's own failure reason — mirrors
    /// `bootstrap_one_reports_failure_when_fallback_also_fails` for the
    /// fresh-install branch.
    /// What: with `present = true`, `loaded = false`, and the fallback set to
    /// fail, asserts `Failed` whose message names both facts.
    /// Test: this is the test.
    #[test]
    fn bootstrap_one_reports_failure_when_present_but_fallback_fails() {
        let env = FakeServiceEnv::new(true, false)
            .not_loaded()
            .failing_fallback();
        let action = bootstrap_one(&env, "trusty-review");
        match action {
            BootstrapAction::Failed(e) => {
                assert!(e.contains("not loaded"), "message: {e}");
                assert!(e.contains("simulated fallback failure"), "message: {e}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// Why: the core happy path — a service member with no plist yet gets
    /// `service install` run exactly once.
    /// What: with `present = false`, asserts `Installed` and one recorded call.
    /// Test: this is the test.
    #[test]
    fn bootstrap_one_installs_when_absent() {
        let env = FakeServiceEnv::new(false, false);
        let action = bootstrap_one(&env, "trusty-memory");
        assert_eq!(action, BootstrapAction::Installed);
        assert_eq!(env.installed.borrow().as_slice(), ["trusty-memory"]);
    }

    /// Why: a `service install` failure must be fail-soft — surfaced as
    /// `Failed`, not a panic or an aborted install.
    /// What: with the fake set to fail, asserts a `Failed` carrying the error.
    /// Test: this is the test.
    #[test]
    fn bootstrap_one_reports_failure() {
        let env = FakeServiceEnv::new(false, true);
        let action = bootstrap_one(&env, "trusty-analyze");
        match action {
            BootstrapAction::Failed(e) => assert!(e.contains("simulated failure")),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(
            env.fallback_calls.borrow().is_empty(),
            "a genuine service-install failure must never trigger the fallback \
             bootstrap — that's only for a misleadingly-successful exit code"
        );
    }

    /// Why: #3836 HIGH fix — the common, honest case: `service install`
    /// exited 0 AND launchd actually loaded the label. Must NOT invoke the
    /// fallback bootstrap (it would be a needless extra `launchctl` call /
    /// redundant restart).
    /// What: with `loaded: true` (the default), asserts plain `Installed` and
    /// zero fallback calls.
    /// Test: this is the test.
    #[test]
    fn bootstrap_one_installed_directly_when_loaded() {
        let env = FakeServiceEnv::new(false, false);
        let action = bootstrap_one(&env, "trusty-search");
        assert_eq!(action, BootstrapAction::Installed);
        assert!(
            env.fallback_calls.borrow().is_empty(),
            "must not force-bootstrap when launchd already loaded the label"
        );
    }

    /// Why: THE #3836 HIGH fix's core safety property — #3832's exact
    /// failure signature (`service install` exits 0 but launchd never loads
    /// the label) must be caught and repaired, not silently reported as a
    /// clean `Installed`.
    /// What: with `loaded: false`, asserts `InstalledByFallback` and exactly
    /// one recorded `bootstrap_fallback` call for the right binary.
    /// Test: this is the test.
    #[test]
    fn bootstrap_one_falls_back_when_not_loaded() {
        let env = FakeServiceEnv::new(false, false).not_loaded();
        let action = bootstrap_one(&env, "trusty-memory");
        assert_eq!(action, BootstrapAction::InstalledByFallback);
        assert_eq!(env.fallback_calls.borrow().as_slice(), ["trusty-memory"]);
    }

    /// Why: if EVEN the installer's own direct `launchctl bootstrap` fallback
    /// fails, that must be surfaced loudly (`Failed`) — never silently
    /// swallowed — and the error text must explain BOTH what happened (a
    /// misleading exit code) and why the recovery attempt itself failed, so
    /// an operator isn't left staring at a bare "Failed" for a
    /// non-obvious reason.
    /// What: with `loaded: false` AND the fallback set to fail, asserts
    /// `Failed` whose message names the fallback failure.
    /// Test: this is the test.
    #[test]
    fn bootstrap_one_reports_failure_when_fallback_also_fails() {
        let env = FakeServiceEnv::new(false, false)
            .not_loaded()
            .failing_fallback();
        let action = bootstrap_one(&env, "trusty-review");
        match action {
            BootstrapAction::Failed(e) => {
                assert!(e.contains("never loaded"), "message: {e}");
                assert!(e.contains("simulated fallback failure"), "message: {e}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// Why: the narration note must name the member for a scannable install log.
    /// What: asserts each variant's note contains the binary name.
    /// Test: this is the test.
    #[test]
    fn note_mentions_binary() {
        assert!(BootstrapAction::Installed
            .note("trusty-search")
            .contains("trusty-search"));
        assert!(BootstrapAction::InstalledByFallback
            .note("trusty-memory")
            .contains("trusty-memory"));
        assert!(BootstrapAction::LoadedByFallback
            .note("trusty-memory")
            .contains("trusty-memory"));
        assert!(BootstrapAction::Skipped("x".into())
            .note("trusty-memory")
            .contains("trusty-memory"));
        assert!(BootstrapAction::Failed("boom".into())
            .note("trusty-review")
            .contains("trusty-review"));
    }

    /// Why: the `tctl start` relaxation is the #2556 lifecycle fix; pin the map.
    /// What: present → Bootstrap, absent → ServiceInstall.
    /// Test: this is the test.
    #[test]
    fn start_plan_maps_presence() {
        assert_eq!(start_plan(true), StartPlan::Bootstrap);
        assert_eq!(start_plan(false), StartPlan::ServiceInstall);
    }

    /// Redirect the REAL (OS-level) stdout file descriptor to `tmp` for the
    /// duration of `f`, then restore it — regardless of whether `f` panics.
    ///
    /// Why (#3830 regression proof): `Command::status()` "inheriting" stdio
    /// means a child writes to whatever fd 1 currently points to; the only
    /// way to OBSERVE that from a test is to control what fd 1 points to
    /// before spawning, run the command, then read it back. Asserting only
    /// `result.is_ok()` (a prior version of this test) proves nothing about
    /// where the child's bytes went — it still passed when code-critic
    /// reverted `run_captured` to `.status()` on PR #3834.
    ///
    /// CRITICAL: fd 1 is process-global, so this must never run as an
    /// ordinary parallel `#[test]` — the default test harness's OWN per-test
    /// "test x ... ok" status line is printed by the harness-controller
    /// thread through the REAL stdout (not the per-test captured sink), and
    /// can land in `tmp` mid-redirect whenever ANY sibling test finishes on
    /// another thread (confirmed empirically; mirrors the identical fix in
    /// `trusty-common::update::tests`). The `#[test]` wrapper below never
    /// calls this on the main invocation — it re-execs the test binary,
    /// selecting ONLY the `_inner` test (`--test-threads=1`), so it is the
    /// sole test running in a dedicated process.
    ///
    /// What: `dup`s the real stdout fd aside, `dup2`s `tmp`'s fd onto it,
    /// runs `f`, restores the saved fd (via a guard so a panic in `f` still
    /// restores it), and returns `f`'s result.
    /// Test: used by `run_captured_never_leaks_to_parent_stdio_inner` below.
    fn with_real_stdout_redirected_to<T>(tmp: &std::fs::File, f: impl FnOnce() -> T) -> T {
        use std::os::unix::io::AsRawFd;

        /// RAII guard: restores the saved real-stdout fd on drop, so a panic
        /// in `f` can never leave the process's stdout permanently redirected.
        struct RestoreStdout(std::os::raw::c_int);
        impl Drop for RestoreStdout {
            fn drop(&mut self) {
                unsafe {
                    libc::dup2(self.0, libc::STDOUT_FILENO);
                    libc::close(self.0);
                }
            }
        }

        let saved = unsafe { libc::dup(libc::STDOUT_FILENO) };
        assert!(saved >= 0, "dup(STDOUT_FILENO) failed");
        let _restore = RestoreStdout(saved);

        let rc = unsafe { libc::dup2(tmp.as_raw_fd(), libc::STDOUT_FILENO) };
        assert!(rc >= 0, "dup2(tmp, STDOUT_FILENO) failed");

        f()
    }

    /// Re-exec this test binary, running ONLY `inner_test_name` (an
    /// `#[ignore]`d test) alone with `--test-threads=1`, and assert it
    /// exits 0.
    ///
    /// Why: see [`with_real_stdout_redirected_to`]'s doc — a test that
    /// hijacks the process's real stdout fd must run with zero sibling test
    /// threads.
    /// What: `Command::new(current_exe())` with
    /// `[name, "--exact", "--ignored", "--test-threads=1"]`; panics with the
    /// child's captured stdout/stderr on a non-zero exit.
    /// Test: exercised by `run_captured_never_leaks_to_parent_stdio` below.
    fn run_isolated_inner_test(inner_test_name: &str) {
        let exe = std::env::current_exe().expect("resolve current test binary");
        let output = std::process::Command::new(&exe)
            .args([inner_test_name, "--exact", "--ignored", "--test-threads=1"])
            .output()
            .expect("re-exec test binary for isolated fd test");
        assert!(
            output.status.success(),
            "isolated test `{inner_test_name}` failed ({}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    /// The #3830 regression proof: `run_captured` must NEVER let a child's
    /// output reach the parent's real (inherited) stdout, no matter how
    /// noisy the child is or whether it succeeds.
    ///
    /// Why: see [`with_real_stdout_redirected_to`]'s doc — this replaces a
    /// weaker predecessor that only asserted `is_ok()` (code-critic finding
    /// on PR #3834). `#[ignore]`d + only ever invoked, alone, via
    /// [`run_isolated_inner_test`] from the `#[test]` wrapper immediately
    /// below.
    /// What: redirects the real stdout fd to a tempfile, runs a noisy,
    /// successful `sh -c` command through `run_captured`, restores stdout,
    /// then asserts the tempfile received ZERO bytes.
    /// Test: this is the test (invoked via
    /// `run_captured_never_leaks_to_parent_stdio`).
    #[test]
    #[ignore]
    fn run_captured_never_leaks_to_parent_stdio_inner() {
        use std::io::Read;

        let tmp = tempfile::NamedTempFile::new().expect("create tempfile");
        let mut cmd = std::process::Command::new("sh");
        cmd.args([
            "-c",
            "for i in $(seq 1 20); do echo \"LEAK_MARKER_3830 stdout $i\"; \
             echo \"LEAK_MARKER_3830 stderr $i\" >&2; done; exit 0",
        ]);

        let result =
            with_real_stdout_redirected_to(tmp.as_file(), || run_captured(cmd, "`noisy-capture`"));
        assert!(result.is_ok(), "expected Ok, got {result:?}");

        let mut contents = String::new();
        tmp.reopen()
            .expect("reopen tempfile")
            .read_to_string(&mut contents)
            .expect("read tempfile");
        assert!(
            contents.is_empty(),
            "run_captured leaked to the parent's real stdout fd: {contents:?}"
        );
    }

    /// `#[test]` entry point for the isolated inner test above — see
    /// [`run_isolated_inner_test`]'s doc for why this indirection exists.
    /// This is the test `cargo test` actually runs; it re-execs the binary
    /// so the real fd-1 hijack happens with zero sibling test threads.
    /// Test: this wraps `run_captured_never_leaks_to_parent_stdio_inner`.
    #[test]
    fn run_captured_never_leaks_to_parent_stdio() {
        run_isolated_inner_test(
            "commands::service_bootstrap::tests::run_captured_never_leaks_to_parent_stdio_inner",
        );
    }

    /// Why (#3830 regression — the core proof): `run_captured`'s error path
    /// must be built from the child's CAPTURED stderr. This is only
    /// reachable at all if the command was run via `.output()`; the pre-fix
    /// `.status()` call has no `stderr` field to read, so this exact
    /// assertion would not compile against that code — pinning the fix at
    /// the type level as well as behaviourally.
    /// What: runs a command that writes a distinctive marker to stderr and
    /// exits non-zero; asserts the returned error's message contains that
    /// exact marker.
    /// Test: this is the test.
    #[test]
    fn run_captured_folds_stderr_into_error() {
        let mut cmd = std::process::Command::new("sh");
        cmd.args([
            "-c",
            "echo 'DISTINCTIVE_MARKER_3830: launchd bootstrap failed' >&2; exit 7",
        ]);
        let err = run_captured(cmd, "`fake service install`").expect_err("expected Err");
        let msg = err.to_string();
        assert!(
            msg.contains("DISTINCTIVE_MARKER_3830: launchd bootstrap failed"),
            "error message did not fold in captured stderr: {msg}"
        );
        assert!(msg.contains("fake service install"));
    }
}
