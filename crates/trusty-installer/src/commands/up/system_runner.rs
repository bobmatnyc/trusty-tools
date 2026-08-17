//! Real `Runner` backed by the OS (process spawns + the shared health probe).
//!
//! Why: `ensure_member` (member.rs) is transport-agnostic; this is the concrete
//! implementation that actually brings members up over the OS — `<binary> start`
//! to start one, and `tctl install <member>` to auto-install an always-on member
//! (RESOLVED Q1).
//!
//! #4246: `probe` used to be its OWN health probe — a second copy of the
//! `<binary> health --json` shell-out that `commands::probe` also had, divergent
//! in two respects and wrong in the same way. It now delegates to the single
//! shared probe, so `tctl up` and `tctl status` can never disagree about whether
//! a daemon is up.
//!
//! What: `SystemRunner` implements `Runner`. `probe` delegates to
//! `commands::probe::probe_member_health`; `start` and `install` spawn the
//! corresponding command and map a non-zero exit into an `Err` with context.
//! [`classify_status`] — the shared `status`-word vocabulary — lives here for
//! historical reasons and is used by `probe_http` as well as by `up`.
//!
//! Test: the spawning halves are side-effect-only; their control flow is covered
//! by the `super::tests` matrix run against the mock `Runner`, and
//! `classify_status` is unit-tested directly.

use std::process::Command;

use super::manifest::BootMember;
use super::member::{MemberHealth, Runner};

/// A `Runner` that drives real member binaries over the OS.
///
/// Why: The production path for `tctl up` STAGE ensure.
///
/// What: Stateless; every method shells out to the member's binary / installer.
///
/// Test: Side-effect-only; see module doc.
#[derive(Debug, Default)]
pub struct SystemRunner;

impl SystemRunner {
    /// Construct a `SystemRunner`.
    ///
    /// Why: Explicit constructor for readability at the call site.
    /// What: Returns the unit struct.
    /// Test: Trivial; used by the orchestrator.
    pub fn new() -> Self {
        Self
    }
}

/// Map a daemon `/health` envelope's `status` string to a `MemberHealth` verdict.
///
/// Why: the probe classifies the envelope's `status` field; isolating the
/// mapping makes the (otherwise side-effecting) probe unit-testable and keeps
/// ONE vocabulary shared by `tctl up`, `status`, `stack` and the verify tail.
///
/// #4246: `"running"`/`"serving"` were missing, so a daemon reporting the
/// DOC-1 D4 vocabulary literally (`running | degraded | down`) fell through to
/// `Down` — a false negative baked into the vocabulary itself, and one the test
/// suite defended (`up::tests` asserted `"anything-else" → Down` where
/// `"running"` IS "anything-else"). No shipped daemon emits `running` today
/// (all six emit `ok`), so adding it changes nothing observable now and closes
/// the trap for the next daemon that follows the spec.
///
/// What: `"healthy"`/`"ok"`/`"ready"`/`"running"`/`"serving"` →
/// `HealthyVersionOk`; `"stale"`/`"version_below_floor"`/`"degraded"` →
/// `HealthyStale`; anything else (including `"down"`/`"error"`) → `Down`.
///
/// A genuinely unrecognised word still maps to `Down`, deliberately: that is a
/// DISPLAY verdict only. Since #4246 the destructive repair is gated on
/// [`super::super::probe_http::ProbeOutcome::is_confirmed_down`] — a
/// transport-level observation — so an unknown status word can no longer
/// kickstart anything.
///
/// Test: `super::tests::classify_status_maps_known_values`,
/// `super::tests::classify_status_accepts_spec_vocabulary`.
pub fn classify_status(status: &str) -> MemberHealth {
    match status.to_ascii_lowercase().as_str() {
        "healthy" | "ok" | "ready" | "running" | "serving" => MemberHealth::HealthyVersionOk,
        "stale" | "version_below_floor" | "degraded" => MemberHealth::HealthyStale,
        _ => MemberHealth::Down,
    }
}

impl Runner for SystemRunner {
    /// #4246: routes through the ONE shared probe
    /// (`commands::probe::probe_member_health`) instead of carrying a second,
    /// divergent copy of the same idea. The deleted version shelled out to
    /// `<binary> health --json` — the contract no daemon implements — and differed
    /// from the shared probe in two ways that were both bugs: it used bare
    /// `which::which` for presence (so an installed-but-off-PATH binary read
    /// `NotInstalled` and triggered a spurious full auto-reinstall, the #3876
    /// failure class), and it mapped an unparseable 0-exit envelope to
    /// `HealthyStale` rather than `Down`.
    ///
    /// Neither divergence could change what `tctl up` *does*: `ensure_member`
    /// treats `HealthyStale` and `Down` identically (both fall through to
    /// `start`). What DOES change, and is the point, is that a genuinely healthy
    /// daemon now reads `HealthyVersionOk` and is reported a no-op instead of
    /// being handed a redundant `start` — the same false-`down` class as the
    /// verify tail's spurious kickstart, one layer up.
    ///
    /// #4925 extended that saving to trusty-mpm. It used to keep its pre-#4246
    /// behaviour exactly — `OwnVerb` → `Unprobeable` → `Down` → a redundant (but
    /// idempotent) `start` on every `tctl up`, indistinguishable in the logs from
    /// a real recovery. It is now probed over HTTP like every other daemon, so a
    /// serving mpm reads `HealthyVersionOk` and `up` reports a no-op instead of
    /// spawning a process against a daemon already known to be answering.
    fn probe(&self, member: &BootMember) -> MemberHealth {
        let manage = crate::commands::stable_set::manage_strategy_for(&member.binary, true);
        crate::commands::probe::probe_member_health(&member.binary, manage).member_health()
    }

    fn start(&self, member: &BootMember) -> anyhow::Result<()> {
        let status = Command::new(&member.binary).arg("start").status()?;
        if status.success() {
            Ok(())
        } else {
            anyhow::bail!("`{} start` exited with status {status}", member.binary)
        }
    }

    fn install(&self, member: &BootMember) -> anyhow::Result<()> {
        // Auto-install via the installer's own install verb so the install
        // mechanics (DOC-8) stay in one place. `trusty-installer` is on PATH by
        // definition when `trusty-installer up` is running, so this self-dispatch
        // is safe. (The `tctl` alias also works during the transition period.)
        let status = Command::new("trusty-installer")
            .args(["install", &member.id, "--yes"])
            .status()?;
        if status.success() {
            Ok(())
        } else {
            anyhow::bail!(
                "`trusty-installer install {} --yes` exited with status {status}",
                member.id
            )
        }
    }
}
