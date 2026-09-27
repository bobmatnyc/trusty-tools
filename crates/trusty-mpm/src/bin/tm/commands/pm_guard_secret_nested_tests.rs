//! Tests for `pm_guard_secret_nested` (#8756 round 2): the secret rules read
//! the body of every command substitution the shell would run.
//!
//! Each case goes through `evaluate_secret_file_read`, the entry `pm_guard`
//! calls, so an allow is an allow from every secret rule. A failure lists
//! every command that misbehaved.

use crate::commands::pm_guard_secret_read::evaluate_secret_file_read;

/// The secret guard's verdict on one Bash `command`.
fn verdict(command: &str) -> Option<String> {
    let input = serde_json::json!({ "command": command });
    evaluate_secret_file_read("Bash", Some(&input))
}

/// Every command must deny with a reason carrying `marker`.
fn assert_denies(marker: &str, commands: &[&str]) {
    let leaked: Vec<&str> = commands
        .iter()
        .copied()
        .filter(|c| !verdict(c).is_some_and(|r| r.contains(marker)))
        .collect();
    assert!(leaked.is_empty(), "must deny under {marker}: {leaked:#?}");
}

fn assert_allows(commands: &[&str]) {
    let denied: Vec<(&str, String)> = commands
        .iter()
        .copied()
        .filter_map(|c| verdict(c).map(|r| (c, r)))
        .collect();
    assert!(denied.is_empty(), "must allow: {denied:#?}");
}

/// #8756 round 2: a process dump inside `$( … )` or backticks, quoted or not,
/// nested or bound to a variable, still runs the dump.
#[test]
fn refuses_a_process_dump_inside_a_command_substitution() {
    assert_denies(
        "#8756",
        &[
            "echo \"$(pm2 jlist)\"",
            "echo $(pm2 jlist)",
            "echo `pm2 jlist`",
            "echo \"`pm2 describe 0`\"",
            "x=$(launchctl print gui/501/x); echo \"$x\"",
            "echo \"$(echo $(pm2 env 0))\"",
            "echo \"$(launchctl print gui/501/com.example.daemon)\"",
            "echo \"${X:-$(pm2 jlist)}\"",
            "cat <<< \"$(pm2 jlist)\"",
        ],
    );
}

/// #8756 round 2: the same through a shell wrapper's string, a process
/// substitution, and an unquoted-delimiter here-document, whose body the
/// shell expands.
#[test]
fn refuses_a_dump_through_a_wrapper_or_process_substitution() {
    assert_denies(
        "#8756",
        &[
            "bash -lc 'echo \"$(pm2 jlist)\"'",
            "sh -c \"echo \\$(pm2 prettylist)\"",
            "diff <(pm2 jlist) saved.json",
            "cat <<EOF\n$(pm2 jlist)\nEOF",
            "ssh host <<'EOF'\npm2 jlist\nEOF",
        ],
    );
}

/// #8756 round 2: `eval` runs its joined operands as a command, quoted or
/// not, alone or inside a substitution.
#[test]
fn refuses_a_dump_run_through_eval() {
    assert_denies(
        "#8756",
        &[
            "eval \"pm2 jlist\"",
            "eval pm2 jlist",
            "eval 'pm2 prettylist'",
            "eval -- \"pm2 env 0\"",
            "eval \"launchctl print gui/501/x\"",
            "command eval \"pm2 describe 0\"",
            "echo \"$(eval \"pm2 jlist\")\"",
            "x=$(eval 'launchctl print gui/501/x'); echo \"$x\"",
            "eval \"sh -c 'pm2 jlist'\"",
        ],
    );
    assert_denies("#7648", &["eval \"docker exec web env\""]);
}

/// #8756 round 2: an `eval` of a routine tool's output is decided by the tool
/// it runs, so the shell-setup idioms still run.
#[test]
fn allows_eval_of_a_routine_command() {
    assert_allows(&[
        "eval \"$(ssh-agent -s)\"",
        "eval \"$(direnv export bash)\"",
        "eval \"$(tm completions zsh)\"",
        "eval \"pm2 ls\"",
    ]);
}

/// #8756 round 2: every spelling of a shell's `-c` option runs its command
/// string — a flag after `-c`, a cluster not ending in `c`, and `--`.
#[test]
fn refuses_a_dump_through_every_dash_c_spelling() {
    assert_denies(
        "#8756",
        &[
            "bash -c \"pm2 jlist\"",
            "sh -c 'pm2 jlist'",
            "zsh -c \"launchctl print gui/501/x\"",
            "bash -lc \"pm2 env 0\"",
            "bash -ce \"pm2 jlist\"",
            "bash -c -e 'pm2 jlist'",
            "sh -c -- 'pm2 jlist'",
            "bash -cl 'launchctl print gui/501/x'",
            "bash -o pipefail -c 'pm2 prettylist'",
        ],
    );
    assert_allows(&["bash -ce \"pm2 ls\"", "bash script.sh -c 'pm2 jlist'"]);
}

/// #8756 round 2: a subshell group runs its body too, alone or inside a
/// substitution.
#[test]
fn refuses_a_dump_inside_a_subshell_group() {
    assert_denies(
        "#8756",
        &[
            "(pm2 jlist)",
            "echo \"$( (pm2 jlist) )\"",
            "(cd /tmp && pm2 env 0)",
        ],
    );
    assert_denies("#7648", &["(docker exec web env)"]);
    assert_allows(&["(cd /tmp && pm2 ls)", "echo \"(pm2 jlist)\""]);
}

/// #8756 round 2: a pod or container env dump inside a substitution.
#[test]
fn refuses_a_pod_env_dump_inside_a_command_substitution() {
    assert_denies(
        "#7648",
        &[
            "echo \"$(docker exec web env)\"",
            "echo `kubectl exec web -- env`",
        ],
    );
}

/// #8756 round 2: a secret-file read and a credential print inside a
/// substitution. Both rules already refuse these unwrapped.
#[test]
fn refuses_a_secret_read_inside_a_command_substitution() {
    assert_denies(
        "refused",
        &[
            "echo \"$(cat .env)\"",
            "echo \"$(echo $(sed -n 1p terraform.tfvars))\"",
            "bash -lc 'echo \"$(cat .env)\"'",
            "echo \"$(gcloud auth print-access-token)\"",
        ],
    );
}

/// #8756 round 2: a substitution that does not close, but names a dump, fails
/// closed; one naming none allows.
#[test]
fn refuses_an_unclosed_substitution_that_names_a_dump() {
    assert_denies(
        "#8756",
        &[
            "echo \"$(pm2 jlist\"",
            "echo $(pm2 jlist",
            "echo `pm2 jlist",
        ],
    );
    assert_allows(&["echo $(date"]);
}

/// #8756 round 2: nesting past the depth bound is flattened and read, so a
/// dump at the bottom still denies and a benign one still allows.
#[test]
fn bounds_nesting_and_still_refuses_a_dump() {
    let nest = |inner: &str| {
        let depth = 40;
        format!("{}{inner}{}", "echo $(".repeat(depth), ")".repeat(depth))
    };
    assert_denies("#8756", &[&nest("pm2 jlist")]);
    assert_allows(&[&nest("date")]);
}

/// #8756 round 2: routine commands carrying a substitution, or naming a dump
/// verb as text, still run.
#[test]
fn allows_routine_commands_that_carry_a_substitution() {
    assert_allows(&[
        "git commit -m \"refuse pm2 jlist and launchctl print\"",
        "echo \"$(date)\"",
        "echo \"$(git rev-parse HEAD)\"",
        "gh pr comment 1 --body \"$(cat notes.md)\"",
        "echo '$(pm2 jlist)'",
        "echo \"$(pm2 pid api)\"",
        r#"git commit -m "refuse \$(pm2 jlist) and \`pm2 env 0\`""#,
        r"echo \$(pm2 jlist)",
        "gh pr create --title t --body \"$(cat <<'EOF'\nRefuse pm2 jlist and launchctl print \
         inside a substitution.\nEOF\n)\"",
    ]);
}

/// #8756 round 2: a quoted-delimiter here-document body is literal text, so a
/// commit message naming a dump allows; an unquoted one expands `$( … )`.
#[test]
fn a_heredoc_body_runs_only_when_its_delimiter_is_unquoted() {
    assert_allows(&[
        "git commit -F - <<'EOF'\nfix: refuse `$(pm2 jlist)` in a substitution\nEOF",
        "git commit -F - <<'EOF'\nfix: refuse pm2 jlist and launchctl print\nEOF",
        "git commit -F - <<EOF\nfix: guard the $(git rev-parse --short HEAD) dump rules\nEOF",
        "git commit -F - <<EOF\nfix: refuse \\$(pm2 jlist)\nEOF",
    ]);
    assert_denies("#8756", &["git commit -F - <<EOF\nfix: $(pm2 jlist)\nEOF"]);
}

/// #8596: a credential captured into a variable or a header never reaches
/// tool output, so the capture forms stay allowed; the credential rule follows
/// substitutions itself and is not re-run on their bodies.
#[test]
fn keeps_the_credential_capture_forms_allowed() {
    assert_allows(&[
        "TOKEN=$(gcloud auth print-access-token)",
        "curl -sS -H \"Authorization: Bearer $(gcloud auth print-access-token)\" \
         https://example.test/v1",
    ]);
}
