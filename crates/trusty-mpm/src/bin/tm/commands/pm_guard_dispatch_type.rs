//! Refuse a main-checkout dispatch whose agent type nothing defines (#8547).
//!
//! Why: a dispatch with no usable `subagent_type` bypasses the roster and every
//! guardrail keyed on an agent name, and granting it a worktree on a guess hid
//! that from the PM. The refusal must reach only names that nothing defines,
//! though: an agent deployed into a roster tier, or a Claude Code built-in, is a
//! real dispatch target even though this binary does not bundle it.
//!
//! What: [`undetermined_type_refusal`] answers `None` when `subagent_type` names
//! a known agent — one of:
//!
//! 1. a bundled agent, or a harness built-in
//!    ([`agent_known_without_roster`]);
//! 2. an agent in any directory [`deployed_agent_dirs`] names — the same tiers
//!    the PM's delegation roster reads — matched on the exact `name:`;
//! 3. an agent in `<dir>/.claude/agents` for any `<dir>` between `cwd` and its
//!    main checkout root ([`ancestor_project_tiers`]).
//!
//! Otherwise it returns deny text naming the defect and the fix, which depends
//! on whether the dispatch tool can carry `isolation` at all.
//!
//! Fail-open check: a tier that cannot be enumerated, or a file that cannot be
//! read, contributes no names, exactly as in the roster. The other tiers, the
//! bundle and the built-ins still count, so one bad directory never makes every
//! name unknown, and a name found nowhere is never admitted because a tier was
//! unreadable. The refusal names every unreadable path, so the loss is not
//! silent. A file whose frontmatter has no `name:` defines no agent, as in the
//! roster.
//!
//! Test: the `#[cfg(test)]` suite below, and the `refuses_*` and
//! `grants_a_worktree_to_*` tests in `pm_guard_worktree_grant`.

use std::path::{Path, PathBuf};

use serde_json::Value;
#[cfg(doc)]
use trusty_mpm::core::delegation_authority::deployed_agent_dirs;
use trusty_mpm::core::delegation_authority::scan_agents_reporting;
use trusty_mpm::core::dispatch_isolation::agent_known_without_roster;
use trusty_mpm::core::project_aliases::main_checkout_root;

/// Resolves the roster tiers for a directory — [`deployed_agent_dirs`] in
/// production, a tempdir-rooted list in tests.
///
/// Why: `deployed_agent_dirs` reads `CLAUDE_CONFIG_DIR` and `$HOME`, and the
/// `tm` bin target bans tests from writing either (`env_isolation_tests`), so a
/// hermetic test injects the tiers instead.
pub(crate) type DeployedTiers<'a> = &'a dyn Fn(&Path) -> Vec<PathBuf>;

/// Deny text for a dispatch whose `subagent_type` names no known agent, or
/// `None` when it names one.
///
/// Why: see the module doc. `accepts_isolation` is `false` for `Task`, which has
/// no `isolation` parameter, so its refusal must point at the `Agent` tool
/// instead of telling it to declare a field it cannot carry.
/// What: resolves `deployed(cwd)` plus [`ancestor_project_tiers`] and defers to
/// [`undetermined_type_refusal_in`].
/// Test: `a_deployed_agent_is_known`, `a_name_found_nowhere_is_refused`,
/// `a_project_agent_is_known_from_a_subdirectory_of_the_checkout`.
pub(crate) fn undetermined_type_refusal(
    tool_input: Option<&Value>,
    cwd: &Path,
    accepts_isolation: bool,
    deployed: DeployedTiers<'_>,
) -> Option<String> {
    let mut tiers = deployed(cwd);
    tiers.extend(ancestor_project_tiers(cwd));
    undetermined_type_refusal_in(tool_input, &tiers, accepts_isolation)
}

/// `<dir>/.claude/agents` for every strict ancestor of `cwd` up to and
/// including its main checkout root.
///
/// Why (#8547 review): the session's Bash `cd` persists and the guard accepts
/// any subdirectory of a checkout, while `deployed_agent_dirs(cwd)` reads only
/// `<cwd>/.claude/agents`. A project agent was refused after `cd crates/x`.
/// Walking up also covers a project root nested below the git root.
/// What: empty when `cwd` is not in a main checkout. Stops at the root and never
/// names a directory above it. Lexical, like [`main_checkout_root`].
/// Test: `a_project_agent_is_known_from_a_subdirectory_of_the_checkout`,
/// `an_agent_defined_above_the_checkout_root_is_refused`.
fn ancestor_project_tiers(cwd: &Path) -> Vec<PathBuf> {
    let Some(root) = main_checkout_root(cwd) else {
        return Vec::new();
    };
    cwd.ancestors()
        .skip(1)
        .take_while(|dir| dir.starts_with(&root))
        .map(|dir| dir.join(".claude").join("agents"))
        .collect()
}

/// [`undetermined_type_refusal`] over an explicit tier list.
///
/// Why: the fail-open cases need an exact tier list a test controls.
/// What: `None` when `subagent_type` is a known name; otherwise the deny text.
/// Tiers are only read when the name is not bundled or built in.
/// Test: `an_unreadable_tier_does_not_hide_the_other_tiers`,
/// `an_unreadable_tier_is_named_in_the_refusal`.
fn undetermined_type_refusal_in(
    tool_input: Option<&Value>,
    tiers: &[PathBuf],
    accepts_isolation: bool,
) -> Option<String> {
    let detail = undetermined_type_detail(tool_input, tiers)?;
    Some(deny_reason(&detail, accepts_isolation))
}

/// What is wrong with the dispatch's `subagent_type`, or `None` when nothing is.
///
/// Why: a missing, unparsable and unknown type each need a different fix, so
/// the refusal has to say which one it is.
/// What: a phrase naming the absent field, the non-string or empty value, or
/// the unknown name plus any tier path that could not be read.
/// Test: `a_name_found_nowhere_is_refused`,
/// `an_unreadable_tier_is_named_in_the_refusal`.
fn undetermined_type_detail(tool_input: Option<&Value>, tiers: &[PathBuf]) -> Option<String> {
    let Some(raw) = tool_input.and_then(|input| input.get("subagent_type")) else {
        return Some("carries no `subagent_type`".to_string());
    };
    let Some(name) = raw.as_str().filter(|name| !name.is_empty()) else {
        return Some(format!(
            "carries a `subagent_type` that is not an agent name (`{raw}`)"
        ));
    };
    if agent_known_without_roster(name) {
        return None;
    }
    let mut unreadable = Vec::new();
    for tier in tiers {
        let scan = scan_agents_reporting(tier);
        // Exact match: the roster's own dedup folds case, the #8547 rule does not.
        if scan.agents.iter().any(|agent| agent.name == name) {
            return None;
        }
        unreadable.extend(scan.unreadable);
    }
    let mut detail = format!(
        "names `subagent_type` `{name}`, which is not a bundled agent, a Claude Code \
         built-in, or an agent in any deployed agent directory"
    );
    if !unreadable.is_empty() {
        let paths: Vec<String> = unreadable.iter().map(|p| p.display().to_string()).collect();
        detail.push_str(&format!(
            " (the roster is incomplete: tm could not read {})",
            paths.join(", ")
        ));
    }
    Some(detail)
}

/// Deny text embedding `detail` from [`undetermined_type_detail`].
///
/// Why: the refusal is the PM's only signal, so it names the defect and both
/// ways to re-issue the dispatch.
/// What: one paragraph; the second fix names the `Agent` tool when the dispatch
/// tool cannot carry `isolation`.
/// Test: `a_task_refusal_points_at_the_agent_tool`.
fn deny_reason(detail: &str, accepts_isolation: bool) -> String {
    let isolation_fix = if accepts_isolation {
        "An agent defined somewhere tm does not read must declare `isolation: \"worktree\"` \
         itself, which this rule leaves alone."
    } else {
        "An agent defined somewhere tm does not read must be dispatched through the `Agent` \
         tool with `isolation: \"worktree\"`, which this rule leaves alone; `Task` carries no \
         isolation parameter."
    };
    format!(
        "Dispatch refused in a main checkout (#8547): this dispatch {detail}, so tm cannot tell \
         whether the agent writes files, and it will not isolate or admit an agent nothing \
         defines. Set `subagent_type` to a known agent — one tm bundles, one deployed in the \
         roster's agent directories, or a Claude Code built-in (for example `rust-engineer` or \
         `research`; names are case-sensitive). {isolation_fix}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::pm_guard_worktree_grant::{WorktreeGrant, evaluate_worktree_grant_with};
    use crate::test_support::hermetic_temp_dir;
    use tempfile::TempDir;
    use trusty_mpm::core::delegation_authority::deployed_agent_dirs_from;

    fn typed(agent: &str) -> Value {
        serde_json::json!({"subagent_type": agent, "prompt": "go"})
    }

    /// A tier directory holding one agent file per `(file stem, name:)` pair.
    fn tier(agents: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for (stem, name) in agents {
            let body = format!("---\nname: {name}\nrole: ops\n---\n\n# {name}\n");
            std::fs::write(dir.path().join(format!("{stem}.md")), body).expect("write agent");
        }
        dir
    }

    /// Make `path` unreadable; `false` when the platform or privilege level
    /// ignores the mode bits, so the caller can skip rather than pass vacuously.
    fn deny_read(path: &Path) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).is_err() {
                return false;
            }
            // Root ignores the mode bits; check the denial took effect.
            if path.is_dir() {
                std::fs::read_dir(path).is_err()
            } else {
                std::fs::read_to_string(path).is_err()
            }
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            false
        }
    }

    fn restore(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
        }
        #[cfg(not(unix))]
        let _ = path;
    }

    /// Roster tiers rooted in `machine` rather than `CLAUDE_CONFIG_DIR` and
    /// `$HOME`, which this target's tests may not write.
    fn hermetic_tiers(machine: &Path) -> impl Fn(&Path) -> Vec<PathBuf> + '_ {
        move |project| {
            deployed_agent_dirs_from(project, Some(machine), &machine.join("home-agents"))
        }
    }

    /// Define the agent `name` in `<dir>/.claude/agents`.
    fn define_agent(dir: &Path, name: &str) {
        let agents = dir.join(".claude/agents");
        std::fs::create_dir_all(&agents).expect("mkdir agents");
        let body = format!("---\nname: {name}\n---\n# x\n");
        std::fs::write(agents.join(format!("{name}.md")), body).expect("write agent");
    }

    /// `<tmp>/repo` as a main checkout holding `sub/deeper`, plus an absent
    /// `<tmp>/machine` for the managed and home tiers.
    fn checkout() -> (TempDir, PathBuf, PathBuf) {
        let tmp = hermetic_temp_dir();
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(root.join(".git")).expect("mkdir .git");
        std::fs::create_dir_all(root.join("sub/deeper")).expect("mkdir sub/deeper");
        let machine = tmp.path().join("machine");
        (tmp, root, machine)
    }

    /// What `evaluate_worktree_grant` decides for `agent` from `cwd`.
    fn decide(agent: &str, cwd: &Path, machine: &Path) -> Option<WorktreeGrant> {
        evaluate_worktree_grant_with("Agent", Some(&typed(agent)), cwd, &hermetic_tiers(machine))
    }

    #[test]
    fn a_deployed_agent_is_known() {
        // The critic's case: `ops` lives only in a deployed tier.
        let (_tmp, root, machine) = checkout();
        define_agent(&root, "fixture-ops");
        let sent = typed("fixture-ops");
        let tiers = hermetic_tiers(&machine);
        assert_eq!(
            undetermined_type_refusal(Some(&sent), &root, true, &tiers),
            None
        );
    }

    #[test]
    fn a_project_agent_is_known_from_a_subdirectory_of_the_checkout() {
        // #8547 review: the session's `cd` persists, so a project agent must stay
        // known below the checkout root, including a nested project root (`sub`).
        let (_tmp, root, machine) = checkout();
        define_agent(&root, "fixture-root-agent");
        define_agent(&root.join("sub"), "fixture-sub-agent");
        for cwd in [root.join("sub"), root.join("sub/deeper")] {
            for agent in ["fixture-root-agent", "fixture-sub-agent"] {
                match decide(agent, &cwd, &machine) {
                    Some(WorktreeGrant::Rewrite(updated)) => {
                        assert_eq!(updated["isolation"], "worktree", "{agent}");
                    }
                    other => panic!("{agent} from {} got {other:?}", cwd.display()),
                }
            }
        }
    }

    #[test]
    fn an_agent_defined_above_the_checkout_root_is_refused() {
        // The walk stops at the checkout root and never reads above it.
        let (tmp, root, machine) = checkout();
        define_agent(tmp.path(), "fixture-above-root");
        for cwd in [root.clone(), root.join("sub/deeper")] {
            match decide("fixture-above-root", &cwd, &machine) {
                Some(WorktreeGrant::Deny(reason)) => {
                    assert!(reason.contains("`fixture-above-root`"), "{reason}");
                }
                other => panic!("from {} got {other:?}", cwd.display()),
            }
        }
    }

    #[test]
    fn an_unreadable_directory_on_the_walk_is_named_in_the_refusal() {
        // Fail-open check: an unreadable intermediate tier never admits its own
        // names and never hides the root's; the refusal names it.
        let (_tmp, root, machine) = checkout();
        define_agent(&root, "fixture-root-agent");
        define_agent(&root.join("sub"), "fixture-sub-hidden");
        let sub_agents = root.join("sub/.claude/agents");
        if !deny_read(&sub_agents) {
            restore(&sub_agents);
            eprintln!("skipping: cannot deny read on this platform/privilege level");
            return;
        }
        let deeper = root.join("sub/deeper");
        let hidden = decide("fixture-sub-hidden", &deeper, &machine);
        let visible = decide("fixture-root-agent", &deeper, &machine);
        restore(&sub_agents);
        let Some(WorktreeGrant::Deny(reason)) = hidden else {
            panic!("an unreadable definition must not admit the name, got {hidden:?}");
        };
        assert!(reason.contains("roster is incomplete"), "{reason}");
        assert!(
            reason.contains(&sub_agents.display().to_string()),
            "{reason}"
        );
        assert!(
            matches!(visible, Some(WorktreeGrant::Rewrite(_))),
            "{visible:?}"
        );
    }

    #[test]
    fn a_name_found_nowhere_is_refused() {
        let deployed = tier(&[("fixture-ops", "fixture-ops")]);
        let tiers = [deployed.path().to_path_buf()];
        for agent in ["fixture-ops-missing", "Fixture-Ops"] {
            let reason = undetermined_type_refusal_in(Some(&typed(agent)), &tiers, true)
                .unwrap_or_else(|| panic!("{agent} is defined nowhere"));
            assert!(reason.contains(&format!("`{agent}`")), "{reason}");
            assert!(reason.contains("deployed in the roster"), "{reason}");
            assert!(!reason.contains("incomplete"), "{reason}");
        }
    }

    #[test]
    fn an_unreadable_tier_does_not_hide_the_other_tiers() {
        // Fail-open check: one bad tier must not make every name unknown.
        let broken = tier(&[("fixture-hidden", "fixture-hidden")]);
        let good = tier(&[("fixture-ops", "fixture-ops")]);
        if !deny_read(broken.path()) {
            eprintln!("skipping: cannot deny read on this platform/privilege level");
            return;
        }
        let tiers = [broken.path().to_path_buf(), good.path().to_path_buf()];
        let found = undetermined_type_refusal_in(Some(&typed("fixture-ops")), &tiers, true);
        let bundled = undetermined_type_refusal_in(Some(&typed("rust-engineer")), &tiers, true);
        restore(broken.path());
        assert_eq!(found, None, "a readable tier still defines its agents");
        assert_eq!(bundled, None, "the bundle does not depend on any tier");
    }

    #[test]
    fn an_unreadable_tier_is_named_in_the_refusal() {
        // Fail-open check: an unreadable tier never admits an unknown name, and
        // the refusal says the roster it checked was incomplete.
        let broken = tier(&[("fixture-hidden", "fixture-hidden")]);
        let file_tier = tier(&[("fixture-bad", "fixture-bad")]);
        let bad_file = file_tier.path().join("fixture-bad.md");
        if !deny_read(broken.path()) || !deny_read(&bad_file) {
            restore(broken.path());
            eprintln!("skipping: cannot deny read on this platform/privilege level");
            return;
        }
        let tiers = [broken.path().to_path_buf(), file_tier.path().to_path_buf()];
        let hidden = undetermined_type_refusal_in(Some(&typed("fixture-hidden")), &tiers, true);
        let bad = undetermined_type_refusal_in(Some(&typed("fixture-bad")), &tiers, true);
        restore(broken.path());
        restore(&bad_file);
        for reason in [hidden, bad] {
            let reason = reason.expect("an unreadable definition must not admit the name");
            assert!(reason.contains("roster is incomplete"), "{reason}");
            assert!(
                reason.contains(&broken.path().display().to_string()),
                "{reason}"
            );
            assert!(reason.contains(&bad_file.display().to_string()), "{reason}");
        }
    }

    #[test]
    fn a_frontmatter_without_a_name_defines_no_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("fixture-nameless.md"),
            "---\nrole: ops\n---\n",
        )
        .expect("write agent");
        let tiers = [dir.path().to_path_buf()];
        let reason = undetermined_type_refusal_in(Some(&typed("fixture-nameless")), &tiers, true);
        assert!(reason.is_some(), "a file stem is not an agent name");
    }

    #[test]
    fn a_task_refusal_points_at_the_agent_tool() {
        // `Task` cannot carry `isolation`, so its fix is the `Agent` tool.
        let reason = undetermined_type_refusal_in(Some(&typed("fixture-none")), &[], false)
            .expect("refused");
        assert!(reason.contains("through the `Agent` tool"), "{reason}");
        assert!(reason.contains("`Task` carries no isolation"), "{reason}");
        let agent =
            undetermined_type_refusal_in(Some(&typed("fixture-none")), &[], true).expect("refused");
        assert!(!agent.contains("`Task`"), "{agent}");
    }
}
