//! Unit tests for the assistant skill floor (#7881).
//!
//! Why: the floor is a security gate — every claim it makes ("config can only
//! narrow", "a coding skill is unreachable", "an unreadable role fails
//! closed") needs a test, or it is only a comment.
//! What: pure, no I/O except the bundled-persona scan, which reads the
//! in-repo `.trusty-agents/agents/` tree through `CARGO_MANIFEST_DIR`.
//! Test: this file.

use super::*;

/// The coding-skill names the owner's ruling excludes. Spelled out rather than
/// derived so a future addition to the floor cannot quietly re-admit one.
const CODING_SKILLS: &[&str] = &[
    "api-design-patterns",
    "api-documentation",
    "artifacts-builder",
    "code-production-process",
    "code-review-standards",
    "condition-based-waiting",
    "contract-driven-testing",
    "database-migration",
    "env-manager",
    "fixture-quality",
    "git-operations",
    "git-workflow",
    "json-data-handling",
    "model-context-builder",
    "python-compat",
    "python-packaging",
    "python-testing",
    "requesting-code-review",
    "root-cause-tracing",
    "rust",
    "rust-build-performance",
    "rust-idiomatic",
    "security-scanning",
    "software-patterns",
    "systematic-debugging",
    "test-driven-development",
    "test-quality-inspector",
    "testing-anti-patterns",
    "typescript-idiomatic",
    "web-performance-optimization",
    "webapp-testing",
    "writing-plans",
    "xlsx",
];

#[test]
fn floor_contains_no_coding_skill() {
    for coding in CODING_SKILLS {
        assert!(
            !ASSISTANT_REACHABLE_SKILLS.contains(coding),
            "coding skill '{coding}' must not be on the assistant floor"
        );
    }
}

#[test]
fn floor_names_are_normalized_and_unique() {
    let mut seen: Vec<&str> = Vec::new();
    for name in ASSISTANT_REACHABLE_SKILLS {
        assert_eq!(
            *name,
            name.trim().to_ascii_lowercase(),
            "floor entries must be trimmed and lowercase so the gate's own \
             normalization is a no-op on them"
        );
        assert!(!seen.contains(name), "duplicate floor entry '{name}'");
        seen.push(name);
    }
}

#[test]
fn absent_allow_list_yields_the_whole_floor() {
    let effective = reachable_skills(None);
    assert_eq!(effective.len(), ASSISTANT_REACHABLE_SKILLS.len());
    for name in ASSISTANT_REACHABLE_SKILLS {
        assert!(effective.iter().any(|s| s == name));
    }
}

#[test]
fn configured_list_narrows_the_floor() {
    let configured = vec!["tm-ticketing".to_string(), " TM-Workflow ".to_string()];
    assert_eq!(
        reachable_skills(Some(&configured)),
        vec!["tm-ticketing".to_string(), "tm-workflow".to_string()],
        "entries are trimmed and case-folded, and caller order is preserved"
    );
}

#[test]
fn configured_list_cannot_widen_the_floor() {
    let configured = vec![
        "tm-ticketing".to_string(),
        "test-driven-development".to_string(),
        "rust-idiomatic".to_string(),
    ];
    assert_eq!(
        reachable_skills(Some(&configured)),
        vec!["tm-ticketing".to_string()],
        "a config naming coding skills contributes nothing"
    );
}

#[test]
fn empty_configured_list_grants_nothing() {
    assert!(reachable_skills(Some(&[])).is_empty());
}

#[test]
fn duplicate_configured_entries_collapse() {
    let configured = vec!["tm-adr".to_string(), "TM-ADR".to_string()];
    assert_eq!(
        reachable_skills(Some(&configured)),
        vec!["tm-adr".to_string()]
    );
}

/// The #7881 regression: before the floor existed, an assistant could load ANY
/// skill the resolver could find, including every coding skill in the
/// operator's `~/.claude/skills/` library.
#[test]
fn assistant_cannot_reach_a_coding_skill() {
    for coding in CODING_SKILLS {
        assert!(
            !skill_is_reachable("assistant", None, coding),
            "an assistant must not reach coding skill '{coding}'"
        );
    }
}

#[test]
fn assistant_reaches_a_floor_skill_by_default() {
    assert!(skill_is_reachable("assistant", None, "tm-ticketing"));
    assert!(skill_is_reachable("assistant", None, "  TM-Ticketing "));
}

#[test]
fn assistant_config_narrows_what_it_reaches() {
    let configured = vec!["tm-ticketing".to_string()];
    assert!(skill_is_reachable(
        "assistant",
        Some(&configured),
        "tm-ticketing"
    ));
    assert!(
        !skill_is_reachable("assistant", Some(&configured), "tm-workflow"),
        "on the floor but not granted by this agent's own list"
    );
}

#[test]
fn non_assistant_roles_are_unaffected() {
    for role in ["engineer", "qa", "researcher", "documentation", "ops"] {
        assert!(
            skill_is_reachable(role, None, "test-driven-development"),
            "the floor binds the assistant kind only; '{role}' is outside it"
        );
    }
}

#[test]
fn blank_skill_name_is_refused() {
    assert!(!skill_is_reachable("assistant", None, "   "));
    assert!(!skill_is_reachable("assistant", None, ""));
}

#[test]
fn unknown_role_is_treated_as_assistant_kind() {
    assert!(
        role_is_assistant_kind_or_unknown(None),
        "an unreadable role must bind the floor, never escape it"
    );
    assert!(role_is_assistant_kind_or_unknown(Some("assistant")));
}

#[test]
fn a_declared_worker_role_is_not_bound() {
    assert!(!role_is_assistant_kind_or_unknown(Some("engineer")));
}

#[test]
fn narrowing_write_is_accepted() {
    let requested = vec!["TM-Ticketing".to_string(), "tm-workflow".to_string()];
    assert_eq!(
        narrow_skills_to_floor(&requested),
        Ok(vec!["tm-ticketing".to_string(), "tm-workflow".to_string()])
    );
}

#[test]
fn widening_write_is_refused() {
    let requested = vec![
        "tm-ticketing".to_string(),
        "test-driven-development".to_string(),
    ];
    assert_eq!(
        narrow_skills_to_floor(&requested),
        Err(vec!["test-driven-development".to_string()])
    );
}

#[test]
fn empty_write_is_accepted() {
    assert_eq!(narrow_skills_to_floor(&[]), Ok(Vec::new()));
}

/// The floor must not silently strip a skill a SHIPPED persona declares — that
/// would be the ADR-0024 decision-4 lesson (live instructions making calls that
/// fail) reintroduced for skills.
#[test]
fn bundled_assistant_personas_declare_only_reachable_skills() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".trusty-agents")
        .join("agents");
    let mut checked = 0usize;
    for entry in std::fs::read_dir(&root)
        .expect("bundled agents dir")
        .flatten()
    {
        let path = if entry.path().is_dir() {
            entry.path().join("agent.toml")
        } else {
            entry.path()
        };
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(doc) = raw.parse::<toml::Value>() else {
            continue;
        };
        let role = doc
            .get("agent")
            .and_then(|a| a.get("role"))
            .and_then(|v| v.as_str());
        if !role_is_assistant_kind_or_unknown(role) {
            continue;
        }
        let Some(skills) = doc
            .get("system_prompt")
            .and_then(|s| s.get("skills"))
            .and_then(|v| v.as_array())
        else {
            continue;
        };
        checked += 1;
        for skill in skills.iter().filter_map(|v| v.as_str()) {
            assert!(
                skill_is_reachable("assistant", None, skill),
                "{}: declares skill '{skill}', which is not on the floor — either \
                 add it or rewrite the persona",
                path.display()
            );
        }
    }
    assert!(
        checked >= 3,
        "expected the bundled assistant personas to be scanned, saw {checked}"
    );
}
