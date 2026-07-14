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
//! Testability: every side effect is behind the [`ServiceEnv`] seam so the
//! decision logic is unit-tested against an in-memory fake — the tests NEVER
//! touch `~/Library/LaunchAgents` or invoke `launchctl` against the live
//! daemons on the host (mirrors trusty-mpm's `FakeNoopTmuxDriver` pattern).
//!
//! Test: `tests` covers the member predicate, the opt-out truth table, every
//! [`bootstrap_one`] branch (via `FakeServiceEnv`), and the [`start_plan`]
//! relaxation used by `lifecycle.rs`.

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
/// install` ran clean; `Failed` carries the (non-fatal) error text.
/// Test: `bootstrap_one_*` tests assert each variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BootstrapAction {
    /// Not attempted — opt-out, not a service member, or plist already present.
    Skipped(String),
    /// `<binary> service install` ran successfully.
    Installed,
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
/// Why: the decision logic (skip vs install, idempotency, member filtering)
/// must be unit-tested WITHOUT writing to `~/Library/LaunchAgents` or running
/// `launchctl` against the host's live daemons. Abstracting the two effects —
/// "does a plist already exist?" and "run `service install`" — lets a fake
/// drive the logic hermetically (the `FakeNoopTmuxDriver` pattern).
/// What: `plist_present` reports whether the member's LaunchAgent plist exists;
/// `run_service_install` shells out to `<binary> service install`.
/// Test: exercised via `RealServiceEnv` (production) and `FakeServiceEnv` (unit
/// tests, `bootstrap_one_*`).
pub trait ServiceEnv {
    /// Does a launchd plist already exist on disk for this member?
    fn plist_present(&self, binary: &str) -> bool;
    /// Run `<binary> service install` (writes the plist and bootstraps it).
    fn run_service_install(&self, binary: &str) -> anyhow::Result<()>;
}

/// Decide and (via the seam) execute the service bootstrap for one member.
///
/// Why: this is the whole per-member policy — filter non-service members, honour
/// idempotency / non-clobber by skipping when a plist already exists, and
/// otherwise delegate to `service install`. Keeping it seam-driven makes every
/// branch testable with no real launchd contact.
/// What: returns [`BootstrapAction::Skipped`] when the member has no `service
/// install` subcommand or a plist is already present; otherwise runs
/// `run_service_install` and returns [`BootstrapAction::Installed`] or
/// [`BootstrapAction::Failed`].
/// Test: `bootstrap_one_skips_non_service_member`,
/// `bootstrap_one_skips_when_plist_present`, `bootstrap_one_installs_when_absent`,
/// `bootstrap_one_reports_failure`.
pub fn bootstrap_one(env: &dyn ServiceEnv, binary: &str) -> BootstrapAction {
    if !member_has_service_install(binary) {
        return BootstrapAction::Skipped(format!("{binary} has no `service install` subcommand"));
    }
    if env.plist_present(binary) {
        return BootstrapAction::Skipped(
            "launchd plist already present — left untouched (re-run `service install` to refresh)"
                .to_string(),
        );
    }
    match env.run_service_install(binary) {
        Ok(()) => BootstrapAction::Installed,
        Err(e) => BootstrapAction::Failed(e.to_string()),
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
/// exit to an error.
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
        let status = std::process::Command::new(binary)
            .args(["service", "install"])
            .status()
            .map_err(|e| anyhow::anyhow!("spawn `{binary} service install`: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            anyhow::bail!("`{binary} service install` exited with {status}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// In-memory [`ServiceEnv`] fake — records `service install` calls and
    /// simulates plist presence, so tests never touch launchd or the real
    /// `~/Library/LaunchAgents`.
    struct FakeServiceEnv {
        present: bool,
        fail: bool,
        installed: RefCell<Vec<String>>,
    }

    impl FakeServiceEnv {
        fn new(present: bool, fail: bool) -> Self {
            Self {
                present,
                fail,
                installed: RefCell::new(Vec::new()),
            }
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

    /// Why: idempotency + non-clobber — an existing plist must be left untouched
    /// with NO `service install` call (no needless restart, no overwrite).
    /// What: with `present = true`, asserts `Skipped` and no recorded install.
    /// Test: this is the test.
    #[test]
    fn bootstrap_one_skips_when_plist_present() {
        let env = FakeServiceEnv::new(true, false);
        let action = bootstrap_one(&env, "trusty-search");
        assert!(matches!(action, BootstrapAction::Skipped(_)));
        assert!(
            env.installed.borrow().is_empty(),
            "must not re-install over an existing plist"
        );
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
    }

    /// Why: the narration note must name the member for a scannable install log.
    /// What: asserts each variant's note contains the binary name.
    /// Test: this is the test.
    #[test]
    fn note_mentions_binary() {
        assert!(BootstrapAction::Installed
            .note("trusty-search")
            .contains("trusty-search"));
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
}
