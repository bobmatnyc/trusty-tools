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
