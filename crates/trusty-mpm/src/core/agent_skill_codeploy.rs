//! Agent-bundled skill co-deployment and observability (DOC-42, issue #2889).
//!
//! Why: `docs/specs/agent-bundled-skills.md` §SPEC-AGENTSKILLS-02 requires
//! that when an agent declaring `skills:` is deployed, its declared skills
//! are also deployed to the same target — and §SPEC-AGENTSKILLS-05 requires
//! logging both the co-deploy set and any declared skill that fails to
//! resolve through the 3-tier precedence. This logic is identical whether the
//! caller is the ordinary session-launch path (`core::session_launch`) or the
//! standalone `tm install` deploy — both call
//! [`agent_deployer::deploy_agents_filtered`] and then need to (a) fold every
//! deployed agent's declared skills into the skill-deploy `select` predicate
//! and (b) log the resolution outcome. Centralising it here keeps the two
//! call sites from re-implementing (and silently diverging on) the same
//! union + resolve + log sequence.
//! What: [`co_deploy_skill_set`] unions every agent's declared skills into
//! one stem set (for the skill deployer's `select` predicate);
//! [`log_declared_skills`] emits the per-agent `info` summary line and a
//! `warn`/`debug` line per declared skill depending on whether it resolves.
//! Test: `co_deploy_skill_set_unions_all_agents`,
//! `co_deploy_skill_set_empty_when_no_agent_declares_skills`, plus the
//! `log_declared_skills` tests in this file (log content is asserted via the
//! returned dangling-skill list, since `tracing` output itself is not
//! directly assertable in a unit test).

use std::collections::{BTreeSet, HashMap};

use crate::core::skill_tiers::resolve_skill_tier;

/// Union every declared-skills list into one stem set.
///
/// Why: the skill deployer's bundled-tier `select` predicate needs the FULL
/// set of skill names any deployed agent depends on, so a declared skill the
/// harness manifest would otherwise exclude still deploys (§SPEC-AGENTSKILLS-02
/// co-deploy guarantee).
/// What: returns the union of every `Vec<String>` in `declared`, de-duplicated
/// via a `BTreeSet` (also gives deterministic iteration order for logging).
/// Test: `co_deploy_skill_set_unions_all_agents`,
/// `co_deploy_skill_set_empty_when_no_agent_declares_skills`.
pub fn co_deploy_skill_set(declared: &HashMap<String, Vec<String>>) -> BTreeSet<String> {
    declared.values().flatten().cloned().collect()
}

/// Log the co-deploy summary and per-skill resolution for every declared skill.
///
/// Why: an operator debugging a "skill not found" error needs to see, in the
/// logs, which skills each deployed agent declared and whether each one
/// resolved — this is the §SPEC-AGENTSKILLS-05 observability contract.
/// What: for each agent with a non-empty `skills` list, emits one `info` line
/// naming the agent and its declared skills, then for each declared skill
/// either a `debug` line naming the resolved tier or a `warn` line when the
/// skill is absent from all three tiers (`project`/`user`/`bundled`, per
/// [`resolve_skill_tier`]). Returns the list of dangling `(agent, skill)`
/// pairs so callers can additionally surface them beyond the log stream (e.g.
/// a deploy summary count) without re-deriving resolution.
/// Test: `log_declared_skills_resolves_all_tiers`,
/// `log_declared_skills_flags_dangling_reference`,
/// `log_declared_skills_skips_agents_with_no_skills`.
pub fn log_declared_skills(
    declared: &HashMap<String, Vec<String>>,
    project: &BTreeSet<String>,
    user: &BTreeSet<String>,
    bundled: &BTreeSet<String>,
) -> Vec<(String, String)> {
    let mut dangling = Vec::new();
    // Sort agent names for deterministic log ordering across runs.
    let mut agents: Vec<&String> = declared.keys().collect();
    agents.sort();

    for agent in agents {
        let skills = &declared[agent];
        if skills.is_empty() {
            continue;
        }
        tracing::info!(
            agent = %agent,
            skills = %skills.join(", "),
            "deploying agent with declared skills"
        );
        for skill in skills {
            match resolve_skill_tier(skill, project, user, bundled) {
                Some(tier) => tracing::debug!(
                    agent = %agent,
                    skill = %skill,
                    tier = tier.label(),
                    "co-deployed skill resolved"
                ),
                None => {
                    tracing::warn!(
                        agent = %agent,
                        skill = %skill,
                        "agent declares skill '{skill}', but it is not found in any tier \
                         (project/user/bundled)"
                    );
                    dangling.push((agent.clone(), skill.clone()));
                }
            }
        }
    }
    dangling
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn declared(pairs: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(agent, skills)| {
                (
                    agent.to_string(),
                    skills.iter().map(|s| s.to_string()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn co_deploy_skill_set_unions_all_agents() {
        let map = declared(&[
            (
                "code-critic",
                &["code-review-standards", "systematic-debugging"],
            ),
            (
                "rust-engineer",
                &["toolchains-rust-core", "systematic-debugging"],
            ),
        ]);
        let union = co_deploy_skill_set(&map);
        assert_eq!(
            union,
            set(&[
                "code-review-standards",
                "systematic-debugging",
                "toolchains-rust-core"
            ])
        );
    }

    #[test]
    fn co_deploy_skill_set_empty_when_no_agent_declares_skills() {
        let map = declared(&[("plain", &[])]);
        assert!(co_deploy_skill_set(&map).is_empty());
    }

    // ---- bundled-asset gates (#4642, #4643) --------------------------------
    //
    // The harness renders every skill named in an agent's `skills:` frontmatter
    // as a FULL SKILL.md body into that agent's system prompt at dispatch,
    // before the Skill tool is ever called. `skills:` is therefore a RESIDENT
    // cost paid on every dispatch, not a capability list — a skill left out is
    // still invokable on demand. These three gates pin the shipped assets:
    // every declared skill resolves, the resident cost stays bounded, and no
    // foundation layer silently adds to a leaf agent's list.

    use std::path::{Path, PathBuf};
    use trusty_agents_common::agents::builder::compose_agent;
    use trusty_agents_common::agents::metadata::agent_metadata_from_str;

    fn agents_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/assets/agents")
    }

    fn skills_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/assets/skills")
    }

    /// Every bundled non-`BASE-*` agent stem, sorted.
    fn bundled_agent_stems() -> Vec<String> {
        let mut stems: Vec<String> = std::fs::read_dir(agents_dir())
            .expect("bundled agent assets dir")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
            .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
            .filter(|stem| !stem.to_ascii_lowercase().starts_with("base"))
            .collect();
        stems.sort();
        stems
    }

    /// The COMPOSED (deployed) `skills:` list for a bundled agent.
    fn composed_skills(stem: &str) -> Vec<String> {
        let composed = compose_agent(stem, &agents_dir())
            .unwrap_or_else(|e| panic!("compose bundled agent `{stem}`: {e}"));
        agent_metadata_from_str(&composed).skills
    }

    #[test]
    fn bundled_agent_declared_skills_all_resolve() {
        // #4642/#4643 keep the existing `agent_skills` doctor check green: a
        // trimmed list must never leave a dangling reference behind, and a
        // typo'd rename must fail here rather than as a runtime WARN.
        let mut dangling: Vec<String> = Vec::new();
        for stem in bundled_agent_stems() {
            for skill in composed_skills(&stem) {
                if !skills_dir().join(format!("{skill}.md")).is_file() {
                    dangling.push(format!("{stem} declares `{skill}`"));
                }
            }
        }
        assert!(
            dangling.is_empty(),
            "bundled agents declare skills with no bundled asset: {dangling:?}"
        );
    }

    #[test]
    fn bundled_agent_resident_skill_cost_stays_within_budget() {
        // #4642 REGRESSION GATE. Before the trim the worst agent (`qa`, 15
        // skills) rendered 84,343 bytes (~21,085 tokens) of skill body into
        // every single dispatch, and `research` 55,894 (~13,974). The budget is
        // deliberately just above today's worst case so that adding a skill to
        // a bundled agent is a decision someone has to make on purpose.
        const BUDGET_BYTES: u64 = 24_000;

        let mut over: Vec<String> = Vec::new();
        for stem in bundled_agent_stems() {
            let total: u64 = composed_skills(&stem)
                .iter()
                .filter_map(|s| std::fs::metadata(skills_dir().join(format!("{s}.md"))).ok())
                .map(|m| m.len())
                .sum();
            if total > BUDGET_BYTES {
                over.push(format!("{stem}: {total} bytes (~{} tokens)", total / 4));
            }
        }
        assert!(
            over.is_empty(),
            "resident skill-body cost exceeds the {BUDGET_BYTES}-byte per-dispatch \
             budget (#4642). Drop the skill from `skills:` — it stays invokable \
             via the Skill tool on demand: {over:?}"
        );
    }

    #[test]
    fn no_foundation_layer_silently_adds_skills_except_base_engineer() {
        // #4643. `skills:` UNIONS across the `extends:` chain, so a declaration
        // on a BASE-* template silently appears in every descendant's DEPLOYED
        // frontmatter while its own source file shows a shorter list. That is
        // exactly the source/deployed divergence #4643 reported: `research.md`
        // declared 11, the deployed copy carried 12 (`tm-capabilities`, from
        // BASE-AGENT). Only BASE-ENGINEER contributes a skill now, and it does
        // so deliberately — `documentation-style` shapes every doc comment an
        // engineer writes in this repo.
        for base in ["BASE-AGENT", "BASE-QA", "BASE-OPS", "BASE-RESEARCH"] {
            let raw = std::fs::read_to_string(agents_dir().join(format!("{base}.md")))
                .unwrap_or_else(|e| panic!("read {base}.md: {e}"));
            assert!(
                agent_metadata_from_str(&raw).skills.is_empty(),
                "{base} declares `skills:`, which every descendant then pays for \
                 on every dispatch while its own source file does not show it (#4643)"
            );
        }
        assert_eq!(
            composed_skills("research"),
            Vec::<String>::new(),
            "the deployed `research` skills list must equal its source declaration (#4643)"
        );
        assert_eq!(
            composed_skills("engineer"),
            vec![
                "documentation-style".to_string(),
                "systematic-debugging".to_string(),
                "test-driven-development".to_string(),
            ],
            "BASE-ENGINEER's `documentation-style` is the one deliberate inherited skill"
        );
    }

    #[test]
    fn log_declared_skills_resolves_all_tiers() {
        let map = declared(&[(
            "code-critic",
            &["project-skill", "user-skill", "bundled-skill"],
        )]);
        let dangling = log_declared_skills(
            &map,
            &set(&["project-skill"]),
            &set(&["user-skill"]),
            &set(&["bundled-skill"]),
        );
        assert!(dangling.is_empty());
    }

    #[test]
    fn log_declared_skills_flags_dangling_reference() {
        let map = declared(&[("code-critic", &["missing-skill"])]);
        let dangling =
            log_declared_skills(&map, &BTreeSet::new(), &BTreeSet::new(), &BTreeSet::new());
        assert_eq!(
            dangling,
            vec![("code-critic".to_string(), "missing-skill".to_string())]
        );
    }

    #[test]
    fn log_declared_skills_skips_agents_with_no_skills() {
        let map = declared(&[("plain", &[])]);
        let dangling =
            log_declared_skills(&map, &BTreeSet::new(), &BTreeSet::new(), &BTreeSet::new());
        assert!(dangling.is_empty());
    }

    #[test]
    fn co_deploy_resolves_sibling_2890_code_critic_fixture() {
        // Cross-PR fixture (issue #2890, PR #2900): `code-critic` declares
        // `[code-review-standards, contract-driven-testing]`. Once both land
        // on main these resolve from the Bundled tier; here we pin the
        // resolution behavior with a synthetic bundled set standing in for
        // the real asset (not yet merged) so #2889's co-deploy logic is
        // proven compatible independent of #2890's landing order.
        let map = declared(&[(
            "code-critic",
            &["code-review-standards", "contract-driven-testing"],
        )]);
        let bundled = set(&["code-review-standards", "contract-driven-testing"]);
        let dangling = log_declared_skills(&map, &BTreeSet::new(), &BTreeSet::new(), &bundled);
        assert!(
            dangling.is_empty(),
            "both skills must resolve from bundled: {dangling:?}"
        );
        assert_eq!(co_deploy_skill_set(&map), bundled);
    }

    #[test]
    fn log_declared_skills_partial_resolution_flags_only_missing() {
        let map = declared(&[("agent", &["ok-skill", "bad-skill"])]);
        let dangling = log_declared_skills(
            &map,
            &BTreeSet::new(),
            &BTreeSet::new(),
            &set(&["ok-skill"]),
        );
        assert_eq!(
            dangling,
            vec![("agent".to_string(), "bad-skill".to_string())]
        );
    }
}
