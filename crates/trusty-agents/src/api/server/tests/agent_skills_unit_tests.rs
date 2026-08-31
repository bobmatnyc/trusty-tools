//! Unit tests for `api::server::agent_skills` — the private helpers behind
//! `GET /api/agents/:name/skills`.
//!
//! Why this file exists at all: these are `#[cfg(test)]` tests of PRIVATE
//! functions, so they must stay a child module of `agent_skills` (a sibling
//! module in `tests/` cannot reach `granted_skill_ids`, `render_skill` or
//! `dead_scope_cards`). They live here rather than inline because the inline
//! module put `agent_skills.rs` within a few lines of the 500-SLOC production
//! cap, and the split is purely mechanical — `#[path]`-included from
//! `agent_skills.rs`, so `use super::*` still resolves to that module and no
//! visibility changes were needed. DOC-57 §5.5/S-8's point that a file split
//! carries no semantics applies here too: nothing about the module boundary
//! moved.
//! What: the former `mod unit_tests` body, verbatim.
//! Test: this file.

use super::*;

#[test]
fn parse_capability_sections_reads_both() {
    let raw = "[agent]\nname = \"izzie\"\n\n[tools]\nallow = [\"git_*\"]\n\n[skills]\nallow = [\"mta-train-time\"]\n";
    let (tools, skills, err) = parse_capability_sections(raw);
    assert!(err.is_none());
    assert_eq!(tools.allow.unwrap(), vec!["git_*".to_string()]);
    assert_eq!(skills.allow.unwrap(), vec!["mta-train-time".to_string()]);
}

#[test]
fn parse_capability_sections_tolerates_package_agent_toml() {
    // A package `agent.toml` has no `[system_prompt]`; parsing must not
    // require one (see this fn's doc comment).
    let (tools, skills, err) = parse_capability_sections("[agent]\nname = \"x\"\n");
    assert!(err.is_none());
    assert!(tools.allow.is_none());
    assert!(skills.allow.is_none());
}

#[test]
fn parse_capability_sections_reports_bad_toml() {
    let (_, _, err) = parse_capability_sections("this is not = = toml");
    assert!(err.is_some());
}

#[test]
fn granted_ids_are_empty_when_nothing_is_declared() {
    // Deny-on-absent: no `[tools].allow` and no `[skills].allow` grants
    // nothing, which must not be confused with "unrestricted".
    let catalog = SkillCatalog::builtin();
    assert!(granted_skill_ids(&catalog, None, None).is_empty());
}

#[test]
fn granted_ids_track_the_dispatch_gate() {
    let catalog = SkillCatalog::builtin();
    let patterns = vec!["get_train_schedule".to_string(), "git_*".to_string()];
    let granted = granted_skill_ids(&catalog, Some(&patterns), None);
    assert!(granted.contains("mta-train-time"));
    assert!(
        granted.contains("git-status"),
        "trailing-* glob is honoured"
    );
    assert!(!granted.contains("gmail-search"));
}

#[test]
fn granted_ids_include_tool_less_skills_named_by_id() {
    let catalog = SkillCatalog::builtin();
    let allow = vec!["handoff-protocol".to_string()];
    let granted = granted_skill_ids(&catalog, Some(&[]), Some(&allow));
    assert!(granted.contains("handoff-protocol"));
}

/// Why: `granted_skill_ids` is the only advisory pane whose OUTPUT changes with
/// #4520/#4054. It used a bare `match_any_glob`, so `[tools].allow = ["*"]`
/// marked the L0 shell skill and the exfiltration-capable Google skills granted
/// while the dispatch gate denied them. This test fails on `b16a56a07`.
/// What: under a `*` allow, no builtin skill wrapping `l0_shell_exec` or one of
/// the seven exfil-capable tools is granted, and an ordinary tool-wrapping skill
/// still is.
/// Test: this test.
#[test]
fn granted_ids_exclude_l0_and_exfil_skills_under_a_wildcard_allow() {
    // Literal names rather than `is_exfil_capable_tool`: asserting against the
    // production predicate would still pass if the set were emptied.
    const DENIED_TOOLS: &[&str] = &[
        "l0_shell_exec",
        "compose_email",
        "manage_file_permissions",
        "manage_gmail_settings",
        "manage_gmail_filters",
        "modify_gmail_messages",
        "manage_events",
        "manage_drive_file",
    ];

    let catalog = SkillCatalog::builtin();
    let wildcard = vec!["*".to_string()];
    let granted = granted_skill_ids(&catalog, Some(&wildcard), None);

    let mut covered = std::collections::BTreeSet::new();
    for manifest in catalog.manifests() {
        let Some(tool) = manifest.tool() else {
            continue;
        };
        if DENIED_TOOLS.contains(&tool) {
            covered.insert(tool);
            assert!(
                !granted.contains(&manifest.id),
                "`{}` wraps `{tool}`, which the dispatch gate denies under `*`",
                manifest.id
            );
        }
    }
    assert_eq!(
        covered,
        DENIED_TOOLS.iter().copied().collect(),
        "every denied tool needs a builtin skill wrapping it, or the loop above proves nothing"
    );

    assert!(
        granted.contains("mta-train-time"),
        "an ordinary tool-wrapping skill is still granted under `*`"
    );
}

#[test]
fn unmatched_patterns_flags_a_pattern_no_skill_wraps() {
    let catalog = SkillCatalog::builtin();
    let patterns = vec!["granola_*".to_string(), "get_weather".to_string()];
    let flagged = unmatched_patterns(&catalog, Some(&patterns));
    assert_eq!(flagged.len(), 1);
    assert_eq!(flagged[0]["pattern"], "granola_*");
}

#[test]
fn unmatched_patterns_excludes_exact_names_that_get_a_derived_card() {
    // One grant, one report. An exact name the catalog does not know gets a
    // derived card; listing it as "unmatched" too would make one grant look
    // like a capability AND a problem simultaneously.
    let catalog = SkillCatalog::builtin();
    let patterns = vec!["granola_list_meetings".to_string()];
    assert!(unmatched_patterns(&catalog, Some(&patterns)).is_empty());
    assert_eq!(derived_skills(&catalog, Some(&patterns)).len(), 1);
}

#[test]
fn derived_skills_wrap_an_exactly_named_unknown_tool() {
    let catalog = SkillCatalog::builtin();
    let patterns = vec![
        "granola_list_meetings".to_string(),
        "get_weather".to_string(),
    ];
    let derived = derived_skills(&catalog, Some(&patterns));
    assert_eq!(derived.len(), 1, "a known tool must not be derived twice");
    assert_eq!(derived[0].name, "Granola List Meetings");
    assert_eq!(derived[0].tools, vec!["granola_list_meetings".to_string()]);
    assert!(
        derived[0].description.is_empty(),
        "a derived skill invents no prose"
    );
}

#[test]
fn derived_skills_ignore_globs() {
    // A glob names no single tool, so there is nothing to wrap 1:1.
    let catalog = SkillCatalog::builtin();
    let patterns = vec!["granola_*".to_string()];
    assert!(derived_skills(&catalog, Some(&patterns)).is_empty());
}

#[test]
fn derived_skills_are_empty_when_nothing_is_declared() {
    assert!(derived_skills(&SkillCatalog::builtin(), None).is_empty());
}

#[test]
fn skill_card_reports_env_credential_state() {
    let catalog = SkillCatalog::builtin();
    // OAuth-backed: `configured` must stay null — unknown, never asserted.
    let gmail = catalog.skill_for_tool("search_gmail_messages").unwrap();
    let card = render_skill(gmail, true, None);
    assert_eq!(card["provider"]["configured"], Value::Null);
    assert_eq!(card["provider"]["provider"], "Google Workspace");
    assert_eq!(card["granted"], true);

    // Env-backed: `configured` is a real boolean derived from the process
    // environment. Its VALUE depends on the machine, so assert only that a
    // boolean — not null — is produced.
    let mta = catalog.skill_for_tool("get_train_schedule").unwrap();
    let card = render_skill(mta, false, None);
    assert!(card["provider"]["configured"].is_boolean());
    assert_eq!(card["provider"]["env_var"], "MTA_API_KEY");
}

/// #3987: the base assistant's `google.read` must reach the panel as a
/// dead grant, with actionable alternatives.
///
/// NOTE: #3987's option B has removed `google.read` from the shipped
/// base; this test asserts on a LITERAL pattern, not on the shipped
/// config, so it survived that change — a USER can still hand-write
/// `google.read` in their own agent, and the panel must still say so.
/// The shipped-config assertion lives in
/// `ctrl::pm_task::dispatch::persona_tests`.
#[test]
fn dead_scope_cards_flags_the_google_read_pattern() {
    let scopes = vec![
        "memory.read".to_string(),
        "search.read".to_string(),
        "google.read".to_string(),
    ];
    let cards = dead_scope_cards(Some(&scopes));
    assert_eq!(cards.len(), 1, "{cards:?}");
    assert_eq!(cards[0]["pattern"], "google.read");
    assert!(
        cards[0]["nearest"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "google.gmail.write"),
        "the card must name a working alternative: {cards:?}"
    );
}

/// The shape option B will declare must produce a clean panel.
#[test]
fn dead_scope_cards_ignores_live_family_patterns() {
    let scopes = vec![
        "google.gmail.*".to_string(),
        "google.calendar.*".to_string(),
        "google.accounts.*".to_string(),
    ];
    assert!(dead_scope_cards(Some(&scopes)).is_empty());
}

#[test]
fn dead_scope_cards_are_empty_when_no_scopes_are_declared() {
    assert!(dead_scope_cards(None).is_empty());
}

#[test]
fn granted_ids_do_not_grant_a_function_skill_by_id_alone() {
    // #4022: a bundle is tool-less, so the tool-less clause would otherwise
    // grant it just for being named — asserting a grant nobody verified.
    // Its state comes from its members, computed by `function_groups`.
    let catalog = SkillCatalog::builtin();
    // Retargeted from `ticketing` to `google-workspace` by ADR-0024 decision 4,
    // which removed the ticketing row from the catalog entirely.
    let allow = vec!["google-workspace".to_string()];
    let granted = granted_skill_ids(&catalog, Some(&[]), Some(&allow));
    assert!(!granted.contains("google-workspace"));
}

#[test]
fn function_group_never_reports_all_when_a_member_is_missing() {
    // Tri-state honesty: nine of ten is `"some"`, never `"all"`. A member
    // that fails to resolve can never enter `granted_ids`, so this is also
    // the shape an unresolvable member produces.
    let catalog = SkillCatalog::builtin();
    // Retargeted from `ticketing` to `google-workspace` by ADR-0024 decision 4.
    let bundle = catalog
        .get("google-workspace")
        .expect("google-workspace bundle");
    let mut granted: std::collections::BTreeSet<String> = bundle.members.iter().cloned().collect();
    let dropped = bundle.members.last().unwrap().clone();
    granted.remove(&dropped);

    let groups = function_groups(&catalog, &granted);
    let g = groups.iter().find(|g| g.id == "google-workspace").unwrap();
    // Counted from the live catalog: #4024 widened the bundle, and the property
    // under test is "one short is `some`", not any particular membership size.
    assert_eq!(g.members.len(), bundle.members.len());
    assert_eq!(g.granted_members.len(), bundle.members.len() - 1);
    assert_eq!(g.state(), "some");
    assert!(!g.granted_members.contains(&dropped));

    // And the full set is `"all"` — the tri-state is not stuck.
    let full: std::collections::BTreeSet<String> = bundle.members.iter().cloned().collect();
    let groups = function_groups(&catalog, &full);
    assert_eq!(
        groups
            .iter()
            .find(|g| g.id == "google-workspace")
            .unwrap()
            .state(),
        "all"
    );
}

#[test]
fn skill_card_carries_one_tool_and_a_human_name() {
    let catalog = SkillCatalog::builtin();
    let mta = catalog.skill_for_tool("get_train_schedule").unwrap();
    let card = render_skill(mta, true, None);
    assert_eq!(card["name"], "MTA Train Time");
    assert_eq!(card["tools"], json!(["get_train_schedule"]));
    assert_eq!(card["origin"]["kind"], "builtin");
}

#[test]
fn group_providers_list_every_distinct_member_requirement() {
    // #4024: the rollup keeps DISTINCT requirements rather than collapsing to a
    // verdict. A synthetic two-provider bundle is the case the real catalog does
    // not currently exercise, and it is exactly the case a silent collapse would
    // hide — so it is pinned here rather than left to a future divergence.
    let catalog = SkillCatalog::builtin();

    // Real bundle, one shared requirement.
    let gw = distinct_member_providers(
        &catalog,
        &catalog
            .get("google-workspace")
            .expect("bundle")
            .members
            .clone(),
    );
    assert_eq!(gw.len(), 1);
    assert_eq!(gw[0].provider, "Google Workspace");
    assert!(gw[0].env_var.is_none(), "OAuth is not env-backed");

    // Divergent members: two providers in, two out, in member order.
    let mixed = vec![
        "gmail-search".to_string(),
        "mta-train-time".to_string(),
        "gmail-read".to_string(), // repeat of the first requirement: deduped
        "web-search".to_string(),
    ];
    let providers = distinct_member_providers(&catalog, &mixed);
    let names: Vec<&str> = providers.iter().map(|p| p.provider).collect();
    assert_eq!(names, vec!["Google Workspace", "MTA", "Brave Search"]);
}

#[test]
fn group_providers_are_empty_when_no_member_needs_a_credential() {
    // Empty must read as "no member states a requirement" — the pane renders
    // nothing rather than an implied green. An unresolvable member likewise
    // contributes nothing (it cannot be granted either).
    let catalog = SkillCatalog::builtin();
    let members = vec![
        "ticket-create".to_string(),
        "git-branch-list".to_string(),
        "no-such-skill".to_string(),
    ];
    assert!(distinct_member_providers(&catalog, &members).is_empty());
}
