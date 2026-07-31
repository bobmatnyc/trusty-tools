//! Stale-daemon version comparison for `tm doctor` (issue #2332).
//!
//! Why: the #2332 incident traced a daemon that ran 46.8h on pre-migration
//! code purely by hand-correlating tmux timestamps against
//! `~/Library/Logs/trusty-mpm/stderr.log` restart banners — nothing anywhere
//! in the stack told the operator the RUNNING daemon's build had drifted from
//! the INSTALLED `tm` binary (the common `cargo install` restart-forgot
//! failure, same family as #2214). `tm doctor` always executes as the
//! just-installed binary (that is what running the command means), so
//! comparing its own `CARGO_PKG_VERSION` against the version the daemon
//! self-reports on `GET /health` (see [`crate::daemon::api::types::HealthResponse::version`])
//! is a purely client-side COMPARISON — the two values it folds together need
//! no shared process or IPC to compare. It is NOT free of network cost,
//! though: `tm doctor` issues one `GET /health` round-trip in addition to the
//! primary `GET /api/v1/doctor` call, and the two requests are not atomic — the
//! daemon can restart between them. That is handled by treating a transport
//! failure on the `/health` call as `Warn` rather than propagating an error (see
//! `stale_daemon_check` in the `tm` CLI binary's `commands::doctor_stale`
//! module — a separate crate target from this library, so it cannot be an
//! intra-doc link here), not by this module, which only ever sees the
//! already-fetched strings. As of #4230 that single snapshot is fetched ONCE by
//! `commands::misc::doctor` and shared with the `daemon_orphan` check, so the two
//! client-side checks always describe the same daemon.
//! What: [`parse_version_triple`] extracts a `(major, minor, patch)` triple
//! from a `CARGO_PKG_VERSION`-shaped string (mirrors the lightweight parser in
//! [`crate::core::output_style::parse_claude_version`] rather than pulling in
//! the `semver` crate for a same-process comparison this simple).
//! [`check_daemon_version_staleness`] folds `(installed, daemon_reported)`
//! into a [`DoctorCheck`] the CLI prints alongside the server-side probes.
//! Test: the `tests` module below covers match / older / newer / unparseable /
//! empty-daemon-version branches.

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// Stable check name for [`check_daemon_version_staleness`]'s [`DoctorCheck`].
pub const CHECK_NAME: &str = "daemon_version";

/// Parse a `major.minor.patch[-suffix]` string into a comparable triple.
///
/// Why: `CARGO_PKG_VERSION` values are already clean semver (no `v` prefix,
/// no extra whitespace), so this needs far less tolerance than
/// [`crate::core::output_style::parse_claude_version`]'s CLI-output parser —
/// but a pre-release suffix on patch (`"0.42.0-beta"`) must still parse the
/// leading digits rather than failing outright.
/// What: splits on `.`; requires three numeric components, taking only the
/// leading digits of the patch segment. Returns `None` for anything that
/// doesn't start with `major.minor.patch`.
/// Test: `parse_version_triple_plain`, `parse_version_triple_prerelease_suffix`,
/// `parse_version_triple_rejects_malformed`.
pub fn parse_version_triple(raw: &str) -> Option<(u64, u64, u64)> {
    let mut parts = raw.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch_raw = parts.next()?;
    let patch_digits: String = patch_raw
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if patch_digits.is_empty() {
        return None;
    }
    let patch = patch_digits.parse::<u64>().ok()?;
    Some((major, minor, patch))
}

/// Compare the installed binary's version against the running daemon's
/// self-reported version and fold the result into a [`DoctorCheck`].
///
/// Why: the whole point of #2332 — surface drift BEFORE it costs another
/// multi-hour forensics session, without ever hard-failing `tm doctor` over
/// something a simple restart fixes.
/// What: `daemon_reported == ""` (an older daemon predating
/// [`crate::daemon::api::types::HealthResponse::version`], or a daemon that
/// is simply unreachable and never got this far) → `Warn`, since that is
/// itself evidence of exactly the staleness this check exists to catch.
/// Equal strings → `Ok`. Different strings that both parse as
/// `major.minor.patch` → `Warn` when the daemon is older, `Warn` (a distinct,
/// less common message) when the daemon is newer than the installed binary
/// (a downgrade — still worth flagging). Different strings where either side
/// fails to parse → `Warn` with a "could not compare" message rather than
/// guessing. Never `Fail` — a stale daemon still serves traffic; this is
/// advisory, matching every other doctor probe's severity convention.
///
/// `restart_hint` is the command that ACTUALLY restarts the daemon on the calling
/// host (issue #4230 review, HIGH-3). This used to be a hardcoded `tm restart`,
/// which #4230 makes refuse on any host where launchd owns the daemon — so
/// `tm doctor` printed "run `tm restart`" two lines above its own new orphan
/// check, and the prescribed command errored out. The caller resolves the verb via
/// `commands::launchd_probe::daemon_restart_command`; taking it as a parameter
/// keeps this module pure (no filesystem or launchd probing in `core`).
/// Test: `staleness_ok_when_versions_match`, `staleness_warns_when_daemon_older`,
/// `staleness_warns_when_daemon_newer`, `staleness_warns_when_daemon_version_empty`,
/// `staleness_warns_when_unparseable`, `staleness_uses_the_caller_restart_hint`.
pub fn check_daemon_version_staleness(
    installed: &str,
    daemon_reported: &str,
    restart_hint: &str,
) -> DoctorCheck {
    if daemon_reported.is_empty() {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Warn,
            format!(
                "daemon did not report a version on /health — it likely predates this build \
                 (installed binary is v{installed}); restart the daemon (`{restart_hint}`)"
            ),
        );
    }

    if daemon_reported == installed {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!("running daemon (v{daemon_reported}) matches the installed binary"),
        );
    }

    match (
        parse_version_triple(installed),
        parse_version_triple(daemon_reported),
    ) {
        (Some(installed_triple), Some(daemon_triple)) if daemon_triple < installed_triple => {
            DoctorCheck::new(
                CHECK_NAME,
                CheckStatus::Warn,
                format!(
                    "running daemon is older than installed binary — restart the daemon \
                     (daemon: v{daemon_reported}, installed: v{installed}); run \
                     `{restart_hint}`"
                ),
            )
        }
        (Some(installed_triple), Some(daemon_triple)) if daemon_triple > installed_triple => {
            DoctorCheck::new(
                CHECK_NAME,
                CheckStatus::Warn,
                format!(
                    "running daemon (v{daemon_reported}) is NEWER than the installed binary \
                     (v{installed}) — the installed binary may have been downgraded"
                ),
            )
        }
        // Equal triples with differing raw strings (e.g. a pre-release suffix
        // difference) — not the drift this check targets.
        (Some(_), Some(_)) => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!("running daemon (v{daemon_reported}) matches the installed binary"),
        ),
        _ => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Warn,
            format!(
                "could not compare daemon version `{daemon_reported}` against installed \
                 binary version `{installed}` — restart the daemon if unsure \
                 (`{restart_hint}`)"
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in restart hint. Deliberately NOT `tm restart`: #4230 made that
    /// verb refuse on launchd hosts, and a test that passed the old hardcoded
    /// string could not tell whether the message used the caller's value or a
    /// leftover literal.
    const HINT: &str = "launchctl kickstart -k gui/$(id -u)/com.trusty.mpm";

    #[test]
    fn parse_version_triple_plain() {
        assert_eq!(parse_version_triple("0.42.1"), Some((0, 42, 1)));
        assert_eq!(parse_version_triple("12.0.7"), Some((12, 0, 7)));
    }

    #[test]
    fn parse_version_triple_prerelease_suffix() {
        assert_eq!(parse_version_triple("0.42.0-beta"), Some((0, 42, 0)));
    }

    #[test]
    fn parse_version_triple_rejects_malformed() {
        assert_eq!(parse_version_triple(""), None);
        assert_eq!(parse_version_triple("0.42"), None);
        assert_eq!(parse_version_triple("not-a-version"), None);
    }

    #[test]
    fn staleness_ok_when_versions_match() {
        let check = check_daemon_version_staleness("0.42.0", "0.42.0", HINT);
        assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
        assert_eq!(check.name, CHECK_NAME);
    }

    #[test]
    fn staleness_warns_when_daemon_older() {
        let check = check_daemon_version_staleness("0.42.0", "0.41.9", HINT);
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("older than installed binary"));
        assert!(check.message.contains(HINT), "message: {}", check.message);
    }

    /// #4230 review, HIGH-3: every remediation must use the CALLER's verb. The
    /// hardcoded `tm restart` this replaced is a hard error on a launchd host, so
    /// `tm doctor` prescribed a command its own next line had just broken. All
    /// three hint-carrying branches are checked, and none may leak the literal.
    #[test]
    fn staleness_uses_the_caller_restart_hint() {
        for (installed, reported) in [("0.42.0", "0.41.9"), ("0.42.0", ""), ("0.42.0", "garbage")] {
            let msg = check_daemon_version_staleness(installed, reported, HINT).message;
            assert!(msg.contains(HINT), "message: {msg}");
            assert!(
                !msg.contains("tm restart"),
                "must not leak the old hardcoded verb: {msg}"
            );
        }
    }

    #[test]
    fn staleness_warns_when_daemon_newer() {
        let check = check_daemon_version_staleness("0.41.9", "0.42.0", HINT);
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("NEWER"));
    }

    #[test]
    fn staleness_warns_when_daemon_version_empty() {
        let check = check_daemon_version_staleness("0.42.0", "", HINT);
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("did not report a version"));
    }

    #[test]
    fn staleness_warns_when_unparseable() {
        let check = check_daemon_version_staleness("0.42.0", "garbage", HINT);
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("could not compare"));
    }

    #[test]
    fn staleness_ok_when_triples_match_but_raw_strings_differ() {
        // Covers the `(Some(_), Some(_))` equal-triple arm specifically — a
        // pre-release suffix difference (e.g. "0.42.0" vs "0.42.0-beta") must
        // NOT hit the `daemon_reported == installed` exact-string
        // short-circuit above it, since the raw strings differ; it must also
        // NOT hit either `<`/`>` ordering arm, since the triples are equal.
        let check = check_daemon_version_staleness("0.42.0", "0.42.0-beta", HINT);
        assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
        assert_eq!(check.name, CHECK_NAME);
    }
}
