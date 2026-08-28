//! Handler for `trusty-analyze service` — retiring the launchd unit (#6350).
//!
//! Why this file no longer installs anything: ADR-0032 makes trusty-analyze an
//! on-demand service. Clients start it through
//! `trusty_common::uds::OnDemandAnalyze` and it exits on its own idle window, so
//! a `KeepAlive: Always` LaunchAgent would fight that directly — launchd would
//! restart the process the moment it reclaimed itself, and the machine would be
//! back to a resident daemon with an idle timer it can never honour.
//!
//! What survives is the half a migration needs. Every host that ran the previous
//! installer has `com.trusty.analyze` loaded RIGHT NOW; upgrading the binary
//! does not unload it. `service uninstall` is the operator-typed escape hatch
//! that evicts it — `tctl install` and `tctl upgrade` do the same thing
//! unattended, through the same `RETIRED_SERVICES` registry, for the hosts
//! where nobody types anything.
//!
//! The eviction itself is NOT implemented here. `LaunchdConfig::evict_legacy_detailed`
//! is the workspace's one implementation (#6290): it boots a label out only when
//! it is actually loaded, VERIFIES launchd let go rather than trusting
//! `bootout`'s exit code, then deletes the plist — and reports each label as
//! `EvictionOutcome::{Evicted, Absent, Failed}`. This file decides what to
//! PRINT and whether to exit non-zero, which is the only part that is
//! trusty-analyze's own.
//!
//! Test: [`render_evictions`] is pure and platform-independent, so the
//! fail-closed contract is proven on Linux CI without unloading a developer's
//! real units.

use anyhow::Result;
use colored::Colorize;
use trusty_common::launchd_labels::{EvictionOutcome, LabelEviction};

/// Subcommand actions for `trusty-analyze service`.
///
/// Why only one variant (#6350): `install`, `status` and `logs` all described a
/// resident launchd unit. There is none — `install` would create the very thing
/// ADR-0032 removed, and `status`/`logs` would report on a job launchd does not
/// have and a log directory nothing writes. `trusty-analyze status` answers the
/// question those two were used for, against the socket rather than launchd.
/// What: `Uninstall` is the migration path off a previously-installed unit.
/// Test: on Linux the action prints "not supported" and exits 1.
#[derive(Debug, Clone)]
pub enum ServiceAction {
    /// Unload and delete the retired LaunchAgent, if one is installed.
    Uninstall,
}

/// Dispatch a `trusty-analyze service <action>` invocation.
///
/// Why: launchd is macOS-specific; on other platforms we exit cleanly with a
/// clear message rather than emitting confusing plist errors.
/// What: macOS routes to `service_uninstall`. Non-macOS prints "not supported"
/// and exits 1.
/// Test: on Linux, the action exits 1 with the platform message.
pub fn run_service_action(action: ServiceAction) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        match action {
            ServiceAction::Uninstall => service_uninstall(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = action;
        eprintln!(
            "{} `trusty-analyze service` is not supported on this platform — \
             trusty-analyze runs on demand and installs no unit anywhere.",
            "✗".red()
        );
        std::process::exit(1);
    }
}

/// What `service uninstall` should print, and whether it must exit non-zero.
///
/// Why a struct rather than four returns: the exit decision and the operator's
/// remediation text are one verdict, and a caller that could take the lines
/// without the `failed` flag is the fail-open shape this exists to prevent.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Rendered {
    /// Lines for stdout — what was cleared.
    pub out: Vec<String>,
    /// Lines for stderr — what refused to go, with how to clear it by hand.
    pub errors: Vec<String>,
    /// Whether anything was actually evicted, i.e. this host had a stale unit.
    pub touched: bool,
    /// Whether the command must exit non-zero.
    pub failed: bool,
}

/// Turn the shared eviction outcomes into operator-facing output.
///
/// Why this is where the fail-closed decision lives: `EvictionOutcome::Failed`
/// covers both a `launchctl bootout` that refused and a plist that would not
/// delete, and BOTH leave a unit the next `launchctl load` — or the next login
/// — brings straight back. Reporting either as done is the one outcome worse
/// than failing, because the operator walks away believing the daemon is gone
/// while it is still restarting. `EvictionOutcome::is_failure` is the shared
/// rule; this function is what acts on it.
///
/// What: `Evicted` prints a cleared line and sets `touched`; `Absent` prints
/// nothing, which is the steady state on every host that never installed the
/// unit; `Failed` names the label, carries the reason through verbatim, and
/// sets `failed`.
///
/// Test: `render_reports_a_cleared_unit`, `render_is_silent_for_an_absent_unit`,
/// `render_fails_closed_on_a_unit_that_would_not_go`,
/// `render_fails_even_when_another_label_was_cleared`.
#[must_use]
pub fn render_evictions(evictions: &[LabelEviction]) -> Rendered {
    let mut r = Rendered::default();
    for e in evictions {
        match &e.outcome {
            EvictionOutcome::Evicted => {
                r.touched = true;
                r.out.push(format!(
                    "{} Cleared the retired LaunchAgent {}.",
                    "✓".green(),
                    e.label
                ));
            }
            EvictionOutcome::Absent => {}
            EvictionOutcome::Failed(why) => {
                r.failed = true;
                r.errors.push(format!(
                    "{} {} is STILL THERE — {why}. Clear it by hand: \
                     `launchctl bootout gui/$(id -u)/{}` then \
                     `rm -f ~/Library/LaunchAgents/{}.plist`",
                    "✗".red(),
                    e.label,
                    e.label,
                    e.label
                ));
            }
            // #6290: `EvictionOutcome` is `#[non_exhaustive]`. A variant added
            // later must not fall through as "nothing happened" — that is the
            // fail-open branch this renderer exists to close, and it would be
            // silent. Failing here forces the new outcome to be given real
            // wording before it can ship.
            other => {
                r.failed = true;
                r.errors.push(format!(
                    "{} {}: unrecognised eviction outcome {other:?} — treat the \
                     unit as still present and check it by hand: \
                     `launchctl list | grep {}`",
                    "✗".red(),
                    e.label,
                    e.label
                ));
            }
        }
    }
    r
}

/// Build a `LaunchdConfig` addressing `label`, for its `launchctl` operations.
///
/// The exec path, args, log dir and keep-alive are irrelevant here:
/// `evict_legacy_detailed` overwrites the label per alias, and only `bootout` /
/// `is_loaded` / `plist_path` are reached — none of which renders a plist. The
/// type requires the rest, so they are filled with the cheapest values that
/// mean nothing, the same minimal-config pattern the installer's
/// `RealServiceEnv::evict_retired` uses.
#[cfg(target_os = "macos")]
fn launchd_handle(label: &str) -> trusty_common::launchd::LaunchdConfig {
    use trusty_common::launchd::{KeepAlive, LaunchdConfig};

    LaunchdConfig {
        label: label.to_owned(),
        exe_path: std::path::PathBuf::from("trusty-analyze"),
        args: Vec::new(),
        log_dir: std::path::PathBuf::from("/tmp"),
        keep_alive: KeepAlive::Always,
        throttle_interval: 0,
        env_vars: Vec::new(),
        fd_limit: None,
        working_directory: None,
    }
}

/// Unload and delete every retired trusty-analyze LaunchAgent.
///
/// Why this is `pub`: it is the migration step, and `setup daemon` runs it on
/// every host so an operator who never types `service uninstall` is still moved
/// off the resident unit.
///
/// # Errors
///
/// Never returns `Err` today — a label that will not go down is reported in the
/// outcome table and exits non-zero, not returned as an error, so the other
/// labels still get their pass. The signature stays fallible for the dispatch
/// seam.
#[cfg(target_os = "macos")]
pub fn service_uninstall() -> Result<()> {
    let labels = trusty_common::launchd_labels::retired_labels_for_member("trusty-analyze");
    // Only `label` matters, and `evict_legacy_detailed` overwrites it per alias.
    let evictions = launchd_handle(labels[0]).evict_legacy_detailed(&labels);
    let r = render_evictions(&evictions);

    for line in &r.out {
        println!("{line}");
    }
    for line in &r.errors {
        eprintln!("{line}");
    }
    if !r.touched && !r.failed {
        println!(
            "{} No trusty-analyze LaunchAgent is installed — it runs on demand.",
            "·".dimmed()
        );
    }
    if r.touched && !r.failed {
        println!(
            "  trusty-analyze now starts on demand; nothing needs to be running \
             for `{}` or `{}` to work.",
            "trusty-analyze deep".cyan(),
            "trusty-review report --analyze".cyan()
        );
    }
    if r.failed {
        std::process::exit(1);
    }
    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(label: &str, outcome: EvictionOutcome) -> LabelEviction {
        LabelEviction::new(label, outcome)
    }

    /// Why: the migration's whole job. A host with `com.trusty.analyze` loaded
    /// must end the command told that it is gone, and must not exit non-zero
    /// for having found work to do.
    /// Test: this is the test.
    #[test]
    fn render_reports_a_cleared_unit() {
        let r = render_evictions(&[ev("com.trusty.analyze", EvictionOutcome::Evicted)]);
        assert!(r.touched);
        assert!(!r.failed);
        assert!(r.errors.is_empty());
        assert_eq!(r.out.len(), 1);
        assert!(r.out[0].contains("com.trusty.analyze"), "{:?}", r.out);
    }

    /// Why: on a machine that never installed the unit the command must be a
    /// silent no-op rather than an error — it runs unconditionally from
    /// `setup daemon`, so a line here would be permanent noise.
    /// Test: this is the test.
    #[test]
    fn render_is_silent_for_an_absent_unit() {
        let r = render_evictions(&[
            ev("com.trusty.analyze", EvictionOutcome::Absent),
            ev("com.trusty.trusty-analyze", EvictionOutcome::Absent),
        ]);
        assert_eq!(r, Rendered::default());
    }

    /// Why (#6350 fail-open check): a `launchctl bootout` that refused, and a
    /// plist that survived its delete, are the same hazard — a unit the next
    /// login brings straight back. `EvictionOutcome::Failed` covers both, and
    /// reporting either as done tells the operator the daemon is gone while it
    /// is still restarting.
    /// What: the command fails, names the label, and carries the reason
    /// through so the operator knows which of the two happened.
    /// Test: this is the test.
    #[test]
    fn render_fails_closed_on_a_unit_that_would_not_go() {
        for why in [
            "still loaded after `launchctl bootout` reported success",
            "could not delete /Users/x/Library/LaunchAgents/com.trusty.analyze.plist: EPERM",
        ] {
            let r = render_evictions(&[ev(
                "com.trusty.analyze",
                EvictionOutcome::Failed(why.to_owned()),
            )]);
            assert!(r.failed, "a surviving unit must exit non-zero: {why}");
            assert!(r.out.is_empty(), "nothing was cleared: {:?}", r.out);
            assert_eq!(r.errors.len(), 1);
            assert!(r.errors[0].contains("com.trusty.analyze"), "{:?}", r.errors);
            assert!(
                r.errors[0].contains(why),
                "the operator needs the cause verbatim: {:?}",
                r.errors
            );
        }
    }

    /// Why: both labels exist on real hosts, and a pass that cleared one while
    /// the other refused must still fail. Letting a success anywhere in the
    /// list mask a failure is how a partially-migrated host reports clean.
    /// Test: this is the test.
    #[test]
    fn render_fails_even_when_another_label_was_cleared() {
        let r = render_evictions(&[
            ev("com.trusty.analyze", EvictionOutcome::Evicted),
            ev(
                "com.trusty.trusty-analyze",
                EvictionOutcome::Failed("still loaded".to_owned()),
            ),
        ]);
        assert!(r.touched);
        assert!(r.failed, "one survivor fails the whole pass");
        assert_eq!(r.errors.len(), 1);
    }

    /// Why: the label set comes from the shared registry, so an alias added
    /// there is evicted here without this file changing. The order is asserted
    /// because the canonical label holds the running process.
    /// Test: this is the test.
    #[test]
    fn the_retired_label_set_comes_from_the_registry() {
        let labels = trusty_common::launchd_labels::retired_labels_for_member("trusty-analyze");
        assert_eq!(
            labels,
            vec![
                trusty_common::launchd_labels::ANALYZE,
                "com.trusty.trusty-analyze"
            ]
        );
    }
}
