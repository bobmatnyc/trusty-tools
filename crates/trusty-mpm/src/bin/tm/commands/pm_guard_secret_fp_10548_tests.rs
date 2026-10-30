//! #10548: the credential rules' false-positive catalogue (P1-P10) and the
//! incident log's true positives and fail-closed cases. Every fixture is a
//! fake placeholder; nothing is executed.
//!
//! The false-positive rows each failed on 5f0290d4d7. The true-positive and
//! fail-closed rows must keep refusing; each was proven to fail under a
//! fail-open mutation of the rule that refuses it.

use super::*;

use std::os::unix::fs::PermissionsExt;

use crate::commands::pm_guard_secret_read::evaluate_secret_file_read_in;

/// A Keychain read that prints its value — refused inline (#8596).
const KEYCHAIN_READ: &str = "security find-generic-password -s fake-svc -w\n";

/// Write `body` to `dir/name`, executable when `exec`, and return the path.
fn file(dir: &Path, name: &str, body: &str, exec: bool) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&path, body).expect("write");
    let mode = if exec { 0o755 } else { 0o644 };
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    path
}

/// The whole credential verdict for a Bash `command` run from `cwd`: the
/// inline rules (#7266, #8596, #8875, the nested rules) and the script-body
/// rule (#8879), as `pm_guard_architect_envfile` composes them.
fn verdict(command: &str, cwd: &Path) -> Option<String> {
    let input = serde_json::json!({ "command": command });
    evaluate_secret_file_read_in("Bash", Some(&input), Some(cwd))
        .or_else(|| evaluate_script_body_secret_read(command, cwd))
}

fn allows(command: &str, cwd: &Path) {
    let reason = verdict(command, cwd);
    assert_eq!(reason, None, "`{command}` must allow");
}

/// Every row of `commands` allows from `cwd`; the refused ones are reported
/// together, each with its reason.
fn all_allow(commands: &[String], cwd: &Path) {
    let refused: Vec<(&String, String)> = commands
        .iter()
        .filter_map(|c| verdict(c, cwd).map(|r| (c, r)))
        .collect();
    assert!(refused.is_empty(), "must allow: {refused:#?}");
}

/// A `ci.yml` that names a `*secrets*` path and a `secrets.` expression, as
/// this repository's own workflow does.
const WORKFLOW: &str = "jobs:\n  ui:\n    steps:\n      - run: npm ci\n        \
                        working-directory: crates/app/ui-secrets\n      - env:\n          \
                        HF_TOKEN: ${{ secrets.HF_TOKEN }}\n";

/// 🔴 REGRESSION (#10548 P1): the alternatives of a `case` arm `a | b)` are
/// patterns, not scripts the body runs, so a workflow file named in one is
/// never judged as an `sh` body.
#[test]
fn a_multi_pattern_case_arm_runs_no_script_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(dir.path(), ".github/workflows/ci.yml", WORKFLOW, false);
    file(dir.path(), "tools/ci-apt-install.sh", "echo apt\n", true);
    file(
        dir.path(),
        "t.sh",
        "case \"$1\" in\n  .github/workflows/ci.yml | tools/ci-apt-install.sh) echo canary ;;\n\
         esac\n",
        false,
    );
    file(
        dir.path(),
        "multi.sh",
        "case \"$p\" in\n  .github/workflows/ci.yml | \\\n    docs/a.md | \\\n    \
         .github/workflows/ci.yml)\n    return 0\n    ;;\n  x) echo x ;; y | z) echo y ;;\nesac\n",
        false,
    );
    allows("bash t.sh", dir.path());
    allows("bash multi.sh", dir.path());
}

/// 🔴 REGRESSION (#10548 P2): an apostrophe in a comment unbalances the body,
/// so the `|` inside the quoted `sed` expression cuts it; the lone `/`
/// between those two cuts is a directory, which runs nothing.
#[test]
fn a_pipe_inside_a_quoted_expression_runs_no_script_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(dir.path(), "notes.md", "a|b\n", false);
    file(
        dir.path(),
        "check.sh",
        "#!/usr/bin/env bash\n# don't split here\nsed 's/a|/|b/' notes.md\n",
        true,
    );
    allows("./check.sh", dir.path());
}

/// 🔴 REGRESSION (#10548 P4): a nested refusal names the word it could not
/// resolve, not the outer script, which exists.
#[test]
fn a_nested_refusal_names_the_inner_word_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(
        dir.path(),
        "outer.sh",
        "echo hi\nbash ./never-written.sh\n",
        true,
    );
    let reason = verdict("bash outer.sh", dir.path()).unwrap_or_default();
    assert!(reason.contains("does not exist"), "must deny: {reason}");
    assert!(
        reason.contains("`./never-written.sh`"),
        "names the inner word: {reason}"
    );
}

/// 🔴 REGRESSION (#10548 P7): two adjacent expansions passed to a program
/// that never runs its operands are no `gh api` call.
#[test]
fn adjacent_expansions_are_no_gh_api_call_10548() {
    let dir = tempfile::tempdir().expect("dir");
    for command in [
        "for c in trusty-secrets trusty-common; do cargo test -p \"$c\" \"$t\" --no-fail-fast; done",
        "rsync -a \"$SRC\" \"$DST\" --delete",
        "cp \"$a\" \"$b\" \"$dir\" --no-clobber",
    ] {
        allows(command, dir.path());
    }
}

/// #10548: shapes the catalogue recorded that allow on 5f0290d4d7 already —
/// P5's `cat`/`printf` prose writes, P6, P8, and an agent-written worktree
/// comparison script (P10). Pinned so a later rule cannot start refusing them.
#[test]
fn the_unreproduced_shapes_allow_10548() {
    let dir = tempfile::tempdir().expect("dir");
    std::fs::create_dir_all(dir.path().join("docs")).expect("mkdir");
    file(
        dir.path(),
        "compare.sh",
        "#!/usr/bin/env bash\nset -euo pipefail\nA=$1 B=$2\n\
         git diff \"$A\"..\"$B\" -- docs/ | head -50\n\
         diff <(git ls-tree -r --name-only \"$A\" -- docs/) <(git ls-tree -r --name-only \"$B\" -- docs/) | head\n\
         git diff --stat \"$A\" \"$B\" | sed 's/|/ /' | sort\n",
        true,
    );
    let rows = [
        "cat <<'EOF' > notes.md\nPricing is quoted as cost/token per model.\nEOF",
        "printf '%s\\n' 'cost/token per model' > out.txt",
        "gh issue comment 1 --body 'quoted as cost/token per model'",
        "aws ec2 create-snapshot --volume-id vol-1 --tag-specifications \
         'ResourceType=snapshot,Tags=[{Key=Name,Value=x}]'",
        "gh issue comment 42 --body-file \"$F\"",
        "gh issue comment 42 --body-file $VAR",
        "bash compare.sh main HEAD",
    ];
    all_allow(&rows.map(String::from), dir.path());
}

/// #10548 bounds: a pipe-adjacent script the body runs by its path is still
/// judged, and a non-executable file the body makes executable first is too.
#[test]
fn a_non_executable_file_made_executable_is_still_judged_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(dir.path(), "probe.sh", KEYCHAIN_READ, true);
    file(dir.path(), "tool.yml", KEYCHAIN_READ, false);
    file(dir.path(), "piped.sh", "echo x | ./probe.sh\n", true);
    file(
        dir.path(),
        "chmod.sh",
        "chmod +x tool.yml\n./tool.yml\n",
        true,
    );
    for command in ["bash piped.sh", "bash chmod.sh"] {
        let reason = verdict(command, dir.path()).unwrap_or_default();
        assert!(
            reason.contains("#8596"),
            "`{command}` must refuse: {reason}"
        );
    }
}

/// #10548 bounds: a pattern list is blanked only where the shell reads one,
/// and a list carrying a command substitution, which runs, is kept.
#[test]
fn a_case_pattern_with_a_substitution_is_still_scanned_10548() {
    let blank = |text: &str| blank_case_patterns(text);
    assert_eq!(
        blank("case \"$1\" in\n  a/b | c/d) echo x ;;\nesac\n"),
        format!("case \"$1\" in\n  {} echo x ;;\nesac\n", " ".repeat(10))
    );
    for kept in [
        "case \"$1\" in\n  $(./run.sh) | x) ;;\nesac\n",
        "case \"$1\" in\n  `./run.sh` | x) ;;\nesac\n",
        "x=$(\n  ./run.sh | cat)\n",
        "echo a | ./run.sh)\n",
    ] {
        assert_eq!(blank(kept), kept, "must keep: {kept}");
    }
}

/// 🔴 REGRESSION (#10548 P7 bounds): a rewritten `gh api` pair in command
/// position, behind a wrapper that runs its operands, or behind a listed
/// program the command redefines still denies.
#[test]
fn a_rewritten_gh_api_pair_in_command_position_still_denies_10548() {
    let dir = tempfile::tempdir().expect("dir");
    for command in [
        "\"$G\" \"$A\" -X DELETE repos/o/r/actions/secrets/K",
        "xargs \"$G\" \"$A\" -X DELETE repos/o/r/actions/secrets/K",
        "do \"$G\" \"$A\" -X DELETE repos/o/r/actions/secrets/K",
        "cargo() { \"$@\"; }; cargo \"$G\" \"$A\" -X DELETE repos/o/r/actions/secrets/K",
        "\"$C\" test \"$G\" \"$A\" -X DELETE repos/o/r/actions/secrets/K",
    ] {
        let reason = verdict(command, dir.path()).unwrap_or_default();
        assert!(reason.contains("#8875"), "`{command}` must deny: {reason}");
    }
}

/// 🔴 REGRESSION (#10548 P1, P2, P4, P10): this repository's scripts the
/// incident log names allow from the repo root.
///
/// Reverted for safety in round 2: `select-test-crates.sh` and its selftest
/// (the raw-body scan cuts a quoted `sed` expression into a missing "script"
/// on a code line) and `package_content.sh` (a comment names a real
/// non-executable file, which is judged now that no execute-bit arm exists).
#[test]
fn the_incident_repo_scripts_allow_10548() {
    let root = trusty_common::test_harness::test_repo_root().expect("resolve the checkout");
    // Bare names, joined below, so no script-path literal sits in crate source.
    let rows = [
        ("detect-docs-only.sh", " < /dev/null"),
        ("check_adr.sh", ""),
        ("check_rustdoc_links.sh", ""),
    ]
    .map(|(gate, args)| format!("bash scripts/{gate}{args}"));
    all_allow(&rows, &root);
}

/// 🔴 TRUE POSITIVE (#10548 incident 1, api-qa): a curl+psql probe script
/// whose database password comes from the Keychain refuses.
#[test]
fn tp_a_curl_psql_probe_script_refuses_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(
        dir.path(),
        "api_probe.sh",
        "#!/usr/bin/env bash\nset -eu\ncurl -s http://localhost:8080/health\n\
         security find-generic-password -s api-qa-db -w\n\
         psql -h localhost -U qa -c 'select 1'\n",
        true,
    );
    let reason = verdict("bash api_probe.sh", dir.path()).unwrap_or_default();
    assert!(reason.contains("#8596"), "must refuse: {reason}");
}

/// 🔴 TRUE POSITIVE (#10548 incident 2): an in-process `bootstrap_ssm` script
/// that builds its SSM client from the AWS credentials file refuses.
#[test]
fn tp_an_in_process_bootstrap_ssm_script_refuses_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(
        dir.path(),
        "bootstrap_ssm.py",
        "import configparser, os, boto3\ncfg = configparser.ConfigParser()\n\
         cfg.read(os.path.expanduser('~/.aws/credentials'))\n\
         ssm = boto3.client('ssm', aws_access_key_id=cfg['default']['aws_access_key_id'])\n\
         for p in ssm.get_parameters_by_path(Path='/app/', WithDecryption=True)['Parameters']:\n\
         \x20   os.environ[p['Name']] = p['Value']\n",
        false,
    );
    let reason = verdict("python3 bootstrap_ssm.py", dir.path()).unwrap_or_default();
    assert!(reason.contains("#7266"), "must refuse: {reason}");
}

/// 🔴 TRUE POSITIVE (#10548 incident 3): a script carrying
/// `GH_TOKEN=$(gh auth token)` refuses.
#[test]
fn tp_a_gh_auth_token_script_refuses_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(
        dir.path(),
        "release.sh",
        "#!/usr/bin/env bash\nGH_TOKEN=$(gh auth token); echo \"using ${GH_TOKEN:0:8}\"\n\
         gh release list -R o/r\n",
        true,
    );
    let reason = verdict("./release.sh", dir.path()).unwrap_or_default();
    assert!(reason.contains("#8596"), "must refuse: {reason}");
}

/// 🔴 TRUE POSITIVE (#10548 incident 4): the `dev-scratch` script that loads
/// `.env.local` refuses. `make` itself is residual 4, so the script is run.
#[test]
fn tp_a_scratch_db_script_reading_env_local_refuses_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(
        dir.path(),
        "tools/db-scratch.sh",
        "#!/usr/bin/env bash\nset -a\nsource .env.local\nset +a\n\
         psql \"$DATABASE_URL\" -c 'create database scratch'\n",
        true,
    );
    let reason = verdict("bash tools/db-scratch.sh", dir.path()).unwrap_or_default();
    assert!(reason.contains("#7266"), "must refuse: {reason}");
}

/// 🔴 FAIL CLOSED (#10548 incident 5): a script created and run in one
/// command, at a path computed at run time, refuses: the guard cannot read a
/// body that does not exist yet. (A literal name written by an earlier stage
/// is documented residual 1.)
#[test]
fn fc_a_script_written_and_run_in_one_command_refuses_10548() {
    let dir = tempfile::tempdir().expect("dir");
    let command = "S=$(mktemp) && printf 'echo hi\\n' > \"$S\" && bash \"$S\"";
    let reason = verdict(command, dir.path()).unwrap_or_default();
    assert!(
        reason.contains("computed at run time"),
        "must refuse: {reason}"
    );
}

/// 🔴 FAIL CLOSED (#10548 incident 6): `bash $S/ssm_run.sh` cannot be
/// resolved, so it refuses.
#[test]
fn fc_an_unresolvable_script_path_refuses_10548() {
    let dir = tempfile::tempdir().expect("dir");
    let reason = verdict("bash $S/ssm_run.sh", dir.path()).unwrap_or_default();
    assert!(
        reason.contains("computed at run time"),
        "must refuse: {reason}"
    );
}

/// 🔴 REGRESSION (#10548 P10): a method call on a value named for the word
/// family (`token.strip()`) in embedded program text is no file; a family
/// name with a directory or a file extension still denies.
#[test]
fn a_method_call_on_a_token_value_is_no_file_10548() {
    let dir = tempfile::tempdir().expect("dir");
    assert_eq!(
        mask_method_calls("token = token.strip(); c = tok.partition(x)"),
        "token = token_strip(); c = tok.partition(x)"
    );
    assert_eq!(
        mask_method_calls("cat vault/token.get(x)"),
        "cat vault/token.get(x)"
    );
    let program = "python3 - <<'PY'\nfor token in cols.split(','):\n    \
                   token = token.strip()\n    c, _, f = token.partition('/')\nPY";
    allows(program, dir.path());
    file(
        dir.path(),
        "lanes.sh",
        &format!("#!/usr/bin/env bash\n{program}\n"),
        true,
    );
    allows("bash lanes.sh", dir.path());
    for command in [
        "cat vault/token",
        "python3 - <<'PY'\nprint(open('token.json').read())\nPY",
        "python3 - <<'PY'\nprint(open('vault/token').read().strip())\nPY",
    ] {
        assert!(
            verdict(command, dir.path()).is_some(),
            "`{command}` must deny"
        );
    }
}

/// An AWS credentials read — refused inline (#7266).
const AWS_READ: &str = "cat ~/.aws/credentials\n";

/// Assert `command` refuses from `cwd` with a reason carrying `marker`.
fn refuses(command: &str, cwd: &Path, marker: &str) {
    all_refuse(&[command.to_string()], cwd, marker);
}

/// Every row of `commands` refuses from `cwd` with a reason carrying
/// `marker`; the rows that do not are reported together.
fn all_refuse(commands: &[String], cwd: &Path, marker: &str) {
    let allowed: Vec<(&String, Option<String>)> = commands
        .iter()
        .map(|c| (c, verdict(c, cwd)))
        .filter(|(_, r)| !r.as_deref().is_some_and(|r| r.contains(marker)))
        .collect();
    assert!(allowed.is_empty(), "must refuse ({marker}): {allowed:#?}");
}

/// 🔴 REGRESSION (#10548 round 2, bypass 1): `case` and `in` as plain
/// arguments open no arm list, so the line after them is still a run.
#[test]
fn a_case_word_in_argument_position_opens_no_pattern_list_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(dir.path(), "steal.sh", AWS_READ, true);
    // No `case WORD in` header, and a `case` word not in command position.
    let rows = [
        "(echo case in\n./steal.sh | cat)\n",
        "(echo case x in\n./steal.sh | cat)\n",
    ]
    .iter()
    .enumerate()
    .map(|(n, body)| {
        assert_eq!(blank_case_patterns(body), *body, "must keep: {body}");
        file(dir.path(), &format!("t{n}.sh"), body, true);
        format!("bash t{n}.sh")
    })
    .collect::<Vec<_>>();
    all_refuse(&rows, dir.path(), "#7266");
}

/// 🔴 REGRESSION (#10548 round 2, bypass 2): a `#` line inside an open quote
/// is no comment; the command after the closing quote runs and is judged.
#[test]
fn a_hash_line_inside_an_open_quote_still_runs_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(dir.path(), "steal.sh", AWS_READ, true);
    file(dir.path(), "t.sh", "echo \"a\n#\"; ./steal.sh\n", true);
    // The same shape in the inline judge of the body itself.
    file(
        dir.path(),
        "inline.sh",
        &format!("echo \"a\n#\"; {KEYCHAIN_READ}"),
        true,
    );
    refuses("bash inline.sh", dir.path(), "#8596");
    refuses("bash t.sh", dir.path(), "#7266");
    // Inside a substitution a `'` opens a quote that a `"` cannot close, so
    // the `#` line is code; an unbalanced body blanks nothing.
    let sub = "x=\"$(echo '\"\n#'; y)\"\n";
    assert_eq!(without_true_comments(sub).as_deref(), Some(sub));
    assert_eq!(without_true_comments("echo \"a\n# b\n"), None);
}

/// 🔴 REGRESSION (#10548 round 2, bypass 3): a non-executable file run by its
/// path is judged, whatever spelling makes it executable first.
#[test]
fn a_non_executable_script_made_runnable_by_any_spelling_is_judged_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(dir.path(), "steal.sh", AWS_READ, false);
    let rows = [
        ("quoted.sh", "ch''mod +x ./steal.sh; ./steal.sh\n"),
        ("printf.sh", "$(printf chm)od +x ./steal.sh; ./steal.sh\n"),
        (
            "install.sh",
            "install -m 755 ./steal.sh ./steal.sh; ./steal.sh\n",
        ),
    ]
    .map(|(name, body)| {
        file(dir.path(), name, body, true);
        format!("bash {name}")
    });
    all_refuse(&rows, dir.path(), "#7266");
}

/// 🔴 REGRESSION (#10548 round 2, bypass 4): a command that can make a
/// listed program run something else trusts no operand list. (A `"$G"` word
/// with `-X` is refused by the `curl` arm already; `--method` and a literal
/// `gh` are not.)
#[test]
fn a_command_that_may_redefine_a_listed_program_trusts_no_operands_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(dir.path(), "defs.sh", "true\n", true);
    let call = "gh api -X DELETE repos/o/r/actions/secrets/K";
    let method = "\"$G\" api --method DELETE repos/o/r/actions/secrets/K";
    let rows = [
        format!("hash -p /usr/bin/env cp; cp {call}"),
        format!("hash -p /usr/bin/env cp; G=gh; cp {method}"),
        format!("PATH=./shim:$PATH cp {call}"),
        format!("source defs.sh; cp {call}"),
        format!(". defs.sh; cp {call}"),
        format!("eval \"c\"\"p(){{ env \\\"\\$@\\\"; }}\"; cp {call}"),
    ];
    all_refuse(&rows, dir.path(), "#8875");
}

/// 🔴 REGRESSION (#10548 round 2, LOW): a method-call spelling in shell argv
/// is a file name the shell opens; only program text is masked.
#[test]
fn a_method_call_spelling_in_shell_argv_still_denies_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(dir.path(), "t.sh", "cat 'token.get(1)'\n", true);
    file(dir.path(), "u.sh", "cat \"my token.get(1)\"\n", true);
    all_refuse(
        &["bash t.sh".into(), "bash u.sh".into()],
        dir.path(),
        "#7266",
    );
}

/// 🔴 REGRESSION (#10548 round 2, Missing gate): a script named only on a
/// true comment line runs nothing, so its absence refuses nothing; one named
/// on a `#` line inside an open quote runs, so its absence still refuses.
#[test]
fn a_missing_script_named_only_in_a_comment_is_waived_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(
        dir.path(),
        "t.sh",
        "#!/usr/bin/env bash\n# usage: make | ./not-built-yet.sh\necho ok\n",
        true,
    );
    allows("bash t.sh", dir.path());
    file(
        dir.path(),
        "q.sh",
        "echo \"a\n#\"; ./not-built-yet.sh\n",
        true,
    );
    refuses("bash q.sh", dir.path(), "does not exist");
}
