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
//! #4925: probeability is a property of the daemon's HTTP transport, not of its
//! lifecycle-management strategy, so [`probe_member_health`] no longer keys the
//! probe off [`ManageStrategy`]. Both axes coincided while the probe was a
//! `<binary> health --json` subprocess; #4246 moved it to HTTP `/health` and they
//! diverged. `OwnVerb` (trusty-mpm) now takes the same transport as `Launchd`.
//!
//! What:
//! - [`probe_member_health`] / [`health_string`]: the daemon health probe. EVERY
//!   daemon member is probed over HTTP, launchd-supervised or process-managed;
//!   only a non-daemon ([`ManageStrategy::None`]) is `Unprobeable`.
//! - [`spawn_member_json`]: spawn `<binary> <verb> --json` and return parsed JSON.
//! - [`validate_version_envelope`]: the DOC-1 conformance check used by
//!   `doctor --self-check` (asserts `contract_version` + `verbs[]`).
//!
//! Test: `tests` covers `health_string`, `validate_version_envelope`, the
//! non-daemon gating in `probe_member_health`, and (against a stub server) BOTH
//! probed arms end-to-end — `Launchd` (a path no test executed before #4246) and
//! `OwnVerb` (#4925).

use std::path::{Path, PathBuf};

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
pub(super) fn resolve_binary_path(binary: &str) -> Option<PathBuf> {
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
/// What: the canonical health strings. `UNKNOWN` is the verdict for a member
/// with no HTTP transport to probe — post-#4925 that is a NON-DAEMON only, so no
/// member of the shipped stable set produces it (callers filter on `m.daemon`).
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

/// Probe a member's health over HTTP, iff it has an HTTP transport to probe.
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
/// Why (#4925): PROBEABILITY IS A PROPERTY OF THE TRANSPORT, NOT OF THE
/// LIFECYCLE. [`ManageStrategy`] answers "how do I start and stop this member?"
/// — `launchctl bootstrap` versus `trusty-mpm start`. While the probe was a
/// `<binary> health --json` subprocess the two axes plausibly coincided, so this
/// dispatch keyed one off the other; #4246 moved the probe to HTTP `/health` and
/// they separated. Keying off `manage` after that split reported `unknown` for
/// trusty-mpm — a daemon with an `http_addr`, a fixed-port fallback
/// (`probe_http::fixed_port_for`) and a 200 on the first try. The host already
/// demonstrates the separation from the other side: launchd members with NO plist
/// probe `healthy` every day, so launchd supervision was never a precondition
/// either. So the dispatch now asks the transport question — does this member
/// serve HTTP at all? — and only a non-daemon answers no.
///
/// `ManageStrategy` is untouched and still governs everything it was for:
/// `lifecycle::apply_to_member`'s start/stop/restart dispatch and
/// [`super::verify_tail::needs_kickstart`], which independently requires
/// `manage == Launchd`. That second one is why this is a one-arm change rather
/// than a cross-cutting one — a confirmed-down mpm reaches `Refused` here yet
/// still cannot be handed to `launchctl kickstart -k` against the nonexistent
/// `com.trusty.mpm` label, which is also why mpm stays `OwnVerb` in
/// `stable_set::manage_strategy_for` rather than being reclassified.
///
/// ACCEPTED CONSEQUENCE (#4925): mpm is `required: true` (`stable_set`), and
/// `down` — unlike `unknown` — fails `VerifyTailReport::build` and `status`'s
/// exit code. A user who has simply not started mpm therefore gets `tctl status`
/// → `degraded`/exit 2 and `tctl install` → NOT VERIFIED. That is intended: it
/// makes mpm consistent with its declared `required` flag instead of exempt from
/// it, exactly as a stopped trusty-search already behaves. Do NOT re-add a
/// tolerance here; demoting `required` would be a separate, argued decision.
///
/// # Postconditions
/// - A binary resolvable on neither `PATH` nor the default install directory is
///   [`ProbeOutcome::NotInstalled`] (#3876) — the probe never runs.
/// - A DAEMON member — `Launchd` or `OwnVerb` — is probed over HTTP via
///   [`super::probe_http`].
/// - A `None` (non-daemon) member is [`ProbeOutcome::Unprobeable`], rendering
///   `unknown`. It never reaches here in production (callers filter on
///   `m.daemon`) but keeps the safe default.
///
/// What: resolves the binary, then dispatches on whether `manage` describes a
/// daemon. The `app` name handed to the transport is the BINARY name — the
/// `http_addr` discovery file is keyed by app name, and
/// `crate_name == binary == app name` holds for every stable-set daemon INCLUDING
/// trusty-mpm, whose entry is `("trusty-mpm", "trusty-mpm", …)`; the second binary
/// it ships (`tm`) is not a stable-set member and is never the lookup key.
/// Test: `tests::own_verb_member_is_probed_over_http`,
/// `tests::own_verb_member_refused_when_nothing_listens`,
/// `tests::probe_member_health_serves_from_http_addr`,
/// `tests::probe_member_health_refused_when_nothing_listens`.
pub fn probe_member_health(binary: &str, manage: ManageStrategy) -> ProbeOutcome {
    if resolve_binary_path(binary).is_none() {
        return ProbeOutcome::NotInstalled;
    }
    match manage {
        // #4925: BOTH daemon strategies take the HTTP transport. `OwnVerb` is
        // process-managed, not transport-less — trusty-mpm answers `/health` on
        // 7880 — and the `unknown` this arm used to return was a carve-out from
        // #4246, not a fact about the daemon. `needs_kickstart` still gates the
        // destructive repair on `Launchd` alone, so widening this arm cannot arm
        // `kickstart -k` against mpm's nonexistent launchd label.
        ManageStrategy::Launchd | ManageStrategy::OwnVerb => {
            probe_member_http_blocking(binary, binary)
        }
        // #6350: an ON-DEMAND member is `None` as well, and for the same reason
        // as #6290's per-invocation one — its launchd service is retired, so it
        // has no lifecycle. It differs in what answers the health question: it
        // still serves a socket, so `probe_daemon_http`'s `on_demand_member`
        // branch STARTS it and asks `<domain>.health`. That is the only probe
        // that separates "working" from "broken" for a service whose healthy
        // resting state is not listening. Kept as its own arm rather than folded
        // into the retired check, because being retired says nothing about
        // whether a transport exists.
        ManageStrategy::None if super::probe_http::on_demand_member(binary) => {
            probe_member_http_blocking(binary, binary)
        }
        // #6290: a per-invocation member is `None` too — it has no lifecycle —
        // but unlike `tga` it DOES have an honest health answer: the binary
        // resolved above, and `probe_daemon_http` routes it to a presence probe
        // that runs `--version`. Reporting `Unprobeable` (rendered `unknown`)
        // for a tool that is installed and working would be a worse answer than
        // the one available. Narrowed to presence-only members on purpose:
        // routing every `None` member through the transport probe would resolve
        // no address for `tga` and report `down` for a binary that is fine.
        ManageStrategy::None if super::probe_http::presence_only(binary) => {
            probe_member_http_blocking(binary, binary)
        }
        // A non-daemon with no presence contract has no health to ask about.
        // `Unprobeable` is the honest answer, not a policy choice.
        ManageStrategy::None => ProbeOutcome::Unprobeable,
    }
}

/// How long a forwarded contract verb may run before it is given up on.
///
/// Why (#5805): matches `trusty_common::update::verify_installed_binary_at_path`'s
/// health-gate window, so the two subprocess probes `tctl` performs on a member
/// binary give up after the same wait rather than each picking a number.
const VERB_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Spawn `<binary> <verb> --json` and return the parsed JSON value.
///
/// Why: `config` and `doctor --self-check` both forward a member's `--json`
/// contract verb and parse the envelope; one helper keeps the spawn + parse
/// shape consistent and testable in the calling handler.
///
/// Why the timeout (#5805): this ran `Command::output()` with no deadline, so a
/// member binary that never exits — blocked on a lock, on stdin, on a hung
/// network read — froze `tctl config` indefinitely with nothing on stdout. That
/// is a defect in its own right, independent of the self-spawn recursion
/// `config::partition_forwardable` fixes; it made the recursion unbounded
/// rather than causing it, and a deadline alone would have left the fork
/// storm intact. Same reasoning `shadow_check::detect` records for refusing
/// the un-timed-out `update_engine::installed_version`.
///
/// # Postconditions
/// - Returns within [`VERB_TIMEOUT`] plus parse time, whatever the child does.
/// - A child still running at the deadline is killed, not orphaned — the
///   `tokio::process::Command` future drops with `kill_on_drop` semantics.
///
/// What: resolves `binary` on `PATH`, then defers to [`spawn_json_at_path`].
/// Callable only from a SYNCHRONOUS context — it builds its own tokio runtime,
/// and a nested `block_on` inside one panics. Both callers (`config::run`,
/// `doctor::run`) are synchronous `main.rs` handlers.
///
/// Test: `tests::spawn_json_at_path_gives_up_on_a_child_that_never_exits`,
/// `tests::spawn_json_at_path_parses_a_well_formed_envelope`; the PATH
/// resolution half is side-effecting.
pub fn spawn_member_json(binary: &str, verb: &str) -> anyhow::Result<serde_json::Value> {
    let Ok(path) = which::which(binary) else {
        anyhow::bail!("{binary} is not installed (not on PATH)");
    };
    super::runtime::block_on(spawn_json_at_path(&path, binary, verb))
}

/// Spawn `<path> <verb> --json` under [`VERB_TIMEOUT`] and parse the envelope.
///
/// Why: separating the concrete-path spawn from the `PATH` lookup is what makes
/// the deadline testable — a test can point this at a script that hangs without
/// mutating the process environment. Mirrors
/// `trusty_common::update::verify_installed_binary_at_path`, which split for the
/// same reason.
///
/// What: `label` is the name used in error messages (the binary name the caller
/// asked for, not the resolved path). Returns `Err` when the process fails to
/// spawn, exceeds [`VERB_TIMEOUT`], exits non-zero, or emits unparseable JSON.
///
/// Test: `tests::spawn_json_at_path_gives_up_on_a_child_that_never_exits`,
/// `tests::spawn_json_at_path_parses_a_well_formed_envelope`.
async fn spawn_json_at_path(
    path: &Path,
    label: &str,
    verb: &str,
) -> anyhow::Result<serde_json::Value> {
    let out = match tokio::time::timeout(
        VERB_TIMEOUT,
        tokio::process::Command::new(path)
            .args([verb, "--json"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    {
        Err(_) => anyhow::bail!(
            "`{label} {verb} --json` did not answer within {}s",
            VERB_TIMEOUT.as_secs()
        ),
        Ok(Err(e)) => anyhow::bail!("failed to spawn `{label} {verb} --json`: {e}"),
        Ok(Ok(out)) => out,
    };
    if !out.status.success() {
        anyhow::bail!("`{label} {verb} --json` exited with {}", out.status);
    }
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .map_err(|e| anyhow::anyhow!("`{label} {verb} --json` emitted invalid JSON: {e}"))
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

    /// Why (#4925): THE reversal. This test used to be
    /// `own_verb_member_is_unknown`, pinning `OwnVerb` → `Unprobeable` →
    /// `unknown` so "a well-meaning follow-up cannot flip it silently". That pin
    /// was the #4246 carve-out, and #4925 is the argued decision that removes it:
    /// probeability tracks the HTTP transport, not the lifecycle strategy, and
    /// trusty-mpm serves `/health`. The assertion is inverted rather than deleted
    /// so the reversal is recorded in the same place the carve-out was.
    /// What: plants an `http_addr` for a stub answering `{"status":"ok"}` and
    /// asserts an `OwnVerb` member probes it — `Serving`/`healthy`, NOT
    /// `Unprobeable`/`unknown`.
    /// Test: This is the test.
    #[test]
    fn own_verb_member_is_probed_over_http() {
        use crate::commands::test_support as ts;
        let _guard = ts::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let addr = ts::stub_seq_blocking(vec![("HTTP/1.1 200 OK", r#"{"status":"ok"}"#)]);
        let dir = ts::stub_data_dir(ts::PROBEABLE_BINARY, &addr);
        let outcome = probe_member_health(ts::PROBEABLE_BINARY, ManageStrategy::OwnVerb);
        ts::clear_data_dir_override(&dir);

        assert_eq!(
            outcome,
            ProbeOutcome::Serving {
                status: "ok".to_owned(),
                version: None,
            },
            "an OwnVerb daemon must take the HTTP transport, not the unknown carve-out"
        );
        assert_eq!(outcome.health_string(), health_str::HEALTHY);
    }

    /// Why (#4925): the mirror of `own_verb_member_is_probed_over_http`, and the
    /// half that carries the accepted user-visible consequence — a stopped mpm
    /// now reads `down`, which (unlike `unknown`) fails
    /// `VerifyTailReport::build`'s required-member gate and degrades `status`'s
    /// exit code. Asserting it here is what makes that consequence deliberate
    /// rather than emergent.
    /// What: plants an `http_addr` at an address guaranteed to refuse and asserts
    /// an `OwnVerb` member reaches `Refused` → `is_confirmed_down` → `down`.
    /// Test: This is the test.
    #[test]
    fn own_verb_member_refused_when_nothing_listens() {
        use crate::commands::test_support as ts;
        let _guard = ts::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = ts::stub_data_dir(ts::PROBEABLE_BINARY, &ts::dead_addr());
        let outcome = probe_member_health(ts::PROBEABLE_BINARY, ManageStrategy::OwnVerb);
        ts::clear_data_dir_override(&dir);

        assert_eq!(outcome, ProbeOutcome::Refused);
        assert!(outcome.is_confirmed_down());
        assert_eq!(outcome.health_string(), health_str::DOWN);
        // The kickstart gate is a SEPARATE axis and must not have moved with the
        // probe: a confirmed-down `OwnVerb` member is still ineligible for
        // `launchctl kickstart -k` (there is no `com.trusty.mpm` label to
        // kickstart). Asserted end-to-end from a REAL probe outcome here;
        // `verify_tail_tests::needs_kickstart_only_for_confirmed_down_launchd`
        // covers the predicate's full truth table.
        assert!(!crate::commands::verify_tail::needs_kickstart(
            &outcome,
            ManageStrategy::OwnVerb
        ));
    }

    /// Why (#4925): a NON-daemon has no `/health` to ask, so `Unprobeable` must
    /// survive the reversal for `ManageStrategy::None`. Without this the widened
    /// arm could quietly grow to cover every strategy and start probing `tga`.
    /// What: an INSTALLED binary probed with `None` is `Unprobeable`, renders
    /// `unknown`, and is not confirmed-down.
    /// Test: This is the test.
    #[test]
    fn non_daemon_member_is_unprobeable() {
        let outcome = probe_member_health(
            crate::commands::test_support::PROBEABLE_BINARY,
            ManageStrategy::None,
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

    /// Write `body` as an executable `#!/bin/sh` script at `path`.
    #[cfg(unix)]
    fn write_script(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("write script");
        let mut perms = std::fs::metadata(path).expect("stat script").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod script");
    }

    /// Why (#5805): `spawn_member_json` ran `Command::output()` with no
    /// deadline, so a member binary that never exits froze `tctl config`
    /// forever with nothing on stdout. That is what made the self-spawn
    /// recursion unbounded instead of merely wasteful, and it bites just as
    /// hard for any OTHER member binary that hangs.
    ///
    /// `start_paused = true` puts the TIMEOUT on tokio's virtual clock while
    /// the `sleep 300` child runs on the real one — the child really is spawned
    /// and really never answers, so the runtime goes idle and tokio advances to
    /// the 10s deadline at no wall-clock cost. The `elapsed` assertion keeps
    /// that honest: it proves the `Err` came from the deadline expiring, not
    /// from a spawn failure that never exercised the hang.
    /// What: points `spawn_json_at_path` at a hanging script and asserts it
    /// returns an error naming the timeout, after a full [`VERB_TIMEOUT`].
    /// Test: This is the test.
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn spawn_json_at_path_gives_up_on_a_child_that_never_exits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("hangs");
        write_script(&script, "sleep 300\necho '{}'");

        let started = tokio::time::Instant::now();
        let err = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            spawn_json_at_path(&script, "hangs", "config"),
        )
        .await
        .expect("must return, never hang")
        .expect_err("a child that never exits must not report a config envelope");
        let elapsed = started.elapsed();

        assert!(
            err.to_string().contains("did not answer within"),
            "the error must name the timeout, not a parse failure: {err}"
        );
        assert!(
            elapsed >= VERB_TIMEOUT,
            "the Err must come from the deadline expiring, but it gave up after \
             {elapsed:?} — this test would no longer catch an un-timed-out spawn"
        );
    }

    /// Why: the timeout must not have broken the ordinary path — a member that
    /// answers promptly still yields its parsed envelope.
    /// What: points `spawn_json_at_path` at a script echoing a version envelope
    /// and asserts the parsed value round-trips.
    /// Test: This is the test.
    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_json_at_path_parses_a_well_formed_envelope() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("answers");
        write_script(
            &script,
            r#"echo '{"contract_version":1,"verbs":["config"]}'"#,
        );

        let v = spawn_json_at_path(&script, "answers", "version")
            .await
            .expect("a prompt, well-formed answer must parse");
        assert_eq!(v["contract_version"], 1);
        assert!(validate_version_envelope(&v).conformant);
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
