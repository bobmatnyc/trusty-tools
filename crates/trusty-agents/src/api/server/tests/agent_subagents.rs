//! `GET /api/agents/:name/subagents` handler tests (#4029, epic #4021 OQ-5).
//!
//! Why: this route's whole reason to exist is that it must never advertise a
//! delegation target the enforcement layer would refuse. Three properties carry
//! that weight and each is pinned here rather than left to the sibling
//! mechanisms' own tests:
//!
//! 1. **The tier interaction (#4169, epic #4167).** An L1 delegator must not be
//!    shown an L0-orchestration target as reachable, and an L0 delegator must
//!    be. Under ADR-0024 decision 3 the assistant kind DERIVES L0 (no persona
//!    declares `tier` on disk, and none needs to), so the L1 side of this
//!    property now needs a fixture that explicitly narrows an assistant with
//!    `tier = "l1"` — see `subagents_route_hides_l0_target_from_l1_delegator`.
//!    That is deliberate: without it, predicate 3 becomes an unexercised
//!    branch, which is the state ADR-0024's conformance checklist forbids.
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

/// Write an assistant-kind agent TOML carrying an explicit
/// `[subagents].delegate_allowed` whitelist (ADR-0024 decision 4).
///
/// Kept separate from [`write_agent`] for the same reason `write_hidden_agent`
/// is: only the decision-4 tests care about the whitelist, and threading an
/// eighth parameter through fifteen call sites would make every existing
/// fixture read as if reachability configuration were part of what it tests.
/// An absent whitelist is exactly what `write_agent` already produces, and is
/// the fail-closed case `subagents_route_absent_whitelist_makes_every_target_unreachable`
/// pins.
fn write_assistant_with_whitelist(
    dir: &std::path::Path,
    name: &str,
    tier: Option<&str>,
    tools_allow: &[&str],
    delegate_allowed: &[&str],
) {
    write_agent(dir, name, "assistant", tier, tools_allow, None);
    let path = dir.join(format!("{name}.toml"));
    let existing = std::fs::read_to_string(&path).unwrap();
    let joined = delegate_allowed
        .iter()
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        &path,
        format!("{existing}\n[subagents]\ndelegate_allowed = [{joined}]\n"),
    )
    .unwrap();
}

/// Write an agent TOML carrying `hidden = true` in `[agent]` (#4235).
///
/// Kept separate from [`write_agent`] rather than adding a seventh parameter:
/// only the #4235 tests care about `hidden`, and threading `Option<bool>`
/// through the other eleven call sites would make every existing fixture read
/// as if visibility were part of what it is testing.
fn write_hidden_agent(dir: &std::path::Path, name: &str, role: &str) {
    std::fs::write(
        dir.join(format!("{name}.toml")),
        format!(
            r#"
[agent]
name = "{name}"
role = "{role}"
model = "anthropic/claude-sonnet-4-6"
description = "test fixture"
hidden = true

[llm]
temperature = 0.2
max_tokens = 1024

[system_prompt]
content = "test"
"#
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
    // ADR-0024 decision 4: the subject declares the SEEDED whitelist the
    // bundled personas now ship, so this roster exercises the shipped posture
    // rather than the fail-closed one (which has its own test below).
    write_assistant_with_whitelist(
        dir,
        "assistant",
        None,
        &["delegate_to_agent", "git_log"],
        &["research-agent", "ticketing-agent"],
    );
    write_agent(dir, "engineer", "engineer", None, &[], None);
    write_agent(dir, "docs-agent", "documentation", None, &[], None);
    write_agent(dir, "research-agent", "researcher", None, &[], None);
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
    // ADR-0024 decision 3: the pane reports the VIEWING agent's own tier, and
    // an assistant is L0 — derived from its kind, with no `tier = "l0"` line
    // in the fixture. This is the label the owner was looking at when he
    // reported the pane calling an assistant "tier l1".
    assert_eq!(
        ip["delegator_tier"], "l0",
        "an assistant-kind agent is tier L0 (ADR-0024 decision 3)"
    );

    let targets = ip["targets"].as_array().unwrap();
    let named: Vec<&str> = targets
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(named.contains(&"engineer"), "{named:?}");
    // ADR-0024 decision 4: role eligibility still decides which agents get a
    // CARD; the whitelist decides which cards are reachable. `engineer` is
    // role-eligible, so it is reported — and refused, with the reachable-set
    // reason, because it is not on the server-owned floor and could not be put
    // there by any config.
    let eng = targets
        .iter()
        .find(|t| t["name"] == "engineer")
        .expect("a role-eligible coding agent is still reported, with a reason");
    assert_eq!(
        eng["reachable"], false,
        "no coding agent is reachable from an assistant (ADR-0024 decision 4): {eng:?}"
    );
    assert!(
        eng["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("reachable sub-agent set"),
        "the pane must explain the whitelist rule: {eng:?}"
    );
    // The two floor members this agent whitelisted ARE reachable — without
    // this, a bug that denied everything would pass the assertions above.
    for reachable_name in ["research-agent", "ticketing-agent"] {
        let t = targets
            .iter()
            .find(|t| t["name"] == reachable_name)
            .unwrap_or_else(|| panic!("{reachable_name} must be a target card: {named:?}"));
        assert_eq!(
            t["reachable"], true,
            "a whitelisted floor member must be reachable: {t:?}"
        );
        assert_eq!(t["reason"], serde_json::Value::Null);
    }
    assert_eq!(
        ip["whitelist_enforced"], true,
        "the whitelist gate applies to an assistant-kind viewer"
    );
    assert_eq!(ip["declares_whitelist"], true);
    let reachable_floor: Vec<&str> = ip["reachable_floor"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    assert_eq!(reachable_floor, vec!["research-agent", "ticketing-agent"]);
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
        izzie["tier"], "l0",
        "a peer assistant renders as tier l0 too (ADR-0024 decision 3): {izzie:?}"
    );
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
    assert_eq!(
        docs["tier"], "l1",
        "a sub-agent stays tier l1 (ADR-0024 decision 3): {docs:?}"
    );
    // ADR-0024 decision 4 REVERSED this assertion (it read `true` before):
    // `documentation` remains the role that exists only in the in-product
    // allowlist, which is still what earns docs-agent a card — but the
    // reachable set is now the whitelist, and no whitelist can name docs-agent
    // because the floor does not.
    assert_eq!(
        docs["reachable"], false,
        "a role-eligible agent off the floor is not reachable: {docs:?}"
    );

    // The role gate's exclusions are COUNTED, never named (no roster dump), and
    // the subject itself never appears as its own target. `ticketing-agent` is
    // no longer among them: ADR-0024 decision 4 added its role to the
    // allowlist, because an agent on the ratified floor must at least be
    // role-eligible for the whitelist to be able to name it.
    assert!(
        !named.contains(&"pm"),
        "role-ineligible agents must not become target cards: {named:?}"
    );
    assert!(!named.contains(&"assistant"), "no self-delegation card");
    assert_eq!(ip["role_excluded_count"], 1, "pm only");

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

/// ADR-0024 decision 4 sub-answer (a) at the reporting surface: an assistant
/// that declares NO whitelist has NO reachable in-product target.
///
/// Why: the pane's whole reason to exist is that it must not advertise what the
/// gate refuses. `delegate_assistant_absent_whitelist_reaches_nothing` pins the
/// gate; this pins that the pane agrees with it. A pane that kept showing the
/// pre-decision-4 role-scan as reachable would be the exact drift this module's
/// honesty rules forbid.
/// What: every target card is `reachable: false` with the reachable-set reason,
/// `declares_whitelist` is false, and the floor is still reported so the pane
/// can tell the operator what a whitelist COULD name.
#[tokio::test]
async fn subagents_route_absent_whitelist_makes_every_target_unreachable() {
    let dir = tempfile::tempdir().unwrap();
    write_agent(
        dir.path(),
        "bare-assistant",
        "assistant",
        None,
        &["delegate_to_agent"],
        None,
    );
    write_agent(dir.path(), "research-agent", "researcher", None, &[], None);
    write_agent(dir.path(), "ticketing-agent", "ticketing", None, &[], None);
    write_agent(dir.path(), "engineer", "engineer", None, &[], None);

    let resp = subagents_at(&[dir.path().to_path_buf()], "bare-assistant", dir.path()).await;
    let body = body_json(resp).await;
    let ip = &body["in_product"];
    assert_eq!(ip["declares_whitelist"], false);
    assert_eq!(ip["whitelist_enforced"], true);
    let targets = ip["targets"].as_array().unwrap();
    assert!(!targets.is_empty(), "role-eligible cards are still drawn");
    for t in targets {
        assert_eq!(
            t["reachable"], false,
            "an absent whitelist reaches nothing (fail-closed): {t:?}"
        );
        assert!(
            t["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("reachable sub-agent set"),
            "{t:?}"
        );
    }
    let floor: Vec<&str> = ip["reachable_floor"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    assert_eq!(floor, vec!["research-agent", "ticketing-agent"]);
}

/// A whitelist that names an agent OFF the floor grants nothing extra — the
/// pane reports the same refusal the gate applies.
///
/// Why: decision 4's sub-answer (b) puts a floor on the write path, but a
/// hand-edited TOML bypasses it. The pane must not become the place an
/// operator "confirms" that a widened config took effect. Same
/// two-layers-not-one posture the cross-product half already reports through
/// `rejected`.
/// What: `[subagents].delegate_allowed = ["engineer", "research-agent"]` —
/// `engineer` stays unreachable, `research-agent` becomes reachable.
#[tokio::test]
async fn subagents_route_reports_a_non_whitelisted_target_as_unreachable() {
    let dir = tempfile::tempdir().unwrap();
    write_assistant_with_whitelist(
        dir.path(),
        "widened",
        None,
        &["delegate_to_agent"],
        &["engineer", "research-agent"],
    );
    write_agent(dir.path(), "engineer", "engineer", None, &[], None);
    write_agent(dir.path(), "research-agent", "researcher", None, &[], None);

    let resp = subagents_at(&[dir.path().to_path_buf()], "widened", dir.path()).await;
    let body = body_json(resp).await;
    let targets = body["in_product"]["targets"].as_array().unwrap();
    let eng = targets.iter().find(|t| t["name"] == "engineer").unwrap();
    assert_eq!(
        eng["reachable"], false,
        "a config naming a coding agent must not make it reachable: {eng:?}"
    );
    let research = targets
        .iter()
        .find(|t| t["name"] == "research-agent")
        .unwrap();
    assert_eq!(
        research["reachable"], true,
        "…and the legitimate entry in the same list still works: {research:?}"
    );
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
///
/// ADR-0024 decision 3 update: the DELEGATOR now has to be explicitly narrowed
/// to L1 (`tier = "l1"`), because an assistant otherwise derives L0 and the
/// tier comparison would be vacuously satisfied. That is not a contrivance —
/// an explicit narrowing is the only way an L1 delegate-capable agent exists
/// under the ratified model, and it is exactly the population this gate is
/// still defending. Keeping the test on that fixture is what stops predicate 3
/// from rotting into an unexercised branch (ADR-0024 conformance item 5).
#[tokio::test]
async fn subagents_route_hides_l0_target_from_l1_delegator() {
    let dir = tempfile::tempdir().unwrap();
    standard_roster(dir.path());
    // An L0-orchestration sub-agent — role-eligible (`engineer`) and NOT the
    // assistant kind, so the ONLY thing that can keep it out of reach is the
    // tier gate.
    write_agent(dir.path(), "orch", "engineer", Some("l0"), &[], None);
    write_assistant_with_whitelist(
        dir.path(),
        "narrowed-assistant",
        Some("l1"),
        &["delegate_to_agent", "git_log"],
        // ADR-0024 decision 4: seeded so the whitelist is not what refuses
        // anything here — the tier gate is the axis under test.
        &["research-agent", "ticketing-agent"],
    );

    let resp = subagents_at(
        &[dir.path().to_path_buf()],
        "narrowed-assistant",
        dir.path(),
    )
    .await;
    let body = body_json(resp).await;
    let ip = &body["in_product"];
    assert_eq!(
        ip["delegator_tier"], "l1",
        "an explicit `tier = \"l1\"` declaration overrides the derived kind"
    );

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
    // with a target the kind rule permits. Decision 4 moved it again, to a
    // target the WHITELIST also permits — `research-agent` — for the same
    // reason: the positive case has to survive every gate, not just this one.)
    let reachable_sub = targets
        .iter()
        .find(|t| t["name"] == "research-agent")
        .unwrap();
    assert_eq!(reachable_sub["tier"], "l1");
    assert_eq!(reachable_sub["reachable"], true);
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
    write_assistant_with_whitelist(
        dir.path(),
        "orchestrator-assistant",
        Some("l0"),
        &["delegate_to_agent"],
        &["research-agent", "ticketing-agent"],
    );
    // ADR-0024 decision 4: NAME and role/tier are independent axes (the
    // whitelist reads the name, the other gates read role and tier), so the
    // L0 fixture takes a floor NAME while keeping the `engineer` role and the
    // `orchestration` tier alias this test is actually about. Without a floor
    // name the whitelist would refuse it and this test would stop exercising
    // the tier comparison at all.
    write_agent(
        dir.path(),
        "research-agent",
        "engineer",
        Some("orchestration"),
        &[],
        None,
    );
    write_agent(dir.path(), "ticketing-agent", "engineer", None, &[], None);

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
    let orch = targets
        .iter()
        .find(|t| t["name"] == "research-agent")
        .unwrap();
    assert_eq!(
        orch["tier"], "l0",
        "the `orchestration` alias must resolve to l0 like `l0` does"
    );
    assert_eq!(orch["reachable"], true);
    assert_eq!(orch["reason"], serde_json::Value::Null);
    let l1_target = targets
        .iter()
        .find(|t| t["name"] == "ticketing-agent")
        .unwrap();
    assert_eq!(l1_target["reachable"], true, "L0 → L1 is not restricted");
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

/// #4353: the route reports the coding lane as its own labelled mechanism, with
/// resolutions produced by the real resolver.
///
/// Why: this is the end-to-end half of `coding_surface`'s unit tests. Those pin
/// the payload's shape; this pins that the ROUTE reads the agent's real
/// `[subagents] default_style` and hands it to the resolver — the one step a
/// unit test over a plain function cannot cover, and the one whose failure mode
/// (a config default that is parsed but never consulted) is silent: the pane
/// would render a perfectly plausible resolution attributed to the built-in.
/// It also pins that the coding target never leaks into the NON-coding
/// vocabulary, which is what keeps `NON_CODING_TARGETS` a closed #4126 floor.
#[tokio::test]
async fn subagents_route_reports_the_coding_lane() {
    let dir = tempfile::tempdir().unwrap();
    write_agent(
        dir.path(),
        "codey",
        "assistant",
        None,
        &["dispatch_task"],
        None,
    );
    // Appended rather than threaded through `write_agent`, for the same reason
    // `write_assistant_with_whitelist` appends: only this test cares about the
    // style default, and an extra parameter would make fifteen unrelated
    // fixtures read as if ceremony configuration were part of what they test.
    let path = dir.path().join("codey.toml");
    let existing = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        format!("{existing}\n[subagents]\ndefault_style = \"vibe\"\n"),
    )
    .unwrap();

    let resp = subagents_at(&[dir.path().to_path_buf()], "codey", dir.path()).await;
    let body = body_json(resp).await;
    let coding = &body["coding"];

    assert_eq!(coding["mechanism"], "coding");
    assert_eq!(coding["target"], "coding-pm");
    assert_eq!(
        coding["tool_granted"], true,
        "`dispatch_task` is allow-listed"
    );
    assert_eq!(
        coding["gated_by_allowed"], false,
        "the coding name resolves before the non-coding allow-set is consulted"
    );

    // The config default was READ, and the no-override row attributes the value
    // to it rather than to the built-in.
    assert_eq!(coding["config_default"], "vibe");
    let rows = coding["resolutions"].as_array().unwrap();
    let no_override = rows
        .iter()
        .find(|r| r["caller"].is_null())
        .expect("a no-override row");
    assert_eq!(no_override["resolution"]["source"], "config");
    assert_eq!(no_override["resolution"]["requested"], "vibe");
    // SM-9, end to end: the configured `vibe` runs `engineer` and says why.
    assert_eq!(no_override["resolution"]["effective"], "engineer");
    assert_eq!(
        no_override["resolution"]["escalations"],
        serde_json::json!(["tier-unimplemented"])
    );

    // The coding target stays out of the non-coding half entirely — no card,
    // and nothing rejected, because nothing declared it.
    let cp_targets = body["cross_product"]["targets"].as_array().unwrap();
    assert!(
        cp_targets.iter().all(|t| t["name"] != "coding-pm"),
        "the coding PM must never appear as a non-coding specialist: {cp_targets:?}"
    );
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
    // ADR-0024 decision 3, the one pane-reporting change beyond the labels: a
    // NON-assistant viewer stays L1, so an assistant target — now L0 — reads as
    // tier-blocked where it used to read as reachable. This is a display
    // artifact on a card the pane already stamps `tool_registered: false`
    // (an engineer never holds `delegate_to_agent` at all), and it is honest:
    // if a worker-role agent somehow held the tool, the tier gate WOULD refuse
    // that edge. Pinned so the next reader knows it is intended, not a bug.
    assert_eq!(
        ip["delegator_tier"], "l1",
        "a worker role derives L1: {ip:?}"
    );
    let targets = ip["targets"].as_array().unwrap();
    let izzie = targets
        .iter()
        .find(|t| t["name"] == "izzie")
        .expect("an assistant-role agent is still role-eligible and still reported");
    assert_eq!(izzie["tier"], "l0");
    assert_eq!(
        izzie["reachable"], false,
        "an L1 viewer is shown the L0 target as refused: {izzie:?}"
    );
    assert!(
        izzie["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("L0/L1"),
        "the tier gate, not the kind gate, is what refuses a non-assistant \
         source's edge: {izzie:?}"
    );
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

/// #4235, the reported bug: the pane offered `hidden = true` agents as
/// delegation targets, so `personal-assistant` (hidden since #3819 precisely
/// to remove it from listings) rendered a SECOND "Izzie" row beside `izzie`.
/// A hidden agent must be omitted from `targets` entirely — not shown with a
/// "hidden" reason, which would still draw the duplicate row AND name an id
/// the module doc's no-roster-dump rule keeps unnamed — while a non-hidden
/// agent of the same role is still offered.
#[tokio::test]
async fn subagents_route_omits_a_hidden_delegation_target() {
    let dir = tempfile::tempdir().unwrap();
    write_agent(
        dir.path(),
        "assistant-subject",
        "assistant",
        None,
        &["delegate_to_agent"],
        None,
    );
    // The visible control: same role as the hidden peer below, so the ONLY
    // difference between them is the `hidden` flag.
    write_agent(dir.path(), "izzie", "assistant", None, &[], None);
    write_hidden_agent(dir.path(), "personal-assistant", "assistant");
    // A hidden agent whose role IS delegable as a sub-agent — proves the
    // filter is not accidentally piggybacking on the peer-assistant kind rule.
    write_hidden_agent(dir.path(), "researcher", "researcher");
    write_agent(dir.path(), "engineer", "engineer", None, &[], None);

    let resp = subagents_at(&[dir.path().to_path_buf()], "assistant-subject", dir.path()).await;
    let body = body_json(resp).await;
    let ip = &body["in_product"];

    let named: Vec<&str> = ip["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        !named.contains(&"personal-assistant"),
        "a hidden agent must not be a delegation-target card: {named:?}"
    );
    assert!(
        !named.contains(&"researcher"),
        "a hidden agent must not be a delegation-target card: {named:?}"
    );
    // …and it is not leaked through the OTHER naming channel either.
    let unresolved: Vec<&str> = ip["unresolved"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(unresolved.is_empty(), "{unresolved:?}");

    // Not a blanket suppression: visible agents of both roles still appear,
    // so this is the `hidden` flag doing the work and nothing else.
    assert!(named.contains(&"izzie"), "{named:?}");
    assert!(named.contains(&"engineer"), "{named:?}");

    // Counted, never named — the `role_excluded_count` treatment.
    assert_eq!(
        ip["hidden_excluded_count"], 2,
        "personal-assistant + researcher: {ip:?}"
    );
}

/// The two suppression counters must stay DISJOINT: adding a hidden agent may
/// not move `role_excluded_count`, or the pane double-counts and its "N agents
/// are not shown" line stops being true.
#[tokio::test]
async fn subagents_route_hidden_filter_leaves_the_role_count_untouched() {
    let dir = tempfile::tempdir().unwrap();
    standard_roster(dir.path());

    // Baseline: the roster's ONE role-ineligible agent (pm) and nothing
    // hidden. ADR-0024 decision 4 moved `ticketing-agent` out of this bucket by
    // adding its role to the allowlist — see
    // `subagents_route_reports_both_mechanisms_for_an_assistant`.
    let resp = subagents_at(&[dir.path().to_path_buf()], "assistant", dir.path()).await;
    let ip = body_json(resp).await["in_product"].clone();
    assert_eq!(ip["role_excluded_count"], 1);
    assert_eq!(ip["hidden_excluded_count"], 0);

    // A hidden agent whose role is ALSO ineligible must land in exactly one
    // bucket — the hidden one, because `hidden` is checked first.
    write_hidden_agent(dir.path(), "ghost", "orchestrator");
    let resp = subagents_at(&[dir.path().to_path_buf()], "assistant", dir.path()).await;
    let ip = body_json(resp).await["in_product"].clone();
    assert_eq!(
        ip["role_excluded_count"], 1,
        "the role count keeps its pre-#4235 meaning: {ip:?}"
    );
    assert_eq!(ip["hidden_excluded_count"], 1, "{ip:?}");
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
