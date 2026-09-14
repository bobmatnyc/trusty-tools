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
//! though: it needs one `GET /health` round-trip, which #4230 made a single
//! snapshot shared with the `daemon_orphan` check so the two client-side
//! checks always describe the same daemon.
//!
//! #6336 removed the OTHER round-trip. `tm doctor` used to fetch the whole
//! report from `GET /api/v1/doctor` and this comparison rode along on the same
//! reachable daemon; the report now runs in-process and that shared `/health`
//! probe is the ONLY daemon call doctor makes. When it fails, this check is
//! SKIPPED, not reported: with no version to compare against there is nothing
//! to say, and the daemon-reachability row already says the daemon is absent.
//! Both call sites live in the `tm` CLI binary's
//! `commands::{doctor_local, doctor_stale}` modules — a separate crate target
//! from this library, so neither can be an intra-doc link here. This module
//! only ever sees already-fetched strings.
//! What: [`parse_version_triple`] extracts a `(major, minor, patch)` triple
//! from a `CARGO_PKG_VERSION`-shaped string (mirrors the lightweight parser in
//! [`crate::core::output_style::parse_claude_version`] rather than pulling in
//! the `semver` crate for a same-process comparison this simple).
//! [`check_daemon_version_staleness`] folds `(installed, daemon_reported)`
//! into a [`DoctorCheck`] the CLI prints alongside the server-side probes.
//!
//! #7822 added the SECOND axis. Two agreeing semvers were treated as a match,
//! which is wrong the moment a daemon outlives a merge cut under the same
//! version — the 2026-09-13 incident, where a 1.5.36 daemon served the pre-#7789
//! settings writer while the installed binary was also 1.5.36 and this check
//! reported `Ok`. When the versions agree the verdict now turns on the build
//! ids in [`BuildIdentity`] (see [`crate::core::build_identity`] for what the
//! id is, and why #7873 made it compile-time rather than a stat of the calling
//! binary's own file). An absent id reports "cannot tell", never `Ok`.
//!
//! Test: the `tests` module below covers match / older / newer / unparseable /
//! empty-daemon-version branches, plus every #7822 build-identity branch.

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

/// The two build ids the verdict turns on once the versions agree (issue
/// #7822).
///
/// Why: passed as one struct rather than two loose `&str` parameters so a call
/// site cannot silently swap the daemon's id for the installed one — they are
/// the same type and the mistake would invert nothing observable.
/// What: `installed` is the id compiled into the binary running the CHECK (`tm
/// doctor` always executes as the just-installed binary). #7873 made that id
/// compile-time, so the in-tree caller always has one; `None` remains the
/// contract for a caller that does not, and never reads as a pass. `daemon` is
/// what the daemon recorded at its own startup and published on `/health`, `""`
/// from a daemon predating the field. Either gap yields "cannot tell".
/// Test: `staleness_warns_when_build_identity_differs`,
/// `staleness_warns_when_daemon_omits_build_identity`,
/// `staleness_warns_when_installed_build_identity_is_unknown`,
/// `staleness_ok_when_version_and_build_identity_match`.
#[derive(Debug, Clone, Copy)]
pub struct BuildIdentity<'a> {
    /// Fingerprint of the binary this process is running; `None` if unresolvable.
    pub installed: Option<&'a str>,
    /// Fingerprint the daemon captured at startup; `""` if it does not report one.
    pub daemon: &'a str,
}

/// Verdict for a daemon whose version already agrees with the installed binary
/// (issue #7822).
///
/// Why: agreeing versions used to end the check with `Ok`. A daemon process
/// started before a same-version merge keeps serving the old code, and that is
/// the failure this whole module exists to catch — the version test cannot see
/// it, so the build fingerprint decides.
/// What: differing fingerprints → `Warn` with the stale-daemon remediation.
/// Either fingerprint missing → `Warn` that says the comparison could not be
/// made; a daemon that omits the field predates this check, which is itself
/// grounds for the restart. Equal fingerprints → `Ok`, the only pass.
/// Test: `staleness_warns_when_build_identity_differs`,
/// `staleness_warns_when_daemon_omits_build_identity`,
/// `staleness_warns_when_installed_build_identity_is_unknown`,
/// `staleness_ok_when_version_and_build_identity_match`.
fn same_version_verdict(
    daemon_reported: &str,
    identity: BuildIdentity<'_>,
    restart_hint: &str,
) -> DoctorCheck {
    if identity.daemon.is_empty() {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Warn,
            format!(
                "running daemon reports v{daemon_reported} but no build fingerprint — it \
                 predates this check, so whether it runs the installed build CANNOT be \
                 determined; restart the daemon (`{restart_hint}`)"
            ),
        );
    }
    let Some(installed_id) = identity.installed else {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Warn,
            format!(
                "could not fingerprint the installed binary, so whether the running daemon \
                 (v{daemon_reported}) is the same build CANNOT be determined; restart the \
                 daemon if unsure (`{restart_hint}`)"
            ),
        );
    };
    if installed_id == identity.daemon {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!(
                "running daemon (v{daemon_reported}, build {installed_id}) matches the \
                 installed binary"
            ),
        );
    }
    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Warn,
        format!(
            "running daemon reports v{daemon_reported} — the installed version — but a \
             DIFFERENT build (daemon build {daemon_id}, installed build {installed_id}): it \
             started before the installed binary was written and is serving stale code; \
             restart the daemon (`{restart_hint}`)",
            daemon_id = identity.daemon
        ),
    )
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
/// Equal versions → the #7822 build-fingerprint verdict over `identity`, which
/// is the ONLY path to `Ok`. Different strings that both parse as
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
/// `staleness_warns_when_unparseable`, `staleness_uses_the_caller_restart_hint`,
/// `staleness_warns_when_build_identity_differs`.
pub fn check_daemon_version_staleness(
    installed: &str,
    daemon_reported: &str,
    identity: BuildIdentity<'_>,
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
        // #7822: an agreeing semver is not evidence of an agreeing BUILD.
        return same_version_verdict(daemon_reported, identity, restart_hint);
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
        // difference) — not the version drift this check targets, so #7822's
        // build-fingerprint verdict decides it exactly as an exact match does.
        (Some(_), Some(_)) => same_version_verdict(daemon_reported, identity, restart_hint),
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

    /// One arbitrary build fingerprint, shaped like what
    /// [`crate::core::build_identity::identity_of`] produces.
    const BUILD_A: &str = "1757731440:74125312";
    /// A second, different build of the SAME version — the #7822 case.
    const BUILD_B: &str = "1757817840:74126688";

    /// Both sides agree on the build, so the version branches are exercised
    /// without the #7822 fingerprint verdict interfering.
    fn matching() -> BuildIdentity<'static> {
        BuildIdentity {
            installed: Some(BUILD_A),
            daemon: BUILD_A,
        }
    }

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

    /// #7822 acceptance case 3: same version AND same build fingerprint is the
    /// only state that reports `Ok`.
    #[test]
    fn staleness_ok_when_versions_match() {
        let check = check_daemon_version_staleness("0.42.0", "0.42.0", matching(), HINT);
        assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
        assert_eq!(check.name, CHECK_NAME);
    }

    #[test]
    fn staleness_warns_when_daemon_older() {
        let check = check_daemon_version_staleness("0.42.0", "0.41.9", matching(), HINT);
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("older than installed binary"));
        assert!(check.message.contains(HINT), "message: {}", check.message);
    }

    /// #4230 review, HIGH-3: every remediation must use the CALLER's verb. The
    /// hardcoded `tm restart` this replaced is a hard error on a launchd host, so
    /// `tm doctor` prescribed a command its own next line had just broken. All
    /// three hint-carrying branches are checked, and none may leak the literal.
    /// #7822 adds a fourth: the same-version/different-build remediation.
    #[test]
    fn staleness_uses_the_caller_restart_hint() {
        for (installed, reported) in [
            ("0.42.0", "0.41.9"),
            ("0.42.0", ""),
            ("0.42.0", "garbage"),
            ("0.42.0", "0.42.0"),
        ] {
            let identity = BuildIdentity {
                installed: Some(BUILD_A),
                daemon: BUILD_B,
            };
            let msg = check_daemon_version_staleness(installed, reported, identity, HINT).message;
            assert!(msg.contains(HINT), "message: {msg}");
            assert!(
                !msg.contains("tm restart"),
                "must not leak the old hardcoded verb: {msg}"
            );
        }
    }

    #[test]
    fn staleness_warns_when_daemon_newer() {
        let check = check_daemon_version_staleness("0.41.9", "0.42.0", matching(), HINT);
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("NEWER"));
    }

    #[test]
    fn staleness_warns_when_daemon_version_empty() {
        let check = check_daemon_version_staleness("0.42.0", "", matching(), HINT);
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("did not report a version"));
    }

    #[test]
    fn staleness_warns_when_unparseable() {
        let check = check_daemon_version_staleness("0.42.0", "garbage", matching(), HINT);
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
        let check = check_daemon_version_staleness("0.42.0", "0.42.0-beta", matching(), HINT);
        assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
        assert_eq!(check.name, CHECK_NAME);
    }

    /// #7822 acceptance case 1, and the 2026-09-13 incident verbatim: daemon and
    /// installed binary both report 1.5.36, but the daemon started before the
    /// binary now on disk was written. The pre-fix check returned `Ok` here and
    /// the stale daemon went on to rewrite a managed `settings.json` with no
    /// backup (#7789).
    #[test]
    fn staleness_warns_when_build_identity_differs() {
        let identity = BuildIdentity {
            installed: Some(BUILD_A),
            daemon: BUILD_B,
        };
        let check = check_daemon_version_staleness("1.5.36", "1.5.36", identity, HINT);
        assert_eq!(
            check.status,
            CheckStatus::Warn,
            "same semver, different build must not report Ok: {}",
            check.message
        );
        assert!(
            check.message.contains("DIFFERENT build"),
            "message: {}",
            check.message
        );
        assert!(
            check.message.contains(BUILD_B),
            "message: {}",
            check.message
        );
        assert!(check.message.contains(HINT), "message: {}", check.message);
    }

    /// #7822 acceptance case 2: a daemon predating the fingerprint field cannot
    /// be cleared. The check must SAY it cannot tell rather than fail open on
    /// the version match alone.
    #[test]
    fn staleness_warns_when_daemon_omits_build_identity() {
        let identity = BuildIdentity {
            installed: Some(BUILD_A),
            daemon: "",
        };
        let check = check_daemon_version_staleness("1.5.36", "1.5.36", identity, HINT);
        assert_eq!(
            check.status,
            CheckStatus::Warn,
            "an unreportable build must not read as a match: {}",
            check.message
        );
        assert!(
            check.message.contains("CANNOT be determined"),
            "message: {}",
            check.message
        );
        assert!(check.message.contains(HINT), "message: {}", check.message);
    }

    /// The mirror of the case above on THIS side: `tm doctor` could not
    /// fingerprint its own binary, so it has learned nothing and must not pass
    /// the daemon on the version string alone.
    #[test]
    fn staleness_warns_when_installed_build_identity_is_unknown() {
        let identity = BuildIdentity {
            installed: None,
            daemon: BUILD_A,
        };
        let check = check_daemon_version_staleness("1.5.36", "1.5.36", identity, HINT);
        assert_eq!(
            check.status,
            CheckStatus::Warn,
            "message: {}",
            check.message
        );
        assert!(
            check
                .message
                .contains("could not fingerprint the installed binary"),
            "message: {}",
            check.message
        );
    }

    /// #7822 acceptance case 3, stated as its own test: the pass names the build
    /// it verified, so the row is evidence rather than an assertion.
    #[test]
    fn staleness_ok_when_version_and_build_identity_match() {
        let check = check_daemon_version_staleness("1.5.36", "1.5.36", matching(), HINT);
        assert_eq!(check.status, CheckStatus::Ok, "message: {}", check.message);
        assert!(
            check.message.contains(BUILD_A),
            "message: {}",
            check.message
        );
    }
}
