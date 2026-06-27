//! Per-member ensure: CHECK → act → VERIFY (DOC-12 §3 STAGE ensure).
//!
//! Why: Every always-on stage step follows the identical idempotent shape from
//! DOC-12 §3.4: CHECK `<binary> health --json` → if healthy no-op; if down
//! `<binary> start`; if missing auto-install (always-on only); then VERIFY with
//! `<binary> health --json`. Centralising the shape keeps the stage code thin
//! and lets the actual process execution be mocked for unit tests.
//!
//! What: Defines the `Runner` trait (probe presence / health / start / install
//! a member) so the orchestrator can run against the real OS or a test double,
//! the real `SystemRunner`, the `EnsureOutcome` result, and `ensure_member()`
//! which implements the §3.4 check→act→verify with auto-install gated on policy.
//!
//! Test: `super::tests` drives `ensure_member` with a scripted `Runner` double
//! across the healthy/down/missing-always-on/missing-opt-in matrices.

use serde::Serialize;

use super::manifest::{BootMember, BootPolicy};

/// The liveness verdict for a member after probing its contract.
///
/// Why: The §3.4 ensure logic and the §5 gating need a small status vocabulary
/// distinguishing "running and current" from "running but stale" from "down"
/// from "not installed".
///
/// What: Mirrors the DOC-1 D4 status enum subset the boot path acts on.
///
/// Test: `super::tests` outcome assertions.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberHealth {
    /// Running and at an acceptable version.
    HealthyVersionOk,
    /// Running but reporting a below-floor / stale version.
    HealthyStale,
    /// Installed but not running.
    Down,
    /// Binary not found on PATH.
    NotInstalled,
}

/// Abstraction over the OS actions `ensure_member` performs on a member binary.
///
/// Why: The orchestrator shells out to each member's contract verbs
/// (`health --json`, `start`) and to the installer. Hiding that behind a trait
/// makes the §3.4 control flow unit-testable without spawning real daemons.
///
/// What: Three operations, each keyed by the member's `binary` / `id`:
/// `probe` (CHECK / VERIFY), `start` (act when down), `install` (act when a
/// missing always-on member must be auto-installed to latest).
///
/// Test: `super::tests::ScriptedRunner` implements this for the matrix tests.
pub trait Runner {
    /// Probe the member's liveness via its `health --json` contract verb.
    ///
    /// Why: This is both the CHECK (before acting) and the VERIFY (after acting)
    /// step of §3.4.
    /// What: Returns the `MemberHealth` verdict for `member`.
    /// Test: scripted in `super::tests`.
    fn probe(&self, member: &BootMember) -> MemberHealth;

    /// Start the member daemon (`<binary> start`).
    ///
    /// Why: The §3.4 act-when-down step.
    /// What: Returns `Ok(())` on a clean start, `Err` with context on failure.
    /// Test: scripted in `super::tests`.
    fn start(&self, member: &BootMember) -> anyhow::Result<()>;

    /// Auto-install the member to latest (`tctl install <member>`).
    ///
    /// Why: The §3.4 / RESOLVED-Q1 act-when-missing step, gated to always-on
    /// members by the caller.
    /// What: Returns `Ok(())` once installed, `Err` with context on failure.
    /// Test: scripted in `super::tests`.
    fn install(&self, member: &BootMember) -> anyhow::Result<()>;
}

/// The result of ensuring a single member.
///
/// Why: The stage layer needs to know both the final health and *what action*
/// was taken (no-op vs started vs installed) to render the §5 report and decide
/// gating.
///
/// What: `health` is the post-ensure verdict; `action` is a human label;
/// `gated_ok` is whether the member reached `healthy + version-ok`.
///
/// Test: `super::tests` outcome assertions.
#[derive(Clone, Debug, Serialize)]
pub struct EnsureOutcome {
    /// Member id ensured.
    pub member: String,
    /// Post-ensure health verdict.
    pub health: MemberHealth,
    /// Human label for the action taken (`no-op`, `started`, `installed+started`, `failed`).
    pub action: String,
    /// Whether the member reached `healthy + version-ok` (the §3.2 gate basis).
    pub gated_ok: bool,
}

impl EnsureOutcome {
    /// Construct an outcome whose health is the gate basis.
    ///
    /// Why: Collapses the repeated "health → gated_ok" derivation.
    /// What: `gated_ok = matches!(health, HealthyVersionOk)`.
    /// Test: Exercised by `ensure_member` and its tests.
    fn new(member: &str, health: MemberHealth, action: &str) -> Self {
        Self {
            member: member.to_owned(),
            health,
            action: action.to_owned(),
            gated_ok: matches!(health, MemberHealth::HealthyVersionOk),
        }
    }
}

/// Ensure a single member is running (DOC-12 §3.4 CHECK → act → VERIFY).
///
/// Why: Implements the load-bearing idempotency contract: a healthy member is a
/// reported no-op (never bounced), a down member is started, and a missing
/// **always-on** member is auto-installed to latest then started (RESOLVED Q1);
/// a missing opt-in/on-demand member is *not* installed (the caller guides).
///
/// What:
/// 1. CHECK `probe`. If `HealthyVersionOk` → no-op.
/// 2. If `NotInstalled` and policy is `AlwaysOn` → `install` then `start`,
///    else return `NotInstalled` (caller guides — §3.4/§6).
/// 3. If `Down` (or `HealthyStale`) → `start`.
/// 4. VERIFY with a second `probe`; the verified health drives `gated_ok`.
///
/// Test: `super::tests::ensure_*` cover healthy-noop, down-start,
/// missing-always-on-installs, missing-opt-in-guides, start-failure.
pub fn ensure_member(runner: &dyn Runner, member: &BootMember) -> EnsureOutcome {
    // STEP 1 — CHECK.
    let initial = runner.probe(member);
    if initial == MemberHealth::HealthyVersionOk {
        return EnsureOutcome::new(&member.id, initial, "no-op (already healthy)");
    }

    // STEP 2/3 — act.
    let mut installed = false;
    if initial == MemberHealth::NotInstalled {
        if member.policy == BootPolicy::AlwaysOn {
            if let Err(e) = runner.install(member) {
                tracing::error!(member = %member.id, error = %e, "auto-install failed");
                return EnsureOutcome::new(
                    &member.id,
                    MemberHealth::NotInstalled,
                    "install-failed",
                );
            }
            installed = true;
        } else {
            // Opt-in / on-demand: never auto-install — caller guides (§3.4/§6).
            return EnsureOutcome::new(
                &member.id,
                MemberHealth::NotInstalled,
                "guide (not installed)",
            );
        }
    }

    if let Err(e) = runner.start(member) {
        tracing::error!(member = %member.id, error = %e, "start failed");
        return EnsureOutcome::new(&member.id, MemberHealth::Down, "start-failed");
    }

    // STEP 4 — VERIFY.
    let verified = runner.probe(member);
    let action = if installed {
        "installed+started"
    } else {
        "started"
    };
    EnsureOutcome::new(&member.id, verified, action)
}
