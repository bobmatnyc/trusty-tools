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

/// #8730: zsh's clobber spellings `>!`/`>>!` (and `>&!`/`>&|`) left the `!`
/// or `|` glued to the target, so `/dev/tty` read as the file `!/dev/tty` and
/// the value was taken as discarded. Each row was allowed at 6a1aeb2b0.
#[test]
fn denies_a_clobber_redirect_to_the_terminal_8730() {
    check(
        true,
        &[
            "security find-generic-password -s x -w >!/dev/tty",
            "security find-generic-password -s x -w >>!/dev/tty",
            "security find-generic-password -s x -w >!/dev/stdout",
            "security find-generic-password -s x -w >&!/dev/tty",
            "security find-generic-password -s x -w >&|/dev/tty",
            "security find-generic-password -s x -w >| /dev/tty",
        ],
    );
}

/// #8730: a clobber redirect to an ordinary file is a file write. The `>|`
/// rows were refused as unreadable at 6a1aeb2b0: the stage split cut at the
/// operator's `|` as if it were a pipe.
#[test]
fn allows_a_clobber_redirect_to_a_file_8730() {
    check(
        false,
        &[
            "security find-generic-password -s x -w >!/tmp/fake-out",
            "security find-generic-password -s x -w >>!/tmp/fake-out",
            "security find-generic-password -s x -w >|/tmp/fake-out",
            "security find-generic-password -s x -w >>| /tmp/fake-out",
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

/// #8676: a credential captured into a shell variable and printed in a later
/// stage. The first three rows are the reported shapes; each row was allowed
/// at 62947a19f.
#[test]
fn denies_a_credential_carried_by_a_variable() {
    check(
        true,
        &[
            "T=$(gcloud auth print-access-token); echo \"${T:0:10}\"",
            "export K=$(security find-generic-password -s fake-svc -w) && printenv K",
            "for t in $(gcloud auth print-access-token); do echo $t; done",
            // Environment and variable dumps while a name is tainted.
            "export K=$(security find-generic-password -s fake-svc -w); env",
            "T=$(gcloud auth print-access-token) printenv T",
            "T=$(gcloud auth print-access-token); sudo -u fake env",
            "T=$(gcloud auth print-access-token); set",
            "T=$(gcloud auth print-access-token); declare -p T",
            "export T=$(gcloud auth print-access-token); export -p",
            "T=$(gcloud auth print-access-token); timeout 5 printenv",
            // Every binding form, and every stage separator.
            "T=$(gcloud auth print-access-token)\nprintf '%s\\n' \"$T\"",
            "local T=$(gcloud auth print-access-token) || echo ${T}",
            "declare -r T=`gcloud auth print-access-token`; echo ${T:-none}",
            "readonly T=$(gcloud auth print-access-token); echo ${T#ya29}",
            "f() { T=$(gcloud auth print-access-token); }; f; echo $T",
            "select t in $(gcloud auth print-access-token); do echo $t; done",
            "set -- $(gcloud auth print-access-token); echo \"$1\"",
            "set -- $(gcloud auth print-access-token); for t; do echo $t; done",
            // A copy, a nameref, a run-time name, and a loop that reprints.
            "T=$(gcloud auth print-access-token); U=\"Bearer $T\"; echo $U",
            "T=$(gcloud auth print-access-token); declare -n R=T; echo $R",
            "declare \"$N=$(gcloud auth print-access-token)\"; echo $OTHER",
            "T=$(gcloud auth print-access-token); echo ${!T}",
            "while true; do echo $T; T=$(gcloud auth print-access-token); done",
            // The value reaching the terminal through another route.
            "T=$(gcloud auth print-access-token); echo \"$(echo $T)\"",
            "T=$(gcloud auth print-access-token); echo $T | head -c 5",
            "T=$(gcloud auth print-access-token); cat <<< \"$T\"",
            "T=$(gcloud auth print-access-token); head <<EOF\n$T\nEOF",
            "T=$(gcloud auth print-access-token); bash -c \"echo $T\"",
            "export T=$(gcloud auth print-access-token); bash -c 'echo $T'",
            "T=$(gcloud auth print-access-token); echo $T >&2",
        ],
    );
}

/// #8676: a variable that holds a credential but is only measured, tested, or
/// handed to a program that does not print it.
#[test]
fn allows_a_carried_variable_that_is_never_printed() {
    check(
        false,
        &[
            "T=$(gcloud auth print-access-token); echo ${#T}",
            "T=$(gcloud auth print-access-token); echo \"len=${#T}\"",
            "T=$(gcloud auth print-access-token); python3 upload.py --token \"$T\"",
            "T=$(gcloud auth print-access-token); curl -sS -H \"Authorization: Bearer $T\" https://example.test/v1",
            "export K=$(security find-generic-password -s fake-svc -w) && printenv HOME",
            "T=$(gcloud auth print-access-token); [ -n \"$T\" ] && echo present",
            "T=$(gcloud auth print-access-token); printenv T > /tmp/fake-out",
            "T=$(gcloud auth print-access-token); set -e; echo done",
            "T=$(gcloud auth print-access-token >/dev/null); echo $T",
            "for t in a b; do echo $t; done; T=$(gcloud auth print-access-token)",
            "T=$(gcloud auth print-access-token); printf '%s' \"$T\" | docker login -u x --password-stdin r.test",
        ],
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

/// #8676 round 2, row 1: inline code reads an exported name with no `$`.
/// One row per `EVALUATORS` entry, plus a here-document and a pipe.
#[test]
fn denies_inline_code_reading_a_tainted_name() {
    check(
        true,
        &[
            "export T=$(gcloud auth print-access-token); eval 'printenv T'",
            "export T=$(gcloud auth print-access-token); source /dev/stdin <<< 'printenv T'",
            "export T=$(gcloud auth print-access-token); . /dev/stdin <<< 'printenv T'",
            "export T=$(gcloud auth print-access-token); osascript -e 'system attribute \"T\"'",
            "export T=$(gcloud auth print-access-token); ssh fake-host printenv T",
            "export T=$(gcloud auth print-access-token); python -c 'import os; print(os.environ[\"T\"])'",
            "export T=$(gcloud auth print-access-token); python3 -c 'import os; print(os.environ[\"T\"])'",
            "export T=$(gcloud auth print-access-token); perl -le 'print for values %ENV'",
            "export T=$(gcloud auth print-access-token); ruby -e 'puts ENV[\"T\"]'",
            "export T=$(gcloud auth print-access-token); node -e 'console.log(process.env.T)'",
            "export T=$(gcloud auth print-access-token); deno eval 'console.log(Deno.env.get(\"T\"))'",
            "export T=$(gcloud auth print-access-token); php -r 'echo getenv(\"T\");'",
            "export T=$(gcloud auth print-access-token); fish -c 'printenv T'",
            "export T=$(gcloud auth print-access-token); python3 <<'PY'\nimport os; print(os.environ['T'])\nPY",
            "export T=$(gcloud auth print-access-token); echo 'import os; print(os.environ[\"T\"])' | python3",
        ],
    );
}

/// #8676 round 2, row 2: zsh expansion flags and prefixes, and a nested `${…}`.
#[test]
fn denies_zsh_expansion_forms() {
    check(
        true,
        &[
            "T=$(gcloud auth print-access-token); N=T; echo ${(P)N}",
            "T=$(gcloud auth print-access-token); echo ${(U)T}",
            "T=$(gcloud auth print-access-token); echo \"${(U)T}\"",
            "T=$(gcloud auth print-access-token); echo ${^T}",
            "T=$(gcloud auth print-access-token); echo ${=T}",
            "T=$(gcloud auth print-access-token); echo ${~T}",
            "T=$(gcloud auth print-access-token); echo $=T",
            "T=$(gcloud auth print-access-token); echo $~T",
            "T=$(gcloud auth print-access-token); echo $^T",
            "T=$(gcloud auth print-access-token); echo ${${T}:0:5}",
        ],
    );
}

/// #8676 round 2, row 3: a compound array assignment copies the value.
#[test]
fn denies_array_assignment_copies() {
    check(
        true,
        &[
            "T=$(gcloud auth print-access-token); A=(x \"$T\"); echo ${A[1]}",
            "T=$(gcloud auth print-access-token); A+=(\"$T\"); echo ${A[@]}",
            "declare -a A=($(gcloud auth print-access-token)); echo ${A[0]}",
            "T=$(gcloud auth print-access-token); local -A M=([k]=\"$T\"); echo ${M[k]}",
        ],
    );
}

/// #8676 round 2, row 4: a word nested ~100k `${` deep. Unbounded, the
/// recursion overflows the stack, which aborts past `catch_unwind`.
#[test]
fn deep_brace_nesting_denies_without_overflow() {
    let depth = 100_000;
    let command = format!(
        "T=$(gcloud auth print-access-token); echo {}T{}",
        "${a".repeat(depth),
        "}".repeat(depth)
    );
    let started = std::time::Instant::now();
    let verdict = evaluate_credential_print_command(&command);
    let spent = started.elapsed();
    assert!(verdict.is_some(), "must deny");
    // #8676 round 4: bounded in time as well as in stack depth.
    // #8765: 5 s, not 1 s. The limit guards overflow and unbounded work, not a
    // constant factor; 1 s was a CI flake risk under #8734's 2x slowdown.
    assert!(spent < std::time::Duration::from_secs(5), "took {spent:?}");
}

/// #8676 round 2, row 5: a scan whose fixed point would take exponential work
/// refuses inside its budget. Eight substitution levels, each with an
/// eight-name copy chain seeded by a credential call; the reversed chain needs
/// one pass per name at every level. #8765: the 5 s limit guards unbounded
/// work, not a constant factor.
#[test]
fn scan_work_is_bounded() {
    let seed = "A=$(gcloud auth print-access-token)";
    let forward = format!("{seed}; B=$A; C=$B; D=$C; E=$D; F=$E; G=$F; H=$G");
    let reversed = format!("H=$G; G=$F; F=$E; E=$D; D=$C; C=$B; B=$A; {seed}");
    for chain in [forward, reversed] {
        let mut text = chain.clone();
        for _ in 0..7 {
            text = format!("{chain}; X=$({text}); echo $H");
        }
        let started = std::time::Instant::now();
        let verdict = evaluate_credential_print_command(&text);
        let spent = started.elapsed();
        assert!(verdict.is_some(), "must deny: {text}");
        // #8765: 5 s, not 1 s; 1 s was a CI flake risk under #8734's 2x slowdown.
        assert!(spent < std::time::Duration::from_secs(5), "took {spent:?}");
    }
}

/// #8676 round 2, row 6: an arithmetic context reads a bare name, and its
/// error message echoes a non-numeric value.
#[test]
fn denies_arithmetic_reads() {
    check(
        true,
        &[
            "T=$(gcloud auth print-access-token); echo $((T))",
            "T=$(gcloud auth print-access-token); X=$((T + 1))",
            "T=$(gcloud auth print-access-token); (( T > 0 ))",
            "T=$(gcloud auth print-access-token); let X=T+1",
            "T=$(gcloud auth print-access-token); [[ T -eq 0 ]]",
            "T=$(gcloud auth print-access-token); [ \"$T\" -gt 0 ]",
            "T=$(gcloud auth print-access-token); echo ${A[T]}",
            "T=$(gcloud auth print-access-token); A[T]=1",
        ],
    );
}

/// #8676 round 2, row 7: `=~` copies the match into the match arrays.
#[test]
fn denies_a_regex_match_copy() {
    check(
        true,
        &[
            "T=$(gcloud auth print-access-token); [[ $T =~ (.*) ]]; echo ${BASH_REMATCH[1]}",
            "T=$(gcloud auth print-access-token); [[ $T =~ (.*) ]] && echo $match",
            "T=$(gcloud auth print-access-token); [[ $T =~ .* ]] && echo $MATCH",
        ],
    );
}

/// #8676 round 2, row 8: a function header before a loop or a printer.
#[test]
fn denies_after_a_function_header() {
    check(
        true,
        &[
            "T=$(gcloud auth print-access-token); f() { echo $T; }; f",
            "T=$(gcloud auth print-access-token); function f { echo $T; }; f",
            "f() { for t in $(gcloud auth print-access-token); do echo $t; done; }; f",
            "function f() { set; }; T=$(gcloud auth print-access-token); f",
        ],
    );
}

/// #8676 round 2, row 9: environment readers that name no `$`.
#[test]
fn denies_environment_readers() {
    check(
        true,
        &[
            "export T=$(gcloud auth print-access-token); awk 'BEGIN { print ENVIRON[\"T\"] }'",
            "export T=$(gcloud auth print-access-token); jq -n env.T",
            "export T=$(gcloud auth print-access-token); jq -n '$ENV.T'",
            "export T=$(gcloud auth print-access-token); ps eww",
            "export T=$(gcloud auth print-access-token); ps -E",
            "export T=$(gcloud auth print-access-token); cat /proc/self/environ",
            "export T=$(gcloud auth print-access-token); strings /proc/$$/environ",
        ],
    );
}

/// #8676 round 2, row 10: zsh prints a name a flagless declaration names.
#[test]
fn denies_a_flagless_declaration_of_a_tainted_name() {
    check(
        true,
        &[
            "T=$(gcloud auth print-access-token); typeset T",
            "T=$(gcloud auth print-access-token); local T",
            "T=$(gcloud auth print-access-token); declare T",
        ],
    );
}

/// #8676 round 2: the neighbours of every row above still pass.
#[test]
fn allows_the_round_two_neighbours() {
    check(
        false,
        &[
            "T=$(gcloud auth print-access-token); python3 upload.py --token \"$T\"",
            "T=$(gcloud auth print-access-token); echo $((1 + 2))",
            "T=$(gcloud auth print-access-token); echo $(( ${#T} + 1 ))",
            "T=$(gcloud auth print-access-token); A=(x y); echo ${A[0]}",
            "T=$(gcloud auth print-access-token); f() { echo hi; }; f",
            "T=$(gcloud auth print-access-token); ps -o pid",
            "T=$(gcloud auth print-access-token); declare -x U=1",
            "T=$(gcloud auth print-access-token); [[ $T =~ ^ya29 ]] && echo ok",
            "T=$(gcloud auth print-access-token); jq -n '.x'",
            "python3 -c 'print(1)'; gcloud auth print-access-token > /tmp/fake-token",
            "T=$(gcloud auth print-access-token); echo ${#T}",
        ],
    );
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

/// #8676 round 3, HIGH 1: brace expansion builds a declared name. Each row was
/// allowed at 90e0813e1.
#[test]
fn denies_brace_expanded_declarer_names() {
    check(
        true,
        &[
            "export {T,U}=$(gcloud auth print-access-token); printenv T",
            "declare {T,U}=$(gcloud auth print-access-token); echo $T",
            "typeset {T,U}=`gcloud auth print-access-token`; echo $U",
            "local T{,}=$(gcloud auth print-access-token); echo $T",
            "export T{,}=$(gcloud auth print-access-token); printenv T",
        ],
    );
}

/// #8676 round 3, HIGH 2: an evaluator reads its script from stdin or a
/// process substitution by path. Each row was allowed at 90e0813e1.
#[test]
fn denies_an_evaluator_reading_its_script_by_path() {
    check(
        true,
        &[
            "export T=$(gcloud auth print-access-token); echo 'printenv T' | bash /dev/stdin",
            "export T=$(gcloud auth print-access-token); echo 'printenv T' | sh /dev/fd/0",
            "export T=$(gcloud auth print-access-token); echo 'printenv T' | bash /proc/self/fd/0",
            "export T=$(gcloud auth print-access-token); bash <(echo 'printenv T')",
            "export T=$(gcloud auth print-access-token); python3 <(echo 'import os;print(os.getenv(\"T\"))')",
            "export T=$(gcloud auth print-access-token); bash < <(echo 'printenv T')",
        ],
    );
}

/// #8676 round 3, HIGH 3: arithmetic contexts bash evaluates, whose error
/// message echoes a non-numeric value. Each row was allowed at 90e0813e1.
#[test]
fn denies_arithmetic_reads_in_offsets_and_integer_declarations() {
    check(
        true,
        &[
            "T=$(gcloud auth print-access-token); declare -i X=T",
            "T=$(gcloud auth print-access-token); local -i X=T",
            "T=$(gcloud auth print-access-token); typeset -i X=T",
            "T=$(gcloud auth print-access-token); echo ${X:T}",
            "T=$(gcloud auth print-access-token); echo ${X:0:T}",
            "T=$(gcloud auth print-access-token); echo $[T]",
            // A name holding the bare name is evaluated recursively.
            "T=$(gcloud auth print-access-token); U=T; echo $((U))",
            "T=$(gcloud auth print-access-token); declare -i X; X=T",
        ],
    );
}

/// #8676 round 3, HIGH 4: a nameref reaches the value through a loop, a
/// subscript, or a later assignment. Each row was allowed at 90e0813e1.
#[test]
fn denies_nameref_loops_and_subscripted_namerefs() {
    check(
        true,
        &[
            "T=$(gcloud auth print-access-token); declare -n R; for R in T; do echo $R; done",
            "T=$(gcloud auth print-access-token); A=$T; declare -n R='A[0]'; echo $R",
            "T=$(gcloud auth print-access-token); declare -n R; R=T; echo $R",
        ],
    );
}

/// #8676 round 3, MEDIUM 5: evaluators the list missed. Each row was allowed
/// at 90e0813e1.
#[test]
fn denies_the_missing_evaluators() {
    check(
        true,
        &[
            "export T=$(gcloud auth print-access-token); trap 'echo $T' EXIT",
            "trap \"echo $(gcloud auth print-access-token)\" EXIT",
            "export T=$(gcloud auth print-access-token); python3.12 -c 'import os; print(os.getenv(\"T\"))'",
            "export T=$(gcloud auth print-access-token); /usr/local/bin/python3.11 -c 'import os; print(os.getenv(\"T\"))'",
            "export T=$(gcloud auth print-access-token); bun -e 'console.log(process.env.T)'",
            "export T=$(gcloud auth print-access-token); lua -e 'print(os.getenv(\"T\"))'",
            "export T=$(gcloud auth print-access-token); pwsh -c '$env:T'",
        ],
    );
}

/// #8676 round 3, MEDIUM 6: builtins whose output or error text echoes the
/// operand. Each row was allowed at 90e0813e1.
#[test]
fn denies_builtins_that_echo_their_operand() {
    check(
        true,
        &[
            "T=$(gcloud auth print-access-token); cd \"$T\"",
            "T=$(gcloud auth print-access-token); exit \"$T\"",
            "T=$(gcloud auth print-access-token); kill \"$T\"",
            "T=$(gcloud auth print-access-token); sleep \"$T\"",
            "T=$(gcloud auth print-access-token); compgen -W \"$T\"",
            "T=$(gcloud auth print-access-token); export \"$T\"",
            "T=$(gcloud auth print-access-token); unset \"$T\"",
        ],
    );
}

/// #8676 round 3: the neighbours of every row above still pass.
#[test]
fn allows_the_round_three_neighbours() {
    check(
        false,
        &[
            "export {A,B}=1; gcloud auth print-access-token > /tmp/fake-token",
            "export T=$(gcloud auth print-access-token); bash deploy.sh",
            "export T=$(gcloud auth print-access-token); python3.12 upload.py",
            "T=$(gcloud auth print-access-token); echo ${X:-T} ${X:0:2}",
            "T=$(gcloud auth print-access-token); declare -i N=5",
            "T=$(gcloud auth print-access-token); declare -n R=HOME; echo $R",
            "T=$(gcloud auth print-access-token); for f in a b; do echo $f; done",
            "T=$(gcloud auth print-access-token); cd /tmp && sleep 1; echo ${#T}",
            "T=$(gcloud auth print-access-token); cd /tmp && curl -H \"Authorization: Bearer $T\" https://example.test",
        ],
    );
}

/// #8676 round 4, HIGH 1: a brace *sequence* builds the declared names. Each
/// row was allowed at 6aa69afab.
#[test]
fn denies_brace_sequence_declarer_names() {
    check(
        true,
        &[
            "declare {A..C}=$(gcloud auth print-access-token); echo $B",
            "export {A..C}=$(gcloud auth print-access-token); printenv B",
            "local T{1..2}=$(gcloud auth print-access-token); echo $T1",
            "T=$(gcloud auth print-access-token); typeset {X..Z}=$T; echo $Y",
        ],
    );
}

/// #8676 round 4, HIGH 2: an indexed array's `[key]=` in a compound
/// assignment is arithmetic, whose error echoes a non-numeric value. Each row
/// was allowed at 6aa69afab.
#[test]
fn denies_array_keys_read_as_arithmetic() {
    check(
        true,
        &[
            "T=$(gcloud auth print-access-token); A=([T]=1)",
            "T=$(gcloud auth print-access-token); A=(x [T]=1)",
            "T=$(gcloud auth print-access-token); A+=([$T]=1)",
            "T=$(gcloud auth print-access-token); declare -a A=([T+1]=x)",
        ],
    );
}

/// #8676 round 4, HIGH 3: a coproc runs its command with its output on a
/// descriptor a later stage reads. Each row was allowed at 6aa69afab.
#[test]
fn denies_a_coproc_that_carries() {
    check(
        true,
        &[
            "T=$(gcloud auth print-access-token); coproc printf %s \"$T\"",
            "T=$(gcloud auth print-access-token); coproc NAME { echo \"$T\"; }",
            "T=$(gcloud auth print-access-token); coproc NAME ( echo \"$T\" )",
            "T=$(gcloud auth print-access-token); coproc { printf %s \"$T\"; }",
            "T=$(gcloud auth print-access-token); coproc NAME for x in $T; do echo $x; done",
        ],
    );
}

/// #8676 round 4: the neighbours of every row above still pass. #8676 round
/// 5: a coproc row moved to `denies_the_round_five_bypasses` — the round-5
/// fix refuses every coproc stage while any name is tainted, including one
/// that (like this one did) never itself reads the name.
#[test]
fn allows_the_round_four_neighbours() {
    check(
        false,
        &[
            "declare {A..C}=1; gcloud auth print-access-token > /tmp/fake-token",
            "T=$(gcloud auth print-access-token); A=([0]=x [1]=y); echo ${A[1]}",
            "coproc cat /etc/hosts; gcloud auth print-access-token > /tmp/fake-token",
            "T=$(gcloud auth print-access-token); curl --oauth2-bearer=$T https://example.test",
        ],
    );
}

/// #8676 round 4 review, round 5 fix: four HIGH shapes the round-4 coproc
/// check and the rest of the scan missed. Each row was allowed at 8cd9ba4e1.
#[test]
fn denies_the_round_five_bypasses() {
    check(
        true,
        &[
            // 1: `ungroup` leaves a coproc's NAME as the program word, so a
            // declarer, `set`, or an evaluator inside never gets judged;
            // round 4's fix only caught a word that itself carried the
            // value. Refusing every coproc while tainted closes it,
            // including the benign neighbour that never reads the name.
            "T=$(gcloud auth print-access-token); coproc X ( declare -p T )",
            "T=$(gcloud auth print-access-token); coproc X ( set )",
            "T=$(gcloud auth print-access-token); coproc X ( eval 'echo $T' )",
            "T=$(gcloud auth print-access-token); coproc NAME { sleep 1; }",
            // 2: `select NAME in WORD...` lists every WORD on stderr.
            "T=$(gcloud auth print-access-token); select x in \"$T\"; do break; done",
            // 3: `${NAME?word}`/`${NAME:?word}` writes a carrying `word` to
            // stderr at expansion time, even in a stage that is only an
            // assignment.
            "T=$(gcloud auth print-access-token); : \"${UNSET:?$T}\"",
            "T=$(gcloud auth print-access-token); Y=${UNSET?$T}",
            // 4: a plain input redirect's missing-file error names the path.
            "T=$(gcloud auth print-access-token); wc -c < \"$T\"",
            "T=$(gcloud auth print-access-token); : < \"$T\"",
        ],
    );
}

/// #8676 round 5: the neighbours of every row above still pass.
#[test]
fn allows_the_round_five_neighbours() {
    check(
        false,
        &[
            "coproc X ( declare -p PATH ); gcloud auth print-access-token > /tmp/fake-token",
            "T=$(gcloud auth print-access-token); select x in a b c; do break; done",
            "T=$(gcloud auth print-access-token); : \"${SAFE:?fallback}\"",
            "T=$(gcloud auth print-access-token); wc -c < /tmp/fake-token-file",
        ],
    );
}

/// #8676 round 6, code-critic CRITICAL on round 5: a plain input redirect's
/// missing-file error names its target the same way round 5's bare `$T`
/// does, but a command-substitution or backtick target has no `$NAME` for
/// `expands_tainted` to match — the placeholder `lift_substitutions` leaves
/// behind carries no `$`. Each row was allowed at c3778fa0b.
#[test]
fn denies_the_round_six_bypasses() {
    check(
        true,
        &[
            "T=$(gcloud auth print-access-token); wc -c < \"$(echo \"$T\")\"",
            "T=$(gcloud auth print-access-token); : < \"$(echo \"$T\")\"",
            "T=$(gcloud auth print-access-token); wc -c < \"`echo $T`\"",
        ],
    );
}

/// #8676 round 6: a `<(…)` target is a real descriptor bash always opens —
/// never a missing-file error — so this sink must not flag it.
#[test]
fn allows_the_round_six_neighbours() {
    check(
        false,
        &["T=$(gcloud auth print-access-token); wc -c < <(echo \"$T\")"],
    );
}

/// Work units one [`super::scan`] of `command` spends, and whether it refuses.
fn scan_units(command: &str) -> (usize, bool) {
    let lifted = super::Lifted::default();
    let sink = super::Sink::Terminal;
    let refused = super::scan(command, sink, sink, 0, &lifted).is_err();
    (lifted.spent.get(), refused)
}

/// Units a scan charges: `passes` top-level passes, each one unit, one per KiB
/// of text, and one per single-pass `$(…)` body it lifts.
fn pinned_units(passes: usize, subs: usize, kib: usize) -> usize {
    passes * (1 + subs + kib)
}

/// #8676 round 3: wide bracket runs scan in linear work; a nest deeper than
/// the cap refuses.
///
/// Why: a scan from each opener to its close took minutes on the round-two
/// nesting row. #8765: the timed form failed CI at 1.00-1.17 s once #8734's
/// fixed point added a second pass, so this counts the scan's own work units.
/// What: at n, 2n and 4n KiB of run, pins the exact units, which fixes the
/// pass count (2 for an allowed row with the `T=` binding, 1 for a refused
/// row or with no binding), the charge per KiB and linear growth; then pins
/// the verdict where two passes cross the budget. The work-unit pins are the
/// primary assertion. #8765: one wall-clock backstop runs first, on the
/// timed form's own input (100k `${X:0:1}` runs, bound), and exists only to
/// catch superlinear work the budget never charges; #8771 tracks the proper
/// close, a `#[cfg(test)]` byte counter. Worst CI time today is 1.17 s, and
/// the 5 s budget is about 4x that.
#[test]
fn wide_bracket_runs_scan_in_linear_work() {
    let bound = "T=$(gcloud auth print-access-token); echo ";
    // #8765: wall-clock backstop for uncharged superlinear work; see #8771.
    let command = format!("{bound}{}", "${X:0:1}".repeat(100_000));
    let started = std::time::Instant::now();
    let verdict = evaluate_credential_print_command(&command);
    let spent = started.elapsed();
    assert!(verdict.is_none(), "100k `${{X:0:1}}` runs must be allowed");
    assert!(spent < std::time::Duration::from_secs(5), "took {spent:?}");
    // #8765: the seeds stay under 1 KiB and every run body is whole KiB, so
    // the charge per pass is exact. 4n stays under `WORK_BUDGET`.
    const KIB: usize = 1_024;
    const N_KIB: usize = 128;
    let unbound = "gcloud auth print-access-token >/dev/null; echo ";
    let body = |run: &str, kib: usize| run.repeat(kib * KIB / run.len());
    // A refused row stops inside its first pass; an allowed one takes two,
    // the second finding `T` already bound.
    for (run, deny, passes) in [
        ("A[", true, 1),
        ("$[", true, 1),
        ("${X:", true, 1),
        ("A[x]", false, 2),
        ("${X:0:1}", false, 2),
    ] {
        let mut units = Vec::new();
        for kib in [N_KIB, 2 * N_KIB, 4 * N_KIB] {
            let text = body(run, kib);
            assert_eq!(text.len(), kib * KIB, "{run:?}");
            let (got, refused) = scan_units(&format!("{bound}{text}"));
            assert_eq!(refused, deny, "{run:?} at {kib} KiB");
            // A third pass, or a costlier byte, changes this count.
            let want = pinned_units(passes, 1, kib);
            assert_eq!(got, want, "{run:?} at {kib} KiB, bound: {passes} pass(es)");
            units.push(got);
            let (got, refused) = scan_units(&format!("{unbound}{text}"));
            assert!(!refused, "{run:?} at {kib} KiB, unbound");
            let want = pinned_units(1, 0, kib);
            assert_eq!(got, want, "{run:?} at {kib} KiB, unbound: pinned 1 pass");
        }
        // Linear: doubling the run doubles the units it adds.
        let (n, n2, n4) = (units[0], units[1], units[2]);
        assert_eq!(n4 - n2, 2 * (n2 - n), "{run:?}: {units:?} not linear");
    }
    // At 2 MiB one pass fits the budget and two do not: the bound form
    // refuses as it charges its second pass, and the unbound form is allowed.
    let text = body("${X:0:1}", 2 * KIB);
    let (got, refused) = scan_units(&format!("{bound}{text}"));
    assert!(refused, "2 MiB bound must cross the budget");
    assert_eq!(got, pinned_units(2, 1, 2 * KIB), "2 MiB bound");
    assert!(pinned_units(1, 1, 2 * KIB) <= super::WORK_BUDGET);
    let (got, refused) = scan_units(&format!("{unbound}{text}"));
    assert!(!refused, "2 MiB unbound fits in one pass");
    assert_eq!(got, pinned_units(1, 0, 2 * KIB), "2 MiB unbound");
}

/// #8676 round 3: the deny reason never quotes the command, so a credential
/// literal pasted into it is not echoed back.
#[test]
fn deny_reason_never_echoes_the_command() {
    let command = "T=$(gcloud auth print-access-token); cd \"$T\" # ya29.fake-literal";
    let reason = evaluate_credential_print_command(command).unwrap_or_default();
    assert!(!reason.is_empty(), "must deny");
    assert!(!reason.contains("ya29.fake-literal"), "{reason}");
    assert!(!reason.contains("cd \""), "{reason}");
}

/// 🔴 REGRESSION (#8677): `security -i` (or `-p`, which implies it) runs the
/// commands its stdin carries. Each row was allowed at 27ec6fa20.
#[test]
fn denies_security_reading_commands_on_stdin_8677() {
    check(
        true,
        &[
            // The issue's reproduction.
            "echo 'find-generic-password -s s -w' | security -i",
            "printf 'find-generic-password -s fake-svc -w\\n' | security -i",
            "security -i <<< 'find-generic-password -s fake-svc -w'",
            "security -i <<'EOF'\nfind-generic-password -s fake-svc -w\nEOF",
            "echo 'dump-keychain -d' | /usr/bin/security -vi",
            "echo 'find-generic-password -s fake-svc -w' | security -q -p 'kc> '",
            "echo 'find-generic-password -s fake-svc -w' | sudo -u fake security -i",
            "echo 'find-generic-password -s fake-svc -w' | timeout 5 security -i",
            "echo 'find-generic-password -s fake-svc -w' | bash -c 'security -i'",
            // No credential subcommand is spelled out, so only `-i` shows it.
            "printf 'find-%s-password -s fake-svc -w\\n' generic | security -i",
            "security -i < fake-cmds.txt",
        ],
    );
}

/// #8677 error arm: a `security -i` whose input the guard cannot read — a
/// run-time file, broken quoting — refuses as unreadable, never allows.
#[test]
fn refuses_an_unclassifiable_interactive_security_8677() {
    for command in [
        "security -i < \"$FAKE_CMDS\"",
        "security -i <<< 'find-generic-password -s fake-svc -w",
        "security -p \"$PROMPT\" <&3",
    ] {
        let reason = evaluate_credential_print_command(command).unwrap_or_default();
        assert!(
            reason.contains("cannot read"),
            "{command:?} must refuse as unreadable: {reason:?}"
        );
    }
}

/// 🔴 REGRESSION (#8677): `curl -v` (and `--trace*`/`--libcurl`) echoes the
/// request headers, so a credential in any argument reaches tool output. Each
/// row was allowed at 27ec6fa20.
#[test]
fn denies_a_verbose_curl_carrying_a_credential_8677() {
    check(
        true,
        &[
            // The issue's reproduction.
            "curl -H \"Authorization: Bearer $(gcloud auth print-access-token)\" -v",
            "curl -v -H \"Authorization: Bearer $(gcloud auth print-access-token)\" https://example.test",
            "curl --verbose -H \"x: $(gcloud auth print-access-token)\" https://example.test",
            "curl -sSv -o /dev/null -H \"x: $(gcloud auth print-access-token)\" https://example.test",
            "curl -vsH \"x: $(gcloud auth print-access-token)\" https://example.test 2>&1 | head",
            "curl -v -u \"fake:$(security find-generic-password -s fake-svc -w)\" https://example.test",
            "T=$(gcloud auth print-access-token); curl -v -H \"Authorization: Bearer $T\" https://example.test",
            "timeout 5 curl -v -H \"x: $(gcloud auth print-access-token)\" https://example.test",
            "curl --trace-ascii - -H \"x: $(gcloud auth print-access-token)\" https://example.test",
            "curl --trace % -H \"x: $(gcloud auth print-access-token)\" https://example.test",
            "curl --trace /dev/tty -H \"x: $(gcloud auth print-access-token)\" https://example.test >/dev/null 2>&1",
            "curl --libcurl - -H \"x: $(gcloud auth print-access-token)\" https://example.test",
            "curl -v --stderr - -H \"x: $(gcloud auth print-access-token)\" https://example.test 2>/dev/null",
        ],
    );
}

/// 🔴 REGRESSION (#8677): a credential written to a redirect target the guard
/// cannot read (`> "$OUT"`), or to a device spelled with extra `/`, `.` or
/// `..` segments. Each row was allowed at 27ec6fa20.
#[test]
fn denies_a_credential_redirected_to_an_unread_target_8677() {
    check(
        true,
        &[
            // The issue's reproductions.
            "security find-generic-password -s fake-svc -w > \"$OUT\"",
            "security find-generic-password -s fake-svc -w > /dev//stdout",
            "security find-generic-password -s fake-svc -w > /dev/./tty",
            "gcloud auth print-access-token > $OUT",
            "gcloud auth print-access-token > \"$HOME/.gcloud-token\"",
            "gcloud auth print-access-token >> \"${OUT}/token\"",
            "gcloud auth print-access-token > `echo /dev/tty`",
            "security find-generic-password -g -s fake-svc 2> \"$ERR\"",
            // A `$(mktemp)` name rebound, or a body that is not a lone mktemp.
            "tmp=$(mktemp); tmp=/dev/tty; gcloud auth print-access-token > \"$tmp\"",
            "tmp=$(mktemp); read tmp < fake-list; gcloud auth print-access-token > \"$tmp\"",
            "tmp=$(mktemp); for tmp in /dev/tty; do gcloud auth print-access-token > \"$tmp\"; done",
            "tmp=$(mktemp; echo /dev/tty); gcloud auth print-access-token > \"$tmp\"",
            "gcloud auth print-access-token > /dev/../dev/tty",
            "gcloud auth print-access-token > //dev/stderr",
            "gcloud auth print-access-token >/dev//fd//1",
            "gcloud auth print-access-token > ../../../../../../dev/tty",
            "gcloud auth print-access-token > /dev/ttys001",
            "gcloud auth print-access-token > /dev/pts/0",
            "gcloud auth print-access-token > /dev/tt?",
            "gcloud auth print-access-token > /dev/stdin",
            "gcloud auth print-access-token > /proc/self/fd/2",
            "gcloud auth print-access-token 1<>/dev//tty",
            "gcloud auth print-access-token | tee /dev//tty >/dev/null",
            // A missing directory's error names the carrying target.
            "T=$(gcloud auth print-access-token); echo x > \"/nonexistent/$T\"",
            // A wrapped call's stderr piped on to a printing reader.
            "sh -c 'security find-generic-password -g -s fake-svc' 2>&1 | cat",
            "sh -c 'security find-generic-password -g -s fake-svc' 2>\"$ERR\"",
        ],
    );
}

/// 🔴 REGRESSION (#8677): the sibling secret-printing CLIs the issue lists.
/// Each row was allowed at 27ec6fa20.
#[test]
fn denies_the_sibling_credential_clis_8677() {
    check(
        true,
        &[
            "gcloud secrets versions access latest --secret=fake-secret",
            "gcloud secrets versions access 1 --secret fake-secret | head -c 10",
            "gcloud secrets versions access latest --secret=fake --out-file=/dev/stdout",
            "gh auth token",
            "gh auth token --hostname github.example.test",
            "/opt/homebrew/bin/gh auth status --show-token",
            "gh auth status -t",
            "op read op://fake-vault/fake-item/password",
            "op read --out-file /dev/stdout op://fake-vault/fake-item/password",
            "aws configure get aws_secret_access_key",
            "aws configure get aws_session_token --profile fake",
            "aws configure export-credentials --profile fake",
            "echo \"$(gh auth token)\"",
            "T=$(op read op://fake-vault/fake-item/password); echo \"$T\"",
        ],
    );
}

/// #8677: the nearest benign forms of each refused shape stay allowed.
#[test]
fn allows_the_8677_neighbours() {
    check(
        false,
        &[
            // curl with no credential, or a credential with no echo flag.
            "curl -v https://example.test",
            "T=$(gcloud auth print-access-token); curl -v https://example.test >/dev/null",
            "curl -sS -H \"Authorization: Bearer $(gcloud auth print-access-token)\" https://example.test/v1",
            "curl -sSH \"x: $(gcloud auth print-access-token)\" https://example.test",
            "curl -H \"x: $(gcloud auth print-access-token)\" -d -v https://example.test",
            "curl -s -H \"x: $(gcloud auth print-access-token)\" --trace /tmp/fake-trace https://example.test",
            // security with no `-w`/`-g`, no `-i`, or help only.
            "security -h",
            "security -h; security find-generic-password -s fake-svc >/dev/null 2>&1",
            "security -v find-generic-password -s fake-svc >/dev/null 2>&1",
            "security find-generic-password -s fake-svc",
            "security find-generic-password -s fake-svc > \"$OUT\"",
            "grep security -i notes.txt; gcloud auth print-access-token > /tmp/fake-token",
            "echo 'security -i' > notes.md",
            // Ordinary redirects of non-secret output, and plain files.
            "gcloud config list > \"$OUT\" && gcloud auth print-access-token > /tmp/fake-token",
            "echo done > \"$LOG\"; T=$(gcloud auth print-access-token)",
            "printf '%s\\n' 'never run gcloud auth print-access-token bare' > \"$OUT\"",
            "gcloud auth print-access-token > /tmp//fake-token-file",
            "gcloud auth print-access-token > ./dev-token.txt",
            "gcloud auth print-access-token > /dev//null 2>&1",
            // #8730 / #8763: clobber redirects to an ordinary file.
            "security find-generic-password -s x -w >|/tmp/fake-out",
            "security find-generic-password -s x -w >>!/tmp/fake-out",
            // Sibling CLIs that print no secret, or capture it.
            "gh auth status",
            "gh auth token | docker login ghcr.io -u fake --password-stdin",
            "GH_TOKEN=$(gh auth token) gh pr list",
            "aws configure get region",
            "aws configure list",
            "op read --out-file /tmp/fake-out op://fake-vault/fake-item/password",
            "gcloud secrets versions access latest --secret=fake --out-file=/tmp/fake-out",
            "gcloud secrets list",
            "X=$(gcloud secrets versions access latest --secret=fake)",
            "git commit -m 'docs: never run gh auth token or op read bare'",
        ],
    );
}

/// 🔴 REGRESSION (#8677 review round 2, finding 1): a non-identifier
/// assignment (`tmp+=`, `tmp[0]=`) rebinds a `$(mktemp)` name. Each row was
/// allowed at d8d2ec909.
#[test]
fn denies_a_temp_name_rebound_by_a_non_identifier_assignment_8677() {
    check(
        true,
        &[
            "tmp=$(mktemp -d); tmp+=/../../../../../../../../dev/stdout; gcloud auth print-access-token > \"$tmp\"",
            "tmp=$(mktemp); tmp[0]=/dev/stdout; gcloud auth print-access-token > \"$tmp\"",
        ],
    );
}

/// 🔴 REGRESSION (#8677 review round 2, finding 2): a `$(mktemp)` name
/// rebound through quoting, a function, a computed name, or a symlink planted
/// on its file. Each row was allowed at d8d2ec909.
#[test]
fn denies_a_temp_name_rebound_out_of_sight_8677() {
    check(
        true,
        &[
            "tmp=$(mktemp); declare t\\mp=/dev/stdout; gcloud auth print-access-token > \"$tmp\"",
            "tmp=$(mktemp); read t''mp <<< /dev/stdout; gcloud auth print-access-token > \"$tmp\"",
            "f() { tmp=/dev/stdout; }; tmp=$(mktemp); f; gcloud auth print-access-token > \"$tmp\"",
            "tmp=$(mktemp); trap 't\\mp=/dev/stdout' DEBUG; gcloud auth print-access-token > \"$tmp\"",
            "tmp=$(mktemp); ln -sf /dev/stdout \"$tmp\"; gcloud auth print-access-token > \"$tmp\"",
            "tmp=$(mktemp); mv -f fake-link \"$tmp\"; gcloud auth print-access-token > \"$tmp\"",
            "n=t; tmp=$(mktemp); printf -v \"${n}mp\" %s /dev/stdout; gcloud auth print-access-token > \"$tmp\"",
            "tmp=$(mktemp); read t{m,}p <<< /dev/stdout; gcloud auth print-access-token > \"$tmp\"",
            // Review round 3: an unlisted linker, inline code, a quoted
            // heredoc body, and a nameref to a name built with `+=`.
            "tmp=$(mktemp); gln -sf /dev/stdout \"$tmp\"; gcloud auth print-access-token > \"$tmp\"",
            "tmp=$(mktemp); perl -e 'symlink q:/dev/stdout:, shift' \"$tmp\"; gcloud auth print-access-token > \"$tmp\"",
            "tmp=$(mktemp /tmp/fake.XXXXXX); sh <<'EOF'\nln -sf /dev/stdout /tmp/fake.*\nEOF\ngcloud auth print-access-token > \"$tmp\"",
            "tmp=$(mktemp); declare -n r=t; declare +n r; r+=mp; declare -n r; r=/dev/stdout; gcloud auth print-access-token > \"$tmp\"",
            // Review round 3: no `$(mktemp)` carve-out; a variable target refuses.
            "tmp=$(mktemp); trap 'rm -f \"$tmp\"' EXIT; gcloud auth print-access-token > \"$tmp\"",
            "tmp=$(mktemp); gcloud auth print-access-token > \"$tmp\"",
            "tmp=$(mktemp -t fake) && gcloud auth print-access-token > \"${tmp}\" && wc -c \"$tmp\"",
            "tmp=$(mktemp); gcloud auth print-access-token | tee \"$tmp\" >/dev/null",
        ],
    );
}

/// 🔴 REGRESSION (#8677 review round 2, finding 3): `security -i` behind a
/// wrapper the guard does not know. Each row was allowed at d8d2ec909.
#[test]
fn denies_interactive_security_behind_an_unknown_wrapper_8677() {
    check(
        true,
        &[
            "echo 'find-generic-password -s s -w' | arch -arm64 security -i",
            "echo 'find-generic-password -s s -w' | script -q /dev/null security -i",
            "echo 'find-generic-password -s s -w' | launchctl asuser 501 security -i",
        ],
    );
}

/// 🔴 REGRESSION (#8677 review round 2, finding 4): APFS matches `/DEV` to
/// `/dev`. Each row was allowed at d8d2ec909.
#[test]
fn denies_a_device_path_in_another_case_8677() {
    check(
        true,
        &[
            "gcloud auth print-access-token > /DEV/stdout",
            "gcloud auth print-access-token > /Dev/Tty",
            "gcloud auth print-access-token | tee /DEV/STDERR >/dev/null",
        ],
    );
}

/// 🔴 REGRESSION (#8677 review round 2, finding 5): a relative target whose
/// directory the guard cannot know. Each row was allowed at d8d2ec909.
#[test]
fn denies_a_relative_target_in_an_unknown_directory_8677() {
    check(
        true,
        &[
            "cd /dev && gcloud auth print-access-token > stdout",
            "cd /dev; gcloud auth print-access-token > fake-out",
            "pushd /dev/fd && gcloud auth print-access-token > 1",
            "cd /dev && cd /tmp && gcloud auth print-access-token > ~-/stdout",
            "gcloud auth print-access-token > ~+/stdout",
            "gcloud auth print-access-token > stdout",
            "gcloud auth print-access-token > fd/1",
            "gcloud auth print-access-token > tty",
        ],
    );
}

/// 🔴 REGRESSION (#8677 review round 2, finding 6): a `tee`/`dd` file operand
/// chosen at run time. Each row was allowed at d8d2ec909.
#[test]
fn denies_a_tee_operand_chosen_at_run_time_8677() {
    check(
        true,
        &[
            "OUT=/dev/stdout; gcloud auth print-access-token | tee \"$OUT\" >/dev/null",
            "gcloud auth print-access-token | tee -a \"$OUT\" >/dev/null",
            "gcloud auth print-access-token | dd of=\"$OUT\" >/dev/null 2>&1",
            "cd /dev; gcloud auth print-access-token | tee stdout >/dev/null",
        ],
    );
}

/// 🔴 REGRESSION (#8677 review round 2, finding 7): a sibling CLI's
/// subcommand chosen at run time. Each row was allowed at d8d2ec909.
#[test]
fn denies_a_cli_subcommand_chosen_at_run_time_8677() {
    check(
        true,
        &[
            "gh auth $(echo token)",
            "gh $(echo auth) token",
            "gh auth `echo token`",
            "op $(echo read) op://fake-vault/fake-item/password",
            "aws configure $(echo get) aws_secret_access_key",
            // Review round 3: a global flag's value is not a subcommand slot.
            "gh -R fake-owner/fake-repo auth $(echo token)",
            // Review round 4: an unquoted expansion inside a flag word splits
            // into the subcommand.
            "gh auth --hostname=$(printf 'github.com token')",
            "T=' token'; gh auth -hgithub.com$T",
        ],
    );
}

/// #8677 review round 2: the nearest benign forms stay allowed.
#[test]
fn allows_the_8677_round_two_neighbours() {
    check(
        false,
        &[
            "gcloud auth print-access-token | tee /tmp/fake-out >/dev/null",
            "gcloud auth print-access-token | tee -a fake-out >/dev/null",
            "gcloud auth print-access-token > ./fake-token.txt",
            // The file form the deny text advises.
            "(umask 077; gcloud auth print-access-token > ~/.gcloud-token)",
            "grep security -i notes.txt",
            "rg security -i notes.txt",
            "git commit -m 'fix: never pipe to security -i or run gh auth $(echo token)'",
            "gh pr create --title fake --body 'use gh auth token, not security -i'",
            "GH_TOKEN=$(gh auth token) gh pr view \"$N\"",
            // Review round 4: `gh auth` then `$` is parsed, not refused.
            "gh auth status --hostname \"$H\"",
            "gh api \"repos/$R/pulls\"",
            "X=$(op read \"op://fake-vault/$ITEM/password\")",
        ],
    );
}
