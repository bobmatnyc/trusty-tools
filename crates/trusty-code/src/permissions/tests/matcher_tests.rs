//! Precedence-table and subject-extraction tests for
//! `crate::permissions::matcher` (#7948).
//!
//! Why: a wrong precedence implementation is invisible until it silently
//! allows something, so each precedence axis is pinned on its own.
//! What: arg-rule over tool-rule, longest literal prefix, the deny>ask>allow
//! tie, unmatched-is-none, multi-subject calls, and `subjects_for`.
//! Test: this module.

use std::path::Path;

use serde_json::json;

use crate::permissions::config::RuleDecision;
use crate::permissions::matcher::{normalise_path_subject, subjects_for, subjects_for_in_root};

use super::agent_with_permissions;

/// The working root the #7948 absolute-path tests below spell their subjects
/// under. Lexical matching means no such directory has to exist.
const ROOT: &str = "/Users/me/proj";

/// Evaluate `tool`/`subject` against a map built from `yaml`.
fn eval(yaml: &str, tool: &str, subject: Option<&str>) -> Option<(RuleDecision, String)> {
    agent_with_permissions(yaml)
        .permissions
        .as_ref()
        .expect("fixture declares a map")
        .evaluate(tool, subject)
        .map(|m| (m.decision, m.rule))
}

/// An argument-level rule beats a tool-level rule for the same call.
#[test]
fn arg_level_rule_beats_tool_level_rule() {
    // The tool-level glob has the LONGER literal prefix, so only the level can
    // be what decides this.
    let yaml = "permissions:\n  \"bas*\": deny\n  bash:\n    \"g*\": allow\n";
    let got = eval(yaml, "bash", Some("git status --short")).expect("a rule matches");
    assert_eq!(got.0, RuleDecision::Allow, "got {got:?}");
}

/// Among argument rules that all match, the longest literal prefix wins.
#[test]
fn longest_literal_prefix_wins_among_arg_rules() {
    let yaml =
        "permissions:\n  bash:\n    \"*\": ask\n    \"git *\": deny\n    \"git status*\": allow\n";
    let decide = |subject| eval(yaml, "bash", Some(subject)).expect("a rule matches").0;
    assert_eq!(decide("git status --short"), RuleDecision::Allow);
    assert_eq!(decide("git push --force"), RuleDecision::Deny);
    assert_eq!(decide("ls -la"), RuleDecision::Ask);
}

/// At equal specificity the safest decision wins: deny > ask > allow.
#[test]
fn deny_beats_ask_beats_allow_at_equal_specificity() {
    // Both globs share a literal prefix of 1 (`b`) and match `bash`.
    let got = eval(
        "permissions:\n  \"b*\": allow\n  \"b?sh\": ask\n",
        "bash",
        None,
    );
    assert_eq!(got.expect("matches").0, RuleDecision::Ask);
    let got = eval(
        "permissions:\n  \"b?sh\": deny\n  \"b*\": ask\n",
        "bash",
        None,
    );
    assert_eq!(got.expect("matches").0, RuleDecision::Deny);
}

/// A tool no rule matches yields no rule at all — which callers read as allow.
#[test]
fn unmatched_tool_has_no_rule() {
    assert!(
        eval(
            "permissions:\n  read_file: deny\n",
            "write_file",
            Some("a.txt")
        )
        .is_none()
    );
}

/// An argument table whose subject globs all miss contributes nothing.
#[test]
fn arg_table_with_no_matching_subject_contributes_nothing() {
    let yaml = "permissions:\n  bash:\n    \"git *\": deny\n";
    assert!(eval(yaml, "bash", Some("ls -la")).is_none());
    assert!(eval(yaml, "bash", None).is_none());
}

/// The reported rule text names the argument pattern, not just the tool.
#[test]
fn rule_text_names_the_arg_pattern() {
    let (_, rule) = eval(
        "permissions:\n  bash:\n    \"rm *\": deny\n",
        "bash",
        Some("rm -rf build"),
    )
    .expect("a rule matches");
    assert_eq!(rule, "bash[rm *]");
    let (_, rule) = eval(
        "permissions:\n  read_file: deny\n",
        "read_file",
        Some("a.txt"),
    )
    .expect("a rule matches");
    assert_eq!(rule, "read_file");
}

/// (#7948 regression) `write_files` is decided by its most restrictive path —
/// a denied path cannot hide behind an allowed one.
#[test]
fn write_files_is_decided_by_its_most_restrictive_path() {
    let agent = agent_with_permissions(
        "permissions:\n  write_files:\n    \"src/**\": allow\n    \".github/**\": deny\n",
    );
    let map = agent.permissions.as_ref().expect("map");
    let args = json!({"files": [
        {"path": "src/lib.rs", "content": "x"},
        {"path": ".github/workflows/ci.yml", "content": "y"}
    ]});
    let got = map
        .evaluate_call("write_files", &subjects_for("write_files", &args))
        .expect("a rule matches");
    assert_eq!(got.decision, RuleDecision::Deny, "got {got:?}");
    assert_eq!(got.rule, "write_files[.github/**]");
}

/// `bash` is discriminated by its `command` argument.
#[test]
fn subject_for_bash_is_the_command() {
    assert_eq!(
        subjects_for("bash", &json!({"command": "git status"})),
        vec!["git status"]
    );
}

/// The file tools are discriminated by their `path` argument.
#[test]
fn subject_for_read_file_is_the_path() {
    for tool in ["read_file", "write_file", "edit", "list_dir"] {
        assert_eq!(
            subjects_for(tool, &json!({"path": "src/main.rs"})),
            vec!["src/main.rs"],
            "{tool}"
        );
    }
}

/// `glob`/`grep` are discriminated by their `pattern` argument.
#[test]
fn subject_for_grep_is_the_pattern() {
    for tool in ["glob", "grep"] {
        assert_eq!(
            subjects_for(tool, &json!({"pattern": "**/*.rs"})),
            vec!["**/*.rs"],
            "{tool}"
        );
    }
}

/// `write_files` yields every entry's path.
#[test]
fn subjects_for_write_files_are_every_path() {
    let args = json!({"files": [{"path": "a.py"}, {"path": "pkg/b.py"}, {"content": "no path"}]});
    assert_eq!(subjects_for("write_files", &args), vec!["a.py", "pkg/b.py"]);
}

/// MCP tools have NO subject, even when their args carry a recognised key.
#[test]
fn mcp_tools_have_no_subject() {
    let args = json!({"path": "/etc/passwd", "command": "rm -rf /"});
    assert!(subjects_for("mcp__fixture__search", &args).is_empty());
}

/// A tool whose arguments name none of the known subject keys has no subject.
#[test]
fn subject_is_none_when_no_known_argument_is_present() {
    assert!(subjects_for("finish_task", &json!({"status": "success"})).is_empty());
    assert!(subjects_for("bash", &json!({"command": 42})).is_empty());
}

// ── Path normalisation (#7948) ──────────────────────────────────────────────

/// Lexical normalisation folds every spelling without touching the filesystem.
#[test]
fn normalise_path_subject_folds_lexical_spellings() {
    for (raw, want) in [
        ("secrets/x", "secrets/x"),
        ("./secrets/x", "secrets/x"),
        ("secrets/./x", "secrets/x"),
        ("a/../secrets/x", "secrets/x"),
        ("secrets//x", "secrets/x"),
        ("secrets/", "secrets"),
        ("a/..", "."),
        ("../secrets/x", "../secrets/x"),
        ("a/../../secrets/x", "../secrets/x"),
        ("/etc/../secrets/x", "/secrets/x"),
        ("/../x", "/x"),
    ] {
        assert_eq!(normalise_path_subject(raw), want, "{raw}");
    }
    assert_eq!(
        subjects_for("read_file", &json!({"path": "./a.rs"})),
        vec!["./a.rs", "a.rs"]
    );
}

/// (#7948 regression) A path deny holds however the path is spelled, for every
/// tool whose subject is a path.
#[test]
fn path_deny_holds_for_every_lexical_spelling() {
    let agent = agent_with_permissions(
        "permissions:\n  read_file:\n    \"secrets/**\": deny\n  edit:\n    \"secrets/**\": deny\n  write_files:\n    \"src/**\": allow\n    \"secrets/**\": deny\n",
    );
    let map = agent.permissions.as_ref().expect("map");
    let mut leaked = Vec::new();
    for path in [
        "secrets/x",
        "./secrets/x",
        "secrets/./x",
        "a/../secrets/x",
        "secrets//x",
    ] {
        for (tool, args) in [
            ("read_file", json!({"path": path})),
            ("edit", json!({"path": path, "old": "a", "new": "b"})),
            (
                "write_files",
                json!({"files": [{"path": "src/lib.rs"}, {"path": path}]}),
            ),
        ] {
            let got = map.evaluate_call(tool, &subjects_for(tool, &args));
            if got.map(|m| m.decision) != Some(RuleDecision::Deny) {
                leaked.push(format!("{tool} {path}"));
            }
        }
    }
    assert!(leaked.is_empty(), "deny bypassed for: {leaked:?}");
}

/// A `..` above the root keeps its `..`, and the raw form is decided too, so
/// neither a climb nor a fold is ever more permissive than the raw path.
#[test]
fn path_climbing_above_the_root_is_never_more_permissive() {
    let agent = agent_with_permissions(
        "permissions:\n  read_file:\n    \"../**\": deny\n    \"secrets/**\": deny\n",
    );
    let map = agent.permissions.as_ref().expect("map");
    for path in ["../x", "a/../../x", "./../x", "secrets/../x"] {
        let got = map.evaluate_call(
            "read_file",
            &subjects_for("read_file", &json!({"path": path})),
        );
        assert_eq!(got.map(|m| m.decision), Some(RuleDecision::Deny), "{path}");
    }
}

/// (#7948) A `bash` rule matches the command TEXT, not the program that runs.
///
/// Why: pinning the documented limit rather than leaving an operator to infer
/// `rm *: deny` means "this agent cannot delete files". `sh -c`, an absolute
/// path, and a second statement after `;` each reach `rm` without the subject
/// ever matching `rm *`. Closing this needs command parsing, which #7948 did
/// not attempt; a reader who changes that must change this test deliberately.
#[test]
fn a_bash_deny_does_not_match_an_indirectly_spelled_command() {
    let yaml = "permissions:\n  bash:\n    \"rm *\": deny\n";
    assert_eq!(
        eval(yaml, "bash", Some("rm x")),
        Some((RuleDecision::Deny, "bash[rm *]".to_string())),
        "the literal spelling is the one the rule catches"
    );
    for command in ["sh -c 'rm x'", "/bin/rm x", "true; rm x"] {
        assert_eq!(
            eval(yaml, "bash", Some(command)),
            None,
            "a bash rule is advisory, not a sandbox: {command} reaches rm"
        );
    }
}

/// (#7948 regression) A path rule an operator wrote RELATIVE also denies the
/// absolute in-root spelling of the same file.
///
/// Why: `tools::fs::scoped_path` accepts an absolute path inside the working
/// root (`scoped_path_accepts_absolute_inside_dir`), so `/<root>/secrets/x`
/// reaches the same file `secrets/x` does. Before the root was threaded in the
/// subject stayed absolute and the relative deny missed it entirely.
#[test]
fn path_deny_holds_for_the_absolute_in_root_spelling() {
    let root = Path::new(ROOT);
    let agent = agent_with_permissions(
        "permissions:\n  read_file:\n    \"secrets/**\": deny\n  write_files:\n    \"src/**\": allow\n    \"secrets/**\": deny\n",
    );
    let map = agent.permissions.as_ref().expect("map");
    let mut leaked = Vec::new();
    for path in [
        "/Users/me/proj/secrets/x",
        "/Users/me/proj/./secrets/x",
        "/Users/me/proj/src/../secrets/x",
    ] {
        for (tool, args) in [
            ("read_file", json!({"path": path})),
            (
                "write_files",
                json!({"files": [{"path": "src/lib.rs"}, {"path": path}]}),
            ),
        ] {
            let got = map.evaluate_call(tool, &subjects_for_in_root(tool, &args, Some(root)));
            if got.map(|m| m.decision) != Some(RuleDecision::Deny) {
                leaked.push(format!("{tool} {path}"));
            }
        }
    }
    assert!(leaked.is_empty(), "deny bypassed for: {leaked:?}");
}

/// An absolute in-root path yields its root-relative spelling ALONGSIDE the
/// forms `subjects_for` already produced, never instead of them. The root's own
/// entry is not a file subject, so it contributes no relative form.
#[test]
fn absolute_in_root_path_also_yields_its_root_relative_form() {
    let root = Path::new(ROOT);
    assert_eq!(
        subjects_for_in_root(
            "read_file",
            &json!({"path": "/Users/me/proj/secrets/x"}),
            Some(root)
        ),
        vec!["/Users/me/proj/secrets/x", "secrets/x"],
    );
    assert_eq!(
        subjects_for_in_root("read_file", &json!({"path": ROOT}), Some(root)),
        vec![ROOT],
    );
}

/// A path outside the root adds no relative form, and a root sharing only a
/// partial segment does not count as a prefix — `/Users/me/project` is not
/// under `/Users/me/proj`.
#[test]
fn absolute_path_outside_the_root_yields_no_relative_form() {
    let root = Path::new(ROOT);
    for path in ["/etc/passwd", "/Users/me/project/secrets/x"] {
        assert_eq!(
            subjects_for_in_root("read_file", &json!({"path": path}), Some(root)),
            vec![path.to_string()],
            "{path}"
        );
    }
    // With no root the absolute spelling is the only form — exactly what
    // `subjects_for` produced before #7948 threaded a root through.
    assert_eq!(
        subjects_for("read_file", &json!({"path": "/Users/me/proj/secrets/x"})),
        vec!["/Users/me/proj/secrets/x"],
    );
}
