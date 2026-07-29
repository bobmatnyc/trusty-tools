//! Tests for the L0-only read-only session-state surface (#4171, epic #4167).
//!
//! Why: this module grants EXACTLY the tools the L1 black-box posture excludes
//! by name, so tier enforcement is the property under test, not a side
//! concern. The security tests therefore go through the REAL registry
//! construction (`runtime::tool_registry::build_assistant_tier_registry`) and
//! the REAL scoping gates (`scope_assistant_allowed_tools` on the subprocess
//! path, `filter_persona_tool_names` + `retain_tier_permitted` on the
//! persona-chat path) rather than hand-built argument lists, so a regression
//! in the wiring — not just in the predicate — fails here.
//! What: gate-list well-formedness, the deny-only tier gate, real-path L0/L1
//! registry construction, the store reader, and the `.trusty-mpm/`
//! path-scoping refusals.
//! Test: this file.

use std::path::PathBuf;

use serde_json::json;

use super::*;
use crate::tools::traits::ToolExecutor;

// --------------------------------------------------------------------------
// Gate-list well-formedness
// --------------------------------------------------------------------------

/// Every tool name in this crate that can mutate or terminate a session.
///
/// Why: issue #4171's acceptance criterion is "output is read-only (no state
/// modification)". Naming the mutating surface explicitly turns that sentence
/// into a gate: if a later change adds one of these to the L0 grant list,
/// `l0_only_list_contains_no_mutating_tool` fails and the decision has to be
/// made deliberately rather than by autocompletion.
/// What: the trusty-mpm session/agent verbs that write, plus the MCP admin
/// verbs that reconfigure the service surface.
const MUTATING_TOOL_NAMES: &[&str] = &[
    "session_send",
    "session_stop",
    "session_new",
    "session_resume",
    "session_prune",
    "session_delete",
    "session_decommission",
    "session_decommission_ephemeral",
    "session_proxy_message",
    "session_proxy_focus",
    "session_proxy_unfocus",
    "agent_delegate",
    "mcp_enable",
    "mcp_disable",
];

#[test]
fn l0_only_list_contains_no_mutating_tool() {
    for name in MUTATING_TOOL_NAMES {
        assert!(
            !is_l0_only_session_state_tool(name),
            "{name} mutates session state and must not be part of the read-only L0 grant (#4171)"
        );
    }
}

#[test]
fn l0_only_list_covers_every_native_session_state_tool() {
    for tool in session_state_tools(&PathBuf::from("/tmp"), AgentTier::L0Orchestration) {
        assert!(
            is_l0_only_session_state_tool(tool.name()),
            "native tool {} is registered for L0 but is not in the tier gate list",
            tool.name()
        );
    }
}

/// The names the L1 BLACK-BOX POSTURE comment excludes must all be gated.
///
/// Why: those names are the reason this module exists. If one is dropped from
/// `L0_ONLY_SESSION_STATE_TOOLS`, an L1 persona could reach it again through
/// the trusty-mpm MCP proxy simply by declaring it — the exact black-box
/// violation the exclusions were written to prevent.
/// What: asserts membership for every session-state name named in
/// `.trusty-agents/agents/assistant/agent.toml` and `cto-assistant/agent.toml`.
/// Test: this function IS the test.
#[test]
fn l0_only_list_covers_the_l1_blackbox_exclusions() {
    for name in [
        "session_list",
        "session_status",
        "project_list",
        "console_metrics",
        "system_status",
    ] {
        assert!(
            is_l0_only_session_state_tool(name),
            "{name} is excluded by name from the L1 allowlists and must stay tier-gated"
        );
    }
}

#[test]
fn is_l0_only_matches_exact_names_only() {
    assert!(is_l0_only_session_state_tool("session_list"));
    assert!(!is_l0_only_session_state_tool("session_lis"));
    assert!(!is_l0_only_session_state_tool("session_list_extra"));
    assert!(!is_l0_only_session_state_tool("SESSION_LIST"));
}

// --------------------------------------------------------------------------
// The deny-only tier gate
// --------------------------------------------------------------------------

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// A minimal persona TOML carrying `tier_line` verbatim.
///
/// Why: the fail-closed test must go through the REAL resolver
/// (`AgentInfo::tier`, #4200/#4168) reached the way production reaches it —
/// by parsing a persona TOML — not through a hand-constructed enum value that
/// would pass even if the resolver regressed.
/// What: the same shape `agents::tests::agent_toml_with_tier` uses.
/// Test: `indeterminate_tier_fails_closed_to_no_session_state_tools`,
/// `declared_l0_tier_keeps_session_state_tools`,
/// `assistant_kind_without_a_declaration_keeps_session_state`.
fn persona_toml(tier_line: &str) -> String {
    persona_toml_with_role("assistant", tier_line)
}

/// [`persona_toml`] with the ROLE parameterized (ADR-0024 decision 3).
///
/// Why: tier is derived from kind now, so a fixture that hardcodes
/// `role = "assistant"` can only exercise one side of the derivation.
/// What: the same shape, with `role` supplied by the caller.
/// Test: `sub_agent_kind_without_a_declaration_is_stripped`.
fn persona_toml_with_role(role: &str, tier_line: &str) -> String {
    format!(
        r#"
[agent]
name = "x"
role = "{role}"
model = "x"
description = "x"
{tier_line}

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#
    )
}

#[test]
fn indeterminate_tier_fails_closed_to_no_session_state_tools() {
    // An UNRECOGNIZED `tier = …` resolves to L1Standard via #4200's fail-closed
    // resolver; that is the value the gate must strip on.
    //
    // ADR-0024 decision 3 narrowed the input set this test covers. An ABSENT or
    // blank declaration is no longer "indeterminate" — it means "derive from
    // kind", and this fixture's `role = "assistant"` derives L0. Those two
    // cases moved to `assistant_kind_without_a_declaration_keeps_session_state`
    // below. What remains here is the property that actually matters: a
    // MALFORMED declaration can only ever narrow, so a typo on an assistant
    // costs it the L0 surface rather than silently keeping it.
    for tier_line in [r#"tier = "L2""#, r#"tier = "yolo""#, r#"tier = "l2""#] {
        let cfg: crate::agents::AgentConfig =
            toml::from_str(&persona_toml(tier_line)).expect("parses");
        assert_eq!(
            cfg.agent.tier(),
            AgentTier::L1Standard,
            "{tier_line:?} must resolve fail-closed"
        );
        let kept = retain_tier_permitted(names(&["session_list", "git_log"]), cfg.agent.tier());
        assert_eq!(
            kept,
            names(&["git_log"]),
            "{tier_line:?} must strip the session-state name"
        );
        assert!(
            session_state_tools(&PathBuf::from("/tmp"), cfg.agent.tier()).is_empty(),
            "{tier_line:?} must register no session-state executor"
        );
    }
}

/// ADR-0024 decision 3, stated where the capability actually lives: an
/// assistant-kind persona with NO `tier` declaration derives L0 and therefore
/// keeps an L0-only session-state name that reaches its resolved allow-list.
///
/// Why: this is the whole practical delta of decision 3 in shipped code. The
/// delta is REGISTRATION-level only for today's roster — `retain_tier_permitted`
/// is deny-only and never adds, and no bundled assistant persona names any
/// `L0_ONLY_SESSION_STATE_TOOLS` entry in its `[tools].allow` (pinned by
/// `agents::tests::loading::bundled_assistant_personas_resolve_l0_and_gain_nothing`),
/// so nothing an operator can observe changes until a persona opts in. Pinning
/// the mechanism separately from the roster means the day a persona DOES opt
/// in, the grant is a deliberate config edit and not a surprise.
/// What: absent and blank declarations on `role = "assistant"` both keep the
/// name and both register the three executors.
/// Test: this function IS the test.
#[test]
fn assistant_kind_without_a_declaration_keeps_session_state() {
    for tier_line in ["", r#"tier = """#, r#"tier = "   ""#] {
        let cfg: crate::agents::AgentConfig =
            toml::from_str(&persona_toml(tier_line)).expect("parses");
        assert_eq!(
            cfg.agent.tier(),
            AgentTier::L0Orchestration,
            "{tier_line:?} on an assistant derives L0 (ADR-0024 decision 3)"
        );
        let kept = retain_tier_permitted(names(&["session_list", "git_log"]), cfg.agent.tier());
        assert_eq!(
            kept,
            names(&["session_list", "git_log"]),
            "{tier_line:?} must NOT strip the session-state name"
        );
        assert_eq!(
            session_state_tools(&PathBuf::from("/tmp"), cfg.agent.tier()).len(),
            3,
            "{tier_line:?} must register the three read-only executors"
        );
    }
}

/// The other half: a SUB-AGENT with no declaration stays L1 and is stripped.
#[test]
fn sub_agent_kind_without_a_declaration_is_stripped() {
    for role in [
        "engineer",
        "qa",
        "researcher",
        "documentation",
        "ops",
        "planner",
    ] {
        let cfg: crate::agents::AgentConfig =
            toml::from_str(&persona_toml_with_role(role, "")).expect("parses");
        assert_eq!(
            cfg.agent.tier(),
            AgentTier::L1Standard,
            "sub-agent role {role:?} stays L1"
        );
        let kept = retain_tier_permitted(names(&["session_list", "git_log"]), cfg.agent.tier());
        assert_eq!(kept, names(&["git_log"]), "role {role:?} must be stripped");
        assert!(
            session_state_tools(&PathBuf::from("/tmp"), cfg.agent.tier()).is_empty(),
            "role {role:?} must register no session-state executor"
        );
    }
}

#[test]
fn declared_l0_tier_keeps_session_state_tools() {
    for tier_line in [r#"tier = "l0""#, r#"tier = "Orchestration""#] {
        let cfg: crate::agents::AgentConfig =
            toml::from_str(&persona_toml(tier_line)).expect("parses");
        assert_eq!(cfg.agent.tier(), AgentTier::L0Orchestration);
        let kept = retain_tier_permitted(names(&["session_list", "git_log"]), cfg.agent.tier());
        assert_eq!(kept, names(&["session_list", "git_log"]));
    }
}

#[test]
fn retain_tier_permitted_never_adds_a_tool() {
    let kept = retain_tier_permitted(names(&["git_log"]), AgentTier::L0Orchestration);
    assert_eq!(kept, names(&["git_log"]));
}

#[test]
fn retain_tier_permitted_preserves_unrelated_tools_and_order() {
    let input = names(&[
        "a_tool",
        "session_status",
        "b_tool",
        "system_status",
        "c_tool",
    ]);
    let kept = retain_tier_permitted(input, AgentTier::L1Standard);
    assert_eq!(kept, names(&["a_tool", "b_tool", "c_tool"]));
}

#[test]
fn session_state_tools_are_empty_for_l1() {
    assert!(
        session_state_tools(&PathBuf::from("/tmp"), AgentTier::L1Standard).is_empty(),
        "an L1 registry must never contain a session-state executor"
    );
}

#[test]
fn session_state_tools_present_for_l0() {
    let got: Vec<String> = session_state_tools(&PathBuf::from("/tmp"), AgentTier::L0Orchestration)
        .iter()
        .map(|t| t.name().to_string())
        .collect();
    assert_eq!(
        got,
        names(&[
            "session_state_list",
            "session_state_status",
            "session_state_snapshot"
        ])
    );
}

// --------------------------------------------------------------------------
// The REAL registry-construction path (subprocess / `--direct` dispatch)
// --------------------------------------------------------------------------

/// Tool names the real assistant-tier registry builds for a given tier.
///
/// Why: the security claim is about the production assembly point, not about a
/// helper, so these tests call `build_assistant_tier_registry` — the function
/// `runtime::subagent_mode` itself calls — and read the names back off the
/// schemas it emits.
/// What: constructs the registry and returns its tool names.
/// Test: used by `assistant_tier_registry_*` and `l*_persona_declaring_*`.
fn registry_names(tier: AgentTier) -> Vec<String> {
    crate::runtime::tool_registry::build_assistant_tier_registry(None, tier)
        .schemas()
        .into_iter()
        .filter_map(|s| {
            s.get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .map(String::from)
        })
        .collect()
}

#[test]
fn assistant_tier_registry_omits_session_state_tools_for_l1() {
    let got = registry_names(AgentTier::L1Standard);
    for name in [
        "session_state_list",
        "session_state_status",
        "session_state_snapshot",
    ] {
        assert!(
            !got.contains(&name.to_string()),
            "L1 assistant-tier registry must not register {name}"
        );
    }
}

#[test]
fn assistant_tier_registry_includes_session_state_tools_for_l0() {
    let got = registry_names(AgentTier::L0Orchestration);
    for name in [
        "session_state_list",
        "session_state_status",
        "session_state_snapshot",
    ] {
        assert!(
            got.contains(&name.to_string()),
            "L0 assistant-tier registry must register {name}"
        );
    }
}

/// An L1 persona that DECLARES the session-state tools still gets none.
///
/// Why: this is hard requirement 1 of #4171 and the whole point of the tier
/// gate — a persona's `[tools].allow` must not be able to buy back what the
/// black-box posture excludes.
/// What: runs the real `build_assistant_tier_registry` +
/// `scope_assistant_allowed_tools` composition — exactly what
/// `runtime::subagent_mode` runs — with an allow-list that names every gated
/// tool plus a `session_*` wildcard, then applies the tier gate as the
/// dispatch path does, and asserts nothing survives.
/// Test: this function IS the test.
#[test]
fn l1_persona_declaring_session_state_tools_gets_none() {
    let allow = names(&[
        "session_*",
        "session_state_list",
        "session_state_status",
        "session_state_snapshot",
        "session_list",
        "session_status",
        "project_list",
        "console_metrics",
        "system_status",
        "git_log",
    ]);
    let reg = crate::runtime::tool_registry::build_assistant_tier_registry(
        Some(&allow),
        AgentTier::L1Standard,
    );
    let scoped = crate::runtime::tool_registry::scope_assistant_allowed_tools(
        true,
        None,
        Some(&allow),
        Some(&reg),
    )
    .expect("assistant tier always resolves to Some");
    let gated = retain_tier_permitted(scoped, AgentTier::L1Standard);
    for name in L0_ONLY_SESSION_STATE_TOOLS {
        assert!(
            !gated.contains(&name.to_string()),
            "an L1 persona declaring {name} must not receive it"
        );
    }
}

/// The same declaration on an L0 persona DOES yield the session-state tools.
///
/// Why: a gate that denies everyone is not a grant. This is the counter-test
/// that proves the L0 path actually works through the same production
/// composition.
/// What: identical to `l1_persona_declaring_session_state_tools_gets_none`
/// except for the tier, asserting the three native tools survive.
/// Test: this function IS the test.
#[test]
fn l0_persona_declaring_session_state_tools_gets_them() {
    let allow = names(&[
        "session_state_list",
        "session_state_status",
        "session_state_snapshot",
    ]);
    let reg = crate::runtime::tool_registry::build_assistant_tier_registry(
        Some(&allow),
        AgentTier::L0Orchestration,
    );
    let scoped = crate::runtime::tool_registry::scope_assistant_allowed_tools(
        true,
        None,
        Some(&allow),
        Some(&reg),
    )
    .expect("assistant tier always resolves to Some");
    let mut gated = retain_tier_permitted(scoped, AgentTier::L0Orchestration);
    gated.sort();
    assert_eq!(
        gated,
        names(&[
            "session_state_list",
            "session_state_snapshot",
            "session_state_status"
        ])
    );
}

// --------------------------------------------------------------------------
// Store reader
// --------------------------------------------------------------------------

fn write_store(dir: &std::path::Path, body: &str) -> PathBuf {
    let path = dir.join("sessions.json");
    std::fs::write(&path, body).unwrap();
    path
}

const TWO_SESSIONS: &str = r#"{
  "sessions": {
    "aaaaaaaa-1111-2222-3333-444444444444": {
      "id": "aaaaaaaa-1111-2222-3333-444444444444",
      "tmux_name": "tm-older",
      "cwd": "/work/alpha",
      "task": "older task",
      "state": "paused",
      "created_at": "2026-07-01T00:00:00Z",
      "last_activity_at": "2026-07-02T00:00:00Z",
      "branch": "feat/alpha",
      "source_id": "acme/alpha"
    },
    "bbbbbbbb-5555-6666-7777-888888888888": {
      "id": "bbbbbbbb-5555-6666-7777-888888888888",
      "tmux_name": "tm-newer",
      "cwd": "/work/beta",
      "task": "newer task",
      "state": "running",
      "created_at": "2026-07-03T00:00:00Z",
      "last_activity_at": "2026-07-09T00:00:00Z",
      "branch": "feat/beta",
      "source_id": "acme/beta"
    }
  }
}"#;

#[test]
fn load_sessions_reads_records_and_sorts_by_activity() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = write_store(tmp.path(), TWO_SESSIONS);
    let got = super::store::load_sessions(&path).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].tmux_name, "tm-newer", "newest activity first");
    assert_eq!(got[1].tmux_name, "tm-older");
}

#[test]
fn load_sessions_tolerates_unknown_and_missing_fields() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = write_store(
        tmp.path(),
        r#"{"sessions":{"only-a-key":{"a_field_from_the_future":42}}}"#,
    );
    let got = super::store::load_sessions(&path).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, "only-a-key", "map key backfills an empty id");
    assert_eq!(got[0].state, "");
}

#[test]
fn load_sessions_absent_store_is_empty_not_an_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let got = super::store::load_sessions(&tmp.path().join("nope.json")).unwrap();
    assert!(got.is_empty());
}

#[test]
fn load_sessions_malformed_json_is_an_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = write_store(tmp.path(), "{ not json");
    assert!(super::store::load_sessions(&path).is_err());
}

#[test]
fn default_store_path_is_under_home() {
    let Some(p) = super::store::default_store_path() else {
        return; // no home dir on this platform; nothing to assert
    };
    assert!(p.ends_with("sessions.json"));
    assert!(p.to_string_lossy().contains(".trusty-mpm"));
}

// --------------------------------------------------------------------------
// session_state_list / session_state_status behaviour
// --------------------------------------------------------------------------

fn list_tool(path: PathBuf) -> SessionStateListTool {
    SessionStateListTool::with_store_path(path)
}

fn status_tool(path: PathBuf) -> SessionStateStatusTool {
    SessionStateStatusTool::with_store_path(path)
}

#[tokio::test]
async fn list_renders_sessions_newest_first() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = write_store(tmp.path(), TWO_SESSIONS);
    let r = list_tool(path).execute(json!({})).await;
    assert!(!r.is_error(), "{}", r.content());
    let body = r.content().to_string();
    let newer = body.find("tm-newer").expect("newer row present");
    let older = body.find("tm-older").expect("older row present");
    assert!(newer < older, "most recently active session first:\n{body}");
    assert!(body.contains("branch=feat/beta"));
}

#[tokio::test]
async fn list_filters_by_state() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = write_store(tmp.path(), TWO_SESSIONS);
    let r = list_tool(path).execute(json!({"state": "RUNNING"})).await;
    let body = r.content().to_string();
    assert!(body.contains("tm-newer"));
    assert!(!body.contains("tm-older"));
}

#[tokio::test]
async fn list_filters_by_project_substring() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = write_store(tmp.path(), TWO_SESSIONS);
    let r = list_tool(path).execute(json!({"project": "alpha"})).await;
    let body = r.content().to_string();
    assert!(body.contains("tm-older"));
    assert!(!body.contains("tm-newer"));
}

/// Build a store with `n` sessions so the row-count defaults can be pinned.
fn many_sessions(n: usize, task: &str) -> String {
    let rows: Vec<String> = (0..n)
        .map(|i| {
            format!(
                r#""id-{i:04}": {{"id":"id-{i:04}","tmux_name":"tm-{i:04}","task":"{task}","state":"running","created_at":"2026-07-{:02}T00:00:00Z"}}"#,
                (i % 28) + 1
            )
        })
        .collect();
    format!("{{\"sessions\":{{{}}}}}", rows.join(","))
}

#[tokio::test]
async fn list_defaults_to_twenty_five_rows() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = write_store(tmp.path(), &many_sessions(40, "t"));
    let r = list_tool(path).execute(json!({})).await;
    let rows = r.content().lines().filter(|l| l.contains("tm-")).count();
    assert_eq!(rows, 25);
}

#[tokio::test]
async fn list_clamps_an_oversized_limit() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = write_store(tmp.path(), &many_sessions(250, "t"));
    let r = list_tool(path).execute(json!({"limit": 100000})).await;
    let rows = r.content().lines().filter(|l| l.contains("tm-")).count();
    assert_eq!(rows, 200, "limit is capped at MAX_LIMIT");
}

#[tokio::test]
async fn list_truncates_long_task_text() {
    let tmp = tempfile::TempDir::new().unwrap();
    let long = "x".repeat(400);
    let path = write_store(tmp.path(), &many_sessions(1, &long));
    let r = list_tool(path).execute(json!({})).await;
    let body = r.content().to_string();
    assert!(body.contains('…'), "long task text must be elided:\n{body}");
    assert!(!body.contains(&"x".repeat(200)));
}

#[tokio::test]
async fn list_reports_empty_store_legibly() {
    let tmp = tempfile::TempDir::new().unwrap();
    let r = list_tool(tmp.path().join("absent.json"))
        .execute(json!({}))
        .await;
    assert!(!r.is_error());
    assert!(r.content().contains("No orchestration sessions matched"));
}

#[tokio::test]
async fn status_renders_full_record() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = write_store(tmp.path(), TWO_SESSIONS);
    let r = status_tool(path)
        .execute(json!({"session": "tm-newer"}))
        .await;
    assert!(!r.is_error(), "{}", r.content());
    let body = r.content().to_string();
    for key in [
        "id:",
        "tmux_name:",
        "state:",
        "branch:",
        "cwd:",
        "workspace_path:",
        "project:",
        "created_at:",
        "last_activity_at:",
        "pending_decision:",
        "task:",
    ] {
        assert!(body.contains(key), "missing field {key} in:\n{body}");
    }
}

#[tokio::test]
async fn status_matches_by_id_tmux_name_and_id_prefix() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = write_store(tmp.path(), TWO_SESSIONS);
    for needle in ["bbbbbbbb-5555-6666-7777-888888888888", "tm-newer", "BBBBBB"] {
        let r = status_tool(path.clone())
            .execute(json!({ "session": needle }))
            .await;
        assert!(!r.is_error(), "{needle}: {}", r.content());
        assert!(r.content().contains("tm-newer"), "{needle}");
    }
}

#[tokio::test]
async fn status_rejects_too_short_a_prefix() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = write_store(tmp.path(), TWO_SESSIONS);
    let r = status_tool(path).execute(json!({"session": "bbb"})).await;
    assert!(r.is_error(), "a 3-char prefix must not resolve");
}

#[tokio::test]
async fn status_reports_ambiguous_prefix() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = write_store(
        tmp.path(),
        r#"{"sessions":{
            "prefix-one":{"id":"prefix-one","tmux_name":"a","created_at":"2026-07-01T00:00:00Z"},
            "prefix-two":{"id":"prefix-two","tmux_name":"b","created_at":"2026-07-01T00:00:00Z"}
        }}"#,
    );
    let r = status_tool(path)
        .execute(json!({"session": "prefix"}))
        .await;
    assert!(r.is_error());
    assert!(r.content().contains("ambiguous"), "{}", r.content());
}

#[tokio::test]
async fn status_unknown_session_is_a_recoverable_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = write_store(tmp.path(), TWO_SESSIONS);
    let r = status_tool(path)
        .execute(json!({"session": "nothing-here"}))
        .await;
    assert!(r.is_error());
    assert!(r.content().contains("no orchestration session matches"));
}

#[tokio::test]
async fn status_requires_the_session_argument() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = write_store(tmp.path(), TWO_SESSIONS);
    let r = status_tool(path).execute(json!({})).await;
    assert!(r.is_error());
    assert!(r.content().contains("session"));
}

// --------------------------------------------------------------------------
// session_state_snapshot — path scoping
// --------------------------------------------------------------------------

/// A project root with a populated `.trusty-mpm/` and a secret OUTSIDE it.
///
/// Why: the escape tests need a real file the tool must never return, placed
/// as a sibling of the snapshot directory so `..` traversal and a symlink both
/// have somewhere plausible to aim.
/// What: returns the tempdir plus the project root inside it.
/// Test: used by every `snapshot_*` test.
fn snapshot_fixture() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().join("project");
    let snap = root.join(".trusty-mpm");
    std::fs::create_dir_all(snap.join("sessions")).unwrap();
    std::fs::write(snap.join("scrollback.txt"), "pane dump\n").unwrap();
    std::fs::write(snap.join("sessions").join("session-1.md"), "# write-up\n").unwrap();
    std::fs::write(root.join("SECRET.txt"), "do not leak\n").unwrap();
    (tmp, root)
}

#[tokio::test]
async fn snapshot_lists_directory_entries() {
    let (_tmp, root) = snapshot_fixture();
    let r = SessionStateSnapshotTool::new(root).execute(json!({})).await;
    assert!(!r.is_error(), "{}", r.content());
    let body = r.content().to_string();
    assert!(body.contains("scrollback.txt"));
    assert!(body.contains("sessions"));
    assert!(
        !body.contains("SECRET.txt"),
        "listing must not leave the dir"
    );
}

#[tokio::test]
async fn snapshot_reads_a_named_file() {
    let (_tmp, root) = snapshot_fixture();
    let r = SessionStateSnapshotTool::new(root)
        .execute(json!({"file": "sessions/session-1.md"}))
        .await;
    assert!(!r.is_error(), "{}", r.content());
    assert!(r.content().contains("# write-up"));
}

#[tokio::test]
async fn snapshot_rejects_parent_traversal() {
    let (_tmp, root) = snapshot_fixture();
    let tool = SessionStateSnapshotTool::new(root);
    for attempt in ["../SECRET.txt", "sessions/../../SECRET.txt", "..", "./x"] {
        let r = tool.execute(json!({ "file": attempt })).await;
        assert!(r.is_error(), "{attempt} must be refused");
        assert!(
            !r.content().contains("do not leak"),
            "{attempt} leaked content"
        );
    }
}

#[tokio::test]
async fn snapshot_rejects_absolute_path() {
    let (tmp, root) = snapshot_fixture();
    let absolute = tmp.path().join("project").join("SECRET.txt");
    let r = SessionStateSnapshotTool::new(root)
        .execute(json!({ "file": absolute.to_string_lossy() }))
        .await;
    assert!(r.is_error());
    assert!(!r.content().contains("do not leak"));
}

#[cfg(unix)]
#[tokio::test]
async fn snapshot_rejects_symlink_escape() {
    let (_tmp, root) = snapshot_fixture();
    let link = root.join(".trusty-mpm").join("escape.txt");
    std::os::unix::fs::symlink(root.join("SECRET.txt"), &link).unwrap();
    // The literal path is syntactically clean — only post-canonicalization
    // containment catches this one.
    let r = SessionStateSnapshotTool::new(root)
        .execute(json!({"file": "escape.txt"}))
        .await;
    assert!(
        r.is_error(),
        "symlink escape must be refused: {}",
        r.content()
    );
    assert!(!r.content().contains("do not leak"));
}

#[tokio::test]
async fn snapshot_rejects_a_directory_target() {
    let (_tmp, root) = snapshot_fixture();
    let r = SessionStateSnapshotTool::new(root)
        .execute(json!({"file": "sessions"}))
        .await;
    assert!(r.is_error());
    assert!(r.content().contains("not a regular file"));
}

#[tokio::test]
async fn snapshot_truncates_a_large_file() {
    let (_tmp, root) = snapshot_fixture();
    std::fs::write(
        root.join(".trusty-mpm").join("big.txt"),
        "y".repeat(100 * 1024),
    )
    .unwrap();
    let r = SessionStateSnapshotTool::new(root)
        .execute(json!({"file": "big.txt"}))
        .await;
    assert!(!r.is_error(), "{}", r.content());
    assert!(r.content().contains("showing first"));
    assert!(r.content().len() < 64 * 1024);
}

#[tokio::test]
async fn snapshot_absent_directory_is_a_recoverable_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let r = SessionStateSnapshotTool::new(tmp.path().join("no-project"))
        .execute(json!({}))
        .await;
    assert!(r.is_error());
    assert!(r.content().contains("no session snapshots"));
}

/// The tool's schema exposes no way to point it at another directory.
///
/// Why: path scoping rests on the root being a construction-time capability.
/// A future edit that added a `project_dir`/`root`/`path` parameter would turn
/// a session-state tool into an arbitrary filesystem reader; this test is the
/// tripwire for that.
/// What: asserts the schema's property set is exactly `file` + `max_bytes`.
/// Test: this function IS the test.
#[test]
fn snapshot_takes_no_root_parameter() {
    let schema = SessionStateSnapshotTool::new(PathBuf::from("/tmp")).schema();
    let props = schema["function"]["parameters"]["properties"]
        .as_object()
        .expect("object schema");
    let mut keys: Vec<&str> = props.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, vec!["file", "max_bytes"]);
}

#[test]
fn resolve_within_snapshot_dir_accepts_a_nested_normal_path() {
    let (_tmp, root) = snapshot_fixture();
    let dir = root.join(".trusty-mpm");
    let got = super::snapshot::resolve_within_snapshot_dir(&dir, "sessions/session-1.md")
        .expect("a plain nested path is accepted");
    assert!(got.ends_with("session-1.md"));
}
