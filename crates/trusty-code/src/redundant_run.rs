//! Redundant full-suite test re-run suppression (#2682).
//!
//! Why: (bake-off L1 diagnosis) The delegated engineer re-runs the full test
//! suite 7-14x per run, typically framed as "one final run to confirm", even
//! when NO code has changed since the last clean pass. Each redundant re-run
//! burns a whole turn (and the suite's wall-clock cost), inflating tcode's
//! turn count (~33-49) far above claude-code's (~15) on the same challenge.
//! The sibling verify-before-finish gate (#2279) guarantees the suite runs at
//! LEAST once; this module is its complement — it stops the suite running MORE
//! than it needs to. Together they bracket the correct behaviour: exactly one
//! clean full-suite pass after the last code change, no more and no less.
//! What: [`is_redundant_test_rerun`] is a pure predicate over a loop's
//! transcript. A proposed `bash` test invocation is redundant iff the most
//! recent prior test invocation ran the BYTE-IDENTICAL command, already PASSED,
//! and nothing that could have changed the tree (a `write_file`/`write_files`/
//! `edit`, or any non-test `bash` command) has run since. Command identity is
//! load-bearing (#2804 review): a narrow run passing (`cargo test -p a`) must
//! never suppress a broader/different one (`cargo test --workspace`,
//! `cargo test -p b`) that never actually ran, nor may a compile-only
//! `cargo test --no-run` suppress the real subsequent run. The predicate reuses
//! `verify_gate`'s `is_test_command` / `bash_command_from_call` so the two
//! gates can never drift on what counts as a test invocation. `agent_loop::AgentLoop`'s
//! dispatch loop consults it (when suppression is enabled via
//! `AgentLoop::with_redundant_run_suppression`) and, on a match, short-circuits
//! the dispatch with [`REDUNDANT_RERUN_MESSAGE`] instead of actually spawning
//! the suite — saving the turn's wall-clock cost and nudging the model to
//! proceed to finish. (#2857) That short-circuit is itself a decision that
//! silently changes the run's outcome (a test invocation the model requested
//! did NOT happen), so `agent_loop::AgentLoop::dispatch_all` logs it via
//! `tracing::info!` at the call site — a suppressed run is still the
//! expected, by-design outcome (not a failure), so `info` rather than `warn`.
//! Test: `redundant_run::tests::*`, plus
//! `agent_loop::tests::redundant_test_rerun_is_suppressed`,
//! `agent_loop::tests::test_rerun_after_edit_is_not_suppressed`, and
//! `agent_loop::tests::redundant_rerun_suppression_logs_info`.

use crate::llm::{ChatMessage, ToolCall};
use crate::tools::BASH_TOOL_NAME;
use crate::verify_gate::{bash_command_from_call, is_test_command};

/// The `ToolResult` text returned in place of a suppressed redundant re-run.
///
/// Why: The model must understand WHY its test command produced no fresh
/// output, so it stops retrying and moves on — a silent no-op would just
/// invite another confirmation run. Phrasing it as a normal (non-error)
/// result keeps the loop calm: nothing failed, the prior pass still stands.
/// What: A fixed, model-facing sentence explaining the suppression and the
/// invariant it relies on — the SAME command already passed and no code has
/// changed since. It is only ever emitted for a byte-identical re-run (see
/// [`is_redundant_test_rerun`]), so "this exact command" is always accurate.
/// Test: `redundant_run::tests::suppression_message_is_informative`.
pub const REDUNDANT_RERUN_MESSAGE: &str = "skipped: this exact test command already passed \
earlier in this session and no code has changed since (no write_file/edit and no other shell \
command ran between that passing run and this one). Re-running the identical command would \
produce the same result and cost a turn. The prior clean pass still stands — proceed to finish \
the task, or run a DIFFERENT (e.g. broader) command if you need to verify something the passing \
run did not cover.";

/// Tool names that are pure read-only discovery and therefore CANNOT
/// invalidate a passing test baseline.
///
/// Why: Between a clean test pass and a redundant re-run the model typically
/// does nothing, or merely re-reads files — none of which changes the tree, so
/// none of which makes a re-run non-redundant. Anything NOT on this allowlist
/// (a write, an edit, an arbitrary shell command, a skill load, a delegation)
/// is treated as potentially state-changing, which errs toward RUNNING the
/// suite — the conservative, correctness-first default the acceptance criteria
/// demand ("the single final run must still occur after the last code change").
/// What: The read-only file/discovery tools registered in the engineer's tool
/// set. `bash` is deliberately absent — a `bash` call is classified by its
/// command (a test command preserves the baseline; any other command is
/// treated as potentially mutating).
/// Test: `redundant_run::tests::read_only_activity_keeps_run_redundant`,
/// `redundant_run::tests::non_test_bash_between_runs_defeats_suppression`.
const READ_ONLY_TOOLS: &[&str] = &[
    "read_file",
    "grep",
    "glob",
    "list_dir",
    "search_code",
    "recall_session",
];

/// Whether a `bash` tool-result's captured output indicates the command exited
/// 0 (the suite PASSED).
///
/// Why: Only a PASSING prior run makes a re-run redundant — re-running a suite
/// that last FAILED is legitimate (the model may have fixed something, or may
/// be re-reading the failure), so the gate must never suppress it. The bash
/// tool always appends a final `exit_code: N` line to its captured output
/// (`tools::bash`), which is the one reliable pass/fail signal available from
/// the transcript.
/// What: `true` iff the result text, trimmed of trailing whitespace, ends with
/// exactly `exit_code: 0`. `exit_code: 10` / `exit_code: 1` and the
/// suppression sentinel (which carries no `exit_code:` line) all read as "not
/// a pass".
/// Test: `redundant_run::tests::exit_zero_reads_as_pass`,
/// `redundant_run::tests::nonzero_and_missing_exit_read_as_not_pass`.
fn test_result_passed(result_content: &str) -> bool {
    result_content.trim_end().ends_with("exit_code: 0")
}

/// Whether the `tool`-role result answering `call_id` in `messages` reports a
/// passing exit.
///
/// Why: A test invocation only establishes a passing baseline once its result
/// has actually been recorded and shows exit 0; a call whose result is not yet
/// present (e.g. the proposed re-run itself, mid-dispatch) must not count.
/// What: Finds the first `tool` message whose `tool_call_id == call_id` and
/// applies [`test_result_passed`] to its content; `false` when absent.
/// Test: exercised via `is_redundant_test_rerun` in
/// `redundant_run::tests::passing_prior_run_makes_rerun_redundant`,
/// `redundant_run::tests::failing_prior_run_is_not_redundant`.
fn call_result_passed(messages: &[ChatMessage], call_id: &str) -> bool {
    messages
        .iter()
        .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some(call_id))
        .and_then(|m| m.content.as_deref())
        .map(test_result_passed)
        .unwrap_or(false)
}

/// Fold one completed tool `call` into the running baseline: the exact command
/// string of the most recent test run that PASSED with nothing state-changing
/// since, or `None`.
///
/// Why: Centralises the single rule that decides, per tool call, whether a
/// prior clean test pass survives it — and, critically (#2804 review),
/// remembers WHICH command passed so a later re-run is only suppressed when it
/// is byte-identical. A bare `bool` would wrongly suppress a broader/different
/// command (`cargo test --workspace`) just because a narrower one
/// (`cargo test -- foo`) passed.
/// What: A passing `bash` test command SETS the baseline to `Some(command)`; a
/// FAILING test command CLEARS it (`None`); any other `bash` command CLEARS it
/// (a shell command may have mutated the tree); a [`READ_ONLY_TOOLS`] call
/// leaves it unchanged; every other tool CLEARS it (conservative — treat
/// unknown effects as state-changing).
/// Test: exercised via `is_redundant_test_rerun` across
/// `redundant_run::tests::*`.
fn fold_call(baseline: &mut Option<String>, call: &ToolCall, messages: &[ChatMessage]) {
    if let Some(command) = bash_command_from_call(call) {
        *baseline = (is_test_command(&command) && call_result_passed(messages, &call.id))
            .then_some(command);
        return;
    }
    if READ_ONLY_TOOLS.contains(&call.function.name.as_str()) {
        return;
    }
    *baseline = None;
}

/// Whether two shell command strings are the "same" test invocation for
/// suppression purposes.
///
/// Why: Suppression must require command IDENTITY (#2804 review): re-running a
/// DIFFERENT command — a broader scope, a different package, a previously
/// compile-only `--no-run` now actually executing — is a genuinely-needed run,
/// never redundant. Defaulting to exact match (not a fuzzy/normalized one)
/// keeps the gate conservative: a false "same" verdict would suppress a real
/// run, so anything short of certainty must read as "different".
/// What: Exact string equality after trimming leading/trailing whitespace only
/// — the sole normalization, so a stray surrounding space cannot force an
/// otherwise-identical re-run to execute. Internal differences (flags, paths,
/// package selectors, `--no-run`) are all preserved and therefore all count as
/// a different command.
/// Test: `redundant_run::tests::different_test_command_after_pass_is_not_redundant`,
/// `redundant_run::tests::passing_prior_run_makes_rerun_redundant`.
fn same_test_command(baseline: &str, pending: &str) -> bool {
    baseline.trim() == pending.trim()
}

/// Whether the pending `bash` test call `current_call_id` would be a redundant
/// re-run: the SAME command already passed with nothing changed since.
///
/// Why: This is the gate `agent_loop::AgentLoop::dispatch_all` consults before
/// spawning a test suite — the whole point of #2682. It must be conservative:
/// a `true` result suppresses a real execution, so a false positive would hide
/// a genuinely-needed run. The predicate therefore only returns `true` when a
/// prior run demonstrably PASSED, every tool call since is demonstrably
/// non-mutating, AND (#2804 review) the pending command is byte-identical to
/// that passing command — a broader or otherwise-different command is never
/// suppressed.
/// What: Folds every assistant tool call in `messages`, in order, up to (but
/// excluding) the call identified by `current_call_id` — the proposed re-run
/// itself, and anything after it, is irrelevant — tracking the exact command
/// of the most recent still-valid passing test run (or `None`). On reaching
/// the pending call, extracts ITS command and returns `true` only when a
/// baseline command exists and [`same_test_command`] holds. Because the
/// proposed call's own (not-yet-recorded) result is never consulted, and any
/// same-turn write/edit BEFORE it clears the baseline, the answer reflects tree
/// state exactly as of just before the re-run.
/// Test: `redundant_run::tests::passing_prior_run_makes_rerun_redundant`,
/// `redundant_run::tests::different_test_command_after_pass_is_not_redundant`,
/// `redundant_run::tests::failing_prior_run_is_not_redundant`,
/// `redundant_run::tests::edit_between_runs_defeats_suppression`,
/// `redundant_run::tests::no_prior_run_is_not_redundant`,
/// `redundant_run::tests::same_turn_edit_before_rerun_defeats_suppression`.
pub fn is_redundant_test_rerun(messages: &[ChatMessage], current_call_id: &str) -> bool {
    let mut baseline: Option<String> = None;
    for message in messages {
        if message.role != "assistant" {
            continue;
        }
        let Some(calls) = message.tool_calls.as_ref() else {
            continue;
        };
        for call in calls {
            if call.id == current_call_id {
                return match (baseline.as_deref(), bash_command_from_call(call)) {
                    (Some(passed), Some(pending)) => same_test_command(passed, &pending),
                    _ => false,
                };
            }
            fold_call(&mut baseline, call, messages);
        }
    }
    // The pending call was not found in the transcript — fail safe (never
    // suppress a call we cannot locate). In practice `dispatch_all` always
    // passes a `call.id` present in the just-pushed assistant turn.
    false
}

/// Whether `call` is a `bash` invocation of a test command — the cheap
/// pre-filter `dispatch_all` applies before the fuller
/// [`is_redundant_test_rerun`] transcript scan.
///
/// Why: Only a `bash` test call can ever be a redundant re-run, so the dispatch
/// loop should not pay for the transcript scan on every other tool call.
/// What: `true` iff `call` names [`BASH_TOOL_NAME`] and its extracted command
/// [`is_test_command`].
/// Test: `redundant_run::tests::is_test_bash_call_true_only_for_test_bash`.
pub fn is_test_bash_call(call: &ToolCall) -> bool {
    call.function.name == BASH_TOOL_NAME
        && bash_command_from_call(call).is_some_and(|c| is_test_command(&c))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::FunctionCall;

    /// Build a `bash` tool call with the given id and command.
    fn bash_call(id: &str, command: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: FunctionCall {
                name: BASH_TOOL_NAME.into(),
                arguments: serde_json::json!({ "command": command }).to_string(),
            },
        }
    }

    /// Build a non-`bash` tool call (e.g. `write_file`, `read_file`).
    fn tool_call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: "{}".into(),
            },
        }
    }

    /// An assistant turn carrying the given tool calls.
    fn assistant(calls: Vec<ToolCall>) -> ChatMessage {
        ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(calls),
            tool_call_id: None,
            name: None,
            cache_control: None,
        }
    }

    /// A `tool`-role result answering `call_id` with a bash-shaped body ending
    /// in `exit_code: {code}`.
    fn bash_result(call_id: &str, code: i32) -> ChatMessage {
        ChatMessage::tool_result(
            call_id,
            BASH_TOOL_NAME,
            format!("stdout:\nok\nexit_code: {code}"),
        )
    }

    /// [`test_result_passed`] treats a trailing `exit_code: 0` as a pass.
    ///
    /// Why: Exit 0 is the one pass signal the bash tool guarantees.
    /// What: Assert on a realistic passing bash body.
    /// Test: this test.
    #[test]
    fn exit_zero_reads_as_pass() {
        assert!(test_result_passed("stdout:\n5 passed\nexit_code: 0"));
        assert!(test_result_passed("exit_code: 0\n"));
    }

    /// [`test_result_passed`] rejects non-zero and missing exit codes.
    ///
    /// Why: A failing or unknown run must never establish a passing baseline —
    /// and `exit_code: 10` must not be mistaken for `exit_code: 0`.
    /// What: Non-zero exits, the suppression sentinel, and empty text.
    /// Test: this test.
    #[test]
    fn nonzero_and_missing_exit_read_as_not_pass() {
        assert!(!test_result_passed("stderr:\nFAILED\nexit_code: 1"));
        assert!(!test_result_passed("exit_code: 10"));
        assert!(!test_result_passed(REDUNDANT_RERUN_MESSAGE));
        assert!(!test_result_passed(""));
    }

    /// A passing prior run with nothing since makes an identical re-run
    /// redundant.
    ///
    /// Why: This is the core acceptance behaviour — the exact "one final run to
    /// confirm" loop the ticket eliminates.
    /// What: Turn 1 runs `cargo test` and it passes (exit 0); turn 2 proposes
    /// `cargo test` again with nothing in between; assert redundant.
    /// Test: this test.
    #[test]
    fn passing_prior_run_makes_rerun_redundant() {
        let messages = vec![
            assistant(vec![bash_call("t1", "cargo test -p trusty-code")]),
            bash_result("t1", 0),
            assistant(vec![bash_call("t2", "cargo test -p trusty-code")]),
        ];
        assert!(is_redundant_test_rerun(&messages, "t2"));
    }

    /// A DIFFERENT test command after a pass is NOT redundant — suppression
    /// requires command IDENTITY (#2804 review).
    ///
    /// Why: A narrow run passing (`cargo test -p a`) must never suppress a
    /// broader/different run (`cargo test -p b`, `cargo test --workspace`) that
    /// was never actually executed — that would silently skip real coverage.
    /// What: Turn 1 runs `cargo test -p a` and passes; turn 2 proposes
    /// `cargo test -p b`; assert NOT redundant. The reverse (same command) is
    /// covered by `passing_prior_run_makes_rerun_redundant`.
    /// Test: this test.
    #[test]
    fn different_test_command_after_pass_is_not_redundant() {
        let messages = vec![
            assistant(vec![bash_call("t1", "cargo test -p a")]),
            bash_result("t1", 0),
            assistant(vec![bash_call("t2", "cargo test -p b")]),
        ];
        assert!(!is_redundant_test_rerun(&messages, "t2"));
    }

    /// A `--no-run` compile-only pass does NOT suppress a subsequent REAL run
    /// of the same suite — because the real run's command string differs
    /// (#2804 review, secondary concern).
    ///
    /// Why: `cargo test --no-run` compiles but executes zero tests, then exits
    /// 0. Command-identity matching already prevents it suppressing a genuine
    /// `cargo test` run: the two command strings are not equal, so the gate
    /// treats them as different runs — no special-casing of `--no-run` needed.
    /// What: Turn 1 runs `cargo test --no-run` (exit 0); turn 2 proposes the
    /// real `cargo test`; assert NOT redundant.
    /// Test: this test.
    #[test]
    fn compile_only_pass_does_not_suppress_real_run() {
        let messages = vec![
            assistant(vec![bash_call("t1", "cargo test --no-run")]),
            bash_result("t1", 0),
            assistant(vec![bash_call("t2", "cargo test")]),
        ];
        assert!(!is_redundant_test_rerun(&messages, "t2"));
    }

    /// Surrounding whitespace does not defeat identity — a re-run of the SAME
    /// command with a stray leading/trailing space is still redundant.
    ///
    /// Why: `same_test_command` trims outer whitespace only, so a trivially
    /// re-emitted command is still recognised while every internal difference
    /// (flags, scope) is preserved.
    /// What: Turn 1 `cargo test` passes; turn 2 proposes `  cargo test ` (outer
    /// spaces); assert redundant.
    /// Test: this test.
    #[test]
    fn outer_whitespace_does_not_defeat_identity() {
        let messages = vec![
            assistant(vec![bash_call("t1", "cargo test")]),
            bash_result("t1", 0),
            assistant(vec![bash_call("t2", "  cargo test ")]),
        ];
        assert!(is_redundant_test_rerun(&messages, "t2"));
    }

    /// A FAILING prior run makes a re-run NOT redundant.
    ///
    /// Why: The model may re-run a failing suite legitimately; suppressing it
    /// would hide the failure and break the loop's self-correction.
    /// What: Turn 1's `cargo test` exits 1; turn 2 proposes it again; assert
    /// NOT redundant.
    /// Test: this test.
    #[test]
    fn failing_prior_run_is_not_redundant() {
        let messages = vec![
            assistant(vec![bash_call("t1", "cargo test")]),
            bash_result("t1", 1),
            assistant(vec![bash_call("t2", "cargo test")]),
        ];
        assert!(!is_redundant_test_rerun(&messages, "t2"));
    }

    /// A `write_file`/`edit` between a passing run and a re-run defeats
    /// suppression — the genuinely-needed post-change run still fires.
    ///
    /// Why: This is the correctness guarantee the acceptance criteria name:
    /// "the single final run must still occur after the last code change".
    /// What: pass → `edit` → propose re-run; assert NOT redundant.
    /// Test: this test.
    #[test]
    fn edit_between_runs_defeats_suppression() {
        let messages = vec![
            assistant(vec![bash_call("t1", "cargo test")]),
            bash_result("t1", 0),
            assistant(vec![tool_call("e1", "edit")]),
            ChatMessage::tool_result("e1", "edit", "edited"),
            assistant(vec![bash_call("t2", "cargo test")]),
        ];
        assert!(!is_redundant_test_rerun(&messages, "t2"));
    }

    /// Read-only activity (read_file/grep/glob) between a passing run and a
    /// re-run keeps the re-run redundant.
    ///
    /// Why: Re-reading files changes nothing on disk, so a re-run after only
    /// reads is still a wasted confirmation.
    /// What: pass → `read_file` + `grep` → propose re-run; assert redundant.
    /// Test: this test.
    #[test]
    fn read_only_activity_keeps_run_redundant() {
        let messages = vec![
            assistant(vec![bash_call("t1", "pytest tests/ -v")]),
            bash_result("t1", 0),
            assistant(vec![tool_call("r1", "read_file"), tool_call("g1", "grep")]),
            ChatMessage::tool_result("r1", "read_file", "..."),
            ChatMessage::tool_result("g1", "grep", "..."),
            assistant(vec![bash_call("t2", "pytest tests/ -v")]),
        ];
        assert!(is_redundant_test_rerun(&messages, "t2"));
    }

    /// A non-test `bash` command between runs defeats suppression.
    ///
    /// Why: An arbitrary shell command (`sed -i`, `git checkout`, a build) may
    /// mutate the tree, so a subsequent test run is not provably redundant.
    /// What: pass → `bash: git checkout .` → propose re-run; assert NOT
    /// redundant.
    /// Test: this test.
    #[test]
    fn non_test_bash_between_runs_defeats_suppression() {
        let messages = vec![
            assistant(vec![bash_call("t1", "cargo test")]),
            bash_result("t1", 0),
            assistant(vec![bash_call("b1", "git checkout .")]),
            bash_result("b1", 0),
            assistant(vec![bash_call("t2", "cargo test")]),
        ];
        assert!(!is_redundant_test_rerun(&messages, "t2"));
    }

    /// With no prior test run at all, the first run is never redundant.
    ///
    /// Why: The first execution is always genuinely needed — the gate must be
    /// inert until a baseline exists.
    /// What: A single proposed `cargo test` with empty history; assert NOT
    /// redundant.
    /// Test: this test.
    #[test]
    fn no_prior_run_is_not_redundant() {
        let messages = vec![assistant(vec![bash_call("t1", "cargo test")])];
        assert!(!is_redundant_test_rerun(&messages, "t1"));
    }

    /// A same-turn edit emitted BEFORE the re-run (batched) defeats
    /// suppression.
    ///
    /// Why: The model can batch `edit` + `cargo test` in one turn; the edit
    /// must invalidate the prior baseline even though it shares the proposed
    /// run's turn.
    /// What: pass → [turn: `edit` then `cargo test`]; propose the same-turn
    /// `cargo test`; assert NOT redundant.
    /// Test: this test.
    #[test]
    fn same_turn_edit_before_rerun_defeats_suppression() {
        let messages = vec![
            assistant(vec![bash_call("t1", "cargo test")]),
            bash_result("t1", 0),
            assistant(vec![tool_call("e1", "edit"), bash_call("t2", "cargo test")]),
        ];
        assert!(!is_redundant_test_rerun(&messages, "t2"));
    }

    /// [`is_test_bash_call`] is true only for a `bash` call whose command is a
    /// test command.
    ///
    /// Why: The dispatch-loop pre-filter must not trip on a non-`bash` tool or
    /// a non-test shell command.
    /// What: A test bash call (true), a non-test bash call (false), a
    /// `write_file` call (false).
    /// Test: this test.
    #[test]
    fn is_test_bash_call_true_only_for_test_bash() {
        assert!(is_test_bash_call(&bash_call("a", "cargo test")));
        assert!(!is_test_bash_call(&bash_call("b", "git status")));
        assert!(!is_test_bash_call(&tool_call("c", "write_file")));
    }

    /// The suppression message names the invariant it relies on so the model
    /// can trust it.
    ///
    /// Why: A vague message would invite the very re-run it means to prevent.
    /// What: Assert the sentinel mentions "already passed" and "proceed to
    /// finish".
    /// Test: this test.
    #[test]
    fn suppression_message_is_informative() {
        assert!(REDUNDANT_RERUN_MESSAGE.contains("already passed"));
        assert!(REDUNDANT_RERUN_MESSAGE.contains("proceed to finish"));
    }
}
