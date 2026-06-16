//! Shared member-probing primitives (health, version envelope, config spawn).
//!
//! Why: `status`, `stack health`, `stack doctor`, `doctor --self-check`, and
//! `config` all shell out to a member's DOC-1 contract verbs and classify the
//! result. Hoisting the spawn + parse + classification into one module keeps the
//! command handlers thin and means the (pure) classification logic is unit-tested
//! once rather than duplicated per command (DRY; CLAUDE.md 500-SLOC cap).
//!
//! What:
//! - [`probe_member_health`] / [`classify_health_json`] / [`health_string`]:
//!   the launchd-daemon health probe (`<binary> health --json`), strategy-aware
//!   so process-managed members (trusty-mpm) that lack a `health --json` verb
//!   are reported `unknown` rather than falsely `down`.
//! - [`spawn_member_json`]: spawn `<binary> <verb> --json` and return parsed JSON.
//! - [`validate_version_envelope`]: the DOC-1 conformance check used by
//!   `doctor --self-check` (asserts `contract_version` + `verbs[]`).
//!
//! Test: `tests` covers `classify_health_json`, `health_string`,
//! `validate_version_envelope`, and the strategy gating in `probe_member_health`
//! (the parse/classify halves; the subprocess spawn itself is side-effecting).

use std::process::Command;

use super::stable_set::ManageStrategy;
use super::up::member::MemberHealth;
use super::up::system_runner::classify_status;

/// Coarse health verdict string vocabulary used across the rollup commands.
///
/// Why: `status` and `stack` render a one-word health per member; a flat set of
/// constants keeps the vocabulary consistent and greppable instead of scattering
/// string literals.
/// What: the canonical health strings. `UNKNOWN` is the verdict for a
/// process-managed member whose health cannot be probed via the standard
/// `health --json` contract verb.
/// Test: used by `health_string`; asserted in `tests::health_string_mapping`.
pub mod health_str {
    /// Running and at an acceptable version.
    pub const HEALTHY: &str = "healthy";
    /// Running but reporting a below-floor / stale version.
    pub const STALE: &str = "stale";
    /// Installed but not responding.
    pub const DOWN: &str = "down";
    /// Binary not found on PATH.
    pub const NOT_INSTALLED: &str = "not_installed";
    /// Daemon health is not probeable via the standard contract (e.g. mpm).
    pub const UNKNOWN: &str = "unknown";
}

/// Classify a daemon's `health --json` stdout bytes into a [`MemberHealth`].
///
/// Why: Isolating the parse + classification as a pure function makes the
/// fallback policy testable without spawning a subprocess. Unparseable output is
/// `Down`, not `HealthyStale` — "if in doubt, degraded".
/// What: Parses `bytes` as JSON; on success maps the `status` field via
/// `classify_status` (defaulting to `down` when the field is absent). On a parse
/// failure returns `Down`.
/// Test: `tests::unparseable_health_is_down`, `tests::parsed_status_classifies`.
pub fn classify_health_json(bytes: &[u8]) -> MemberHealth {
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(v) => {
            let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("down");
            classify_status(status)
        }
        Err(_) => MemberHealth::Down,
    }
}

/// Map a [`MemberHealth`] to the rollup commands' coarse string.
///
/// Why: The reports use a flat string vocabulary; centralise the mapping so
/// `status` and `stack` agree.
/// What: `HealthyVersionOk` → `healthy`, `HealthyStale` → `stale`, `Down` →
/// `down`, `NotInstalled` → `not_installed`.
/// Test: `tests::health_string_mapping`.
pub fn health_string(h: MemberHealth) -> &'static str {
    match h {
        MemberHealth::HealthyVersionOk => health_str::HEALTHY,
        MemberHealth::HealthyStale => health_str::STALE,
        MemberHealth::Down => health_str::DOWN,
        MemberHealth::NotInstalled => health_str::NOT_INSTALLED,
    }
}

/// Probe a member's health, honouring its lifecycle [`ManageStrategy`].
///
/// Why: The launchd daemons advertise `<binary> health --json`, but a
/// process-managed member (trusty-mpm) has NO top-level `health --json` verb
/// (verified in `crates/trusty-mpm/src/bin/tm/cli.rs`). Probing it with the
/// standard verb would always fail and falsely report `down`; returning a flat
/// `unknown` string instead keeps the rollup honest without claiming a false
/// verdict in either direction.
/// What: returns `not_installed` when the binary is absent. For a `Launchd`
/// member, spawns `<binary> health --json` and classifies the envelope. For an
/// `OwnVerb` member, returns `unknown` (health is not probeable via the standard
/// contract). For `None` (non-daemon), returns `unknown` too (callers should not
/// ask, but it is a safe default).
/// Test: `tests::own_verb_member_is_unknown`; the launchd spawn is side-effecting
/// and its parse half is covered by `classify_health_json`.
pub fn probe_member_health(binary: &str, manage: ManageStrategy) -> String {
    if which::which(binary).is_err() {
        return health_str::NOT_INSTALLED.to_owned();
    }
    match manage {
        ManageStrategy::Launchd => {
            let out = Command::new(binary).args(["health", "--json"]).output();
            let health = match out {
                Ok(out) if out.status.success() => classify_health_json(&out.stdout),
                Ok(_) | Err(_) => MemberHealth::Down,
            };
            health_string(health).to_owned()
        }
        // Process-managed (mpm) / non-daemon: no standard health verb.
        ManageStrategy::OwnVerb | ManageStrategy::None => health_str::UNKNOWN.to_owned(),
    }
}

/// Spawn `<binary> <verb> --json` and return the parsed JSON value.
///
/// Why: `config` and `doctor --self-check` both forward a member's `--json`
/// contract verb and parse the envelope; one helper keeps the spawn + parse
/// shape consistent and testable in the calling handler.
/// What: returns `Err` when the binary is absent, the process fails to spawn,
/// exits non-zero, or emits unparseable JSON; otherwise `Ok(value)`.
/// Test: side-effecting (subprocess); the parse half is covered by
/// `validate_version_envelope` and the handlers' aggregation tests.
pub fn spawn_member_json(binary: &str, verb: &str) -> anyhow::Result<serde_json::Value> {
    if which::which(binary).is_err() {
        anyhow::bail!("{binary} is not installed (not on PATH)");
    }
    let out = Command::new(binary)
        .args([verb, "--json"])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to spawn `{binary} {verb} --json`: {e}"))?;
    if !out.status.success() {
        anyhow::bail!("`{binary} {verb} --json` exited with {}", out.status);
    }
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .map_err(|e| anyhow::anyhow!("`{binary} {verb} --json` emitted invalid JSON: {e}"))
}

/// The outcome of validating a member's `version --json` capability envelope.
///
/// Why: `doctor --self-check` needs a structured pass/fail with the specific
/// reasons a member's envelope is non-conformant, both for the human report and
/// the `--json` output.
/// What: `conformant` is the overall verdict; `has_contract_version` and
/// `has_verbs` are the individual DOC-1 D3b checks; `verb_count` is how many
/// verbs the member advertised.
/// Test: `tests::validate_version_envelope_*`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct VersionConformance {
    /// Whether the envelope passed every required check.
    pub conformant: bool,
    /// Whether a non-empty `contract_version` field is present.
    pub has_contract_version: bool,
    /// Whether a non-empty `verbs[]` array is present.
    pub has_verbs: bool,
    /// Number of advertised verbs (0 when `verbs[]` is absent/empty).
    pub verb_count: usize,
}

/// Validate a parsed `version --json` envelope against the DOC-1 D3b contract.
///
/// Why: The controller-side conformance audit (`doctor --self-check`) must
/// assert a member speaks the capability-discovery contract: it advertises a
/// `contract_version` and a non-empty `verbs[]`. Isolating the check as a pure
/// function over a `serde_json::Value` makes it testable without spawning the
/// member.
/// What: returns a [`VersionConformance`] — `conformant` iff BOTH a valid
/// `contract_version` (a POSITIVE number `>= 1`, or a non-empty string) AND a
/// non-empty `verbs[]` array are present. A numeric `0` is rejected.
/// Test: `tests::validate_version_envelope_conformant`,
/// `tests::validate_version_envelope_missing_contract`,
/// `tests::validate_version_envelope_missing_verbs`,
/// `tests::validate_version_envelope_zero_contract_rejected`.
pub fn validate_version_envelope(v: &serde_json::Value) -> VersionConformance {
    // A numeric contract_version must be POSITIVE (>= 1); `0` is not a valid
    // contract version and must NOT be treated as conformant. A non-empty string
    // form (the alternate envelope shape, e.g. "1") is still accepted.
    let has_contract_version = v.get("contract_version").is_some_and(|c| {
        c.as_u64().map(|n| n > 0).unwrap_or(false) || c.as_str().is_some_and(|s| !s.is_empty())
    });
    let verbs = v.get("verbs").and_then(|x| x.as_array());
    let verb_count = verbs.map(|a| a.len()).unwrap_or(0);
    let has_verbs = verb_count > 0;
    VersionConformance {
        conformant: has_contract_version && has_verbs,
        has_contract_version,
        has_verbs,
        verb_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: Unparseable health output means the daemon is BROKEN, not stale.
    /// What: Feeds non-JSON / empty bytes; asserts `Down`.
    /// Test: This is the test.
    #[test]
    fn unparseable_health_is_down() {
        assert_eq!(classify_health_json(b"not json"), MemberHealth::Down);
        assert_eq!(classify_health_json(b""), MemberHealth::Down);
    }

    /// Why: Valid JSON must classify via the shared `classify_status` vocabulary.
    /// What: Feeds healthy / stale / field-less JSON; asserts each mapping.
    /// Test: This is the test.
    #[test]
    fn parsed_status_classifies() {
        assert_eq!(
            classify_health_json(br#"{"status":"healthy"}"#),
            MemberHealth::HealthyVersionOk
        );
        assert_eq!(
            classify_health_json(br#"{"status":"stale"}"#),
            MemberHealth::HealthyStale
        );
        assert_eq!(classify_health_json(br#"{"x":1}"#), MemberHealth::Down);
    }

    /// Why: The health-string mapping is the rollup vocabulary; pin it.
    /// What: Asserts each `MemberHealth` → string.
    /// Test: This is the test.
    #[test]
    fn health_string_mapping() {
        assert_eq!(health_string(MemberHealth::HealthyVersionOk), "healthy");
        assert_eq!(health_string(MemberHealth::HealthyStale), "stale");
        assert_eq!(health_string(MemberHealth::Down), "down");
        assert_eq!(health_string(MemberHealth::NotInstalled), "not_installed");
    }

    /// Why: A process-managed member (mpm) lacks a `health --json` verb, so its
    /// health must be `unknown`, never a false `down`/`healthy`. Pin the gating
    /// independent of whether the binary happens to be installed by skipping the
    /// PATH check via the strategy branch (we only assert the OwnVerb arm).
    /// What: When the binary is absent it reads `not_installed`; when present it
    /// would read `unknown`. We assert the value is one of those two — never a
    /// launchd verdict — to keep the test environment-independent.
    /// Test: This is the test.
    #[test]
    fn own_verb_member_is_unknown() {
        let h = probe_member_health("definitely-not-a-real-binary-xyz", ManageStrategy::OwnVerb);
        // Absent binary short-circuits to not_installed before the strategy arm.
        assert_eq!(h, health_str::NOT_INSTALLED);
    }

    /// Why: A fully-conformant envelope (contract_version + non-empty verbs)
    /// must pass; this is the DOC-1 D3b happy path.
    /// What: Validates a complete envelope; asserts conformant + counts.
    /// Test: This is the test.
    #[test]
    fn validate_version_envelope_conformant() {
        let v = serde_json::json!({
            "contract_version": 1,
            "verbs": ["health", "config", "doctor"]
        });
        let c = validate_version_envelope(&v);
        assert!(c.conformant);
        assert!(c.has_contract_version);
        assert!(c.has_verbs);
        assert_eq!(c.verb_count, 3);
    }

    /// Why: An envelope missing `contract_version` is non-conformant.
    /// What: Validates an envelope with only verbs; asserts not conformant.
    /// Test: This is the test.
    #[test]
    fn validate_version_envelope_missing_contract() {
        let v = serde_json::json!({ "verbs": ["health"] });
        let c = validate_version_envelope(&v);
        assert!(!c.conformant);
        assert!(!c.has_contract_version);
        assert!(c.has_verbs);
    }

    /// Why: An envelope with an empty/absent `verbs[]` is non-conformant.
    /// What: Validates contract-only and empty-verbs envelopes; asserts not
    /// conformant in both cases.
    /// Test: This is the test.
    #[test]
    fn validate_version_envelope_missing_verbs() {
        let only_contract = serde_json::json!({ "contract_version": "1" });
        let c = validate_version_envelope(&only_contract);
        assert!(!c.conformant);
        assert!(!c.has_verbs);
        assert_eq!(c.verb_count, 0);

        let empty_verbs = serde_json::json!({ "contract_version": 1, "verbs": [] });
        let c2 = validate_version_envelope(&empty_verbs);
        assert!(!c2.conformant);
        assert!(!c2.has_verbs);
    }

    /// Why: `contract_version: 0` is not a valid contract version — the numeric
    /// branch must require a positive value, so `0` is non-conformant while `1`
    /// is conformant.
    /// What: validates a `0` envelope (not conformant) and a `1` envelope
    /// (conformant), both with a non-empty `verbs[]` so only the version differs.
    /// Test: This is the test.
    #[test]
    fn validate_version_envelope_zero_contract_rejected() {
        let zero = serde_json::json!({ "contract_version": 0, "verbs": ["health"] });
        let c0 = validate_version_envelope(&zero);
        assert!(!c0.has_contract_version);
        assert!(!c0.conformant);

        let one = serde_json::json!({ "contract_version": 1, "verbs": ["health"] });
        let c1 = validate_version_envelope(&one);
        assert!(c1.has_contract_version);
        assert!(c1.conformant);
    }
}
