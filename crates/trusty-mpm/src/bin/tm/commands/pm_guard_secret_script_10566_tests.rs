//! #10566: a script run inside a command substitution is judged like one run
//! directly. Every fixture is a fake placeholder; nothing is executed.

use super::*;

use crate::commands::pm_guard_bash::MAX_SUBSTITUTION_DEPTH;

/// A Keychain read that prints its value — refused inline (#8596).
const KEYCHAIN_READ: &str = "security find-generic-password -s fake-svc -w\n";

/// Write `body` to `dir/name`.
fn script(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("write script");
}

/// 🔴 REGRESSION (#10566): a credential-reading script run from a `$( … )`
/// or backtick body — assigned, quoted, nested, or inside a script — refuses
/// with the reason the plain `bash probe.sh` gets, naming the same script.
#[test]
fn a_script_run_in_a_substitution_is_judged_10566() {
    let dir = tempfile::tempdir().expect("dir");
    script(dir.path(), "probe.sh", KEYCHAIN_READ);
    script(
        dir.path(),
        "outer.sh",
        "T=\"$(bash ./probe.sh)\"\necho \"$T\"\n",
    );
    script(dir.path(), "tick.sh", "T=`bash probe.sh`\necho \"$T\"\n");
    let plain = evaluate_script_body_secret_read("bash probe.sh", dir.path())
        .expect("the plain run refuses");
    for (command, word) in [
        ("T=\"$(bash probe.sh)\"", "probe.sh"),
        ("T=$(bash probe.sh); echo \"$T\"", "probe.sh"),
        ("T=`bash probe.sh`", "probe.sh"),
        ("echo \"$(echo \"$(bash probe.sh)\")\"", "probe.sh"),
        ("T=\"$(sh ./probe.sh)\"", "./probe.sh"),
        ("bash outer.sh", "outer.sh"),
        ("bash tick.sh", "tick.sh"),
    ] {
        let reason = evaluate_script_body_secret_read(command, dir.path()).unwrap_or_default();
        assert!(reason.contains("#8879"), "`{command}` must deny: {reason}");
        assert!(reason.contains(&format!("`{word}`")), "{command}: {reason}");
        assert!(
            reason.contains("runs a credential-reading command"),
            "same rule class as the plain run ({plain}): {reason}"
        );
        assert!(!reason.contains("fake-svc"), "never quotes the body");
    }
}

/// 🔴 REGRESSION (#10566, fail closed): substitutions nested past
/// `MAX_SUBSTITUTION_DEPTH` refuse rather than go unread.
#[test]
fn substitutions_nested_past_the_bound_refuse_10566() {
    let dir = tempfile::tempdir().expect("dir");
    let deep = MAX_SUBSTITUTION_DEPTH + 2;
    let command = format!("echo {}true{}", "$(".repeat(deep), ")".repeat(deep));
    let reason = evaluate_script_body_secret_read(&command, dir.path()).unwrap_or_default();
    assert!(
        reason.contains("nests command substitutions past"),
        "must deny: {reason}"
    );
    let shallow = format!("echo {}true{}", "$(".repeat(4), ")".repeat(4));
    assert_eq!(evaluate_script_body_secret_read(&shallow, dir.path()), None);
}

/// No over-deny (#10566): a substitution running a clean script or no
/// script, a quoted `$(` mention, and a script an earlier stage writes allow.
#[test]
fn a_substitution_with_no_credential_read_allows_10566() {
    let dir = tempfile::tempdir().expect("dir");
    script(dir.path(), "clean.sh", "echo fake-value\n");
    script(dir.path(), "probe.sh", KEYCHAIN_READ);
    // Residual 10: in a body, a substitution running no regular file allows.
    script(
        dir.path(),
        "prose.sh",
        "echo \"run `./never-written.sh` in `/`\"\n",
    );
    for command in [
        "bash prose.sh",
        "T=\"$(bash clean.sh)\"; echo \"$T\"",
        "echo \"$(date +%s)\" `pwd`",
        "git commit -m \"$(cat msg.txt)\"",
        "echo 'run $(bash probe.sh) later'",
        "printf 'echo hi' > later.sh; T=\"$(bash later.sh)\"",
    ] {
        assert_eq!(
            evaluate_script_body_secret_read(command, dir.path()),
            None,
            "`{command}` must allow"
        );
    }
}

/// 🔴 REGRESSION (#10566): a backtick pair in a trailing `#` comment is not
/// run, so it is not judged; a `#` inside quotes or a word opens no comment.
#[test]
fn a_substitution_in_a_comment_is_not_run_10566() {
    let dir = tempfile::tempdir().expect("dir");
    script(dir.path(), "probe.sh", KEYCHAIN_READ);
    let commented = "echo hi  # see `bash probe.sh` and `//`";
    assert_eq!(
        evaluate_script_body_secret_read(commented, dir.path()),
        None
    );
    for command in [
        "echo \"a # b\" $(bash probe.sh)",
        "echo a#b $(bash probe.sh)",
    ] {
        let reason = evaluate_script_body_secret_read(command, dir.path()).unwrap_or_default();
        assert!(
            reason.contains("`probe.sh`"),
            "`{command}` must deny: {reason}"
        );
    }
}
