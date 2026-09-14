//! Unit tests for the assistant skill floor (#7881).
//!
//! Why: the floor is a security gate — every claim it makes ("config can only
//! narrow", "a coding skill is unreachable", "an unreadable role fails
//! closed") needs a test, or it is only a comment.
//! What: mostly pure. Three tests read the repo: the bundled-persona scan and
//! the two catalog enumerations behind
//! `every_bundled_skill_is_classified`, all rooted at `CARGO_MANIFEST_DIR`.
//! Test: this file.

use super::*;

/// The coding-skill names the owner's ruling excludes. Spelled out rather than
/// derived so a future addition to the floor cannot quietly re-admit one, and
/// so `every_bundled_skill_is_classified` has a list to check a new catalog
/// entry against.
const CODING_SKILLS: &[&str] = &[
    // trusty-mpm bundled
    "api-design-patterns",
    "artifacts-builder",
    "code-production-process",
    "code-review-standards",
    "condition-based-waiting",
    "contract-driven-testing",
    "database-migration",
    "env-manager",
    "model-context-builder",
    "requesting-code-review",
    "root-cause-tracing",
    "rust-build-performance",
    "security-scanning",
    "software-patterns",
    "systematic-debugging",
    "test-driven-development",
    "test-quality-inspector",
    "testing-anti-patterns",
    "web-performance-optimization",
    "webapp-testing",
    // trusty-agents bundled — top level
    "fixture-quality",
    "git-operations",
    "python-compat",
    "python-packaging",
    "python-testing",
    // trusty-agents bundled — `frameworks/` and `languages/`
    "fastapi",
    "pytest",
    "go-idiomatic",
    "java-idiomatic",
    "python",
    "python-idiomatic",
    "react-idiomatic",
    "rust",
    "rust-idiomatic",
    "typescript-idiomatic",
    // trusty-agents bundled — `personas/`, coding-role voices
    "engineer",
    "hacker",
    "novice",
    "vibe-coder",
    // trusty-agents bundled — `workflow/`. `delegation` and `wave-planning`
    // name internal orchestration concepts an assistant must never recite,
    // which is already why `build_assistant_tier_registry` withholds the
    // skill-catalog tools from this tier.
    "delegation",
    "docker",
    "tdd",
    "wave-planning",
];

/// Bundled skills whose coding-vs-not split is genuinely arguable, EXCLUDED
/// pending an owner ruling.
///
/// Why: the brief's instruction was to fail closed on an ambiguous split and
/// record it rather than decide it quietly. A separate list is what makes
/// "undecided" a state the code carries, instead of five names indistinguishable
/// from the twenty that were decided.
/// What: each is off the floor today. Moving one ONTO the floor means deleting
/// its row here; `every_bundled_skill_is_classified` keeps the row honest
/// either way, and `pending_owner_ruling_names_are_off_the_floor` refuses the
/// half-move where a name sits in both.
const PENDING_OWNER_RULING: &[&str] = &[
    // "documentation" is on the keep list, but this skill is about docstrings
    // and JSDoc for code interfaces.
    "api-documentation",
    // Planning is assistant work; this skill is explicitly implementation
    // plans for engineers.
    "writing-plans",
    // A generic data skill an assistant plausibly wants; reads as coding
    // guidance.
    "json-data-handling",
    // Spreadsheet work is squarely the CTO-assistant use case; the skill is
    // "working with Excel files programmatically".
    "xlsx",
    // Out as source-control mechanics, while `tm-git-file-tracking` is on the
    // floor as a harness protocol. If either ruling is wrong they move together.
    "git-workflow",
];

/// Skill names on the floor that belong to NEITHER bundled catalog.
///
/// Why: the four connector/memory skills the shipped `assistant` persona
/// declares resolve from the operator's own skill directories
/// (`~/.claude/skills`, `~/.trusty-agents/skills`), not from either bundled
/// tree. `every_bundled_skill_is_classified` checks catalog ⊆ decided, so it
/// never sees these; naming them here keeps "on the floor but in no catalog"
/// an enumerated, reviewable set rather than whatever is left over.
/// Test: `floor_is_either_bundled_or_declared_off_catalog`.
const OFF_CATALOG_FLOOR_NAMES: &[&str] = &[
    "gworkspace-calendar",
    "gworkspace-drive",
    "gworkspace-gmail",
    "trusty-memory-openrpc",
];

/// The skill names trusty-mpm bundles, read from the manifest that OWNS that
/// answer.
///
/// Why: `framework-manifest.toml` says of itself that it is "the authority for
/// WHICH skills are bundled". Reading it is what turns the floor from a list
/// someone maintained once into a list a new bundled skill forces a decision
/// about.
/// What: `[skill_categories].universal`. Panics when the sibling crate is not
/// on disk — fail closed, because a silent skip would retire the forcing
/// function exactly when the tree stops looking familiar. The crate is a
/// workspace member, so every `cargo test -p trusty-agents` run has it.
fn mpm_bundled_skills() -> Vec<String> {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("trusty-mpm")
        .join("src")
        .join("assets")
        .join("framework-manifest.toml");
    let raw = std::fs::read_to_string(&manifest).unwrap_or_else(|e| {
        panic!(
            "cannot read the trusty-mpm bundled skill catalog at {}: {e} — the skill floor's \
             completeness check cannot run without it",
            manifest.display()
        )
    });
    let doc: toml::Value = raw.parse().expect("framework-manifest.toml is valid TOML");
    doc.get("skill_categories")
        .and_then(|c| c.get("universal"))
        .and_then(|v| v.as_array())
        .expect("framework-manifest.toml declares [skill_categories].universal")
        .iter()
        .filter_map(|v| v.as_str())
        .map(str::to_string)
        .collect()
}

/// The skill names this crate bundles under `.trusty-agents/skills/`.
///
/// Why: the second catalog an assistant's resolver reaches. Enumerated from
/// disk for the same reason as above — a new `.md` dropped into that tree must
/// force a floor decision, not inherit one.
/// What: a directory holding `SKILL.md` contributes the DIRECTORY name and is
/// not descended into (its internals — `cto-db/python/**` — are not separate
/// skills); every other `.md` contributes its file stem, recursively, matching
/// how `skills::registry` indexes the tree.
fn trusty_agents_bundled_skills() -> Vec<String> {
    fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.join("SKILL.md").is_file() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        out.push(name.to_string());
                    }
                } else {
                    walk(&path, out);
                }
            } else if path.extension().and_then(|e| e.to_str()) == Some("md")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                out.push(stem.to_string());
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".trusty-agents")
        .join("skills");
    let mut out = Vec::new();
    walk(&root, &mut out);
    assert!(
        !out.is_empty(),
        "no bundled skills found under {} — the completeness check would pass vacuously",
        root.display()
    );
    out
}

/// THE forcing function: every bundled skill is either ON the floor or on a
/// maintained exclusion list. A newly bundled skill fails here until someone
/// decides.
///
/// Why (code-critic, PR #7897, MEDIUM): `ASSISTANT_REACHABLE_SKILLS` is
/// hand-authored. Without a diff against the live catalogs, adding a skill to
/// `framework-manifest.toml` or dropping a `.md` into
/// `.trusty-agents/skills/` silently leaves it unreachable — a capability
/// decision made by omission, which is the failure mode this whole module
/// exists to remove for the assistant tier.
/// What: reads both catalogs from disk and requires each name to appear in
/// `ASSISTANT_REACHABLE_SKILLS`, `CODING_SKILLS`, or `PENDING_OWNER_RULING`.
/// The failure message names the undecided skill AND all three lists, so the
/// next reader does not have to work out what the choice is.
/// Test: this function IS the test.
#[test]
fn every_bundled_skill_is_classified() {
    let mut undecided: Vec<String> = Vec::new();
    let mut catalog: Vec<String> = mpm_bundled_skills();
    catalog.extend(trusty_agents_bundled_skills());
    catalog.sort();
    catalog.dedup();
    for name in &catalog {
        let decided = ASSISTANT_REACHABLE_SKILLS.contains(&name.as_str())
            || CODING_SKILLS.contains(&name.as_str())
            || PENDING_OWNER_RULING.contains(&name.as_str());
        if !decided {
            undecided.push(name.clone());
        }
    }
    assert!(
        undecided.is_empty(),
        "bundled skill(s) with no floor decision: {undecided:?}\n\
         Each must be added to exactly one of:\n  \
         - agents::skill_floor::ASSISTANT_REACHABLE_SKILLS (reachable by an assistant)\n  \
         - CODING_SKILLS in this file (excluded: engineer toolchain, testing, debugging, \
           review, build, language idioms, app work, CI security, VCS, internal orchestration)\n  \
         - PENDING_OWNER_RULING in this file (ambiguous — excluded, recorded, awaiting a call)\n\
         Fail closed: when the split is unclear, PENDING_OWNER_RULING is the answer.\n\
         Catalogs scanned: {} name(s).",
        catalog.len()
    );
}

/// A name cannot be both granted and excluded.
#[test]
fn floor_and_exclusions_are_disjoint() {
    for excluded in CODING_SKILLS {
        assert!(
            !ASSISTANT_REACHABLE_SKILLS.contains(excluded),
            "'{excluded}' is on the floor AND on CODING_SKILLS"
        );
    }
}

/// A pending name is EXCLUDED while it is pending — recording the open question
/// must not be a way of quietly granting it.
#[test]
fn pending_owner_ruling_names_are_off_the_floor() {
    for pending in PENDING_OWNER_RULING {
        assert!(
            !ASSISTANT_REACHABLE_SKILLS.contains(pending),
            "'{pending}' is awaiting an owner ruling and must stay off the floor until it lands"
        );
        assert!(
            !CODING_SKILLS.contains(pending),
            "'{pending}' is in both PENDING_OWNER_RULING and CODING_SKILLS — it is either \
             decided or it is not"
        );
    }
}

/// Every floor name is either bundled or an enumerated off-catalog exception.
///
/// Why: the reverse direction of `every_bundled_skill_is_classified`. Without
/// it, a typo on the floor (`tm-tickting`) is a dead entry that grants nothing
/// and reads as a grant.
/// Test: this function IS the test.
#[test]
fn floor_is_either_bundled_or_declared_off_catalog() {
    let mut catalog: Vec<String> = mpm_bundled_skills();
    catalog.extend(trusty_agents_bundled_skills());
    let orphans: Vec<&&str> = ASSISTANT_REACHABLE_SKILLS
        .iter()
        .filter(|name| {
            !catalog.iter().any(|c| c == *name) && !OFF_CATALOG_FLOOR_NAMES.contains(name)
        })
        .collect();
    assert!(
        orphans.is_empty(),
        "floor name(s) in neither bundled catalog and not declared in \
         OFF_CATALOG_FLOOR_NAMES: {orphans:?} — a misspelled floor entry grants nothing"
    );
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
/// operator's `~/.claude/skills/` library. Covers the pending names too — they
/// are excluded until an owner says otherwise, and that has to be observable
/// behaviour, not just a list membership.
#[test]
fn assistant_cannot_reach_a_coding_skill() {
    for excluded in CODING_SKILLS.iter().chain(PENDING_OWNER_RULING) {
        assert!(
            !skill_is_reachable("assistant", None, excluded),
            "an assistant must not reach excluded skill '{excluded}'"
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
