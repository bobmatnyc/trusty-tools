//! Unit tests for `pm_guard_secret_gh_api_delete` (#8875).
//!
//! Every DENY row asks [`evaluate_gh_api_secret_delete`] directly, so a row
//! the #7266 read rule would also refuse still proves this rule; the unified
//! entry point is asked too, so the hook in `pm_guard_secret_nested` is proved.

use super::*;
use crate::commands::pm_guard_secret_read::evaluate_secret_file_read;

/// The unified Bash verdict: `Some(reason)` denies.
fn bash(command: &str) -> Option<String> {
    evaluate_secret_file_read("Bash", Some(&serde_json::json!({ "command": command })))
}

/// Assert this rule, and the unified entry point, refuse every row.
fn assert_rule_denies(rows: &[&str]) {
    for command in rows {
        let reason = evaluate_gh_api_secret_delete(command)
            .unwrap_or_else(|| panic!("`{command}` must be refused by the #8875 rule"));
        assert!(reason.contains("issue #8875"), "{command}: {reason}");
        assert!(bash(command).is_some(), "`{command}` must deny end to end");
    }
}

/// 🔴 REGRESSION (#8875): a DELETE the `pm_guard_secret_nested` walk reaches
/// — a substitution body, a subshell group, a here-document run as code — is
/// refused by THIS rule: the #8875 reason is what the entry point returns.
#[test]
fn the_nested_walk_reaches_a_delete_in_a_body_8875() {
    for command in [
        "echo \"$(gh api -X DELETE repos/o/r/actions/secrets/K)\"",
        "(gh api -X DELETE repos/o/r/actions/secrets/K)",
        "bash <<'EOF'\ngh api -X DELETE repos/o/r/actions/secrets/K\nEOF",
    ] {
        let reason = bash(command).unwrap_or_else(|| panic!("`{command}` must deny"));
        assert!(reason.contains("issue #8875"), "{command}: {reason}");
    }
}

/// 🔴 REGRESSION (#8875): a `gh api` DELETE of a named secret, in every method
/// spelling pflag reads, flag before or after the endpoint, for each scope and
/// area, and inside a wrapper, `sh -c`, `ssh` and `eval`. Every `…/secrets/NAME` row was allowed on
/// 474acf4470; the `ssh` and `$G` rows were allowed on b3e1980304 too.
#[test]
fn denies_a_gh_api_delete_of_any_secret_8875() {
    assert_rule_denies(&[
        "gh api -X DELETE repos/example-org/apex/actions/secrets/APEX_KEY",
        "gh api -XDELETE repos/example-org/apex/actions/secrets/APEX_KEY",
        "gh api -X=DELETE repos/example-org/apex/actions/secrets/APEX_KEY",
        "gh api --method DELETE repos/example-org/apex/actions/secrets/APEX_KEY",
        "gh api --method=DELETE repos/example-org/apex/actions/secrets/APEX_KEY",
        "gh api -X delete repos/example-org/apex/actions/secrets/APEX_KEY",
        "gh api --method=Delete /repos/example-org/apex/actions/secrets/APEX_KEY",
        "gh api repos/example-org/apex/actions/secrets/APEX_KEY -X DELETE",
        "gh api repos/example-org/apex/actions/secrets/APEX_KEY --method=DELETE",
        "gh api -iX DELETE repos/example-org/apex/actions/secrets/APEX_KEY",
        "gh api -X DELETE repos/{owner}/{repo}/environments/prod/secrets/APEX_KEY",
        "gh api -X DELETE orgs/example-org/actions/secrets/APEX_KEY",
        "gh api -X DELETE orgs/example-org/dependabot/secrets/APEX_KEY",
        "gh api -X DELETE user/codespaces/secrets/APEX_KEY",
        "gh api -X DELETE repositories/123/environments/prod/secrets/APEX_KEY",
        "gh api -X DELETE https://api.github.com/repos/o/r/actions/secrets/K",
        "gh api -X DELETE repos/o/r/actions/sec''rets/K",
        "gh api -X DELETE repos/o/r/actions/%73ecrets/K",
        "gh api -X DELETE repos/o/r/actions/secrets",
        "sudo gh api -X DELETE repos/o/r/actions/secrets/K",
        "/opt/homebrew/bin/gh api -X DELETE repos/o/r/actions/secrets/K",
        "cd /tmp && gh api -X DELETE repos/o/r/actions/secrets/K --silent",
        "sh -c 'gh api -X DELETE repos/o/r/actions/secrets/K'",
        "ssh build-host 'gh api -X DELETE repos/o/r/actions/secrets/K'",
        "ssh -p 2222 build-host \"gh api --method=DELETE repos/o/r/actions/secrets/K\"",
        "eval 'gh api -X DELETE repos/o/r/actions/secrets/K'",
        "G=gh; $G api -X DELETE repos/o/r/actions/secrets/K",
    ]);
}

/// 🔴 REGRESSION (#8875 round 2): a DELETE whose endpoint the shell rewrites
/// or feeds in is refused whether or not the command names `secrets` — no row
/// carries that word literally. Each row was allowed on b3e1980304.
#[test]
fn denies_a_delete_whose_endpoint_the_guard_cannot_read_8875() {
    assert_rule_denies(&[
        // (a) the shell decodes `\x72` to `r`.
        "gh api -X DELETE $'repos/o/r/actions/sec\\x72ets/K'",
        // (b) the endpoint arrives on xargs' stdin: gh sees none in argv.
        "printf 'repos/o/r/actions/sec%sets/K' r | xargs gh api -X DELETE",
        // (c) brace expansion.
        "gh api -X DELETE repos/o/r/actions/sec{r..r}ets/K",
        // (d) `EP` was bound by an earlier call.
        "gh api -X DELETE \"$EP\"",
        "gh api -X DELETE repos/$OWNER/$REPO/git/refs/heads/x",
        "gh api -X DELETE \"repos/o/r/actions/$(printf sec)rets/K\"",
        "gh api -X DELETE repos/o/r/actions/sec*/K",
        "gh api -X DELETE",
        // A non-literal program word followed by `api`.
        "$G api -X DELETE \"$EP\"",
        "`which gh` api --method DELETE repos/o/r/actions/sec{r..r}ets/K",
    ]);
}

/// 🔴 REGRESSION (#8875): the #8869 GET listing still passes, and a DELETE of
/// a literal endpoint that is not a secret is left to the other rules.
#[test]
fn keeps_the_get_listing_and_literal_non_secret_deletes_8875() {
    for command in [
        "gh api repos/example-org/apex/environments/production/secrets --jq '.secrets[]|{name,updated_at}'",
        "gh api repos/{owner}/{repo}/actions/secrets/APEX_KEY",
        "gh api -X GET repos/example-org/apex/actions/secrets/APEX_KEY",
        "gh api --method=get repos/example-org/apex/actions/secrets/APEX_KEY -q .name",
        "gh api orgs/example-org/codespaces/secrets --paginate --slurp",
        "gh api -X DELETE repos/example-org/apex/git/refs/heads/old-branch",
        "gh api --method DELETE repos/{owner}/{repo}/issues/comments/123",
        "gh api -X DELETE repos/o/r/actions/variables/NOTE > out.json && echo secrets",
        "gh api -X DELETE 'repos/o/r/actions/runs?per_page=1'",
        "gh api -X POST repos/o/r/issues -f title=x",
        "ssh build-host 'gh api -X DELETE repos/o/r/git/refs/heads/x'",
        "gh secret list --repo example-org/apex",
        // #8875 round 3: normal PM work and the forms the new arms read.
        "gh pr view 8875 --json state,title",
        "gh api repos/o/r/issues/1/comments --paginate",
        "gh api repos/o/r/pulls/$N",
        "gh secret set APEX_KEY --body x -R o/r",
        "gh alias list",
        // Adjacent expansions are no `gh secret` call without a delete word.
        "echo \"$A\" \"$B\" \"$C\" $(echo $(echo x))",
        "gh alias set co 'pr checkout'",
        "curl -s -H \"Authorization: Bearer $T\" https://api.github.com/repos/o/r/issues/1",
        "curl -X DELETE -H \"Authorization: Bearer $T\" https://api.github.com/repos/o/r/git/refs/heads/x",
        "eval eval eval eval eval eval eval eval gh api -X DELETE repos/o/r/git/refs/heads/x",
    ] {
        assert_eq!(evaluate_gh_api_secret_delete(command), None, "{command}");
        assert_eq!(bash(command), None, "{command} must allow");
    }
}

/// Nine `eval` layers around a literal non-secret DELETE: one past
/// [`MAX_DEPTH`], so only the depth cap refuses it.
const NINE_EVALS: &str =
    "eval eval eval eval eval eval eval eval eval gh api -X DELETE repos/o/r/git/refs/heads/x";

/// 🔴 REGRESSION (#8875 round 3): the owner-ruled forms. Each row was allowed
/// by the rule at 8a451f2698: an unlexable call (the critic's here-document
/// row), a DELETE hidden by a brace in the method, a rewritten `api` word or
/// program word, a rewritten shell's `-c` text, `gh secret delete`, a
/// `gh alias` that deletes, a `curl` DELETE of a secrets URL, and nesting past
/// the depth cap.
#[test]
fn denies_the_round_three_forms_8875() {
    assert_rule_denies(ROUND_THREE_ROWS);
}

/// The rows of `denies_the_round_three_forms_8875`.
const ROUND_THREE_ROWS: &[&str] = &[
    "gh api -X DEL\"ETE\" repos/o/r/actions/sec{r..r}ets/K --jq . <<EOF\nit's\nEOF",
    "gh api -X D{E..E}LETE repos/o/r/issues/1 --jq . <<EOF\nit's\nEOF",
    "gh alias set rmk 'api -X D{E..E}LETE repos/o/r/actions/secrets/K'",
    "A=api; gh $A -X DELETE repos/o/r/actions/secrets/K",
    "gh {api,} -X DELETE repos/o/r/actions/secrets/K",
    "g[h] api -X DELETE repos/o/r/actions/secrets/K",
    "/opt/homebrew/bin/g[h] api -X DELETE repos/o/r/actions/secrets/K",
    "S=sh; $S -c 'gh api -X DELETE repos/o/r/actions/secrets/K'",
    "gh secret delete APEX_KEY",
    "gh secret delete APEX_KEY -R example-org/apex --env production",
    "gh secret delete APEX_KEY --org example-org",
    "gh secret remove APEX_KEY -R o/r",
    "gh secret -R o/r delete APEX_KEY",
    "curl -X DELETE -H \"Authorization: Bearer $GH_TOKEN\" https://api.github.com/repos/o/r/actions/secrets/K",
    "curl --request DELETE https://api.github.com/repos/o/r/actions/secrets/K",
    "curl -sXDELETE https://api.github.com/orgs/o/actions/secrets/K",
    "curl --request=DELETE \"https://api.github.com/repos/o/r/environments/prod/secrets/K\"",
    "gh alias set rmk 'api -X DELETE repos/o/r/actions/secrets/K'",
    "gh alias set rmref 'api --method DELETE repos/o/r/git/refs/heads/$1'",
    "gh alias set rms 'secret delete $1'",
    "gh alias set --shell rms 'gh api -X DELETE repos/o/r/actions/secrets/$1'",
    "gh alias import aliases.yml",
    NINE_EVALS,
];

/// #8875 Fail-Open Check: one row per arm that cannot read the call, asked
/// of the rule directly, so a fail-open mutation of any arm leaves a row red.
#[test]
fn every_gh_api_delete_arm_fails_closed_8875() {
    let rows: &[(&str, &str)] = &[
        // #8875 round 3: no `secrets` or `delete` word, so only the
        // unlexable arm can refuse it.
        (
            "unlexable segment",
            "gh api repos/o/r/issues/1 --jq . <<EOF\nit's\nEOF",
        ),
        // `--method`, not `-X`: the curl arm, which also reads a rewritten
        // program word, does not know it, so only the `api` arm can refuse.
        (
            "rewritten subcommand",
            "gh $A --method DELETE repos/o/r/actions/secrets/K",
        ),
        (
            "glob program word",
            "g[h] api -X DELETE repos/o/r/actions/secrets/K",
        ),
        (
            "rewritten shell -c",
            "$S -c 'gh api -X DELETE repos/o/r/actions/secrets/K'",
        ),
        ("gh secret delete", "gh secret delete APEX_KEY -R o/r"),
        (
            "gh alias api DELETE",
            "gh alias set rm 'api -X DELETE repos/o/r/git/refs/heads/x'",
        ),
        (
            "gh alias secret delete",
            "gh alias set rms 'secret delete $1'",
        ),
        ("gh alias import", "gh alias import aliases.yml"),
        (
            "curl DELETE",
            "curl -X DELETE https://api.github.com/repos/o/r/actions/secrets/K",
        ),
        ("depth cap", NINE_EVALS),
        (
            "non-literal method",
            "gh api -X \"$M\" repos/o/r/actions/secrets/K",
        ),
        (
            "flag missing its value",
            "gh api repos/o/r/actions/secrets/K -X",
        ),
        (
            "unknown long option",
            "gh api --frobnicate DELETE repos/o/r/actions/secrets/K",
        ),
        (
            "unknown short option",
            "gh api -Z DELETE repos/o/r/issues/1",
        ),
        ("unknown option, expansion", "gh api --frobnicate x \"$EP\""),
        (
            "two endpoints",
            "gh api -X DELETE repos/o/r/issues/1 repos/o/r/issues/2",
        ),
        ("absent endpoint", "echo x | xargs gh api -X DELETE"),
        ("non-literal endpoint", "gh api -X DELETE \"$EP\""),
        (
            "non-literal program word",
            "$G api -X DELETE repos/o/r/actions/secrets/K",
        ),
        (
            "ssh operand",
            "ssh h 'gh api -X DELETE repos/o/r/actions/secrets/K'",
        ),
    ];
    for (arm, command) in rows {
        assert!(
            evaluate_gh_api_secret_delete(command).is_some(),
            "{arm}: `{command}` must deny"
        );
    }
}

/// 🔴 REGRESSION (#9006): a Python here-document that edits a local file,
/// with a line that does not lex and subscript pairs such as `d[k] x[0]`,
/// names no secrets or delete and is no `gh api` call. Denied on origin/main.
#[test]
fn a_python_subscript_pair_is_no_gh_api_call_9006() {
    for command in [
        "python3 - <<'EOF'\n\
         p = 'docs/notes.md'\n\
         s = open(p).read()\n\
         print('it's d[k] x[0]')\n\
         open(p, 'w').write(s.replace('old', 'new'))\n\
         EOF",
        "python3 - <<'EOF'\n\
         p = 'src/x_tests.rs'\n\
         print('it's the `cd` in d[k] x[0]')\n\
         s = open(p).read().replace('\"cd $WT && x\"', '\"cd $WT && y\"')\n\
         open(p, 'w').write(s)\n\
         EOF",
    ] {
        assert_eq!(evaluate_gh_api_secret_delete(command), None, "{command}");
        assert_eq!(bash(command), None, "{command}");
    }
}

/// #9006 bound: a glob that can still spell `gh`, `api` or `curl` — `g?`,
/// `g[h]`, `*`, a case-folded `G?`, or syntax the glob reader does not model —
/// keeps the deny, in a body that does not lex and in one that does.
#[test]
fn a_glob_that_can_spell_gh_still_denies_9006() {
    assert_rule_denies(&[
        "python3 - <<'EOF'\nprint('it's')\nos.system('g? api -X DELETE repos/o/r/actions/secrets/X')\nEOF",
        "python3 - <<'EOF'\nprint('it's')\nos.system('g[h] a?i -X DELETE repos/o/r/actions/secrets/X')\nEOF",
        "python3 - <<'EOF'\nprint('it's')\nos.system('* api -X DELETE repos/o/r/actions/secrets/X')\nEOF",
        "python3 - <<'EOF'\nprint('it's')\nos.system('G? api -X DELETE repos/o/r/actions/secrets/X')\nEOF",
        "python3 - <<'EOF'\nprint('it's')\nos.system('g?<1-9> api -X DELETE repos/o/r/actions/secrets/X')\nEOF",
        "/opt/homebrew/bin/g[h] api -X DELETE repos/o/r/actions/secrets/X",
        "g? secret delete NAME",
        "c?rl -X DELETE https://api.github.com/repos/o/r/actions/secrets/X",
    ]);
    // The glob reader itself, on the bytes the matcher folds.
    for (pattern, name, can) in [
        ("g?", "gh", true),
        ("*h", "gh", true),
        ("g[!x]", "gh", true),
        ("d[k]", "gh", false),
        ("x[0]", "api", false),
        ("a*i", "api", true),
        ("x?y?z", "gh", false),
        ("g[h", "gh", true),
        ("<1-9>", "gh", true),
    ] {
        assert_eq!(glob_could_match(pattern, name), can, "{pattern} vs {name}");
    }
}
