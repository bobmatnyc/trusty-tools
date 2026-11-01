//! Tests for the guard policy table (#10641 PR1).

use super::*;
use crate::core::architect_core::ARCHITECT_CORE;

/// The Architect core's hard limits: the first twenty members, in `core.md` order.
const HARD_LIMIT_COUNT: usize = 20;

#[test]
fn every_entry_has_a_source_and_a_unique_id_and_rule() {
    let mut ids = std::collections::HashSet::new();
    let mut rules = std::collections::HashSet::new();
    for (n, p) in POLICIES.iter().enumerate() {
        assert_eq!(p.id, format!("GP-{n:03}"), "ids run in table order: {p:?}");
        assert!(!p.source.trim().is_empty(), "{} has no source", p.id);
        assert!(!p.summary.trim().is_empty(), "{} has no summary", p.id);
        assert!(ids.insert(p.id), "duplicate id {}", p.id);
        assert!(rules.insert(p.rule), "duplicate rule {}", p.rule);
        // The doc table and the refusal tag both break on these.
        for text in [p.rule, p.summary, p.source] {
            assert!(
                !text.contains('|') && !text.contains('\n'),
                "{}: {text}",
                p.id
            );
        }
    }
}

#[test]
fn every_entry_binds_at_least_one_role() {
    for p in POLICIES {
        assert!(!p.applies.is_empty(), "{} binds no role", p.id);
    }
}

#[test]
fn no_entry_masks_output_yet() {
    // #10641: masking arrives in PR3; PR1 changes no verdict.
    assert!(POLICIES.iter().all(|p| !p.mask));
}

#[test]
fn architect_entries_cite_an_architect_core_hard_limit() {
    // Ruling D29 item 5: an entry binds the Architect only through a current
    // Architect core hard limit, and an entry that does not bind it cites none.
    let hard_limits: Vec<&str> = ARCHITECT_CORE[..HARD_LIMIT_COUNT]
        .iter()
        .map(|m| m.name)
        .collect();
    assert_eq!(
        ARCHITECT_CORE[HARD_LIMIT_COUNT - 1].name,
        "the guard stays on"
    );
    for p in POLICIES {
        match (p.binds(Role::Architect), p.architect_limit) {
            (true, Some(limit)) => assert!(
                hard_limits.contains(&limit),
                "{} cites {limit:?}, not a core hard limit",
                p.id
            ),
            (true, None) => panic!("{} binds the Architect but cites no hard limit", p.id),
            (false, Some(limit)) => panic!("{} cites {limit:?} but binds no Architect", p.id),
            (false, None) => {}
        }
    }
}

#[test]
fn find_answers_every_listed_rule_and_nothing_else() {
    for p in POLICIES {
        assert_eq!(find(p.rule), Some(*p));
    }
    assert_eq!(find("no-such-rule"), None);
    assert_eq!(find(""), None);
}

#[test]
fn a_policy_lookup_miss_denies() {
    // #10641: a miss answers an entry that is itself a refusal, never "absent".
    let miss = for_rule("no-such-rule");
    assert_eq!(miss, &POLICY_LOOKUP_MISS);
    assert_eq!(miss.id, "GP-000");
    assert!(miss.absolute, "the miss holds under every bypass");
    assert_eq!(miss.applies, &[Role::Pm, Role::Architect, Role::Agent]);
    assert_eq!(for_rule("destructive-delete"), &DESTRUCTIVE_DELETE);
}

#[test]
fn the_tag_names_id_rule_and_source() {
    assert_eq!(
        DESTRUCTIVE_DELETE.tag(),
        "[policy GP-005 destructive-delete; source: #4031]"
    );
    for p in POLICIES {
        let tag = p.tag();
        assert!(tag.contains(p.id) && tag.contains(p.rule) && tag.contains(p.source));
    }
}

#[test]
fn policy_doc_matches_table() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/reference/guard-policies.md");
    let rendered = render_markdown();
    if std::env::var_os("TM_UPDATE_GUARD_POLICY_DOC").is_some() {
        std::fs::write(&path, &rendered).expect("write the policy doc");
    }
    let on_disk = std::fs::read_to_string(&path).expect("read docs/reference/guard-policies.md");
    assert_eq!(
        on_disk, rendered,
        "docs/reference/guard-policies.md is stale; rerun with TM_UPDATE_GUARD_POLICY_DOC=1"
    );
}
