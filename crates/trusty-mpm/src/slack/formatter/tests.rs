//! Unit tests for the Slack `mrkdwn` formatter.
//!
//! Why: rendering is pure (no network, no Slack runtime), so every
//! [`CommandResult`] variant's `mrkdwn` output can be asserted directly — this
//! is the adapter's presentation contract.
//! What: one test per representative variant, plus the small render helpers.
//! Test: this IS the test module.

use super::*;
use crate::client::{
    CommandResult, DecisionCounts, DiscoveredProjectSummary, HealthReport, ManagedSessionView,
    ProjectFleetView, SessionSummary, TmuxSessionSummary,
};
use crate::core::doctor::{CheckStatus, DoctorCheck, DoctorReport};

#[test]
fn format_sessions_empty() {
    let body = SlackFormatter::format(&CommandResult::Sessions(vec![]));
    assert_eq!(body, "No active sessions.");
}

#[test]
fn format_sessions_lists_each() {
    let body = SlackFormatter::format(&CommandResult::Sessions(vec![SessionSummary {
        id: "abcdef0123456789".into(),
        status: "active".into(),
        workdir: "/work/p".into(),
    }]));
    // mrkdwn bold heading, status dot, short id in inline code, and workdir.
    assert!(body.contains("*trusty-mpm sessions*"));
    assert!(body.contains("🟢"));
    assert!(body.contains("`abcdef01…`"));
    assert!(body.contains("`/work/p`"));
}

#[test]
fn format_session_detail_lists_events() {
    let body = SlackFormatter::format(&CommandResult::SessionDetail {
        id: "abcdef0123456789".into(),
        status: "active".into(),
        events: vec!["PreToolUse".into(), "Stop".into()],
    });
    assert!(body.contains("*Session abcdef01…*"));
    assert!(body.contains("• PreToolUse"));
    assert!(body.contains("• Stop"));
}

#[test]
fn format_tmux_lists_each() {
    let body = SlackFormatter::format(&CommandResult::TmuxSessions(vec![
        TmuxSessionSummary {
            name: "tmpm-quiet-falcon".into(),
            managed: true,
        },
        TmuxSessionSummary {
            name: "random-shell".into(),
            managed: false,
        },
    ]));
    assert!(body.contains("`tmpm-quiet-falcon` — 🟢 managed"));
    assert!(body.contains("`random-shell` — ⚪ external"));
}

#[test]
fn format_discovered_projects_lists_each() {
    let body = format_discovered_projects(&[DiscoveredProjectSummary {
        path: "/work/proj".into(),
        session_count: 3,
        last_session: Some("2026-06-19T12:00:00Z".into()),
    }]);
    assert!(body.contains("*Discovered projects*"));
    assert!(body.contains("`/work/proj`"));
    assert!(body.contains("3 session(s) · last used 2026-06-19"));
}

#[test]
fn format_managed_sessions_lists_each() {
    let body = format_managed_sessions(&[ManagedSessionView {
        id: "abcdef0123456789".into(),
        name: "blue-otter".into(),
        state: "running".into(),
        workspace_path: None,
        repo_url: None,
        branch: None,
        pending_decision: Some("apply patch?".into()),
        proposed_default: Some("yes".into()),
        slot: 0,
        deleted: false,
    }]);
    assert!(body.contains("*managed sessions*"));
    assert!(body.contains("`abcdef01…` blue-otter [running]"));
    // A blocked session is flagged.
    assert!(body.contains("⚠️"));
}

#[test]
fn format_managed_session_renders_fields() {
    let body = format_managed_session(&ManagedSessionView {
        id: "abcdef0123456789".into(),
        name: "blue-otter".into(),
        state: "running".into(),
        workspace_path: Some("/ws/blue".into()),
        repo_url: Some("git@host:repo.git".into()),
        branch: Some("main".into()),
        pending_decision: Some("apply patch?".into()),
        proposed_default: Some("yes".into()),
        slot: 0,
        deleted: false,
    });
    assert!(body.contains("*blue-otter* (`abcdef01…`) [running]"));
    assert!(body.contains("📁 `/ws/blue`"));
    assert!(body.contains("🔗 git@host:repo.git main"));
    assert!(body.contains("⚠️ pending: apply patch? (default: `yes`)"));
}

#[test]
fn format_health_up_and_down() {
    let up = SlackFormatter::format(&CommandResult::Health(HealthReport {
        reachable: true,
        url: "http://localhost:9999".into(),
        status: "ok".into(),
        catalog_stale: true,
        catalog_unknown: false,
        managed_total: 4,
        managed_pending_decisions: 1,
    }));
    assert!(up.contains("✅ *daemon ok*"));
    assert!(up.contains("catalog: ⚠️ updates available"));
    assert!(up.contains("fleet: 4 session(s), 1 awaiting a decision"));

    let down = SlackFormatter::format(&CommandResult::Health(HealthReport {
        reachable: false,
        url: "http://localhost:9999".into(),
        ..Default::default()
    }));
    assert!(down.contains("❌ *daemon unreachable*"));
    // A down daemon must not render misleading fleet counts.
    assert!(!down.contains("fleet:"));
}

#[test]
fn format_overseer_status() {
    let body = SlackFormatter::format(&CommandResult::OverseerStatus {
        enabled: true,
        handler: "llm".into(),
        decisions: DecisionCounts {
            allow: 5,
            block: 1,
            flag: 2,
        },
    });
    assert!(body.contains("*Overseer Status*"));
    assert!(body.contains("Handler: `llm`"));
    assert!(body.contains("allow (5), block (1), flag (2)"));
}

#[test]
fn format_doctor_report_lists_each_check() {
    let report = DoctorReport {
        checks: vec![
            DoctorCheck {
                name: "daemon".into(),
                status: CheckStatus::Ok,
                message: "reachable".into(),
            },
            DoctorCheck {
                name: "catalog".into(),
                status: CheckStatus::Warn,
                message: "stale".into(),
            },
        ],
        overall: CheckStatus::Warn,
        generated_at: chrono::Utc::now(),
    };
    let body = SlackFormatter::format(&CommandResult::Doctor(report));
    assert!(body.contains("✅ *daemon* — reachable"));
    assert!(body.contains("⚠️ *catalog* — stale"));
    assert!(body.contains("⚠️ *overall: warnings*"));
}

#[test]
fn format_command_sent_uses_code_block() {
    let body = SlackFormatter::format(&CommandResult::CommandSent {
        session: "frontend".into(),
        output: "build complete".into(),
    });
    assert!(body.contains("*📨 frontend*"));
    assert!(body.contains("```\nbuild complete\n```"));
}

#[test]
fn format_command_sent_no_output() {
    let body = SlackFormatter::format(&CommandResult::CommandSent {
        session: "frontend".into(),
        output: "   ".into(),
    });
    assert_eq!(body, "📨 Sent to `frontend` — no output captured");
}

#[test]
fn format_snapshot_tails_output() {
    // 60 lines collapse to the last 50 (MAX_OUTPUT_LINES).
    let output = (1..=60)
        .map(|n| format!("line{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let body = SlackFormatter::format(&CommandResult::Snapshot {
        session: "sess".into(),
        output,
    });
    assert!(body.contains("*Snapshot: sess*"));
    assert!(body.contains("```"));
    // The oldest line is dropped; the newest is kept.
    assert!(!body.contains("line1\n"));
    assert!(body.contains("line60"));
}

#[test]
fn format_error_renders_message() {
    let body = SlackFormatter::format(&CommandResult::Error("daemon unreachable".into()));
    assert_eq!(body, "❌ daemon unreachable");
}

#[test]
fn format_chat_reply_passthrough() {
    // A free-text chat reply renders verbatim (Slack mrkdwn from the LLM).
    let body = SlackFormatter::format(&CommandResult::ChatReply {
        reply: "spun up *blue-otter*".into(),
    });
    assert_eq!(body, "spun up *blue-otter*");
}

#[test]
fn format_managed_spawned() {
    let body = SlackFormatter::format(&CommandResult::ManagedSpawned {
        id: "abcdef0123456789".into(),
        name: "blue-otter".into(),
        state: "running".into(),
        runtime: "claude-code".into(),
        attach_cmd: "tmux attach -t blue-otter".into(),
    });
    assert!(body.contains("✅ Spawned *blue-otter* (`abcdef01…`) [running] runtime=claude-code"));
    assert!(body.contains("attach: `tmux attach -t blue-otter`"));
}

#[test]
fn short_id_truncates_long_ids() {
    assert_eq!(short_id("abcdef0123456789"), "abcdef01…");
    // Already short ids pass through unchanged.
    assert_eq!(short_id("short"), "short");
    // An id of exactly SHORT_ID_LEN chars has no 9th char → no ellipsis.
    assert_eq!(short_id("12345678"), "12345678");
}

#[test]
fn short_id_handles_multibyte() {
    // A multi-byte UTF-8 id must truncate on a CHAR boundary, never panic on a
    // byte slice (regression: `&id[..8]` panicked mid-codepoint).
    let id = "日本語のセッションです"; // 11 chars, 3 bytes each
    let out = short_id(id);
    // First SHORT_ID_LEN (8) chars: 日本語のセッショ, then the ellipsis.
    assert_eq!(out, "日本語のセッショ…");
    // Char count of the head is SHORT_ID_LEN; the ellipsis is the only extra char.
    assert_eq!(out.chars().count(), SHORT_ID_LEN + 1);
}

// ── WI-B (#1586): fleet-by-project formatter (Slack mrkdwn) ──────────────────

/// Empty fleet renders a placeholder, not an empty body.
///
/// Why: the Slack handler must always emit non-empty text; the sentinel message
/// lets the operator know the project registry is empty.
/// What: `SlackFormatter::format` on a `ManagedFleet([])` returns
/// "No registered projects."
/// Test: this function IS the test.
#[test]
fn format_fleet_by_project_slack_empty_returns_placeholder() {
    let body = SlackFormatter::format(&CommandResult::ManagedFleet(vec![]));
    assert_eq!(body, "No registered projects.");
}

/// Projects with sessions are rendered with correct glyph/flag/mrkdwn structure.
///
/// Why: Slack `mrkdwn` uses `*bold*` and backtick code — not HTML tags — so the
/// fleet-by-project output must differ from the Telegram counterpart in markup
/// while covering the same semantic content.
/// What: heading, bold project name, inline-code repo URL, session rows with
/// state glyphs and ⚠️ flag for pending decisions.
/// Test: this function IS the test.
#[test]
fn format_fleet_by_project_slack_renders_projects() {
    let fleet = vec![
        ProjectFleetView {
            project_name: "proj-alpha".into(),
            repo_url: "https://github.com/org/alpha".into(),
            sessions: vec![
                ManagedSessionView {
                    id: "aaaa1111bbbb2222".into(),
                    name: "tmpm-alpha-1".into(),
                    state: "active".into(),
                    workspace_path: None,
                    repo_url: None,
                    branch: None,
                    pending_decision: None,
                    proposed_default: None,
                    slot: 0,
                    deleted: false,
                },
                ManagedSessionView {
                    id: "cccc3333dddd4444".into(),
                    name: "tmpm-alpha-2".into(),
                    state: "stopped".into(),
                    workspace_path: None,
                    repo_url: None,
                    branch: None,
                    pending_decision: Some("apply patch?".into()),
                    proposed_default: None,
                    slot: 0,
                    deleted: false,
                },
            ],
        },
        ProjectFleetView {
            project_name: "proj-beta".into(),
            repo_url: "https://github.com/org/beta".into(),
            sessions: vec![],
        },
    ];
    let body = SlackFormatter::format(&CommandResult::ManagedFleet(fleet));

    // Heading uses mrkdwn bold (asterisks, not HTML)
    assert!(body.contains("*fleet by project*"), "heading: {body}");

    // Project alpha: mrkdwn bold name, backtick-code repo url
    assert!(body.contains("*proj-alpha*"), "bold project name: {body}");
    assert!(
        body.contains("`https://github.com/org/alpha`"),
        "code repo url: {body}"
    );

    // Active session → green glyph
    assert!(body.contains("🟢"), "active → 🟢: {body}");
    assert!(body.contains("`aaaa1111…`"), "short id: {body}");
    assert!(body.contains("tmpm-alpha-1"), "session name: {body}");
    assert!(body.contains("[active]"), "state bracket: {body}");

    // Stopped + pending decision → red glyph + warning flag
    assert!(body.contains("🔴"), "stopped → 🔴: {body}");
    assert!(body.contains("⚠️"), "pending decision flag: {body}");

    // Project beta empty → dash placeholder
    assert!(body.contains("*proj-beta*"), "beta project header: {body}");
    assert!(body.contains("\n  —"), "empty project placeholder: {body}");
}

/// Provisioning state maps to the yellow glyph in Slack output.
///
/// Why: `fleet_state_glyph` must emit 🟡 for `"provisioning"` in the Slack
/// formatter, matching the Telegram formatter's three-tier convention.
/// What: a `ManagedFleet` entry with one `provisioning` session renders 🟡.
/// Test: this function IS the test.
#[test]
fn format_fleet_by_project_slack_provisioning_glyph() {
    let fleet = vec![ProjectFleetView {
        project_name: "proj".into(),
        repo_url: "https://example.com/repo".into(),
        sessions: vec![ManagedSessionView {
            id: "eeee5555ffff6666".into(),
            name: "tmpm-prov".into(),
            state: "provisioning".into(),
            workspace_path: None,
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            slot: 0,
            deleted: false,
        }],
    }];
    let body = SlackFormatter::format(&CommandResult::ManagedFleet(fleet));
    assert!(body.contains("🟡"), "provisioning → 🟡: {body}");
}

#[test]
fn mrkdwn_escape_escapes_ampersand_lt_gt() {
    // The three mrkdwn-significant characters are escaped, in an order that
    // never double-escapes the `&` produced by the `<`/`>` substitutions.
    assert_eq!(mrkdwn_escape("a & b"), "a &amp; b");
    assert_eq!(mrkdwn_escape("<tag>"), "&lt;tag&gt;");
    assert_eq!(mrkdwn_escape("a < b & c > d"), "a &lt; b &amp; c &gt; d");
    // Plain text is unchanged.
    assert_eq!(mrkdwn_escape("plain text"), "plain text");
}

#[test]
fn mrkdwn_escape_neutralizes_channel_broadcast_span() {
    // Why (#2565 review): an unescaped `<!channel>` in a session name would
    // broadcast-ping the whole channel when interpolated into a proxy reply.
    // Escaping must turn it into inert literal text.
    let hostile = "<!channel> pwned";
    let escaped = mrkdwn_escape(hostile);
    assert_eq!(escaped, "&lt;!channel&gt; pwned");
    assert!(
        !escaped.contains("<!channel>"),
        "the literal broadcast span must not survive escaping: {escaped}"
    );
}
