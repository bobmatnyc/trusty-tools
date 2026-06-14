use super::*;
use crate::client::{
    DecisionCounts, DiscoveredProjectSummary, RecommendationSummary, SessionSummary,
    TmuxSessionSummary,
};

#[test]
fn format_sessions_empty() {
    let text = TelegramFormatter::format(&CommandResult::Sessions(vec![]));
    assert_eq!(text, "No active sessions.");
}

#[test]
fn format_sessions_lists_each() {
    let result = CommandResult::Sessions(vec![SessionSummary {
        id: "abcd1234-5678".into(),
        status: "active".into(),
        workdir: "/tmp/proj".into(),
    }]);
    let text = TelegramFormatter::format(&result);
    assert!(text.contains("/tmp/proj"));
    assert!(text.contains("trusty-mpm sessions"));
}

#[test]
fn keyboard_for_sessions_has_rows() {
    let result = CommandResult::Sessions(vec![SessionSummary {
        id: "abc".into(),
        status: "active".into(),
        workdir: "/p".into(),
    }]);
    let keyboard = TelegramFormatter::keyboard_for(&result).expect("keyboard");
    assert_eq!(keyboard.inline_keyboard.len(), 1);
    assert_eq!(keyboard.inline_keyboard[0].len(), 3);
}

#[test]
fn keyboard_for_help_is_none() {
    let result = CommandResult::Help("help".into());
    assert!(TelegramFormatter::keyboard_for(&result).is_none());
}

#[test]
fn pair_code_command_formats_correctly() {
    let result = CommandResult::PairCode {
        code: "A4X9KZ".into(),
        expires_in_seconds: 300,
    };
    let text = TelegramFormatter::format(&result);
    assert!(text.contains("A4X9KZ"), "code must be visible: {text}");
    assert!(text.contains("5 minutes"));
}

#[test]
fn pair_success_formats_correctly() {
    let result = CommandResult::PairSuccess {
        chat_info: "chat 12345678".into(),
    };
    let text = TelegramFormatter::format(&result);
    assert!(text.contains("Successfully paired"));
    assert!(text.contains("12345678"));
}

#[test]
fn pair_state_unpaired_prompts_pairing() {
    let text = TelegramFormatter::format(&CommandResult::PairState { paired: false });
    assert!(text.contains("tm pair"));
    let paired = TelegramFormatter::format(&CommandResult::PairState { paired: true });
    assert!(paired.contains("paired"));
}

#[test]
fn format_error_marks_failure() {
    let text = TelegramFormatter::format(&CommandResult::Error("boom".into()));
    assert!(text.contains("boom"));
}

#[test]
fn format_overseer_status() {
    let result = CommandResult::OverseerStatus {
        enabled: true,
        handler: "deterministic".into(),
        decisions: DecisionCounts {
            allow: 3,
            block: 1,
            flag: 0,
        },
    };
    let text = TelegramFormatter::format(&result);
    assert!(text.contains("deterministic"));
    assert!(text.contains("allow (3)"));
}

#[test]
fn format_tmux_and_config() {
    let tmux = CommandResult::TmuxSessions(vec![TmuxSessionSummary {
        name: "tmpm-a".into(),
        managed: true,
    }]);
    let text = TelegramFormatter::format(&tmux);
    assert!(text.contains("tmpm-a"));
    assert!(text.contains("managed"));

    let config = CommandResult::ConfigAnalysis {
        project: "/p".into(),
        recommendations: vec![RecommendationSummary {
            id: "r1".into(),
            message: "enable hooks".into(),
        }],
    };
    assert!(TelegramFormatter::format(&config).contains("enable hooks"));
}

#[test]
fn format_tmux_marks_external() {
    let tmux = CommandResult::TmuxSessions(vec![TmuxSessionSummary {
        name: "vim".into(),
        managed: false,
    }]);
    assert!(TelegramFormatter::format(&tmux).contains("external"));
}

#[test]
fn keyboard_for_tmux_adopts_external() {
    // Only the unmanaged session gets an [Adopt] button.
    let tmux = CommandResult::TmuxSessions(vec![
        TmuxSessionSummary {
            name: "tmpm-a".into(),
            managed: true,
        },
        TmuxSessionSummary {
            name: "vim".into(),
            managed: false,
        },
    ]);
    let keyboard = TelegramFormatter::keyboard_for(&tmux).expect("keyboard");
    assert_eq!(keyboard.inline_keyboard.len(), 1);
    // All-managed sessions yield no keyboard.
    let managed_only = CommandResult::TmuxSessions(vec![TmuxSessionSummary {
        name: "tmpm-a".into(),
        managed: true,
    }]);
    assert!(TelegramFormatter::keyboard_for(&managed_only).is_none());
}

#[test]
fn format_discovered_projects_empty() {
    let text = TelegramFormatter::format(&CommandResult::DiscoveredProjects(vec![]));
    assert!(text.contains("No projects discovered"));
}

#[test]
fn format_discovered_projects_lists_each() {
    let projects = vec![DiscoveredProjectSummary {
        path: "/work/demo".into(),
        session_count: 3,
        last_session: Some("2026-05-17T10:00:00+00:00".into()),
    }];
    let text = format_discovered_projects(&projects);
    assert!(text.contains("/work/demo"));
    assert!(text.contains("3 session(s)"));
    assert!(text.contains("2026-05-17"));
}

#[test]
fn keyboard_for_projects_has_rows() {
    let projects = CommandResult::DiscoveredProjects(vec![DiscoveredProjectSummary {
        path: "/work/demo".into(),
        session_count: 1,
        last_session: None,
    }]);
    let keyboard = TelegramFormatter::keyboard_for(&projects).expect("keyboard");
    assert_eq!(keyboard.inline_keyboard.len(), 1);
    assert_eq!(keyboard.inline_keyboard[0].len(), 1);
}

#[test]
fn adopted_and_registered_format() {
    let adopted = CommandResult::Adopted {
        session: "vim".into(),
    };
    assert!(TelegramFormatter::format(&adopted).contains("vim"));
    let registered = CommandResult::ProjectRegistered {
        path: "/work/demo".into(),
    };
    assert!(TelegramFormatter::format(&registered).contains("/work/demo"));
}

#[test]
fn project_basename_extracts_dir_name() {
    assert_eq!(project_basename("/work/demo"), "demo");
    assert_eq!(project_basename("solo"), "solo");
}

#[test]
fn short_id_truncates_long_ids() {
    assert_eq!(short_id("0123456789abcdef"), "01234567…");
    assert_eq!(short_id("short"), "short");
}

#[test]
fn format_doctor_report_lists_each_check() {
    use crate::client::{CheckStatus, DoctorCheck, DoctorReport};
    let report = DoctorReport::from_checks(vec![
        DoctorCheck::new("instructions", CheckStatus::Ok, "pipeline ran"),
        DoctorCheck::new("memory", CheckStatus::Fail, "unreachable"),
    ]);
    let text = TelegramFormatter::format(&CommandResult::Doctor(report));
    assert!(text.contains("trusty-mpm doctor"));
    assert!(text.contains("instructions"));
    assert!(text.contains("memory"));
    assert!(text.contains("unreachable"));
    // A single Fail makes the overall verdict failed.
    assert!(text.contains("overall: failed"));
}

#[test]
fn snapshot_escapes_html() {
    let result = CommandResult::Snapshot {
        session: "s".into(),
        output: "<script>".into(),
    };
    let text = TelegramFormatter::format(&result);
    assert!(text.contains("&lt;script&gt;"));
}
