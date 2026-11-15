//! #11276: a segment whose shell operators `tokenize` cannot place refuses at
//! each rule that used to read it as no words.
//!
//! Why: #11262 added `TokenizeError::AmbiguousOperators`, and the script
//! walk, script operand, computed-read and copy rules turned any tokenize
//! error into "skip this segment".
//! What: per call site, one segment `tokenize` cannot place that runs, reads
//! or copies what the rule refuses, asserted at the rule (the regression row)
//! and through the secret bands the guard composes for a Bash command (a pin:
//! the ANSI-C check ahead of every rule already refuses these shapes). The
//! fifth site, `fixed_output`, cannot receive the new error: it returns
//! before tokenizing any body holding `$`, `` ` `` or `\`, and its `None`
//! already refuses. Every fixture is a placeholder; nothing is executed.
//! Test: itself.

use std::path::Path;

use super::bash_tokens::{TokenizeError, tokenize};
use super::{evaluate_secret_file_copy_command, unclassifiable_command};
use crate::commands::pm_guard_secret_read::evaluate_secret_file_read_in;
use crate::commands::pm_guard_secret_script::evaluate_script_body_secret_read;
use crate::commands::pm_guard_secret_substitution_read::evaluate_substitution_read_command;

/// Text `tokenize` cannot place: bash and the `shlex` view disagree on where
/// the ANSI-C quoted span ends, so the `>` inside is live to bash only.
const SPAN: &str = r"$'\'' 2>/dev/null \'";

/// A worktree destination the copy rule refuses a secret into.
const WORKTREE: &str = "/repo/.claude/worktrees/agent-x/";

/// A Keychain read that prints its value, refused inline (#8596).
const KEYCHAIN_READ: &str = "security find-generic-password -s fake-svc -w\n";

/// A cwd outside any repository.
fn probe_cwd() -> &'static Path {
    Path::new("/tmp/pm-guard-11276-probe")
}

/// A directory holding `probe.sh`, a script that reads a credential.
fn script_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("dir");
    std::fs::write(dir.path().join("probe.sh"), KEYCHAIN_READ).expect("write script");
    dir
}

/// The secret bands `pm_guard` asks for a Bash command: the unclassifiable
/// check, the script-body rule, the #7266 read entry point (which runs the
/// computed-read rule) and the copy rule.
fn guard(command: &str, cwd: &Path) -> Option<String> {
    let input = serde_json::json!({ "command": command });
    unclassifiable_command(command)
        .map(str::to_string)
        .or_else(|| evaluate_script_body_secret_read(command, cwd))
        .or_else(|| evaluate_secret_file_read_in("Bash", Some(&input), Some(cwd)))
        .or_else(|| evaluate_secret_file_copy_command(command, cwd))
}

/// The rows below reach the error arm.
#[test]
fn the_rows_are_segments_tokenize_cannot_place_11276() {
    for segment in [
        format!("bash probe.sh {SPAN}"),
        "B=. A=$(case x in y) z>f)".to_string(),
        format!("cp .env {SPAN} {WORKTREE}"),
    ] {
        assert_eq!(
            tokenize(&segment),
            Err(TokenizeError::AmbiguousOperators),
            "{segment}"
        );
    }
}

/// Site 1, the script walk: a script run in a segment it cannot place
/// refuses as unplaceable instead of reading as no words.
#[test]
fn an_unplaced_script_run_refuses_11276() {
    let dir = script_dir();
    let command = format!("bash probe.sh {SPAN}");
    let reason = evaluate_script_body_secret_read(&command, dir.path()).unwrap_or_default();
    assert!(
        reason.contains("cannot place the shell operators"),
        "must deny as unplaceable: {reason}"
    );
    assert!(!reason.contains("fake-svc"), "never quotes the body");
    assert!(guard(&command, dir.path()).is_some(), "guard pin");
}

/// Site 2, the script operand: an earlier assignment it cannot place leaves
/// `$B` unknown, and the script it names refuses as unplaceable.
#[test]
fn an_unplaced_assignment_before_a_script_refuses_11276() {
    let dir = script_dir();
    let command = "B=. A=$(case x in y) z>f) ; bash $B/probe.sh";
    let reason = evaluate_script_body_secret_read(command, dir.path()).unwrap_or_default();
    assert!(
        reason.contains("cannot place the shell operators"),
        "must deny as unplaceable: {reason}"
    );
    assert!(guard(command, dir.path()).is_some(), "guard pin");
}

/// Site 3, the computed-read rule: a reader with a computed operand in a
/// segment it cannot place refuses.
#[test]
fn an_unplaced_computed_read_refuses_11276() {
    let command = format!("cat $(ls | head -1) {SPAN}");
    let reason = evaluate_substitution_read_command(&command).unwrap_or_default();
    assert!(reason.contains("#11276"), "must deny: {reason}");
    assert!(guard(&command, probe_cwd()).is_some(), "guard pin");
}

/// Site 4, the copy rule: a secret-shaped word in a segment it cannot place
/// refuses.
#[test]
fn an_unplaced_copy_of_a_secret_refuses_11276() {
    let command = format!("cp .env {SPAN} {WORKTREE}");
    let reason = evaluate_secret_file_copy_command(&command, probe_cwd()).unwrap_or_default();
    assert!(reason.contains("#11276"), "must deny: {reason}");
    assert!(reason.contains("`.env`"), "names the word: {reason}");
    assert!(guard(&command, probe_cwd()).is_some(), "guard pin");
}

/// Site 4, the copy rule's `cd` tracking: a `cd` it cannot place leaves a
/// later relative destination unknown, so a secret copy there refuses.
#[test]
fn an_unplaced_cd_before_a_secret_copy_refuses_11276() {
    let command = format!("cd /tmp/elsewhere {SPAN} && cp .env .");
    let reason = evaluate_secret_file_copy_command(&command, probe_cwd()).unwrap_or_default();
    assert!(reason.contains("#11276"), "must deny: {reason}");
    assert!(guard(&command, probe_cwd()).is_some(), "guard pin");
}
