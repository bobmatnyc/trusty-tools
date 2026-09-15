//! Runtime-decision tests for `crate::permissions::gate` and `session`
//! (#7948).
//!
//! Why: one test per acceptance criterion — `ask` blocks and resolves, a
//! timeout or an abandoned request denies, headless denies, `allow-asks`
//! allows, `allow_for_session` is remembered within a session and not across
//! sessions, and a credential in a subject is redacted.
//! What: `PermissionGate::evaluate` (pure) and `resolve` (async, against a real
//! broker), plus `redact_subject`.
//! Test: this module.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use crate::agents::config::AgentConfig;
use crate::agents::md_loader::project_embedded_md;
use crate::assets::{DEFAULT_AGENTS, EmbeddedAgent};
use crate::permissions::{
    Decision, DenySource, HARNESS_REGISTERED_TOOLS, Outcome, PermissionBroker, PermissionContext,
    PermissionDecision, PermissionEvents, PermissionGate, PermissionMode, SessionPermissions,
    redact_subject,
};

use super::{agent_with_allowlist, agent_with_permissions};

// ── The pure half: `PermissionGate::evaluate` ───────────────────────────────

/// The legacy allowlist is checked BEFORE the map, so a map cannot widen it.
#[test]
fn legacy_allowlist_denial_precedes_the_map() {
    let agent = agent_with_allowlist(&["read_file"], Some("permissions:\n  bash: allow\n"));
    assert_eq!(
        PermissionGate::evaluate(&agent, "bash", &json!({"command": "ls"})),
        Decision::Deny {
            rule: "tcode_tools allowlist".to_string()
        }
    );
}

/// An absent allowlist and an absent map permit every tool, as before #7948.
#[test]
fn legacy_allowlist_absent_means_every_tool() {
    assert_eq!(
        PermissionGate::evaluate(&AgentConfig::default(), "bash", &json!({"command": "ls"})),
        Decision::Allow
    );
}

/// A call no rule matches is allowed.
#[test]
fn unmatched_tool_is_allowed() {
    let agent = agent_with_permissions("permissions:\n  read_file: deny\n");
    assert_eq!(
        PermissionGate::evaluate(&agent, "bash", &json!({"command": "ls"})),
        Decision::Allow
    );
}

/// A `deny` rule evaluates to `Deny`, carrying the rule text.
#[test]
fn deny_rule_produces_deny() {
    let agent = agent_with_permissions("permissions:\n  bash:\n    \"rm *\": deny\n");
    assert_eq!(
        PermissionGate::evaluate(&agent, "bash", &json!({"command": "rm -rf build"})),
        Decision::Deny {
            rule: "bash[rm *]".to_string()
        }
    );
}

/// An `ask` rule evaluates to `Ask`, carrying the rule text.
#[test]
fn ask_rule_produces_ask() {
    let agent = agent_with_permissions("permissions:\n  \"mcp__*\": ask\n");
    assert_eq!(
        PermissionGate::evaluate(&agent, "mcp__fixture__search", &json!({"q": "x"})),
        Decision::Ask {
            rule: "mcp__*".to_string()
        }
    );
}

// ── Harness-registered tools vs. the legacy allowlist (#7948) ───────────────

/// The stock `pm` agent, loaded through the real embedded-agent path so the
/// test sees the shipped `tcode_tools:` line rather than a hand-written copy.
fn stock_pm_agent() -> AgentConfig {
    let md = DEFAULT_AGENTS
        .iter()
        .find_map(|a| match a {
            EmbeddedAgent::Direct { name, md } if *name == "pm" => Some(*md),
            _ => None,
        })
        .expect("the embedded roster carries a Direct `pm` agent");
    project_embedded_md("pm", md).expect("stock pm.md loads")
}

/// #7948 regression: `delegate_to_agent` is registered on the PM
/// unconditionally (`run_task::execute_run_task`, `task::executor`), yet stock
/// `pm.md` never lists it, so the allowlist branch refused the PM's own
/// delegation on both run-task paths.
#[test]
fn stock_pm_agent_may_call_delegate_to_agent() {
    let pm = stock_pm_agent();
    let allowed = pm
        .tools
        .as_ref()
        .and_then(|t| t.allowed.as_ref())
        .expect("stock pm.md declares a tcode_tools allowlist");
    assert!(
        !allowed.iter().any(|t| t == "delegate_to_agent"),
        "fixture premise: stock pm.md must still omit delegate_to_agent, got {allowed:?}"
    );
    assert_eq!(
        PermissionGate::evaluate(
            &pm,
            "delegate_to_agent",
            &json!({"agent_name": "engineer", "task": "build it"})
        ),
        Decision::Allow
    );
}

/// Every tool the harness registers clears an allowlist naming none of them.
#[test]
fn harness_registered_tool_bypasses_the_legacy_allowlist() {
    let agent = agent_with_allowlist(&["read_file"], None);
    for tool in HARNESS_REGISTERED_TOOLS {
        assert_eq!(
            PermissionGate::evaluate(&agent, tool, &json!({})),
            Decision::Allow,
            "{tool} is harness-registered and must clear the allowlist"
        );
    }
}

/// The exemption is a closed list, not a widening — a tool the author left out
/// is still denied by the allowlist.
#[test]
fn harness_exemption_does_not_widen_an_ordinary_tool() {
    let agent = agent_with_allowlist(&["read_file"], None);
    assert_eq!(
        PermissionGate::evaluate(&agent, "bash", &json!({"command": "ls"})),
        Decision::Deny {
            rule: "tcode_tools allowlist".to_string()
        }
    );
}

/// An operator's written `deny` still refuses a harness-registered tool: the
/// exemption waives only the allowlist's blanket "absent means denied".
#[test]
fn explicit_deny_beats_the_harness_registered_exemption() {
    let agent = agent_with_allowlist(
        &["read_file"],
        Some("permissions:\n  delegate_to_agent: deny\n"),
    );
    assert_eq!(
        PermissionGate::evaluate(&agent, "delegate_to_agent", &json!({"agent_name": "qa"})),
        Decision::Deny {
            rule: "delegate_to_agent".to_string()
        }
    );
}

/// (#7948 regression) A path `deny` written RELATIVE refuses the absolute
/// in-root spelling of the same file once the gate knows the run's root.
///
/// Why: `tools::fs::scoped_path` accepts an absolute path inside the working
/// root, so both spellings reach the same file; before the root was threaded
/// into `PermissionContext`, only the relative one met the rule.
/// What: the same agent and the same absolute path, decided with and without a
/// root — the pair pins that the root, not the rule text, is what closed it.
#[tokio::test]
async fn absolute_in_root_path_meets_a_relative_deny() {
    let agent = agent_with_permissions("permissions:\n  read_file:\n    \"secrets/**\": deny\n");
    let root = std::path::PathBuf::from("/Users/me/proj");
    let call = json!({"path": "/Users/me/proj/secrets/x"});

    let rooted = PermissionContext::headless(PermissionMode::Default).with_root(root);
    assert_eq!(
        gate(&rooted, agent.clone())
            .resolve("read_file", &call)
            .await,
        Outcome::Denied {
            rule: "read_file[secrets/**]".to_string(),
            source: DenySource::Policy
        }
    );

    let rootless = PermissionContext::headless(PermissionMode::Default);
    assert_eq!(
        gate(&rootless, agent).resolve("read_file", &call).await,
        Outcome::Allow,
        "with no root the gate can only judge the spelling the model wrote"
    );
}

// ── The async half: `PermissionGate::resolve` ───────────────────────────────

/// A recording `PermissionEvents` sink; the event is also how a test learns a
/// request id, exactly as a real client does.
#[derive(Default)]
struct RecordingEvents {
    lines: Mutex<Vec<String>>,
    request_ids: Mutex<Vec<String>>,
}

impl RecordingEvents {
    fn lines(&self) -> Vec<String> {
        self.lines.lock().expect("lock").clone()
    }

    fn pending_request_id(&self) -> Option<String> {
        self.request_ids.lock().expect("lock").last().cloned()
    }
}

impl PermissionEvents for RecordingEvents {
    fn permission_requested(
        &self,
        _session_id: &str,
        request_id: &str,
        agent: &str,
        _agent_id: &str,
        tool: &str,
        subject: &str,
        rule: &str,
    ) {
        self.lines
            .lock()
            .expect("lock")
            .push(format!("requested {agent} {tool} {subject} {rule}"));
        self.request_ids
            .lock()
            .expect("lock")
            .push(request_id.to_string());
    }

    fn permission_resolved(
        &self,
        _session_id: &str,
        _request_id: &str,
        _agent: &str,
        _agent_id: &str,
        decision: &str,
        source: &str,
    ) {
        self.lines
            .lock()
            .expect("lock")
            .push(format!("resolved {decision} {source}"));
    }
}

/// An interactive context wired to `broker`'s slot for `session`.
fn interactive_ctx(
    broker: &PermissionBroker,
    session: &str,
    timeout: Duration,
) -> (PermissionContext, Arc<RecordingEvents>) {
    let events = Arc::new(RecordingEvents::default());
    let ctx = PermissionContext {
        mode: PermissionMode::Default,
        timeout,
        session_id: session.to_string(),
        ask: Some(broker.session(session)),
        events: Some(Arc::clone(&events) as Arc<dyn PermissionEvents>),
        root: None,
    };
    (ctx, events)
}

fn gate(ctx: &PermissionContext, agent: AgentConfig) -> PermissionGate {
    ctx.gate_for(Arc::new(agent), "agent-1")
}

fn ask_bash() -> AgentConfig {
    agent_with_permissions("permissions:\n  bash: ask\n")
}

/// A gate for an agent with no map allows everything.
#[tokio::test]
async fn gate_for_agent_without_a_map_allows_everything() {
    let ctx = PermissionContext::headless(PermissionMode::Default);
    let outcome = gate(&ctx, AgentConfig::default())
        .resolve("bash", &json!({"command": "rm -rf /"}))
        .await;
    assert_eq!(outcome, Outcome::Allow);
}

/// HEADLESS: an `ask` with no client to answer it is denied.
#[tokio::test]
async fn headless_ask_is_denied() {
    let ctx = PermissionContext::headless(PermissionMode::Default);
    assert_eq!(
        gate(&ctx, ask_bash())
            .resolve("bash", &json!({"command": "ls"}))
            .await,
        Outcome::Denied {
            rule: "bash".to_string(),
            source: DenySource::Headless
        }
    );
}

/// `allow-asks` permits an `ask` a headless run could not put to anyone.
#[tokio::test]
async fn allow_asks_mode_permits_an_ask() {
    let ctx = PermissionContext::headless(PermissionMode::AllowAsks);
    assert_eq!(
        gate(&ctx, ask_bash())
            .resolve("bash", &json!({"command": "ls"}))
            .await,
        Outcome::Allow
    );
}

/// (#7948) `allow-asks` leaves an audit trail: the auto-approval emits
/// `permission_resolved` marked `allow_asks`, with no request event before it.
#[tokio::test]
async fn allow_asks_mode_emits_an_audit_event() {
    let events = Arc::new(RecordingEvents::default());
    let ctx = PermissionContext {
        events: Some(Arc::clone(&events) as Arc<dyn PermissionEvents>),
        ..PermissionContext::headless(PermissionMode::AllowAsks)
    };
    let outcome = gate(&ctx, ask_bash())
        .resolve("bash", &json!({"command": "ls"}))
        .await;
    assert_eq!(outcome, Outcome::Allow);
    assert_eq!(events.lines(), vec!["resolved allow_once allow_asks"]);
}

/// `allow-asks` does NOT soften a `deny`.
#[tokio::test]
async fn allow_asks_mode_does_not_soften_a_deny() {
    let ctx = PermissionContext::headless(PermissionMode::AllowAsks);
    let agent = agent_with_permissions("permissions:\n  bash: deny\n");
    assert!(matches!(
        gate(&ctx, agent)
            .resolve("bash", &json!({"command": "ls"}))
            .await,
        Outcome::Denied {
            source: DenySource::Policy,
            ..
        }
    ));
}

/// TIMEOUT: an `ask` nobody answers inside the budget resolves to deny.
#[tokio::test]
async fn timed_out_ask_is_denied() {
    let broker = PermissionBroker::new();
    let (ctx, events) = interactive_ctx(&broker, "s-timeout", Duration::from_millis(30));
    let outcome = gate(&ctx, ask_bash())
        .resolve("bash", &json!({"command": "ls"}))
        .await;
    assert_eq!(
        outcome,
        Outcome::Denied {
            rule: "bash".to_string(),
            source: DenySource::Timeout
        }
    );
    assert_eq!(
        outcome.message().as_deref(),
        Some("denied by policy (bash): no decision")
    );
    assert_eq!(
        events.lines().last().map(String::as_str),
        Some("resolved deny timeout")
    );
}

/// FAIL-CLOSED (#7948): a request whose responder is dropped without an answer
/// resolves to deny long before the timeout — an error never becomes an allow.
#[tokio::test]
async fn abandoned_request_resolves_to_deny() {
    let broker = Arc::new(PermissionBroker::new());
    let (ctx, events) = interactive_ctx(&broker, "s-abandon", Duration::from_secs(30));
    let state = broker.session("s-abandon");
    let abandoning = tokio::spawn({
        let events = Arc::clone(&events);
        async move {
            let id = wait_for_request(&events).await;
            state.forget(&id);
        }
    });
    let started = std::time::Instant::now();
    let outcome = gate(&ctx, ask_bash())
        .resolve("bash", &json!({"command": "ls"}))
        .await;
    abandoning.await.expect("abandoning task must not panic");
    assert!(matches!(
        outcome,
        Outcome::Denied {
            source: DenySource::Timeout,
            ..
        }
    ));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "a dropped responder must resolve at once, not wait out the timeout"
    );
}

/// A client's `deny` refuses the call.
#[tokio::test]
async fn client_deny_is_denied() {
    let broker = Arc::new(PermissionBroker::new());
    let (ctx, events) = interactive_ctx(&broker, "s-deny", Duration::from_secs(5));
    let answering = answer_next_request(&broker, "s-deny", events, PermissionDecision::Deny);
    let outcome = gate(&ctx, ask_bash())
        .resolve("bash", &json!({"command": "ls"}))
        .await;
    answering.await.expect("answering task must not panic");
    assert!(matches!(
        outcome,
        Outcome::Denied {
            source: DenySource::Client,
            ..
        }
    ));
}

/// The request/resolve event pair is emitted around an answered `ask`.
#[tokio::test]
async fn ask_emits_requested_then_resolved() {
    let broker = Arc::new(PermissionBroker::new());
    let (ctx, events) = interactive_ctx(&broker, "s-events", Duration::from_secs(5));
    let answering = answer_next_request(
        &broker,
        "s-events",
        Arc::clone(&events),
        PermissionDecision::AllowOnce,
    );
    let outcome = gate(&ctx, ask_bash())
        .resolve("bash", &json!({"command": "ls -la"}))
        .await;
    answering.await.expect("answering task must not panic");

    assert_eq!(outcome, Outcome::Allow);
    assert_eq!(
        events.lines(),
        vec!["requested t bash ls -la bash", "resolved allow_once client"]
    );
}

/// `allow_for_session` is remembered for a later matching call in the SAME
/// session — the second call emits no second request.
#[tokio::test]
async fn allow_for_session_is_remembered_for_a_later_matching_call() {
    let broker = Arc::new(PermissionBroker::new());
    let (ctx, events) = interactive_ctx(&broker, "s-remember", Duration::from_secs(5));
    let gate = gate(&ctx, ask_bash());
    let answering = answer_next_request(
        &broker,
        "s-remember",
        Arc::clone(&events),
        PermissionDecision::AllowForSession {
            pattern: Some("git *".to_string()),
        },
    );
    let first = gate
        .resolve("bash", &json!({"command": "git status"}))
        .await;
    answering.await.expect("answering task must not panic");
    assert_eq!(first, Outcome::Allow);

    let second = gate.resolve("bash", &json!({"command": "git diff"})).await;
    assert_eq!(second, Outcome::Allow);
    // #7948: the reuse is recorded, but nobody is asked again — one resolution
    // with no `requested` before it, not a second request/resolve pair.
    assert_eq!(
        events.lines(),
        vec![
            "requested t bash git status bash",
            "resolved allow_for_session client",
            "resolved allow_for_session remembered",
        ]
    );
}

/// (#7948 regression) Reusing a remembered grant emits `permission_resolved`,
/// so an audit sees the call the grant covered.
///
/// Why: the short-circuit returned `Outcome::Allow` with no log and no event,
/// making every call after the first one invisible — the grant's whole point is
/// that there is no prompt to see instead.
/// What: one `allow_for_session` answer, then a second matching call; assert
/// the second call's own resolution line, sourced `remembered` rather than
/// `client` because no client was consulted.
#[tokio::test]
async fn remembered_grant_emits_a_resolution_event() {
    let broker = Arc::new(PermissionBroker::new());
    let (ctx, events) = interactive_ctx(&broker, "s-remember-event", Duration::from_secs(5));
    let gate = gate(&ctx, ask_bash());
    let answering = answer_next_request(
        &broker,
        "s-remember-event",
        Arc::clone(&events),
        PermissionDecision::AllowForSession {
            pattern: Some("git *".to_string()),
        },
    );
    let first = gate
        .resolve("bash", &json!({"command": "git status"}))
        .await;
    answering.await.expect("answering task must not panic");
    assert_eq!(first, Outcome::Allow);

    let before = events.lines().len();
    assert_eq!(
        gate.resolve("bash", &json!({"command": "git diff"})).await,
        Outcome::Allow
    );
    assert_eq!(
        events.lines().get(before..),
        Some(["resolved allow_for_session remembered".to_string()].as_slice()),
        "the remembered grant must record the call it covered"
    );
}

/// A remembered grant does not leak into another session.
#[tokio::test]
async fn allow_for_session_does_not_leak_across_sessions() {
    let broker = Arc::new(PermissionBroker::new());
    let (first, events) = interactive_ctx(&broker, "s-a", Duration::from_secs(5));
    let answering = answer_next_request(
        &broker,
        "s-a",
        events,
        PermissionDecision::AllowForSession {
            pattern: Some("git *".to_string()),
        },
    );
    let granted = gate(&first, ask_bash())
        .resolve("bash", &json!({"command": "git status"}))
        .await;
    answering.await.expect("answering task must not panic");
    assert_eq!(granted, Outcome::Allow);

    let (second, _events) = interactive_ctx(&broker, "s-b", Duration::from_millis(30));
    let other = gate(&second, ask_bash())
        .resolve("bash", &json!({"command": "git diff"}))
        .await;
    assert!(
        matches!(
            other,
            Outcome::Denied {
                source: DenySource::Timeout,
                ..
            }
        ),
        "session A's grant must not reach session B"
    );
}

/// A bare `allow_for_session` (no pattern) grants exactly that subject.
#[tokio::test]
async fn bare_allow_for_session_grants_only_that_subject() {
    let broker = Arc::new(PermissionBroker::new());
    let (ctx, events) = interactive_ctx(&broker, "s-bare", Duration::from_millis(30));
    let gate = gate(&ctx, ask_bash());
    let answering = answer_next_request(
        &broker,
        "s-bare",
        events,
        PermissionDecision::AllowForSession { pattern: None },
    );
    let first = gate
        .resolve("bash", &json!({"command": "git status"}))
        .await;
    answering.await.expect("answering task must not panic");
    assert_eq!(first, Outcome::Allow);

    assert_eq!(
        gate.resolve("bash", &json!({"command": "git status"}))
            .await,
        Outcome::Allow,
        "the exact same subject is covered"
    );
    assert!(
        matches!(
            gate.resolve("bash", &json!({"command": "git status --short"}))
                .await,
            Outcome::Denied { .. }
        ),
        "a bare grant must not widen to a prefix match"
    );
}

/// A client-supplied pattern grants the whole glob.
#[tokio::test]
async fn allow_for_session_pattern_grants_the_glob() {
    let broker = Arc::new(PermissionBroker::new());
    let (ctx, events) = interactive_ctx(&broker, "s-glob", Duration::from_millis(30));
    let gate = gate(&ctx, ask_bash());
    let answering = answer_next_request(
        &broker,
        "s-glob",
        events,
        PermissionDecision::AllowForSession {
            pattern: Some("*".to_string()),
        },
    );
    let first = gate
        .resolve("bash", &json!({"command": "git status"}))
        .await;
    answering.await.expect("answering task must not panic");
    assert_eq!(first, Outcome::Allow);
    assert_eq!(
        gate.resolve("bash", &json!({"command": "anything at all"}))
            .await,
        Outcome::Allow
    );
}

/// An uncompilable pattern still allows THIS call but remembers nothing.
#[tokio::test]
async fn unusable_allow_for_session_pattern_remembers_nothing() {
    let broker = Arc::new(PermissionBroker::new());
    let (ctx, events) = interactive_ctx(&broker, "s-badglob", Duration::from_millis(30));
    let gate = gate(&ctx, ask_bash());
    let answering = answer_next_request(
        &broker,
        "s-badglob",
        events,
        PermissionDecision::AllowForSession {
            pattern: Some("a[".to_string()),
        },
    );
    let first = gate
        .resolve("bash", &json!({"command": "git status"}))
        .await;
    answering.await.expect("answering task must not panic");
    assert_eq!(first, Outcome::Allow);
    assert!(matches!(
        gate.resolve("bash", &json!({"command": "git status"}))
            .await,
        Outcome::Denied { .. }
    ));
}

/// The refusal message names the rule that produced it.
#[test]
fn deny_message_names_the_rule() {
    let denied = Outcome::Denied {
        rule: "bash[rm *]".to_string(),
        source: DenySource::Policy,
    };
    assert_eq!(
        denied.message().as_deref(),
        Some("denied by policy (bash[rm *])")
    );
    assert!(Outcome::Allow.message().is_none());
}

// ── Broker / responder ──────────────────────────────────────────────────────

/// The broker hands back the same state for one session id.
#[test]
fn broker_hands_the_same_state_back_for_one_session() {
    let broker = PermissionBroker::new();
    let a = broker.session("s");
    assert!(Arc::ptr_eq(&a, &broker.session("s")));
    assert!(!Arc::ptr_eq(&a, &broker.session("other")));
}

/// Answering a request nobody is waiting on is a client error.
#[test]
fn respond_to_unknown_request_is_an_error() {
    let err = SessionPermissions::default()
        .respond("missing", PermissionDecision::AllowOnce)
        .expect_err("an unknown request id must be rejected");
    assert!(err.message.contains("missing"), "{}", err.message);
}

/// Answering a session that has never asked anything is a client error.
#[test]
fn respond_to_unknown_session_is_an_error() {
    let err = PermissionBroker::new()
        .respond("nope", "req", PermissionDecision::AllowOnce)
        .expect_err("an unknown session must be rejected");
    assert!(err.message.contains("nope"), "{}", err.message);
}

/// A response delivered while a gate waits resolves that gate.
#[tokio::test]
async fn respond_resolves_the_waiting_gate() {
    let broker = Arc::new(PermissionBroker::new());
    let (ctx, events) = interactive_ctx(&broker, "s-resp", Duration::from_secs(5));
    let answering = answer_next_request(&broker, "s-resp", events, PermissionDecision::AllowOnce);
    let outcome = gate(&ctx, ask_bash())
        .resolve("bash", &json!({"command": "ls"}))
        .await;
    answering.await.expect("answering task must not panic");
    assert_eq!(outcome, Outcome::Allow);
}

// ── Mode resolution ─────────────────────────────────────────────────────────

/// The `allow-asks` spellings an operator might type all parse.
#[test]
fn permission_mode_parses_allow_asks_spellings() {
    for spelling in ["allow-asks", "allow_asks", "ALLOW-ASKS", "  allow-asks "] {
        assert_eq!(
            PermissionMode::parse_lenient(spelling),
            Some(PermissionMode::AllowAsks),
            "{spelling}"
        );
    }
    assert_eq!(
        PermissionMode::parse_lenient("default"),
        Some(PermissionMode::Default)
    );
    assert_eq!(PermissionMode::parse_lenient("yolo"), None);
}

/// A flag outranks the environment variable.
#[test]
fn cli_permission_mode_outranks_the_env_var() {
    assert_eq!(
        PermissionMode::resolve_from(Some("default"), Some("allow-asks")),
        PermissionMode::Default
    );
}

/// With no flag, the environment variable applies.
#[test]
fn env_permission_mode_applies_when_no_flag_is_given() {
    assert_eq!(
        PermissionMode::resolve_from(None, Some("allow-asks")),
        PermissionMode::AllowAsks
    );
}

/// FAIL-CLOSED: an unrecognised value never widens; the next tier or `Default`
/// applies.
#[test]
fn unrecognised_permission_mode_falls_back_to_default() {
    assert_eq!(
        PermissionMode::resolve_from(Some("yolo"), None),
        PermissionMode::Default
    );
    assert_eq!(
        PermissionMode::resolve_from(Some("yolo"), Some("allow-asks")),
        PermissionMode::AllowAsks
    );
    assert_eq!(
        PermissionMode::resolve_from(None, Some("yolo")),
        PermissionMode::Default
    );
}

// ── Redaction ───────────────────────────────────────────────────────────────

/// A lowercase `token=` assignment is redacted.
#[test]
fn redacts_a_lowercase_token_assignment() {
    let out = redact_subject("curl -d token=abc123 https://example.test");
    assert!(!out.contains("abc123"), "{out}");
    assert!(out.contains("token=<redacted>"), "{out}");
}

/// An uppercase environment assignment prefixing a command is redacted.
#[test]
fn redacts_an_uppercase_env_assignment() {
    let out = redact_subject("GITHUB_TOKEN=ghp_realsecret gh pr create");
    assert!(!out.contains("ghp_realsecret"), "{out}");
    assert!(out.contains("GITHUB_TOKEN=<redacted>"), "{out}");
    assert!(out.contains("gh pr create"), "{out}");
}

/// An `Authorization:` header value is redacted.
#[test]
fn redacts_an_authorization_header() {
    // #7948: a value under 8 chars stays below gitleaks' `curl-auth-header` rule.
    let out = redact_subject("curl -H 'Authorization: Bearer abc123' https://example.test");
    assert!(!out.contains("abc123"), "{out}");
    assert!(out.contains("Authorization: <redacted>"), "{out}");
}

/// An ordinary command passes through untouched.
#[test]
fn leaves_an_ordinary_command_untouched() {
    let command = "cargo test -p trusty-code --no-fail-fast";
    assert_eq!(redact_subject(command), command);
}

/// The subject is bounded so a multi-KB command cannot saturate the bus.
#[test]
fn bounds_the_subject_to_500_chars() {
    assert_eq!(redact_subject(&"a".repeat(2_000)).chars().count(), 501);
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Poll the recorder until a request id appears (5 s hang guard).
async fn wait_for_request(events: &RecordingEvents) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(id) = events.pending_request_id() {
            return id;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no permission request arrived within 5s"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Answer the next `PermissionRequested` through the broker — the same round
/// trip a real client makes.
fn answer_next_request(
    broker: &Arc<PermissionBroker>,
    session: &str,
    events: Arc<RecordingEvents>,
    decision: PermissionDecision,
) -> tokio::task::JoinHandle<()> {
    let broker = Arc::clone(broker);
    let session = session.to_string();
    tokio::spawn(async move {
        let id = wait_for_request(&events).await;
        broker
            .respond(&session, &id, decision)
            .expect("respond must succeed");
    })
}
