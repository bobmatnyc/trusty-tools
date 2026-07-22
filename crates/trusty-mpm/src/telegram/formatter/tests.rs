use super::*;
use crate::client::{
    DecisionCounts, DiscoveredProjectSummary, ManagedSessionView, ProjectFleetView,
    RecommendationSummary, SessionSummary, TmuxSessionSummary,
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
fn managed_arms_escape_daemon_sourced_fields() {
    // Why: daemon-sourced names/states (repo/tmux/session names) may contain
    // HTML-significant characters; un-escaped `<`/`>`/`&` make Telegram reject
    // or misrender the message. These arms must run every interpolated
    // daemon-sourced field through `html_escape`, matching the other formatters.
    let raw = "a<b>&c";
    let escaped = "a&lt;b&gt;&amp;c";

    let spawned = CommandResult::ManagedSpawned {
        id: "abcd1234-5678".into(),
        name: raw.into(),
        state: raw.into(),
        runtime: raw.into(),
        attach_cmd: raw.into(),
    };
    let spawned_text = TelegramFormatter::format(&spawned);
    assert!(
        spawned_text.contains(escaped),
        "ManagedSpawned must escape daemon fields: {spawned_text}"
    );
    assert!(
        !spawned_text.contains("a<b>&c"),
        "ManagedSpawned must not emit raw chars: {spawned_text}"
    );

    let lifecycle = CommandResult::ManagedLifecycle {
        id: "abcd1234-5678".into(),
        name: raw.into(),
        state: raw.into(),
        action: raw.into(),
    };
    let lifecycle_text = TelegramFormatter::format(&lifecycle);
    assert!(
        lifecycle_text.contains(escaped),
        "ManagedLifecycle must escape daemon fields: {lifecycle_text}"
    );
    assert!(
        !lifecycle_text.contains("a<b>&c"),
        "ManagedLifecycle must not emit raw chars: {lifecycle_text}"
    );
}

#[test]
fn managed_adopted_renders_and_escapes_cwd() {
    // Why: review #1502 — the adopt result must SURFACE the registered cwd so the
    // operator can confirm the directory the daemon recorded for the adopted pane
    // (the pane's provenance is otherwise unknown). The cwd is also daemon-sourced
    // and must be HTML-escaped like every other interpolated field.
    // What: format a `ManagedAdopted` carrying a cwd with HTML-significant chars
    // and assert the escaped cwd appears (and no raw chars leak).
    // Test: this function IS the test.
    let adopted = CommandResult::ManagedAdopted {
        id: "abcd1234-5678".into(),
        name: "tmpm-hand-started".into(),
        state: "active".into(),
        cwd: "/Users/op/work/<proj>&x".into(),
        runtime: "claude-code".into(),
        attach_cmd: "tmux attach -t tmpm-hand-started".into(),
    };
    let text = TelegramFormatter::format(&adopted);
    assert!(
        text.contains("cwd: <code>/Users/op/work/&lt;proj&gt;&amp;x</code>"),
        "ManagedAdopted must render the escaped cwd line: {text}"
    );
    assert!(
        !text.contains("/Users/op/work/<proj>&x"),
        "ManagedAdopted must not emit the raw cwd: {text}"
    );
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

#[test]
fn format_health_up_and_down() {
    use crate::client::HealthReport;
    // A reachable daemon renders the liveness line, catalog state, and fleet counts.
    let up = CommandResult::Health(HealthReport {
        reachable: true,
        url: "http://127.0.0.1:7880".into(),
        status: "ok".into(),
        catalog_stale: true,
        catalog_unknown: false,
        managed_total: 3,
        managed_pending_decisions: 1,
    });
    let up_text = TelegramFormatter::format(&up);
    assert!(up_text.contains("daemon"));
    assert!(up_text.contains("updates available"));
    assert!(up_text.contains("3 session(s)"));
    assert!(up_text.contains("1 awaiting a decision"));

    // A dead daemon renders only the unreachable line (no misleading zero fleet).
    let down = CommandResult::Health(HealthReport {
        reachable: false,
        url: "http://127.0.0.1:0".into(),
        status: "unreachable".into(),
        ..HealthReport::default()
    });
    let down_text = TelegramFormatter::format(&down);
    assert!(down_text.contains("unreachable"));
    assert!(!down_text.contains("fleet:"));
}

// --- Issue #1514: HTML-escape for CommandResult::Help and ampersands ----------

/// Help text angle-bracket placeholders must not appear raw in the output.
///
/// Why: `help_text()` contains lines like `/status <id> — Session status`;
/// sending that under `ParseMode::Html` causes Telegram to reject `<id>` as an
/// unsupported HTML start tag, silently dropping the reply (issue #1514).
/// What: `TelegramFormatter::format` on a `CommandResult::Help` must escape
/// `<` and `>` to `&lt;` and `&gt;`.
/// Test: this function IS the test.
#[test]
fn help_escapes_angle_bracket_placeholders() {
    // Simulate the real help_text which contains lines like `/status <id> — …`
    let help = CommandResult::Help("/status <id> — Session status".into());
    let text = TelegramFormatter::format(&help);
    assert!(
        text.contains("&lt;id&gt;"),
        "angle-bracket placeholders must be escaped to &lt;/&gt;: {text}"
    );
    assert!(
        !text.contains("<id>"),
        "raw <id> must not appear in output (Telegram rejects it): {text}"
    );
}

/// Intentional HTML formatting tags emitted by other variants must be preserved.
///
/// Why: the fix for #1514 escapes Help text but must NOT double-escape tags that
/// other `CommandResult` arms write deliberately (e.g. `<b>`, `<code>`, `<pre>`).
/// What: a `CommandResult::Sessions` reply is confirmed to still carry the
/// intentional `<b>` tag without it being escaped to `&lt;b&gt;`.
/// Test: this function IS the test.
#[test]
fn intentional_html_tags_are_preserved_in_other_variants() {
    use crate::client::SessionSummary;
    let result = CommandResult::Sessions(vec![SessionSummary {
        id: "abc12345-0000".into(),
        status: "active".into(),
        workdir: "/tmp/safe".into(),
    }]);
    let text = TelegramFormatter::format(&result);
    // The sessions formatter writes `<b>trusty-mpm sessions</b>` intentionally.
    assert!(
        text.contains("<b>trusty-mpm sessions</b>"),
        "intentional <b> tags must be preserved, not escaped: {text}"
    );
    assert!(
        !text.contains("&lt;b&gt;"),
        "intentional <b> must not be double-escaped: {text}"
    );
}

// ── WI-B (#1586): fleet-by-project formatter ──────────────────────────────────

/// Empty fleet renders a placeholder, not an empty string.
///
/// Why: the operator needs a non-empty message even when no projects are
/// registered, so Telegram always has something to display.
/// What: `format_fleet_by_project` with an empty slice returns the sentinel
/// "No registered projects." string.
/// Test: this function IS the test.
#[test]
fn format_fleet_by_project_empty_returns_placeholder() {
    let text = format_fleet_by_project(&[]);
    assert_eq!(text, "No registered projects.");
}

/// Projects with sessions are rendered with correct glyph/flag/HTML structure.
///
/// Why: the Telegram bot sends HTML-escaped content under `ParseMode::Html`;
/// project names and repo URLs that contain `<`, `>`, or `&` would break the
/// Telegram message if unescaped.
/// What: the formatter emits the heading, one block per project with its
/// sessions, the pending-decision ⚠️ flag when applicable, and the state glyph.
/// Test: this function IS the test.
#[test]
fn format_fleet_by_project_renders_projects() {
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
                    proposed_default: Some("yes".into()),
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
    let text = format_fleet_by_project(&fleet);

    // Heading
    assert!(text.contains("<b>fleet by project</b>"), "heading: {text}");

    // Project alpha header
    assert!(
        text.contains("<b>proj-alpha</b>"),
        "project name must be bold: {text}"
    );
    assert!(
        text.contains("<code>https://github.com/org/alpha</code>"),
        "repo url must be in code tag: {text}"
    );

    // Active session gets green glyph, short id, name, state
    assert!(text.contains("🟢"), "active → 🟢: {text}");
    assert!(text.contains("<code>aaaa1111…</code>"), "short id: {text}");
    assert!(text.contains("tmpm-alpha-1"), "session name: {text}");
    assert!(text.contains("[active]"), "state bracket: {text}");

    // Stopped + pending decision gets red glyph + warning flag
    assert!(text.contains("🔴"), "stopped → 🔴: {text}");
    assert!(text.contains("⚠️"), "pending decision flag: {text}");
    assert!(text.contains("tmpm-alpha-2"), "second session name: {text}");

    // Project beta has no sessions → dash placeholder
    assert!(
        text.contains("<b>proj-beta</b>"),
        "beta project header: {text}"
    );
    assert!(
        text.contains("\n  —"),
        "empty project placeholder dash: {text}"
    );
}

/// Project names / repo URLs with HTML-significant chars must be escaped.
///
/// Why: daemon-sourced project names and URLs may contain `<`, `>`, or `&`;
/// sending them raw under `ParseMode::Html` causes Telegram to reject or
/// misrender the message (same class of bug as issue #1514).
/// What: `format_fleet_by_project` must escape project_name and repo_url.
/// Test: this function IS the test.
#[test]
fn format_fleet_by_project_escapes_html_in_project_fields() {
    let fleet = vec![ProjectFleetView {
        project_name: "org/proj<1>&x".into(),
        repo_url: "https://host/repo&foo<bar>".into(),
        sessions: vec![],
    }];
    let text = format_fleet_by_project(&fleet);
    assert!(
        text.contains("org/proj&lt;1&gt;&amp;x"),
        "project_name must be HTML-escaped: {text}"
    );
    assert!(
        text.contains("https://host/repo&amp;foo&lt;bar&gt;"),
        "repo_url must be HTML-escaped: {text}"
    );
    assert!(
        !text.contains("org/proj<1>&x"),
        "raw < and & must not appear in project_name: {text}"
    );
}

/// Provisioning state maps to the yellow glyph.
///
/// Why: `state_glyph` must emit 🟡 for `"provisioning"` to keep the three-tier
/// traffic-light convention consistent.
/// What: a fleet entry with one `provisioning` session produces the 🟡 glyph.
/// Test: this function IS the test.
#[test]
fn format_fleet_by_project_provisioning_glyph() {
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
    let text = format_fleet_by_project(&fleet);
    assert!(text.contains("🟡"), "provisioning → 🟡: {text}");
}

/// Ampersands in Help text must be escaped to `&amp;`.
///
/// Why: `&` is an HTML-significant character; sending `&foo` under
/// `ParseMode::Html` causes Telegram to misparse or reject the message.
/// What: `TelegramFormatter::format` on a `CommandResult::Help` that contains
/// `&` must emit `&amp;` in its place.
/// Test: this function IS the test.
#[test]
fn help_escapes_ampersand() {
    let help = CommandResult::Help("options: -a & -b".into());
    let text = TelegramFormatter::format(&help);
    assert!(
        text.contains("&amp;"),
        "ampersand must be escaped to &amp;: {text}"
    );
    assert!(
        !text.contains(" & "),
        "raw & must not appear in formatted help output: {text}"
    );
}

/// Error messages with HTML-significant chars must be escaped (issue #1514).
///
/// Why: error strings can originate from daemon internals and may contain
/// `<stdin>`, path fragments with `&`, or other HTML-significant characters.
/// Under `ParseMode::Html` Telegram rejects or silently drops messages with
/// bare `<`, `>`, or `&` in the body — the same class of bug as the Help-arm
/// fix for #1514. The `CommandResult::Error` arm must run `msg` through
/// `html_escape` before interpolation.
/// What: `TelegramFormatter::format` on a `CommandResult::Error` carrying a
/// message with `<`, `>`, and `&` must emit the properly escaped entities and
/// must NOT pass raw HTML-significant characters through to the output.
/// Test: this function IS the test.
#[test]
fn error_escapes_html_significant_chars() {
    let result = CommandResult::Error("<stdin> & co".into());
    let text = TelegramFormatter::format(&result);
    assert!(
        text.contains("&lt;stdin&gt;"),
        "< and > in error msg must be escaped to &lt;/&gt;: {text}"
    );
    assert!(
        text.contains("&amp;"),
        "& in error msg must be escaped to &amp;: {text}"
    );
    assert!(
        !text.contains("<stdin>"),
        "raw <stdin> must not appear in Error output (Telegram rejects it): {text}"
    );
    assert!(
        !text.contains(" & "),
        "raw & must not appear in Error output: {text}"
    );
    // The ❌ prefix must still be present.
    assert!(
        text.contains("❌"),
        "Error prefix emoji must be preserved: {text}"
    );
}

#[test]
fn focus_keyboard_has_a_button_per_session() {
    // The `/fleet` list decorates every session (across all projects) with a
    // `🎯 Focus` button whose callback data is `focus:<id>` (TELUI-6, #1440);
    // empty projects contribute no buttons.
    let fleet = vec![
        ProjectFleetView {
            project_name: "alpha".into(),
            repo_url: "https://github.com/org/alpha".into(),
            sessions: vec![
                ManagedSessionView {
                    id: "id-1".into(),
                    name: "tmpm-a1".into(),
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
                    id: "id-2".into(),
                    name: "tmpm-a2".into(),
                    state: "stopped".into(),
                    workspace_path: None,
                    repo_url: None,
                    branch: None,
                    pending_decision: None,
                    proposed_default: None,
                    slot: 0,
                    deleted: false,
                },
            ],
        },
        ProjectFleetView {
            project_name: "beta".into(),
            repo_url: "https://github.com/org/beta".into(),
            sessions: vec![],
        },
    ];
    let keyboard =
        TelegramFormatter::keyboard_for(&CommandResult::ManagedFleet(fleet)).expect("keyboard");
    // One row per session, two sessions total.
    assert_eq!(keyboard.inline_keyboard.len(), 2);
    let data: Vec<&str> = keyboard
        .inline_keyboard
        .iter()
        .flat_map(|row| row.iter())
        .filter_map(|b| match &b.kind {
            teloxide::types::InlineKeyboardButtonKind::CallbackData(d) => Some(d.as_str()),
            _ => None,
        })
        .collect();
    assert!(data.contains(&"focus:id-1"), "callback data: {data:?}");
    assert!(data.contains(&"focus:id-2"), "callback data: {data:?}");
}

#[test]
fn focus_keyboard_empty_fleet_is_none() {
    // A fleet with no sessions warrants no keyboard.
    let fleet = vec![ProjectFleetView {
        project_name: "beta".into(),
        repo_url: "r".into(),
        sessions: vec![],
    }];
    assert!(TelegramFormatter::keyboard_for(&CommandResult::ManagedFleet(fleet)).is_none());
}

#[test]
fn session_detail_has_focus_button() {
    // The `/get` detail card offers a single `🎯 Focus` button (TELUI-6, #1440).
    let view = ManagedSessionView {
        id: "id-9".into(),
        name: "tmpm-solo".into(),
        state: "active".into(),
        workspace_path: None,
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
        slot: 0,
        deleted: false,
    };
    let keyboard =
        TelegramFormatter::keyboard_for(&CommandResult::ManagedSession(view)).expect("keyboard");
    assert_eq!(keyboard.inline_keyboard.len(), 1);
    assert_eq!(keyboard.inline_keyboard[0].len(), 1);
    match &keyboard.inline_keyboard[0][0].kind {
        teloxide::types::InlineKeyboardButtonKind::CallbackData(d) => {
            assert_eq!(d, "focus:id-9")
        }
        other => panic!("expected callback data, got {other:?}"),
    }
}
