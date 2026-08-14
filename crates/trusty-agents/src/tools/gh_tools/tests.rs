//! Unit tests for the L0-only GitHub PR/CI tool surface (#4170).
//!
//! Why: Two families of property are pinned here — the TIER GATE (an L1 or
//! indeterminate tier gets no tools from the factory, an L0 tier gets exactly
//! five) and OPERAND VALIDATION (no LLM-supplied string can become a `gh`
//! flag). The complementary proof that the gate holds through the REAL
//! registry-construction path — not just this factory — lives in
//! `crate::runtime::tool_registry_tests`.
//! What: Factory/tier tests, name-set agreement, schema shape, read-only
//! subcommand assertions, and validator rejection cases.
//! Test: this file.

use super::helpers::{enum_arg, limit_arg, map_gh_outcome, plain_arg, repo_arg, run_gh};
use super::*;
use serde_json::json;

fn root() -> PathBuf {
    std::env::current_dir().unwrap()
}

fn l0_tools() -> Vec<Arc<dyn ToolExecutor>> {
    gh_tools(AgentTier::L0Orchestration, root())
}

// --------------------------------------------------------------------------
// The tier gate
// --------------------------------------------------------------------------

#[test]
fn gh_tools_are_denied_to_l1() {
    // The headline requirement of #4170: these tools are grantable to L0 ONLY,
    // enforced in code. An L1 caller gets nothing to register, so no
    // `[tools].allow` entry an L1 persona can write has anything to match.
    assert!(
        gh_tools(AgentTier::L1Standard, root()).is_empty(),
        "an L1-tier caller must receive no GitHub tools"
    );
}

#[test]
fn gh_tools_are_denied_by_default_tier() {
    // Fail-closed on an indeterminate tier. `AgentTier::default()` is what
    // `AgentInfo::tier()` resolves to for an absent, blank, or unrecognized
    // `tier =` declaration (#4200), so this is the "tier could not be
    // determined" case stated at the type level.
    assert_eq!(AgentTier::default(), AgentTier::L1Standard);
    assert!(
        gh_tools(AgentTier::default(), root()).is_empty(),
        "an undeclared/unresolvable tier must deny, never grant"
    );
}

#[test]
fn gh_tools_are_granted_to_l0() {
    let tools = l0_tools();
    assert_eq!(tools.len(), GH_TOOL_NAMES.len());
}

#[test]
fn register_is_a_no_op_for_l1() {
    let mut reg = ToolRegistry::new();
    register(&mut reg, AgentTier::L1Standard, &root());
    for name in GH_TOOL_NAMES {
        assert!(!reg.contains(name), "{name} leaked into an L1 registry");
    }
}

#[test]
fn register_adds_the_full_surface_for_l0() {
    let mut reg = ToolRegistry::new();
    register(&mut reg, AgentTier::L0Orchestration, &root());
    for name in GH_TOOL_NAMES {
        assert!(reg.contains(name), "{name} missing from an L0 registry");
    }
}

#[test]
fn gh_tool_names_match_the_factory_output() {
    // `GH_TOOL_NAMES` is what the skill catalog and the registry tests read;
    // drift between it and the factory would make those tests assert about
    // tools that no longer exist.
    let tools = l0_tools();
    let produced: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert_eq!(produced, GH_TOOL_NAMES);
}

#[test]
fn every_gh_tool_has_a_skill_in_the_builtin_catalog() {
    // The one-skill-per-tool rule (owner ruling, #3933) asserted against the
    // live catalog for THIS family specifically, so a missing row fails here
    // with a precise name rather than only in the crate-wide source scan.
    let catalog = crate::skills::manifest::SkillCatalog::builtin();
    for name in GH_TOOL_NAMES {
        assert!(
            catalog.skill_for_tool(name).is_some(),
            "{name} has no skill wrapping it"
        );
    }
}

// --------------------------------------------------------------------------
// Read-only posture
// --------------------------------------------------------------------------

#[test]
fn gh_tools_expose_only_read_only_subcommands() {
    // A mutating verb must never appear in a tool NAME or in the DESCRIPTION
    // the model reads to decide what a tool does. #4170 explicitly forbids
    // quietly granting merge power; this is the tripwire for a future edit
    // that adds one to this module.
    //
    // Scoped to name + description deliberately: `gh_pr_list`'s `state`
    // parameter legitimately offers the value `"merged"` (a FILTER over
    // existing PRs, not an action), so scanning the whole schema JSON would
    // flag a read-only capability. What must stay clean is what the tool
    // claims to DO.
    for tool in l0_tools() {
        let schema = tool.schema();
        let description = schema["function"]["description"]
            .as_str()
            .expect("every tool declares a description")
            .to_lowercase();
        let text = format!("{} {description}", tool.name());
        for forbidden in [
            "merge", "create", "comment", "close", "edit", "rerun", "delete", "approve", "trigger",
        ] {
            assert!(
                !text.contains(forbidden),
                "{} mentions the mutating verb {forbidden:?} in its name/description; \
                 this module is read-only",
                tool.name()
            );
        }
    }
}

#[test]
fn gh_pr_tools_are_read_only_subcommands() {
    let tools = l0_tools();
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(names.contains(&"gh_pr_list"));
    assert!(names.contains(&"gh_pr_view"));
    assert!(names.contains(&"gh_pr_checks"));
    assert!(
        !names.iter().any(|n| n.contains("merge")),
        "no merge tool may be registered: {names:?}"
    );
}

#[test]
fn gh_ci_tools_are_read_only_subcommands() {
    let tools = l0_tools();
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(names.contains(&"gh_run_list"));
    assert!(names.contains(&"gh_run_view"));
    // `gh run watch` blocks for the life of a run; see `ci.rs`'s doc comment.
    assert!(!names.iter().any(|n| n.contains("watch")));
}

// --------------------------------------------------------------------------
// Schemas
// --------------------------------------------------------------------------

fn tool_named(name: &str) -> Arc<dyn ToolExecutor> {
    l0_tools()
        .into_iter()
        .find(|t| t.name() == name)
        .unwrap_or_else(|| panic!("{name} not registered"))
}

#[test]
fn gh_pr_view_schema_has_the_expected_envelope() {
    let s = tool_named("gh_pr_view").schema();
    assert_eq!(s["type"], "function");
    assert_eq!(s["function"]["name"], "gh_pr_view");
    let required: Vec<&str> = s["function"]["parameters"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(required, vec!["pr"]);
    assert!(
        s["function"]["parameters"]["properties"]
            .get("repo")
            .is_some(),
        "cross-repo reach is part of the L0 grant"
    );
}

#[test]
fn gh_run_view_requires_a_run_id() {
    let s = tool_named("gh_run_view").schema();
    let required: Vec<&str> = s["function"]["parameters"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(required, vec!["run_id"]);
}

#[test]
fn list_tools_require_no_arguments() {
    for name in ["gh_pr_list", "gh_run_list"] {
        let s = tool_named(name).schema();
        assert!(
            s["function"]["parameters"]["required"]
                .as_array()
                .unwrap()
                .is_empty(),
            "{name} should be callable with no arguments"
        );
    }
}

// --------------------------------------------------------------------------
// Operand validation — the flag-injection gate
// --------------------------------------------------------------------------

#[test]
fn plain_arg_rejects_flag_shaped_values() {
    for bad in ["--json", "-q", "--template", "-R"] {
        let err = plain_arg("pr", bad).expect_err("must be rejected");
        assert!(err.contains("must not start with '-'"), "{err}");
    }
}

#[test]
fn plain_arg_rejects_whitespace_and_control_characters() {
    for bad in [
        "4200 --json",
        "a\tb",
        "a\nb",
        "a b",
        "a;b",
        "a|b",
        "a&b",
        "$(x)",
        "`x`",
        "café",
    ] {
        assert!(plain_arg("pr", bad).is_err(), "{bad:?} must be rejected");
    }
    assert!(plain_arg("pr", "").is_err(), "empty must be rejected");
    assert!(
        plain_arg("pr", &"9".repeat(500)).is_err(),
        "an over-long operand must be rejected"
    );
}

#[test]
fn plain_arg_accepts_ordinary_selectors() {
    for good in [
        "4200",
        "feat/4170-l0-github-pr-ci",
        "https://github.com/bobmatnyc/trusty-tools/pull/4200",
        "bobmatnyc",
        "ci.yml",
    ] {
        assert!(plain_arg("pr", good).is_ok(), "{good:?} must be accepted");
    }
}

#[test]
fn repo_arg_requires_owner_slash_repo() {
    assert_eq!(
        repo_arg("bobmatnyc/trusty-tools").unwrap(),
        "bobmatnyc/trusty-tools"
    );
    for bad in ["trusty-tools", "a/b/c", "/b", "a/", "--repo=a/b", "a b/c"] {
        assert!(repo_arg(bad).is_err(), "{bad:?} must be rejected");
    }
}

#[test]
fn limit_arg_clamps_to_the_supported_range() {
    assert_eq!(limit_arg(None, 20), 20);
    assert_eq!(limit_arg(Some(0), 20), 1);
    assert_eq!(limit_arg(Some(5), 20), 5);
    assert_eq!(limit_arg(Some(100_000), 20), 100);
}

#[test]
fn enum_arg_rejects_unlisted_values() {
    assert_eq!(enum_arg("state", "open", &["open", "all"]).unwrap(), "open");
    let err = enum_arg("state", "merged; rm -rf /", &["open", "all"]).expect_err("rejected");
    assert!(err.contains("must be one of open, all"), "{err}");
}

// --------------------------------------------------------------------------
// Execution — argument rejection happens BEFORE any subprocess spawn, so
// these run without `gh` installed or authenticated.
// --------------------------------------------------------------------------

#[tokio::test]
async fn gh_pr_view_rejects_a_flag_shaped_selector() {
    let out = tool_named("gh_pr_view")
        .execute(json!({"pr": "--repo=attacker/repo"}))
        .await;
    assert!(out.is_error(), "got: {}", out.content());
    assert!(out.content().contains("must not start with '-'"));
}

#[tokio::test]
async fn gh_pr_view_rejects_a_missing_selector() {
    let out = tool_named("gh_pr_view").execute(json!({})).await;
    assert!(out.is_error());
    assert!(out.content().contains("'pr' is required"));
}

#[tokio::test]
async fn gh_pr_list_rejects_an_unknown_state() {
    let out = tool_named("gh_pr_list")
        .execute(json!({"state": "everything"}))
        .await;
    assert!(out.is_error());
    assert!(out.content().contains("'state' must be one of"));
}

#[tokio::test]
async fn gh_pr_list_rejects_a_malformed_repo() {
    let out = tool_named("gh_pr_list")
        .execute(json!({"repo": "not-a-slug"}))
        .await;
    assert!(out.is_error());
    assert!(
        out.content()
            .contains("'repo' must be exactly 'owner/repo'")
    );
}

#[tokio::test]
async fn gh_run_view_rejects_a_flag_shaped_run_id() {
    let out = tool_named("gh_run_view")
        .execute(json!({"run_id": "-1 --json url"}))
        .await;
    assert!(out.is_error());
}

#[tokio::test]
async fn gh_run_list_rejects_an_unknown_status() {
    let out = tool_named("gh_run_list")
        .execute(json!({"status": "green"}))
        .await;
    assert!(out.is_error());
    assert!(out.content().contains("'status' must be one of"));
}

#[tokio::test]
async fn gh_run_view_rejects_a_missing_run_id() {
    let out = tool_named("gh_run_view").execute(json!({})).await;
    assert!(out.is_error());
    assert!(out.content().contains("'run_id' is required"));
}

// --------------------------------------------------------------------------
// Outcome mapping — pinned deterministically against `GhOutput` values, so
// the tolerance rule holds without an installed, authenticated `gh` (#5475;
// a test that can only skip in CI proves nothing).
// --------------------------------------------------------------------------

fn gh_out(code: i32, stdout: &str, stderr: &str) -> trusty_common::gh::GhOutput {
    trusty_common::gh::GhOutput::from_parts("pr checks 1", Some(code), stdout, stderr)
}

#[test]
fn map_gh_outcome_tolerates_a_nonzero_exit_when_asked() {
    // THE rule `gh pr checks` depends on: a non-zero exit is check STATE, not
    // a tool failure. If this regresses, every red or pending PR starts
    // reading as `is_error` to the model.
    let out = map_gh_outcome(&gh_out(1, "pending\n", ""), true);
    assert!(
        !out.is_error(),
        "a tolerated non-zero exit must stay a success: {}",
        out.content()
    );
    assert!(out.content().contains("(exit 1)"), "{}", out.content());
    assert!(out.content().contains("pending"), "{}", out.content());
}

#[test]
fn map_gh_outcome_reports_a_nonzero_exit_as_an_error_by_default() {
    let out = map_gh_outcome(&gh_out(1, "", "no such PR"), false);
    assert!(out.is_error());
    assert!(
        out.content().contains("failed (exit 1)"),
        "{}",
        out.content()
    );
    assert!(out.content().contains("no such PR"), "{}", out.content());
}

#[test]
fn map_gh_outcome_notes_empty_output() {
    let out = map_gh_outcome(&gh_out(0, "  \n", ""), false);
    assert!(!out.is_error());
    assert!(out.content().contains("no output"), "{}", out.content());
}

#[tokio::test]
async fn run_gh_reports_a_missing_binary_with_the_login_hint() {
    // The entry point's `NotInstalled` message must survive out to the model.
    let e = trusty_common::gh::GhError::NotInstalled.to_string();
    assert!(e.contains("gh auth login"), "{e}");
}

#[tokio::test]
async fn run_gh_surfaces_the_cli_state_legibly() {
    // Environment-tolerant but non-vacuous: whichever branch this host takes,
    // the payload must be one a model can act on — never a panic and never an
    // empty message.
    let out = run_gh(&root(), &["--version".to_string()], false).await;
    if out.is_error() {
        assert!(
            out.content().contains("gh auth login") || out.content().contains("failed (exit"),
            "unhelpful gh failure: {}",
            out.content()
        );
    } else {
        assert!(!out.content().trim().is_empty());
    }
}
