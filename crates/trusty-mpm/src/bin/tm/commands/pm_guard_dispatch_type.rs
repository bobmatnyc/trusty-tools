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
//!    the PM's delegation roster reads — matched on the exact `name:`.
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
use trusty_mpm::core::delegation_authority::{deployed_agent_dirs, scan_agents_reporting};
use trusty_mpm::core::dispatch_isolation::agent_known_without_roster;

/// Deny text for a dispatch whose `subagent_type` names no known agent, or
/// `None` when it names one.
///
/// Why: see the module doc. `accepts_isolation` is `false` for `Task`, which has
/// no `isolation` parameter, so its refusal must point at the `Agent` tool
/// instead of telling it to declare a field it cannot carry.
/// What: resolves the deployed tiers from `cwd` and defers to
/// [`undetermined_type_refusal_in`].
/// Test: `a_deployed_agent_is_known`, `a_name_found_nowhere_is_refused`.
pub(crate) fn undetermined_type_refusal(
    tool_input: Option<&Value>,
    cwd: &Path,
    accepts_isolation: bool,
) -> Option<String> {
    undetermined_type_refusal_in(tool_input, &deployed_agent_dirs(cwd), accepts_isolation)
}

/// [`undetermined_type_refusal`] over an explicit tier list.
///
/// Why: the shipped entry point reads `CLAUDE_CONFIG_DIR` and the home
/// directory; the fail-open cases need tiers a test controls.
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

    #[test]
    fn a_deployed_agent_is_known() {
        // The critic's case: `ops` lives only in a deployed tier.
        let project = tempfile::tempdir().expect("tempdir");
        let agents = project.path().join(".claude/agents");
        std::fs::create_dir_all(&agents).expect("mkdir");
        std::fs::write(
            agents.join("fixture-ops.md"),
            "---\nname: fixture-ops\n---\n# x\n",
        )
        .expect("write agent");
        let sent = typed("fixture-ops");
        assert_eq!(
            undetermined_type_refusal(Some(&sent), project.path(), true),
            None
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
