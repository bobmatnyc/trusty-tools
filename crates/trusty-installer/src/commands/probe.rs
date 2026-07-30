//! Shared member-probing primitives (health, version envelope, config spawn).
//!
//! Why: `status`, `stack health`, `stack doctor`, `doctor --self-check`, `up` and
//! the install verify tail all need one member's health verdict, and `config` /
//! `doctor` also forward a member's `--json` contract verb. Hoisting resolution +
//! probe + classification into one module keeps the command handlers thin and
//! means the decision logic is unit-tested once rather than per command (DRY;
//! CLAUDE.md 500-SLOC cap).
//!
//! #4246: [`probe_member_health`] used to spawn `<binary> health --json` — a
//! contract no shipped daemon implements — and collapse every failure into
//! `down`, which then drove `launchctl kickstart -k` against healthy daemons. It
//! now delegates to the HTTP `/health` transport in [`super::probe_http`] and
//! returns a TYPED [`ProbeOutcome`] so the destructive repair can be gated on a
//! real transport-level observation. `classify_health_json` and the
//! `output_with_timeout` subprocess wrapper it needed were deleted with the
//! transport they served.
//!
//! What:
//! - [`probe_member_health`] / [`health_string`]: the strategy-aware daemon
//!   health probe. Process-managed members (trusty-mpm) are reported
//!   `Unprobeable` rather than falsely `down`.
//! - [`spawn_member_json`]: spawn `<binary> <verb> --json` and return parsed JSON.
//! - [`validate_version_envelope`]: the DOC-1 conformance check used by
//!   `doctor --self-check` (asserts `contract_version` + `verbs[]`).
//!
//! Test: `tests` covers `health_string`, `validate_version_envelope`, the
//! strategy gating in `probe_member_health`, and (against a stub server) the
//! launchd arm end-to-end — a path no test executed before #4246.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::probe_http::{probe_member_http_blocking, ProbeOutcome};
use super::stable_set::ManageStrategy;
use super::up::member::MemberHealth;

/// Whether `path` exists and is executable (unix) / a regular file (other
/// platforms) — the concrete-file half of [`resolve_binary_path`]'s #3876
/// PATH-independent fallback.
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Resolve `binary` to a concrete path, trying `PATH` first and falling back
/// to the default install directory (#3876).
///
/// Why: the verify table classified genuinely-installed binaries as
/// `not_installed` whenever `PATH` was incomplete (#3874 — a fresh
/// non-login/non-interactive shell lacks `~/.local/bin`). Relying on `which`
/// alone conflates "not on PATH" with "not installed", which are different
/// facts; probing the install directory directly makes the verdict resilient
/// to a broken PATH instead of cascading its failure into a false negative.
/// What: returns `which::which(binary)`'s path when it resolves; otherwise
/// probes `<default_install_dir>/<binary>` and returns it iff
/// [`is_executable`]; otherwise `None` (genuinely not installed).
/// Test: `tests::resolve_binary_path_install_dir_fallback_detects_executable`,
/// `tests::resolve_binary_path_none_when_absent_everywhere`.
fn resolve_binary_path(binary: &str) -> Option<PathBuf> {
    if let Ok(p) = which::which(binary) {
        return Some(p);
    }
    let candidate = crate::download::default_install_dir()?.join(binary);
    is_executable(&candidate).then_some(candidate)
}

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
/// Why (#4246): this is THE health probe every `tctl` rollup and the install
/// verify tail agree on. It used to spawn `<binary> health --json`, a contract no
/// shipped daemon implements, so every daemon read `down` while every one of them
/// was answering `GET /health` — and that false `down` drove `launchctl kickstart
/// -k`, hard-restarting a healthy stack on every `tctl install`. It now probes
/// over HTTP and returns a TYPED outcome instead of a flat `String`, because the
/// only safe basis for a destructive repair is a transport-level observation
/// ([`ProbeOutcome::is_confirmed_down`]) — a distinction a `String` cannot carry.
///
/// # Postconditions
/// - A binary resolvable on neither `PATH` nor the default install directory is
///   [`ProbeOutcome::NotInstalled`] (#3876) — the probe never runs.
/// - A `Launchd` member is probed over HTTP via [`super::probe_http`].
/// - An `OwnVerb`/`None` member is [`ProbeOutcome::Unprobeable`], rendering
///   `unknown`, and can therefore never be kickstarted.
///
/// What: resolves the binary, then dispatches on `manage`. The `app` name handed
/// to the transport is the BINARY name — the `http_addr` discovery file is keyed
/// by app name, and `crate_name == binary == app name` holds for every stable-set
/// daemon (see `stable_set`; the one member where it does not, trusty-mpm which
/// ships both `tm` and `trusty-mpm`, takes the `OwnVerb` arm and is never
/// probed).
/// Test: `tests::own_verb_member_is_unknown`,
/// `tests::probe_member_health_serves_from_http_addr`,
/// `tests::probe_member_health_refused_when_nothing_listens`.
pub fn probe_member_health(binary: &str, manage: ManageStrategy) -> ProbeOutcome {
    if resolve_binary_path(binary).is_none() {
        return ProbeOutcome::NotInstalled;
    }
    match manage {
        ManageStrategy::Launchd => probe_member_http_blocking(binary, binary),
        // #4246: trusty-mpm (`OwnVerb`) is DELIBERATELY left unprobed and
        // reported `unknown`, even though it does answer `/health` on 7880.
        // It is `required: true` (`stable_set`), and `unknown` is the only
        // verdict `VerifyTailReport::build` and `status`'s exit code tolerate —
        // so probing it would flip `tctl status` to exit 2 and `tctl install` to
        // NOT VERIFIED for every user who simply has not started mpm. Enabling
        // it is a separate, user-visible policy change, tracked separately.
        // `None` (non-daemon) never reaches here in production (callers filter on
        // `m.daemon`) but shares the safe default.
        ManageStrategy::OwnVerb | ManageStrategy::None => ProbeOutcome::Unprobeable,
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

    /// Why: an absent binary must short-circuit to `NotInstalled` BEFORE any
    /// probe runs — resolution failure is a fact about the filesystem, not
    /// evidence about a daemon, and probing anyway would waste a connect bound
    /// per member on a partially-installed stack.
    /// What: a deliberately-fake binary name yields `NotInstalled` for every
    /// strategy.
    /// Test: This is the test.
    #[test]
    fn absent_binary_is_not_installed_for_every_strategy() {
        for manage in [
            ManageStrategy::Launchd,
            ManageStrategy::OwnVerb,
            ManageStrategy::None,
        ] {
            assert_eq!(
                probe_member_health("definitely-not-a-real-binary-xyz", manage),
                ProbeOutcome::NotInstalled,
                "{manage:?}"
            );
        }
    }

    /// Why (#4246): trusty-mpm is `required: true`, and `unknown` is the only
    /// verdict `VerifyTailReport::build` and `status`'s exit code tolerate — so
    /// the `OwnVerb` arm staying `Unprobeable` is what keeps `tctl status` from
    /// exiting 2, and `tctl install` from printing NOT VERIFIED, for every user
    /// who has simply not started mpm. Deliberately unchanged by this fix; pin
    /// it so a well-meaning follow-up cannot flip it silently.
    /// What: an INSTALLED binary probed with `OwnVerb` is `Unprobeable` and
    /// renders `unknown`, never a launchd verdict.
    /// Test: This is the test.
    #[test]
    fn own_verb_member_is_unknown() {
        let outcome = probe_member_health(
            crate::commands::test_support::PROBEABLE_BINARY,
            ManageStrategy::OwnVerb,
        );
        assert_eq!(outcome, ProbeOutcome::Unprobeable);
        assert_eq!(outcome.health_string(), health_str::UNKNOWN);
        assert!(!outcome.is_confirmed_down());
    }

    /// Why (#4246): THE path no test in this crate ever executed — the launchd
    /// arm end-to-end, from `resolve_binary_path` through `http_addr` discovery
    /// to the HTTP round trip. `tctl status` reported six healthy daemons as
    /// `down` for months precisely because nothing exercised it.
    /// What: plants an `http_addr` pointing at a stub answering
    /// `{"status":"ok"}` and asserts the probe reports it serving/`healthy`.
    /// Test: This is the test.
    #[test]
    fn probe_member_health_serves_from_http_addr() {
        use crate::commands::test_support as ts;
        let _guard = ts::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let addr = ts::stub_seq_blocking(vec![("HTTP/1.1 200 OK", r#"{"status":"ok"}"#)]);
        let dir = ts::stub_data_dir(ts::PROBEABLE_BINARY, &addr);
        let outcome = probe_member_health(ts::PROBEABLE_BINARY, ManageStrategy::Launchd);
        ts::clear_data_dir_override(&dir);

        assert_eq!(
            outcome,
            ProbeOutcome::Serving {
                status: "ok".to_owned(),
                version: None,
            }
        );
        assert_eq!(outcome.health_string(), health_str::HEALTHY);
    }

    /// Why (#4246): the mirror — a member whose recorded address refuses must
    /// still reach `Refused`, or the fix would trade a false `down` for a stack
    /// that is never repaired. This is the ONLY outcome (with `Timeout`) allowed
    /// to authorise a kickstart.
    /// What: plants an `http_addr` pointing at a released ephemeral port and
    /// asserts `Refused` + `is_confirmed_down`.
    /// Test: This is the test.
    #[test]
    fn probe_member_health_refused_when_nothing_listens() {
        use crate::commands::test_support as ts;
        let _guard = ts::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = ts::stub_data_dir(ts::PROBEABLE_BINARY, &ts::dead_addr());
        let outcome = probe_member_health(ts::PROBEABLE_BINARY, ManageStrategy::Launchd);
        ts::clear_data_dir_override(&dir);

        assert_eq!(outcome, ProbeOutcome::Refused);
        assert!(outcome.is_confirmed_down());
        assert_eq!(outcome.health_string(), health_str::DOWN);
    }

    /// Why (#3876): the verify table must not report a genuinely-installed
    /// binary as `not_installed` just because it is missing from `PATH` — the
    /// exact false-negative the VM run hit under a broken (#3874) PATH.
    /// What: creates a fake executable at a temp "install dir", monkeys
    /// `resolve_binary_path`'s install-dir fallback by exercising
    /// [`is_executable`] + the join logic it uses directly (since
    /// `default_install_dir` is HOME-derived and not injectable here), proving
    /// the executable-detection half of the fallback in isolation.
    /// Test: This is the test.
    #[test]
    fn resolve_binary_path_install_dir_fallback_detects_executable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin_path = dir.path().join("fake-trusty-search");
        std::fs::write(&bin_path, b"#!/bin/sh\necho hi\n").expect("write fake binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&bin_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bin_path, perms).unwrap();
        }
        assert!(is_executable(&bin_path));

        let non_exec = dir.path().join("not-a-binary.txt");
        std::fs::write(&non_exec, b"just text").expect("write plain file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&non_exec).unwrap().permissions();
            perms.set_mode(0o644);
            std::fs::set_permissions(&non_exec, perms).unwrap();
        }
        #[cfg(unix)]
        assert!(!is_executable(&non_exec));
    }

    /// Why (#3876): a binary absent from both `PATH` and the install
    /// directory must still resolve to `None` (genuinely not installed) — the
    /// fallback must not turn every lookup into a false positive.
    /// What: asserts `resolve_binary_path` returns `None` for an
    /// unmistakably-fake binary name.
    /// Test: This is the test.
    #[test]
    fn resolve_binary_path_none_when_absent_everywhere() {
        assert!(resolve_binary_path("definitely-not-a-real-binary-xyz-3876").is_none());
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
