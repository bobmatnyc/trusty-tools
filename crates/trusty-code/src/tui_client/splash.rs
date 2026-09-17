//! The `tcode tui` startup splash (#8164) — pure text assembly, no I/O.
//!
//! Why: before #8164 a launch reported one line, `connected to tcode daemon
//! at <socket> (session <id>)`, and the banner above it printed
//! `trusty-code-tui`'s OWN crate version (`tcode v0.2.0`) beside a binary
//! that was 0.7.0. Neither named the project the session was homed in, so
//! the 2026-09-16 transcript that motivated #8205 — an agent finding an
//! empty directory inside a full repository — looked normal at launch. The
//! facts that decide what a session can do are all knowable at connect;
//! this module is where they become one readable block.
//! What: [`splash_lines`] turns [`SplashFacts`] into the line block
//! `ReplEvent::SplashUpdated` carries. It is deliberately a pure function
//! over already-resolved values: every fact is fetched by `engine::setup`,
//! so this file has nothing to fail at and is testable without a daemon.
//! Rendering (the framed two-column box) belongs to
//! `trusty_code_tui::widgets::banner`, which treats these lines as opaque.
//! Test: `tests::*` below; the live `setup` -> splash path is covered by
//! `tests/tui_client_engine.rs::setup_publishes_a_splash_naming_the_project`
//! and `setup_splash_says_projectless_when_unbound`.

use std::path::Path;

/// The header glyph, matching trusty-mpm's own launch/attribution banner so
/// the two products read as one family (owner directive, 2026-09-16).
const ROBOT: &str = "🤖🤖🤖";

/// Everything the splash names, already resolved by `engine::setup`.
///
/// Why: a struct rather than nine positional parameters because six of them
/// are strings and a transposed pair would compile silently. Borrowed, not
/// owned, so `setup` hands over what it already has.
/// What: `client_*` come from [`crate::build_info`] (the same constants
/// `tcode --version` prints); `daemon_*` are `None` when the daemon's
/// `health` could not be read or predates #8164's `build` field — an
/// unverifiable daemon, never a guessed one. `project` is the binding's
/// display root, `None` for a projectless session. `shape` is
/// `engine::session_shape_summary`'s sentence (#8184), passed in rather
/// than recomputed here so there is one definition of it.
pub(super) struct SplashFacts<'a> {
    pub(super) client_version: &'a str,
    pub(super) client_build: &'a str,
    pub(super) daemon_version: Option<&'a str>,
    pub(super) daemon_build: Option<&'a str>,
    pub(super) socket: &'a Path,
    pub(super) session_id: &'a str,
    pub(super) project: Option<&'a str>,
    pub(super) workstream: Option<&'a str>,
    pub(super) shape: &'a str,
}

/// Assemble the splash block, header line first.
///
/// Why: the header line is first because the banner widget styles line 0 as
/// the identity row it replaces — see `ReplEvent::SplashUpdated`.
/// What: header, daemon, session, project, optional workstream, agent shape,
/// and — only when both builds are known AND differ — one plain warning
/// naming both. The `agent` line repeats the root on purpose: `project` is
/// what THIS client asked to bind, while the shape sentence reports where
/// the DAEMON actually rooted the session's file tools, so the two
/// disagreeing is the thing worth seeing.
/// Test: `tests::splash_names_a_bound_project`,
/// `tests::splash_says_projectless_when_unbound`,
/// `tests::splash_warns_when_the_daemon_is_a_different_build`,
/// `tests::splash_is_silent_when_the_daemon_build_is_unknown`,
/// `tests::splash_omits_an_unbound_workstream`.
pub(super) fn splash_lines(facts: &SplashFacts<'_>) -> Vec<String> {
    let mut lines = vec![
        format!("{ROBOT} tcode v{}", facts.client_version),
        format!(
            "daemon {} at {}",
            describe_daemon_build(facts.daemon_version, facts.daemon_build),
            facts.socket.display()
        ),
        format!("session {}", facts.session_id),
        match facts.project {
            Some(root) => format!("project {root}"),
            None => "project projectless".to_string(),
        },
    ];
    if let Some(ws) = facts.workstream {
        lines.push(format!("workstream {ws}"));
    }
    lines.push(format!("agent {}", facts.shape));
    if let Some(daemon_build) = facts.daemon_build
        && daemon_build != facts.client_build
    {
        // #8164: detection only — restarting the daemon is #8203's job.
        lines.push(format!(
            "warning: this client is build {}, the daemon is build {daemon_build} — \
             restart the daemon to match",
            facts.client_build
        ));
    }
    lines
}

/// How the daemon's identity reads on the splash's `daemon` line.
///
/// Why: "unreported" has to read as a state, not as missing text — the same
/// reasoning `cli::daemon_autospawn::ReportedBinding::describe` applies to an
/// unverifiable binding. A daemon that answered `health` without a `build`
/// field predates #8164 and is therefore old by definition, which is itself
/// worth showing.
/// What: `"<version> (<build>)"` when both are known, degrading one field at
/// a time to `"<version> (build unreported)"` and finally to
/// `"(unreachable)"` when `health` itself did not answer.
/// Test: `tests::splash_is_silent_when_the_daemon_build_is_unknown`,
/// `tests::daemon_description_degrades_field_by_field`.
fn describe_daemon_build(version: Option<&str>, build: Option<&str>) -> String {
    match (version, build) {
        (Some(v), Some(b)) => format!("v{v} ({b})"),
        (Some(v), None) => format!("v{v} (build unreported)"),
        (None, _) => "(unreachable)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts<'a>(project: Option<&'a str>, daemon_build: Option<&'a str>) -> SplashFacts<'a> {
        SplashFacts {
            client_version: "0.7.0 (ea6a1a9e 2026-09-16)",
            client_build: "ea6a1a9e",
            daemon_version: Some("0.7.0"),
            daemon_build,
            socket: Path::new("/tmp/tcode.sock"),
            session_id: "8ff7f92d",
            project,
            workstream: Some("bobmatnyc/bakeoff-l1 (548f2143)"),
            shape: "solo agent (no delegation), file tools rooted at /repo",
        }
    }

    fn rendered(lines: &[String]) -> String {
        lines.join("\n")
    }

    /// A bound session names the repository, the session, the daemon and the
    /// agent shape — the whole #8164 acceptance list, in one block.
    #[test]
    fn splash_names_a_bound_project() {
        let lines = splash_lines(&facts(Some("/repo"), Some("ea6a1a9e")));
        let text = rendered(&lines);
        assert!(lines[0].starts_with(ROBOT), "{text}");
        assert!(
            lines[0].contains("tcode v0.7.0 (ea6a1a9e 2026-09-16)"),
            "{text}"
        );
        assert!(
            text.contains("daemon v0.7.0 (ea6a1a9e) at /tmp/tcode.sock"),
            "{text}"
        );
        assert!(text.contains("session 8ff7f92d"), "{text}");
        assert!(text.contains("project /repo"), "{text}");
        assert!(text.contains("workstream bobmatnyc/bakeoff-l1"), "{text}");
        assert!(text.contains("agent solo agent (no delegation)"), "{text}");
        assert!(
            !text.contains("warning:"),
            "matching builds must not warn: {text}"
        );
    }

    /// A projectless session says so in the same slot a path would occupy —
    /// the #8205 transcript's whole problem was that this state was silent.
    #[test]
    fn splash_says_projectless_when_unbound() {
        let text = rendered(&splash_lines(&facts(None, Some("ea6a1a9e"))));
        assert!(text.contains("project projectless"), "{text}");
    }

    /// Differing builds print ONE plain line naming both, so a stale daemon
    /// is visible at launch rather than after a confusing failure.
    #[test]
    fn splash_warns_when_the_daemon_is_a_different_build() {
        let lines = splash_lines(&facts(Some("/repo"), Some("deadbeef")));
        let warnings: Vec<&String> = lines.iter().filter(|l| l.contains("warning:")).collect();
        assert_eq!(warnings.len(), 1, "exactly one warning line: {lines:?}");
        assert!(warnings[0].contains("ea6a1a9e"), "{warnings:?}");
        assert!(warnings[0].contains("deadbeef"), "{warnings:?}");
    }

    /// An unknown daemon build must not warn: "cannot compare" is not
    /// "different", and a warning on every old daemon would train the
    /// operator to ignore the one that matters.
    #[test]
    fn splash_is_silent_when_the_daemon_build_is_unknown() {
        let text = rendered(&splash_lines(&facts(Some("/repo"), None)));
        assert!(!text.contains("warning:"), "{text}");
        assert!(text.contains("build unreported"), "{text}");
    }

    /// With no active workstream the line is absent, not blank or "none" —
    /// the banner has few rows and an empty label buys nothing.
    #[test]
    fn splash_omits_an_unbound_workstream() {
        let mut f = facts(Some("/repo"), Some("ea6a1a9e"));
        f.workstream = None;
        let text = rendered(&splash_lines(&f));
        assert!(!text.contains("workstream"), "{text}");
    }

    /// Each daemon field degrades on its own, so "answered but old" never
    /// reads the same as "did not answer".
    #[test]
    fn daemon_description_degrades_field_by_field() {
        assert_eq!(
            describe_daemon_build(Some("1.0"), Some("abc")),
            "v1.0 (abc)"
        );
        assert_eq!(
            describe_daemon_build(Some("1.0"), None),
            "v1.0 (build unreported)"
        );
        assert_eq!(describe_daemon_build(None, None), "(unreachable)");
    }
}
