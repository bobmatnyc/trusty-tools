//! `GET /api/agents/:name/subagents` handler tests (#4029, epic #4021 OQ-5).
//!
//! Why: this route's whole reason to exist is that it must never advertise a
//! delegation target the enforcement layer would refuse. Three properties carry
//! that weight and each is pinned here rather than left to the sibling
//! mechanisms' own tests:
//!
//! 1. **The tier interaction (#4169, epic #4167).** An L1 delegator must not be
//!    shown an L0-orchestration target as reachable, and an L0 delegator must
//!    be. Today NO shipped persona declares `tier` at all, so this is
//!    exercisable only against synthetic fixtures — which is exactly why it
//!    needs a test rather than a manual check.
//! 2. **Cross-product deny-by-default (OQ-7).** An agent with no `[subagents]`
//!    section must see every floor target denied — the pane may not read an
//!    absent section as a silent grant.
//! 3. **The two mechanisms stay labelled and separate.** `documentation` is an
//!    in-product role and nothing else; `research`/`ticketing` are
//!    cross-product specialists and nothing else. A flattened list would lose
//!    that and the pane could not say which enforcement layer applies.
//!
//! What: [`subagents_at`] driven against a `tempfile::TempDir` roster (the
//! `agent_knowledge`/`agent_skills` pattern), plus one full-router test proving
//! the route is wired. Fixtures use the `[agent]`+`[llm]`+`[system_prompt]`
//! shape `AgentConfig::by_name_in` requires — the same shape
//! `tools::delegate_tests::write_agent_toml_with_role_and_tier` uses, because
//! this route mirrors that gate and therefore resolves configs the same way.
//! Test: This module IS the test.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::api::server::agent_subagents::subagents_at;
use crate::api::server::routes::build_router;
use crate::api::server::state::AppState;

/// Write a fully-parseable agent TOML. `tier` and `subagents_allowed` are
/// omitted from the file entirely when `None`, so an "absent section" fixture is
/// genuinely absent rather than an empty list.
fn write_agent(
    dir: &std::path::Path,
    name: &str,
    role: &str,
    tier: Option<&str>,
    tools_allow: &[&str],
    subagents_allowed: Option<&[&str]>,
) {
    let tier_line = tier
        .map(|t| format!("tier = \"{t}\"\n"))
        .unwrap_or_default();
    let tools_block = if tools_allow.is_empty() {
        String::new()
    } else {
        let list = tools_allow
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!("\n[tools]\nallow = [{list}]\n")
    };
    let subagents_block = match subagents_allowed {
        None => String::new(),
        Some(list) => {
            let joined = list
                .iter()
                .map(|t| format!("\"{t}\""))
                .collect::<Vec<_>>()
                .join(", ");
            format!("\n[subagents]\nallowed = [{joined}]\n")
        }
    };
    std::fs::write(
        dir.join(format!("{name}.toml")),
        format!(
            r#"
[agent]
name = "{name}"
role = "{role}"
model = "anthropic/claude-sonnet-4-6"
description = "test fixture"
{tier_line}
[llm]
temperature = 0.2
max_tokens = 1024

[system_prompt]
content = "test"
{tools_block}{subagents_block}"#
        ),
    )
    .unwrap();
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// A roster shaped like the real bundled one: one assistant-tier subject, one
/// worker of each interesting role, and one agent whose role is NOT in
/// `ASSISTANT_ALLOWED_DELEGATE_ROLES` (`orchestrator`, the escalation target
/// #3555's role gate closed).
fn standard_roster(dir: &std::path::Path) {
    write_agent(
        dir,
        "assistant",
        "assistant",
        None,
        &["delegate_to_agent", "git_log"],
        None,
    );
    write_agent(dir, "engineer", "engineer", None, &[], None);
    write_agent(dir, "docs-agent", "documentation", None, &[], None);
    write_agent(dir, "izzie", "assistant", None, &[], None);
    write_agent(dir, "pm", "orchestrator", None, &[], None);
    write_agent(dir, "ticketing-agent", "ticketing", None, &[], None);
}

#[tokio::test]
async fn subagents_route_reports_both_mechanisms_for_an_assistant() {
    let dir = tempfile::tempdir().unwrap();
    standard_roster(dir.path());

    let resp = subagents_at(&[dir.path().to_path_buf()], "assistant", dir.path()).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["resolved"], true);

    // Both halves are present and LABELLED — never flattened into one list.
    let ip = &body["in_product"];
    let cp = &body["cross_product"];
    assert_eq!(ip["mechanism"], "in_product");
    assert_eq!(ip["tool"], "delegate_to_agent");
    assert_eq!(cp["mechanism"], "cross_product");
    assert_eq!(cp["tool"], "dispatch_task");

    // `delegate_to_agent` is both registered (role == "assistant") and granted.
    assert_eq!(ip["tool_registered"], true);
    assert_eq!(ip["tool_granted"], true);
    assert_eq!(
        ip["delegator_tier"], "l1",
        "no tier declared ⇒ fail-closed L1"
    );

    let targets = ip["targets"].as_array().unwrap();
    let named: Vec<&str> = targets
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(named.contains(&"engineer"), "{named:?}");
    // ADR-0024: a peer assistant is still REPORTED (role-eligible, so it is
    // not silently dropped) but is no longer REACHABLE — assistants
    // communicate with each other rather than delegating. This assertion
    // replaces the pre-ADR-0024 one that treated izzie as a live target; the
    // pane must never advertise a capability the gate refuses.
    let izzie = targets
        .iter()
        .find(|t| t["name"] == "izzie")
        .expect("peer assistant must still be reported, with a reason");
    assert_eq!(
        izzie["reachable"], false,
        "peer assistant is not a delegation target: {izzie:?}"
    );
    assert!(
        izzie["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("peer assistant"),
        "the pane must explain the kind rule: {izzie:?}"
    );
    // `documentation` is the role that exists ONLY in
    // `ASSISTANT_ALLOWED_DELEGATE_ROLES` — its presence here is what proves the
    // in-product half is sourced from that constant and not from the
    // cross-product floor.
    let docs = targets
        .iter()
        .find(|t| t["name"] == "docs-agent")
        .expect("a documentation-role agent must be an in-product target");
    assert_eq!(docs["role"], "documentation");
    assert_eq!(docs["reachable"], true);
    assert_eq!(docs["reason"], serde_json::Value::Null);

    // The role gate's exclusions are COUNTED, never named (no roster dump), and
    // the subject itself never appears as its own target.
    assert!(
        !named.contains(&"pm") && !named.contains(&"ticketing-agent"),
        "role-ineligible agents must not become target cards: {named:?}"
    );
    assert!(!named.contains(&"assistant"), "no self-delegation card");
    assert_eq!(ip["role_excluded_count"], 2, "pm + ticketing-agent");

    // The role allowlist is reported verbatim so the pane can explain the rule.
    let roles: Vec<&str> = ip["allowed_roles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    assert!(roles.contains(&"documentation"));
    assert!(!roles.contains(&"orchestrator"));

    // Cross-product: nothing declared ⇒ nothing granted, and `dispatch_task` is
    // not in this agent's allow-list either.
    assert_eq!(cp["declares_allowed"], false);
    assert_eq!(cp["tool_granted"], false);
    let floor: Vec<&str> = cp["bridge_floor"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    assert_eq!(floor, vec!["research", "ticketing"]);
}

/// THE test #4029 names explicitly: "an L1 agent must not be shown an L0
/// target". The gate (`tools::delegate`, #4169) refuses that delegation, so a
/// config surface reporting it as reachable would advertise a capability the
/// runtime denies.
///
/// ADR-0024 update: the L0 fixture is now a SUB-AGENT (`engineer`), not an
/// assistant. An assistant-role L0 target is refused by the kind predicate
/// before the tier comparison is consulted, which would leave this test
/// green while testing nothing about tier — the same isolation applied to
/// `delegate_l1_to_l0_is_refused` in `tools::delegate`'s tests.
#[tokio::test]
async fn subagents_route_hides_l0_target_from_l1_delegator() {
    let dir = tempfile::tempdir().unwrap();
    standard_roster(dir.path());
    // An L0-orchestration sub-agent — role-eligible (`engineer`) and NOT the
    // assistant kind, so the ONLY thing that can keep it out of reach is the
    // tier gate.
    write_agent(dir.path(), "orch", "engineer", Some("l0"), &[], None);

    let resp = subagents_at(&[dir.path().to_path_buf()], "assistant", dir.path()).await;
    let body = body_json(resp).await;
    let ip = &body["in_product"];
    assert_eq!(ip["delegator_tier"], "l1");

    let targets = ip["targets"].as_array().unwrap();
    let orch = targets
        .iter()
        .find(|t| t["name"] == "orch")
        .expect("the L0 target must be REPORTED, with a reason — not silently dropped");
    assert_eq!(orch["tier"], "l0");
    assert_eq!(
        orch["reachable"], false,
        "an L1 delegator may not reach an L0 target: {orch:?}"
    );
    let reason = orch["reason"].as_str().unwrap();
    assert!(reason.contains("L0/L1"), "{reason}");
    assert!(reason.contains("#4169"), "{reason}");

    // Not a blanket denial: an L1 SUB-AGENT is still reachable. (Pre-ADR-0024
    // this assertion used the peer assistant `izzie`; a peer is now refused by
    // kind, so the "one-directional, not blanket" property is demonstrated
    // with a target the kind rule permits.)
    let eng = targets.iter().find(|t| t["name"] == "engineer").unwrap();
    assert_eq!(eng["tier"], "l1");
    assert_eq!(eng["reachable"], true);
}

/// The counter-test: an L0 delegator DOES reach an L0 target (and still reaches
/// L1 ones). Without this, a bug that reported every L0 target as unreachable
/// would pass the test above.
///
/// ADR-0024 update: the L0 target is a sub-agent (`engineer` role) for the
/// same isolation reason as the test above — an assistant-role target would
/// be refused by kind, so this would no longer prove anything about tier.
#[tokio::test]
async fn subagents_route_shows_l0_target_to_l0_delegator() {
    let dir = tempfile::tempdir().unwrap();
    write_agent(
        dir.path(),
        "orchestrator-assistant",
        "assistant",
        Some("l0"),
        &["delegate_to_agent"],
        None,
    );
    write_agent(
        dir.path(),
        "orch",
        "engineer",
        Some("orchestration"),
        &[],
        None,
    );
    write_agent(dir.path(), "engineer", "engineer", None, &[], None);

    let resp = subagents_at(
        &[dir.path().to_path_buf()],
        "orchestrator-assistant",
        dir.path(),
    )
    .await;
    let body = body_json(resp).await;
    let ip = &body["in_product"];
    assert_eq!(ip["delegator_tier"], "l0");

    let targets = ip["targets"].as_array().unwrap();
    let orch = targets.iter().find(|t| t["name"] == "orch").unwrap();
    assert_eq!(
        orch["tier"], "l0",
        "the `orchestration` alias must resolve to l0 like `l0` does"
    );
    assert_eq!(orch["reachable"], true);
    assert_eq!(orch["reason"], serde_json::Value::Null);
    let eng = targets.iter().find(|t| t["name"] == "engineer").unwrap();
    assert_eq!(eng["reachable"], true, "L0 → L1 is not restricted");
}

/// An unrecognized/blank `tier` string must fail closed to L1 on BOTH sides of
/// the comparison — the same posture `AgentInfo::tier()` guarantees, asserted
/// through this route so a future refactor of the wire label cannot quietly
/// promote a typo to L0.
#[tokio::test]
async fn subagents_route_tier_fails_closed_for_an_unrecognized_value() {
    let dir = tempfile::tempdir().unwrap();
    write_agent(
        dir.path(),
        "assistant",
        "assistant",
        Some("L0-ish"),
        &["delegate_to_agent"],
        None,
    );
    // ADR-0024: sub-agent-kind L0 target, so the tier fail-closed posture is
    // what this test observes rather than the kind predicate.
    write_agent(dir.path(), "orch", "engineer", Some("l0"), &[], None);

    let resp = subagents_at(&[dir.path().to_path_buf()], "assistant", dir.path()).await;
    let body = body_json(resp).await;
    let ip = &body["in_product"];
    assert_eq!(
        ip["delegator_tier"], "l1",
        "a typo'd tier must never be read as an L0 grant"
    );
    let targets = ip["targets"].as_array().unwrap();
    let orch = targets.iter().find(|t| t["name"] == "orch").unwrap();
    assert_eq!(
        orch["reachable"], false,
        "…and therefore the L0 target stays out of reach: {orch:?}"
    );
}

/// #4029's deny-by-default requirement: no `[subagents]` section at all.
#[tokio::test]
async fn subagents_route_cross_product_denies_everything_without_the_section() {
    let dir = tempfile::tempdir().unwrap();
    write_agent(dir.path(), "bare", "assistant", None, &[], None);

    let resp = subagents_at(&[dir.path().to_path_buf()], "bare", dir.path()).await;
    let body = body_json(resp).await;
    let cp = &body["cross_product"];
    assert_eq!(cp["declares_allowed"], false);
    let targets = cp["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 2, "the whole floor is listed: {targets:?}");
    for t in targets {
        assert_eq!(t["granted"], false, "{t:?}");
        assert!(
            t["reason"].as_str().unwrap().contains("fail-closed"),
            "{t:?}"
        );
    }
    // An EMPTY declared list is the same posture, and must not read as a grant
    // either.
    write_agent(dir.path(), "empty", "assistant", None, &[], Some(&[]));
    let resp = subagents_at(&[dir.path().to_path_buf()], "empty", dir.path()).await;
    let body = body_json(resp).await;
    assert_eq!(body["cross_product"]["declares_allowed"], true);
    assert!(
        body["cross_product"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["granted"] == false),
        "an empty [subagents].allowed grants nothing: {body:?}"
    );
}

/// The positive cross-product case, resolved through the bridge's OWN allow-set
/// (OQ-7) — and a permissive declaration that names a coding target, which the
/// floor must still refuse.
#[tokio::test]
async fn subagents_route_cross_product_grants_a_declared_floor_target() {
    let dir = tempfile::tempdir().unwrap();
    write_agent(
        dir.path(),
        "researchy",
        "assistant",
        None,
        &["dispatch_task"],
        Some(&["research", "engineer"]),
    );

    let resp = subagents_at(&[dir.path().to_path_buf()], "researchy", dir.path()).await;
    let body = body_json(resp).await;
    let cp = &body["cross_product"];
    assert_eq!(cp["declares_allowed"], true);
    assert_eq!(cp["tool_granted"], true, "`dispatch_task` is allow-listed");

    let targets = cp["targets"].as_array().unwrap();
    let research = targets.iter().find(|t| t["name"] == "research").unwrap();
    assert_eq!(research["granted"], true);
    let ticketing = targets.iter().find(|t| t["name"] == "ticketing").unwrap();
    assert_eq!(
        ticketing["granted"], false,
        "declaring one floor target must not grant the other"
    );

    // The coding target never becomes a card and IS reported as rejected — the
    // one way a hand-written `[subagents].allowed` silently does nothing.
    assert!(targets.iter().all(|t| t["name"] != "engineer"));
    let rejected = cp["rejected"].as_array().unwrap();
    assert_eq!(rejected.len(), 1, "{rejected:?}");
    assert_eq!(rejected[0]["name"], "engineer");
}

/// Registered ≠ granted: a worker-role agent never gets `delegate_to_agent`
/// registered at all (`build_registry_for_agent` routes only
/// `role == "assistant"` into the tier registry), so the pane must say so
/// instead of listing targets it could never call.
#[tokio::test]
async fn subagents_route_reports_tool_not_registered_for_a_worker_role() {
    let dir = tempfile::tempdir().unwrap();
    standard_roster(dir.path());

    let resp = subagents_at(&[dir.path().to_path_buf()], "engineer", dir.path()).await;
    let body = body_json(resp).await;
    let ip = &body["in_product"];
    assert_eq!(
        ip["tool_registered"], false,
        "an engineer-role agent never registers delegate_to_agent"
    );
    assert_eq!(ip["tool_granted"], false, "and declares no allow-list");
}

/// An agent declaring `[tools].allow` WITHOUT `delegate_to_agent` is registered
/// but ungranted — the two flags must move independently, or the pane collapses
/// "the tool exists" into "this agent may call it".
#[tokio::test]
async fn subagents_route_separates_registered_from_granted() {
    let dir = tempfile::tempdir().unwrap();
    write_agent(dir.path(), "quiet", "assistant", None, &["git_log"], None);
    write_agent(dir.path(), "engineer", "engineer", None, &[], None);

    let resp = subagents_at(&[dir.path().to_path_buf()], "quiet", dir.path()).await;
    let body = body_json(resp).await;
    let ip = &body["in_product"];
    assert_eq!(ip["tool_registered"], true);
    assert_eq!(
        ip["tool_granted"], false,
        "`delegate_to_agent` is not in this agent's allow-list: {ip:?}"
    );
}

/// A catalog entry whose config does not resolve is what the gate REFUSES
/// (`by_name_in` → `Err` ⇒ rejected), so it must be reported as unresolved
/// rather than dropped — a vanished target looks identical to one that was
/// never there.
#[tokio::test]
async fn subagents_route_reports_unresolvable_target_rather_than_dropping_it() {
    let dir = tempfile::tempdir().unwrap();
    write_agent(
        dir.path(),
        "assistant",
        "assistant",
        None,
        &["delegate_to_agent"],
        None,
    );
    // Parses as TOML (so it enters the catalog) but has no resolvable
    // `[llm]`/`[system_prompt]`, so full resolution fails.
    std::fs::write(
        dir.path().join("halfbaked.toml"),
        "[agent]\nname = \"halfbaked\"\nrole = \"engineer\"\n",
    )
    .unwrap();

    let resp = subagents_at(&[dir.path().to_path_buf()], "assistant", dir.path()).await;
    let body = body_json(resp).await;
    let ip = &body["in_product"];
    let unresolved = ip["unresolved"].as_array().unwrap();
    assert_eq!(unresolved.len(), 1, "{ip:?}");
    assert_eq!(unresolved[0]["name"], "halfbaked");
    assert!(
        unresolved[0]["reason"]
            .as_str()
            .unwrap()
            .contains("did not resolve")
    );
    assert!(
        ip["targets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["name"] != "halfbaked"),
        "an unresolvable agent is never a reachable target"
    );
}

/// This route sees `extends` — unlike `/skills`, `/knowledge` and
/// `/permissions`, whose partial parse cannot. It matters: the gate resolves
/// through `by_name_in`, so a `[subagents]` grant inherited from a base IS
/// enforced and must therefore be shown.
#[tokio::test]
async fn subagents_route_resolves_a_subagents_grant_inherited_via_extends() {
    let dir = tempfile::tempdir().unwrap();
    write_agent(
        dir.path(),
        "base-assistant",
        "assistant",
        None,
        &["dispatch_task"],
        Some(&["research"]),
    );
    std::fs::write(
        dir.path().join("child.toml"),
        r#"
[agent]
name = "child"
role = "assistant"
model = "anthropic/claude-sonnet-4-6"
description = "test fixture"
extends = "base-assistant"

[llm]
temperature = 0.2
max_tokens = 1024

[system_prompt]
content = "test"
"#,
    )
    .unwrap();

    let resp = subagents_at(&[dir.path().to_path_buf()], "child", dir.path()).await;
    let body = body_json(resp).await;
    let cp = &body["cross_product"];
    let research = cp["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "research")
        .unwrap()
        .clone();
    assert_eq!(
        research["granted"], true,
        "an inherited [subagents].allowed grant is enforced, so it must be shown: {body:?}"
    );
}

#[tokio::test]
async fn subagents_route_unknown_agent_404() {
    let dir = tempfile::tempdir().unwrap();
    let resp = subagents_at(&[dir.path().to_path_buf()], "nobody", dir.path()).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn subagents_route_rejects_traversal_name() {
    let dir = tempfile::tempdir().unwrap();
    let resp = subagents_at(&[dir.path().to_path_buf()], "../etc", dir.path()).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// A hand-edit typo must not 500 the panel — and must not be read as a grant
/// either. `resolved: false` with empty target lists is the fail-closed answer.
#[tokio::test]
async fn subagents_route_degrades_on_malformed_toml() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("broken.toml"), "not = = toml").unwrap();

    let resp = subagents_at(&[dir.path().to_path_buf()], "broken", dir.path()).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["resolved"], false);
    assert!(body["config_error"].is_string());
    assert!(body["in_product"]["targets"].as_array().unwrap().is_empty());
    assert_eq!(body["in_product"]["tool_registered"], false);
    assert!(
        body["cross_product"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["granted"] == false),
        "an unparseable config grants nothing: {body:?}"
    );
}

/// Proves the route is wired into `build_router` (not just that the core
/// function works).
#[tokio::test]
async fn subagents_route_is_wired_into_router() {
    let app: Router = build_router(AppState::default());
    let req = Request::builder()
        .uri("/api/agents/definitely-not-an-agent-4029/subagents")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_json(resp).await;
    assert_eq!(
        body["error"], "unknown agent",
        "a 404 from the handler, not from an unrouted path"
    );
}
