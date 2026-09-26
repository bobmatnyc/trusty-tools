//! #8596, #8248: a credential value never reaches tool output. Every command
//! here uses a fake service name; none is ever run.

use super::evaluate_credential_print_command;

/// Assert every row gets the wanted verdict, reporting all misses at once.
fn check(want_deny: bool, rows: &[&str]) {
    let wrong: Vec<String> = rows
        .iter()
        .filter_map(|c| {
            let got = evaluate_credential_print_command(c);
            (got.is_some() != want_deny).then(|| format!("{c:?} -> {got:?}"))
        })
        .collect();
    let verdict = if want_deny { "refused" } else { "allowed" };
    assert!(
        wrong.is_empty(),
        "expected {verdict}:\n{}",
        wrong.join("\n")
    );
}

/// The two reported leaks and the shapes one step away from them.
#[test]
fn denies_the_reported_leaks() {
    check(
        true,
        &[
            // #8596: the incident — the value is shorter than 50 bytes.
            "security find-generic-password -s fake-svc -a fake-acct -w | head -c 50",
            "security find-generic-password -s fake-svc -w",
            "/usr/bin/security find-internet-password -s example.test -w",
            "security find-generic-password -ws fake-svc",
            "security find-generic-password -s fake-svc -w | cut -c1-8",
            "security find-generic-password -s fake-svc -w | tee /tmp/fake-out",
            "security find-generic-password -s fake-svc -w > /dev/stdout",
            // `-g` prints the value on STDERR.
            "security find-generic-password -g -s fake-svc",
            "security find-generic-password -g -s fake-svc >/dev/null",
            "security find-generic-password -g -s fake-svc 2>&1 | head -c 20",
            // #8248: the incident, and its siblings.
            "gcloud auth application-default print-access-token",
            "gcloud auth print-access-token",
            "gcloud auth print-identity-token --audiences=https://example.test",
            "gcloud --project fake-proj auth print-access-token | head -c 12",
            "timeout 5 gcloud auth print-access-token",
            "cd /tmp && gcloud auth print-access-token",
            // A captured value handed back to the terminal.
            "echo \"$(gcloud auth print-access-token)\"",
            "printf '%s' `security find-generic-password -s fake-svc -w`",
            "cat <(gcloud auth print-access-token)",
            "$(gcloud auth print-access-token)",
            "cat <<< \"$(gcloud auth print-access-token)\"",
            "gcloud auth print-access-token | xargs echo",
            // #8596 finding 10 narrows xargs to printing programs only.
            "gcloud auth print-access-token | xargs -I{} echo {}",
            "gcloud auth print-access-token | xargs -I{} sh -c 'echo {}'",
            "gcloud auth print-access-token | xargs -I{} {}",
            "gcloud auth print-access-token | xargs",
            "bash -c 'gcloud auth print-access-token'",
            "sh -c \"security find-generic-password -s fake-svc -w\" | head",
            "X=$(echo \"$(gcloud auth print-access-token)\" | head -c 5); echo hi; gcloud auth print-access-token",
        ],
    );
}

/// The capturing forms legitimate flows need keep working.
#[test]
fn allows_the_capturing_forms() {
    check(
        false,
        &[
            // Existence check by exit status (#8596's proposed pattern).
            "security find-generic-password -s fake-svc >/dev/null 2>&1 && echo present",
            "security find-generic-password -s fake-svc -a fake-acct",
            "security find-generic-password -s fake-svc -w >/dev/null 2>&1",
            "security find-generic-password -s fake-svc -g &>/dev/null",
            "security find-generic-password -s w -a g",
            // A token consumed inside the command that needs it (#8248).
            "curl -sS -H \"Authorization: Bearer $(gcloud auth print-access-token)\" https://example.test/v1",
            "TOKEN=$(gcloud auth print-access-token)",
            "export FAKE_TOKEN=$(security find-generic-password -s fake-svc -w)",
            "FAKE=$(gcloud auth application-default print-access-token) curl -H \"x: $FAKE\" https://example.test",
            "gcloud auth print-access-token | docker login -u oauth2accesstoken --password-stdin https://example.test",
            "security find-generic-password -s fake-svc -w | pbcopy",
            "security find-generic-password -s fake-svc -w | wc -c",
            "security find-generic-password -s fake-svc -w | grep -q .",
            "gcloud auth print-access-token > /tmp/fake-token-file",
            "X=$(echo \"$(gcloud auth print-access-token)\")",
            // Naming the commands as text runs none of them.
            "git commit -m 'docs: never run gcloud auth print-access-token bare'",
            "grep -rn 'find-generic-password' crates/",
            "rg print-access-token docs/",
            "echo 'security find-generic-password -s x -w'",
            "gcloud auth list",
        ],
    );
}

/// #8596 round 2: every bypass the code-critic found. Each row was allowed at
/// f61a451ae.
#[test]
fn denies_the_round_two_bypasses() {
    check(
        true,
        &[
            // 1: a `<(…)` file is printed by any reader.
            "head -c 8 <(security find-generic-password -s fake-svc -w)",
            "diff <(gcloud auth print-access-token) want.txt",
            "sort < <(gcloud auth print-access-token)",
            // 2: a substitution the outer shell lifted, inside a wrapper.
            "bash -c \"echo $(gcloud auth print-access-token)\"",
            "echo x | xargs echo $(gcloud auth print-access-token)",
            "env -S \"echo $(gcloud auth print-access-token)\"",
            // 3: a copy through a descriptor the rule did not track.
            "security find-generic-password -s fake-svc -w 3>&1 1>&3",
            "security find-generic-password -s fake-svc -w 3>&1 1>&3-",
            "security find-generic-password -s fake-svc -w 1>&4",
            "gcloud auth print-access-token >& 1",
            // 4: APFS is case-insensitive.
            "SECURITY find-generic-password -s fake-svc -w",
            "GCloud auth print-access-token",
            // Subshell grouping and a keyword before a run-time program.
            "(gcloud auth print-access-token)",
            "if true; then $(gcloud auth print-access-token); fi",
            // 6: consumers accepted only where they really consume.
            "gcloud auth print-access-token | tee --password-stdin",
            "gcloud auth print-access-token | cat --with-token",
            "gcloud auth print-access-token | grep -eq .",
            "gcloud auth print-access-token | rg -rq .",
            "gcloud auth print-access-token | wc --files0-from=-",
            "gcloud auth print-access-token | wc -c -L --files0-from -",
            // 7: xtrace prints expanded values.
            "set -x; TOKEN=$(gcloud auth print-access-token)",
            "set -o xtrace; curl -H \"x: $(gcloud auth print-access-token)\" https://example.test",
            "set -euxo pipefail; T=$(gcloud auth print-access-token)",
            "bash -x -c 'T=$(gcloud auth print-access-token)'",
            "bash -xc 'T=$(gcloud auth print-access-token)'",
            "SHELLOPTS=xtrace bash -c 'T=$(gcloud auth print-access-token)'",
            // 8: more credential-printing calls.
            "gcloud config config-helper --format='value(credential.access_token)'",
            "gcloud config config-helper --format=json",
            "gcloud config config-helper",
            "security dump-keychain -d",
            "security dump-keychain -d login.keychain | head",
        ],
    );
}

/// #8596 round 2, finding 5: credential-command text handed to something that
/// runs it, or a program named at run time. The guard cannot follow the value,
/// so it refuses as unreadable.
#[test]
fn denies_evaluated_trigger_text() {
    let rows = [
        "S=security; $S find-generic-password -s fake-svc -w",
        "G=gcloud; timeout 5 $G auth print-access-token",
        "C='gcloud auth print-access-token'; $C",
        "eval \"security find-generic-password -s fake-svc -w\"",
        "bash <<< 'gcloud auth print-access-token'",
        "echo 'gcloud auth print-access-token' | sh",
        "osascript -e 'do shell script \"security find-generic-password -s fake-svc -w\"'",
        "python3 -c 'import os; os.system(\"gcloud auth print-access-token\")'",
        "ssh fake-host 'security find-generic-password -s fake-svc -w'",
        "sudo -u fake bash -c 'gcloud auth print-access-token'",
        "bash <<'EOF'\ngcloud auth print-access-token\nEOF",
        "python3 <<'PY'\nimport os; os.system('gcloud auth print-access-token')\nPY",
        "python3 <<PY\nimport os; os.system(\"gcloud auth print-access-token\")\nPY",
    ];
    let wrong: Vec<String> = rows
        .iter()
        .filter_map(|c| {
            let reason = evaluate_credential_print_command(c);
            (!reason.as_deref().is_some_and(|r| r.contains("cannot read")))
                .then(|| format!("{c:?} -> {reason:?}"))
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "expected unreadable:\n{}",
        wrong.join("\n")
    );
    // An unquoted body is shell source: its credential call is judged as one.
    check(true, &["bash <<EOF\ngcloud auth print-access-token\nEOF"]);
}

/// #8596 round 2, findings 9 and 10: data here-documents and a non-printing
/// xargs program are allowed; so are the mentions finding 5 must not catch.
#[test]
fn allows_quoted_heredoc_bodies() {
    check(
        false,
        &[
            "gh issue comment 1 --body-file - <<'EOF'\nNever run gcloud auth print-access-token bare; it's a leak.\nEOF",
            "cat > notes.md <<'EOF'\nsecurity find-generic-password -s x -w | head -c 50 (don't)\nEOF",
            "gh pr create --title t --body \"$(cat <<'EOF'\nuse `security find-generic-password -s x >/dev/null`, isn't that it\nEOF\n)\"",
            "gcloud auth print-access-token | xargs -I{} curl -H \"Authorization: Bearer {}\" https://example.test",
            "gcloud auth print-access-token | xargs -I % curl -sS -H 'Authorization: Bearer %' https://example.test",
            "git log --grep print-access-token",
            "git grep -n find-generic-password",
            "gh issue create --title x --body 'gcloud auth print-access-token leaked'",
            "gcloud auth print-access-token | gh auth login --with-token",
            "gcloud auth print-access-token | podman login -u oauth2accesstoken --password-stdin example.test",
            "gcloud auth print-access-token | grep -cq .",
            "gcloud auth print-access-token | wc -lc",
            "wc -c <(gcloud auth print-access-token)",
            "gcloud config config-helper --format='value(configuration.properties.core.project)'",
            "security find-generic-password -s fake-svc -w 3>/dev/null 1>&3",
            "set +x; T=$(gcloud auth print-access-token)",
        ],
    );
}

/// #8596 round 3: every bypass the second code-critic round found. Each row
/// was allowed at 99bc6da75.
#[test]
fn denies_the_round_three_bypasses() {
    check(
        true,
        &[
            // 1: a comment hid a heredoc operator or held an apostrophe.
            "true # <<'X'\ngcloud auth print-access-token\nX",
            "# don't print it\ngcloud auth print-access-token\n# that's all",
            "# don't\ngcloud auth print-access-token >/dev/null; gcloud auth print-access-token\n# that's all",
            "echo ${X:-<<'E'}\ngcloud auth print-access-token\nE",
            "echo ${X:-<<'E' }\ngcloud auth print-access-token\nE",
            // 2: a file-name argument that names the terminal.
            "gcloud auth print-access-token | tee /dev/stderr | docker login -u x --password-stdin r.test",
            "gcloud auth print-access-token | tee /dev/tty >/dev/null",
            "gcloud auth print-access-token | tee >(cat) >/dev/null",
            "cp <(gcloud auth print-access-token) /dev/stderr >/dev/null",
            "gcloud auth print-access-token | dd of=/dev/stderr status=none >/dev/null",
            "gcloud auth print-access-token | tee /dev/fd/2 >/dev/null",
            // 3: program text flowing through a filter into an evaluator.
            "echo 'gcloud auth print-access-token' | cat | sh",
            "cat <<'EOF' | tee /dev/null | bash\ngcloud auth print-access-token\nEOF",
            // 4: input-side copies, read-write opens, a run-time descriptor.
            "security find-generic-password -s x -w >/dev/null 1<&2",
            "security find-generic-password -s x -w >/dev/null 1<>/dev/tty",
            "exec {fd}>&1; gcloud auth print-access-token >&$fd",
            // 5: `-o` at the end of a cluster, and option-name spelling.
            "set -eo xtrace; T=$(gcloud auth print-access-token)",
            "set -o XTRACE; T=$(gcloud auth print-access-token)",
            "set -o x_trace; T=$(gcloud auth print-access-token)",
        ],
    );
}

/// #8596 round 3, finding 6: only an evaluator's code operand is judged, so a
/// script-path operand is an ordinary argument; a comment, or a `#` inside a
/// word, changes nothing that was allowed.
#[test]
fn allows_script_operands_and_comments() {
    check(
        false,
        &[
            "T=$(gcloud auth print-access-token); python3 upload.py --token \"$T\"",
            "gcloud auth print-access-token > /tmp/fake-token # keep it out of the log",
            "cat <<'EOF' > notes.md # a note\n# don't run gcloud auth print-access-token\nEOF",
            "T=$(gcloud auth print-access-token); echo ${#T} a#b '# not a comment'",
            "gcloud auth print-access-token | tee /tmp/fake-token >/dev/null",
            "security find-generic-password -s x -w 1<>/tmp/fake-out",
        ],
    );
    check(
        true,
        &["python3 -c \"print('$(gcloud auth print-access-token)')\""],
    );
}

/// No prefix of a credential command panics, and each gets a verdict: a panic
/// would exit the hook 101 and fail open.
#[test]
fn no_prefix_of_a_command_panics() {
    let commands = [
        "gh pr create --body \"$(cat <<'EOF'\nsecurity find-generic-password -s é -w\nEOF\n)\" 3>&1 1>&3- 2>& 1",
        "bash -c \"echo $(gcloud auth print-access-token)\" | xargs -I{} sh -c '{}' <<< `x` >(cat) <(y)",
        "(set -o xtrace; eval \"$(security dump-keychain -d)\") <<-\\EOF\n\tbody ünï\n\tEOF\n",
        // Round 3: comments, `${…}`, backticks and ANSI-C quotes around heredocs.
        "true # <<'X' é\n`echo # c` ${X:-<<'E'} $'it\\'s' 1<&2 1<>/dev/tty >&$fd | tee /dev/fd/3 >(cat)\ncat <<E # d\n# b ü\nE\n",
    ];
    for command in commands {
        for (end, _) in command.char_indices().chain([(command.len(), ' ')]) {
            let prefix = &command[..end];
            let outcome = std::panic::catch_unwind(|| scan_for_test(prefix));
            assert!(outcome.is_ok(), "panicked on {prefix:?}");
        }
    }
}

/// The scan without the entry point's panic guard, so a panic is visible.
fn scan_for_test(command: &str) -> bool {
    super::scan(
        command,
        super::Sink::Terminal,
        super::Sink::Terminal,
        0,
        &super::Lifted::default(),
    )
    .is_ok()
}

/// Fail-closed: a credential command the scanner cannot read is refused, and
/// the reason names the guard's blindness rather than a leak.
#[test]
fn denies_what_it_cannot_read() {
    let rows = [
        "gcloud auth print-access-token 'unterminated",
        "echo $(gcloud auth print-access-token",
        "echo `security find-generic-password -s fake-svc -w",
        "gcloud auth print-access-token >",
    ];
    for command in rows {
        let reason = evaluate_credential_print_command(command);
        assert!(
            reason.as_deref().is_some_and(|r| r.contains("cannot read")),
            "{command:?} -> {reason:?}"
        );
    }
    let deep = format!(
        "X={}gcloud auth print-access-token{}",
        "$(".repeat(12),
        ")".repeat(12)
    );
    assert!(evaluate_credential_print_command(&deep).is_some());
}

/// A command that names no trigger is never parsed, broken quoting included.
#[test]
fn ignores_commands_that_name_no_credential_cli() {
    check(
        false,
        &[
            "echo 'unterminated",
            "security list-keychains",
            "gcloud config list",
        ],
    );
}
