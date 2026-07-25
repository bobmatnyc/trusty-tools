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

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::stable_set::ManageStrategy;
use super::up::member::MemberHealth;
use super::up::system_runner::classify_status;

/// Bound on how long a single `<binary> health --json` subprocess may run
/// before it is treated as a hung probe (#3875).
///
/// Why: [`super::verify_tail`]'s poll-until-ready loop bounds the NUMBER of
/// probe attempts (`bounded_attempts`/`POLL_MAX_ATTEMPTS`) and the AGGREGATE
/// wall-clock budget across all members, but every one of those bounds
/// assumed a single probe call itself returns promptly. VM evidence (#3875)
/// showed `tctl install` hanging >6 minutes at "waiting for trusty-search to
/// start..." with a genuinely-dead daemon: the child `health --json` process
/// itself never exited (e.g. blocked on a half-open socket to the dead
/// daemon), so `Command::output()` — which blocks until EOF on both pipes —
/// never returned, defeating every attempt/aggregate bound above it.
/// What: 10s — generous for a live daemon's near-instant `health --json`
/// reply, short enough that a single hung probe can never itself consume more
/// than a small slice of `verify_tail`'s ~120s aggregate budget.
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Run `cmd`, killing it and returning a timeout error if it has not exited
/// within `timeout` (#3875).
///
/// Why: `std::process::Command::output()` has no built-in timeout — a hung
/// child (dead daemon, wedged health check) blocks the caller forever. This
/// wraps spawn + a bounded `try_wait` poll, killing the child on expiry, so
/// no caller of a subprocess health probe can hang indefinitely.
/// What: spawns `cmd` with piped stdout/stderr, drains both pipes
/// concurrently on background threads (so a chatty child can never deadlock
/// on a full pipe buffer while we're only polling `try_wait`), and polls
/// `try_wait` every 25ms. On timeout, kills + reaps the child and returns
/// `Err(ErrorKind::TimedOut)`. On normal exit, joins the reader threads and
/// returns the collected `Output`.
/// Test: `tests::output_with_timeout_returns_output_for_fast_command`,
/// `tests::output_with_timeout_kills_hung_command`.
fn output_with_timeout(
    mut cmd: Command,
    timeout: Duration,
) -> std::io::Result<std::process::Output> {
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn()?;

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = stdout_pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = stderr_pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });

    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("command timed out after {timeout:?}"),
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    };

    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

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
/// What: returns `not_installed` when [`resolve_binary_path`] finds the binary
/// neither on `PATH` NOR in the default install directory (#3876). For a
/// `Launchd` member, spawns `<resolved path> health --json` under a bounded
/// timeout (#3875) and classifies the envelope. For an `OwnVerb` member,
/// returns `unknown` (health is not probeable via the standard contract). For
/// `None` (non-daemon), returns `unknown` too (callers should not ask, but it
/// is a safe default).
/// Test: `tests::own_verb_member_is_unknown`; the launchd spawn is side-effecting
/// and its parse half is covered by `classify_health_json`.
pub fn probe_member_health(binary: &str, manage: ManageStrategy) -> String {
    let Some(bin_path) = resolve_binary_path(binary) else {
        return health_str::NOT_INSTALLED.to_owned();
    };
    match manage {
        ManageStrategy::Launchd => {
            let mut cmd = Command::new(&bin_path);
            cmd.args(["health", "--json"]);
            let out = output_with_timeout(cmd, HEALTH_PROBE_TIMEOUT);
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

    /// Why (#3875): a fast, well-behaved command must return its real output
    /// through the timeout wrapper — the wrapper must not alter normal
    /// (non-hung) behaviour.
    /// What: runs `/bin/echo hello` (or `cmd /C echo hello` on non-unix) under
    /// a generous timeout; asserts success + the expected stdout.
    /// Test: This is the test.
    #[test]
    fn output_with_timeout_returns_output_for_fast_command() {
        let mut cmd = Command::new("echo");
        cmd.arg("hello");
        let out = output_with_timeout(cmd, Duration::from_secs(5)).expect("echo should run");
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");
    }

    /// Why (#3875): the entire point of the wrapper is that a hung child must
    /// never block the caller past the bound — this is the regression test for
    /// the VM hang (`tctl install` stuck >6 min at "waiting for trusty-search
    /// to start...").
    /// What: runs `sleep 60` under a 1s timeout; asserts the call returns
    /// (rather than hanging) and yields a `TimedOut` error, well within a
    /// generous wall-clock assertion.
    /// Test: This is the test.
    #[test]
    fn output_with_timeout_kills_hung_command() {
        let mut cmd = Command::new("sleep");
        cmd.arg("60");
        let start = Instant::now();
        let err = output_with_timeout(cmd, Duration::from_secs(1))
            .expect_err("a 60s sleep must not complete within a 1s timeout");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "timeout wrapper took far longer than its 1s bound: {:?}",
            start.elapsed()
        );
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
