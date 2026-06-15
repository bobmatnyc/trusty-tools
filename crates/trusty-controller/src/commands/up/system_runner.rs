//! Real `Runner` backed by the OS (process spawns + PATH probes).
//!
//! Why: `ensure_member` (member.rs) is transport-agnostic; this is the concrete
//! implementation that actually probes each member's DOC-1 contract verbs over
//! the OS — `which <binary>` for presence, `<binary> health --json` for
//! liveness, `<binary> start` to bring it up, and `tctl install <member>` to
//! auto-install an always-on member (RESOLVED Q1).
//!
//! What: `SystemRunner` implements `Runner`. `probe` resolves the binary on
//! PATH then parses the `health --json` envelope's `status` field into a
//! `MemberHealth`; `start` and `install` spawn the corresponding command and
//! map a non-zero exit into an `Err` with context.
//!
//! Test: This file is side-effect-only (it spawns real subprocesses); its logic
//! is covered indirectly by the `super::tests` matrix run against the mock
//! `Runner`, and the JSON-status mapping is unit-tested via `classify_status`.

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

/// Map a DOC-1 `health --json` `status` string to a `MemberHealth` verdict.
///
/// Why: The probe parses the contract envelope's `status` field; isolating the
/// mapping makes the (otherwise side-effecting) probe partially unit-testable.
///
/// What: `"healthy"`/`"ok"`/`"ready"` → `HealthyVersionOk`;
/// `"stale"`/`"version_below_floor"`/`"degraded"` → `HealthyStale`; anything
/// else (including `"down"`/`"error"`) → `Down`.
///
/// Test: `super::tests::classify_status_maps_known_values`.
pub fn classify_status(status: &str) -> MemberHealth {
    match status.to_ascii_lowercase().as_str() {
        "healthy" | "ok" | "ready" => MemberHealth::HealthyVersionOk,
        "stale" | "version_below_floor" | "degraded" => MemberHealth::HealthyStale,
        _ => MemberHealth::Down,
    }
}

impl Runner for SystemRunner {
    fn probe(&self, member: &BootMember) -> MemberHealth {
        // Presence first: a binary not on PATH is NotInstalled (drives auto-install).
        if which::which(&member.binary).is_err() {
            return MemberHealth::NotInstalled;
        }
        // CHECK / VERIFY: `<binary> health --json`. A non-2xx contract or an
        // unparseable envelope means "not healthy" → Down (so the act step runs).
        let out = Command::new(&member.binary)
            .args(["health", "--json"])
            .output();
        let Ok(out) = out else {
            return MemberHealth::Down;
        };
        if !out.status.success() {
            return MemberHealth::Down;
        }
        let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&out.stdout);
        match parsed {
            Ok(v) => {
                let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("down");
                classify_status(status)
            }
            // A 0-exit `health` with no/odd JSON: treat as alive-but-unknown =>
            // stale rather than down so we do not bounce a possibly-healthy daemon.
            Err(_) => MemberHealth::HealthyStale,
        }
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
        // Auto-install via the controller's own install verb so the install
        // mechanics (DOC-8) stay in one place. `tctl` is on PATH by definition
        // when `tctl up` is running, so this self-dispatch is safe.
        let status = Command::new("tctl")
            .args(["install", &member.id, "--yes"])
            .status()?;
        if status.success() {
            Ok(())
        } else {
            anyhow::bail!(
                "`tctl install {} --yes` exited with status {status}",
                member.id
            )
        }
    }
}
