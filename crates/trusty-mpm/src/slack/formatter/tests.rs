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
    SessionSummary, TmuxSessionSummary,
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
