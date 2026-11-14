//! #11262: a segment whose shell operators `tokenize` cannot place refuses in
//! every rule that used to read it as no words.
//!
//! Why: the operator cut added a second `tokenize` error, and the script,
//! computed-read, copy and write rules turned any error into "skip this
//! segment".
//! What: per rule, one segment `tokenize` cannot place that runs, reads,
//! copies or writes what the rule refuses, asserted at the rule and through
//! the secret bands the guard asks for a Bash command. The ANSI-C check ahead
//! of every rule already refuses a live `$'…'`, so a guard-level row built on
//! [`SPAN`] is a pin, not a regression row. Every fixture is a placeholder;
//! nothing is executed.
//! Test: itself.

use std::path::Path;

use crate::commands::pm_guard_bash::{
    TokenizeError, anchor_writes, evaluate_secret_file_copy_command, shell_write_targets, tokenize,
    unclassifiable_command,
};
use crate::commands::pm_guard_secret_read::evaluate_secret_file_read_in;
use crate::commands::pm_guard_secret_script::evaluate_script_body_secret_read;
use crate::commands::pm_guard_secret_substitution_read::evaluate_substitution_read_command;

/// Text `tokenize` cannot place: bash reads `$'\''` as one quote character
/// and the closing `\'` as another, while the `shlex` view reads one quoted
/// span between them, so the `>` inside is live to bash only.
const SPAN: &str = r"$'\'' 2>/dev/null \'";

/// A worktree destination the copy rule refuses a secret into.
const WORKTREE: &str = "/repo/.claude/worktrees/agent-x/";

/// A Keychain read that prints its value, refused inline (#8596).
const KEYCHAIN_READ: &str = "security find-generic-password -s fake-svc -w\n";

/// A cwd outside any repository.
fn probe_cwd() -> &'static Path {
    Path::new("/tmp/pm-guard-11262-probe")
}

/// A directory holding `probe.sh`, a script that reads a credential.
fn script_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("dir");
    std::fs::write(dir.path().join("probe.sh"), KEYCHAIN_READ).expect("write script");
    dir
}

/// The secret bands `pm_guard` asks for a Bash command: the unclassifiable
/// check, the script-body rule, the #7266 read entry point (which runs the
/// credential-print, nested and computed-read rules) and the copy rule.
fn guard(command: &str, cwd: &Path) -> Option<String> {
    let input = serde_json::json!({ "command": command });
    unclassifiable_command(command)
        .map(str::to_string)
        .or_else(|| evaluate_script_body_secret_read(command, cwd))
        .or_else(|| evaluate_secret_file_read_in("Bash", Some(&input), Some(cwd)))
        .or_else(|| evaluate_secret_file_copy_command(command, cwd))
}

/// The rows below reach the error arm: both shapes are ones `tokenize`
/// cannot place.
#[test]
fn the_rows_are_segments_tokenize_cannot_place_11262() {
    for segment in [
        format!("bash probe.sh {SPAN}"),
        "B=. A=$(case x in y) z>f)".to_string(),
    ] {
        assert_eq!(
            tokenize(&segment),
            Err(TokenizeError::AmbiguousOperators),
            "{segment}"
        );
    }
}

/// The script walk: a script run in a segment it cannot place refuses as
/// unplaceable instead of reading as no words.
#[test]
fn an_unplaced_script_run_refuses_11262() {
    let dir = script_dir();
    let command = format!("bash probe.sh {SPAN}");
    let reason = evaluate_script_body_secret_read(&command, dir.path()).unwrap_or_default();
    assert!(reason.contains("#8879"), "must deny: {reason}");
    assert!(
        reason.contains("cannot place the shell operators"),
        "names the parse problem: {reason}"
    );
    assert!(!reason.contains("fake-svc"), "never quotes the body");
    assert!(guard(&command, dir.path()).is_some(), "guard pin");
}

/// The script operand: an earlier assignment it cannot place leaves `$B`
/// unknown, and the script it names refuses as unplaceable.
#[test]
fn an_unplaced_assignment_before_a_script_refuses_11262() {
    let dir = script_dir();
    let command = "B=. A=$(case x in y) z>f) ; bash $B/probe.sh";
    let reason = evaluate_script_body_secret_read(command, dir.path()).unwrap_or_default();
    assert!(
        reason.contains("cannot place the shell operators"),
        "must deny as unplaceable: {reason}"
    );
    assert!(guard(command, dir.path()).is_some(), "guard pin");
}

/// The computed-read rule: a reader with a computed operand in a segment it
/// cannot place refuses; a substitution outside any read does not.
#[test]
fn an_unplaced_computed_read_refuses_11262() {
    let command = format!("cat $(ls | head -1) {SPAN}");
    let reason = evaluate_substitution_read_command(&command).unwrap_or_default();
    assert!(reason.contains("#8931"), "must deny: {reason}");
    assert!(
        reason.contains("cannot place its shell operators"),
        "names the parse problem: {reason}"
    );
    assert!(guard(&command, probe_cwd()).is_some(), "guard pin");
    assert_eq!(
        evaluate_substitution_read_command(&format!("echo $(date) {SPAN}")),
        None
    );
}

/// The copy rule: a secret-shaped word in a segment it cannot place refuses;
/// an ordinary copy and unbalanced quoting keep their answers.
#[test]
fn an_unplaced_copy_of_a_secret_refuses_11262() {
    let command = format!("cp .env {SPAN} {WORKTREE}");
    let reason = evaluate_secret_file_copy_command(&command, probe_cwd()).unwrap_or_default();
    assert!(reason.contains("#11262"), "must deny: {reason}");
    assert!(reason.contains("`.env`"), "names the word: {reason}");
    assert!(guard(&command, probe_cwd()).is_some(), "guard pin");
    let ordinary = format!("cp notes.txt {SPAN} {WORKTREE}");
    assert_eq!(
        evaluate_secret_file_copy_command(&ordinary, probe_cwd()),
        None
    );
}

/// The copy rule's `cd` tracking: a `cd` it cannot place leaves a later
/// relative destination unknown, so a secret copy there refuses.
#[test]
fn an_unplaced_cd_before_a_secret_copy_refuses_11262() {
    let command = format!("cd /tmp/elsewhere {SPAN} && cp .env .");
    let reason = evaluate_secret_file_copy_command(&command, probe_cwd()).unwrap_or_default();
    assert!(reason.contains("#11262"), "must deny: {reason}");
    assert!(guard(&command, probe_cwd()).is_some(), "guard pin");
    let ordinary = format!("cd /tmp/elsewhere {SPAN} && cp notes.txt .");
    assert_eq!(
        evaluate_secret_file_copy_command(&ordinary, probe_cwd()),
        None
    );
}

/// The sweep's write rules: a write verb behind a flagged wrapper, in a
/// segment they cannot place, refuses as unplaceable in each reader.
#[test]
fn an_unplaced_write_verb_refuses_11262() {
    for command in [
        format!("sudo -u x tee out.txt {SPAN}"),
        format!("sudo -u x cp a.rs src/lib.rs {SPAN}"),
    ] {
        assert!(
            shell_write_targets(&command, &|_| false).is_err(),
            "{command}"
        );
    }
    let anchor = format!("sudo -u x cp a.rs src/lib.rs {SPAN}");
    assert!(anchor_writes(&anchor).is_err(), "{anchor}");
}
