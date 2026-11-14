//! Regression rows for the bracket-expression secret-name bypass (#10761,
//! #10548): `cat .[!x]nv` and `cat .[^x]nv` ALLOWED on 1.14.0.
//!
//! What: deny rows for every bracket form a POSIX shell, bash or zsh accepts;
//! allow rows for classes that reach no secret-class name; fail-closed rows for
//! brackets the guard cannot read; and the `Grep` tool's glob and path arms.
//! Test: itself.

use crate::commands::pm_guard_secret_brackets::{bracket_readings, path_words, widen_brackets};
use crate::commands::pm_guard_secret_read::{
    evaluate_secret_file_read_command, evaluate_secret_file_read_tool,
};

fn denies(command: &str) -> bool {
    evaluate_secret_file_read_command(command).is_some()
}

#[test]
fn denies_a_negated_or_ranged_bracket_glob_onto_a_secret_name() {
    for command in [
        // The issue's live repro and the brief's named shapes.
        "cat .[!x]nv",
        "cat .[^x]nv",
        "head -n 1 .[!x]nv",
        "grep KEY .e[^z]v",
        "cat ./.[!a-d]nv",
        // Ranges, multi-member and named classes.
        "cat .[a-f]nv",
        "cat .[xyze]nv",
        "cat .[A-F]nv",
        "cat .[[:alpha:]]nv",
        "cat .[![:digit:]]nv",
        "cat .[^[:upper:]]n[[:lower:]]",
        "cat .[!x-z]nv",
        // A `]` first in the class is a member, not the close.
        "cat .[!]x]nv",
        "cat .[]e]nv",
        // A bracket in every position of the name.
        "cat .e[!x]v",
        "cat .en[^x]",
        "cat [!x]env",
        "cat id_[!x]sa",
        "cat id_rs[^x]",
        "cat *.p[!x]m",
        "cat server.[!x]em",
        "cat terraform.tfvar[!x]",
        "cat .net[!x]c",
        // Combined with `?` and `*`.
        "cat .[!x]n?",
        "cat .[!x]n*",
        "cat ?[!x]nv",
        // Under a directory prefix.
        "cat ~/.ssh/id_[!x]sa",
        "cat /repo/.[!x]nv",
        "cat ./config/.e[^z]v",
        // Other readers, and the class inside a substitution.
        "sed -n 1p .[!x]nv",
        "less .[^x]nv",
        "echo \"$(cat .[!x]nv)\"",
        "grep -r KEY --include=.[!x]nv .",
        // Quoting and escapes inside the class are members the shell keeps.
        "cat .[!\"x\"]nv",
        "cat .[!'x']nv",
        "cat .[!x\\ y]nv",
        "cat .[!\"x y\"]nv",
    ] {
        assert!(denies(command), "`{command}` must deny");
    }
}

#[test]
fn allows_a_bracket_glob_that_reaches_no_secret_name() {
    for command in [
        "cat .[!eE]nv",
        "cat .[a-d]nv",
        "cat .[xyz]nv",
        "cat .[[:digit:]]nv",
        "cat id_[!a-zA-Z]sa",
        "ls [a-z]*.rs",
        "cat file[!0-9].txt",
        "grep '[^a-z]' src/lib.rs",
        "tr '[:upper:]' '[:lower:]' < notes.txt",
        "jq '.[0]' data.json",
        "if [[ -f README.md ]]; then echo ok; fi",
        "echo \"[WIP] done\"",
    ] {
        assert_eq!(
            evaluate_secret_file_read_command(command),
            None,
            "`{command}` must allow"
        );
    }
}

/// `.[!e]nv` denies on purpose: bash globbing is case-sensitive, so it matches
/// `.Env`, and this guard classifies names case-insensitively.
#[test]
fn a_negated_class_excluding_one_case_still_reaches_the_other() {
    assert!(denies("cat .Env"));
    assert!(denies("cat .[!e]nv"));
    assert!(!denies("cat .[!eE]nv"));
}

#[test]
fn a_bracket_the_guard_cannot_read_fails_closed() {
    for command in [
        // Unknown class name, multi-character collating element, reversed range.
        "cat .[[:bogus:]]nv",
        "cat .[[.space.]]nv",
        "cat .[z-a]nv",
        // A shell expansion inside the class.
        "cat .[!$X]nv",
        // An unterminated `[` is a literal the guard widens, never drops alone.
        "cat .[nv",
        "cat [.env",
        "cat .env[",
        // More combinations than the guard enumerates widen to `?`.
        "cat .[!x][!x][!x]",
    ] {
        assert!(denies(command), "`{command}` must fail closed");
    }
    // An unbalanced quote inside the class is unlexable: never an allow.
    assert!(denies("cat .[!\"x]nv"));
}

#[test]
fn the_grep_tool_reads_a_bracket_glob_the_same_way() {
    for glob in [".[!x]nv", ".[^x]nv", ".e[^z]v", "id_[!x]sa", "*.p[!x]m"] {
        let input = serde_json::json!({"pattern": "KEY", "path": "/repo", "glob": glob});
        assert!(
            evaluate_secret_file_read_tool("Grep", Some(&input)).is_some(),
            "glob `{glob}` must deny"
        );
    }
    let path = serde_json::json!({"pattern": "KEY", "path": "/repo/.[!x]nv"});
    assert!(evaluate_secret_file_read_tool("Grep", Some(&path)).is_some());
    let allowed = serde_json::json!({"pattern": "KEY", "path": "/repo", "glob": ".[!eE]nv"});
    assert_eq!(evaluate_secret_file_read_tool("Grep", Some(&allowed)), None);
}

#[test]
fn parses_every_bracket_form_a_shell_accepts() {
    assert_eq!(widen_brackets(".[!]x]nv", ""), ".?nv");
    assert_eq!(widen_brackets(".[[:alpha:]]nv", ""), ".?nv");
    assert_eq!(widen_brackets(".[nv", "?"), ".?nv");
    assert_eq!(bracket_readings("id_[r]sa"), vec!["id_rsa".to_string()]);
    let negated = bracket_readings(".[!eE]nv");
    assert!(!negated.iter().any(|r| r == ".env"), "{negated:?}");
    assert!(bracket_readings(".[!e]nv").iter().any(|r| r == ".Env"));
    assert!(bracket_readings(".[a-c]").iter().any(|r| r == ".B"));
    assert!(bracket_readings("[/]").is_empty());
}

#[test]
fn keeps_a_bracket_expression_whole_through_the_cut() {
    // The plain cut comes first and is unchanged; the joined word follows.
    assert_eq!(
        path_words("cat .[!x]nv"),
        vec!["cat", ".[", "x]nv", ".[!x]nv"]
    );
    assert!(path_words("cat .[^[:digit:]]nv").contains(&".[^[:digit:]]nv".to_string()));
    assert!(path_words("cat .[!x\\ y]nv").contains(&".[!x\\ y]nv".to_string()));
    // A quoted name inside a bracket span is still cut out on its own.
    let quoted = path_words(r#"--parameters 'commands=["grep -c K /srv/app/.env"]'"#);
    assert!(quoted.contains(&"/srv/app/.env".to_string()), "{quoted:?}");
    assert_eq!(path_words("cat .[!x"), vec!["cat", ".[", "x"]);
}
