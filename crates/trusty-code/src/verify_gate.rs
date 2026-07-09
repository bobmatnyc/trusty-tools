//! Verify-before-finish gate (#2279): a recoverable retry, not a hard block,
//! that keeps a coding agent from calling `finish_task` before it has run a
//! test suite it was explicitly told about.
//!
//! Why: (bake-off L2 diagnosis) tcode's delegated engineer authored 81 tests
//! validating the wrong contract and never ran the visible, prompt-named
//! test suite the L2 challenge instructed it to run before submitting. The
//! task prompt and `challenges/README.md` (folded into project context)
//! named a runnable test command (`pytest challenges/level-2-git-analyzer/
//! test_suite/ -v`); the transcript never shows it being invoked. This
//! module is the structural fix: at the `finish_task` turn boundary
//! (`agent_loop::AgentLoop::dispatch_all`), if the task/project-context text
//! names a runnable test command AND no matching `bash` invocation appears
//! in the relevant transcript, `finish_task` is rejected with a recoverable
//! error — the SAME `ToolResult::err` retry path every other tool failure
//! uses — instead of terminating the loop. If no test command is named at
//! all, the gate is INERT: #2279 explicitly defers general polyglot
//! test-suite discovery (the "(b2)" follow-up) rather than inventing one
//! here.
//! What: [`names_test_command`] and [`is_test_command`] are pure,
//! independently unit-testable regex predicates — Improvement 2's own
//! testability principle applied to its sibling Improvement 1. Two
//! `agent_loop::FinishGate` constructors build on them: [`default_finish_gate`]
//! scans a single loop's OWN `Transcript` — what a delegated engineer's
//! sub-agent loop attaches (`runner::in_process::InProcessAgentRunner`),
//! since its own `bash` calls land directly in that transcript.
//! [`pm_finish_gate`] is for the delegating PM's loop, which never calls
//! `bash` itself — it scans the externally shared `run_task::SharedTranscript`
//! of `TurnRecord`s the delegated engineer's turns are recorded into
//! instead, via each record's `ran_test_command` flag (set by
//! `run_task::recorder::RecordingLlmClient` using the SAME [`is_test_command`]
//! predicate this module owns, so the two paths can never independently
//! drift on what counts as a matching invocation).
//! Test: `verify_gate::tests::*`.

use std::sync::Arc;

use regex::Regex;
use serde_json::Value;

use crate::agent_loop::{FinishGate, Transcript};
use crate::llm::{ChatMessage, ToolCall};
use crate::run_task::SharedTranscript;
use crate::tools::BASH_TOOL_NAME;

/// Recoverable-retry message fed back to a delegated engineer's own loop
/// when its `finish_task` call is rejected.
const GATE_REASON_ENGINEER: &str = "finish_task rejected: the task prompt or project context \
names a runnable test command (a pytest/cargo/npm/pnpm/go test invocation), but no matching \
command has been run yet in this session. Run the named test suite via the bash tool, \
reconcile any failures, and only then call finish_task again.";

/// Recoverable-retry message fed back to the delegating PM's own loop when
/// its `finish_task` call is rejected because the delegated engineer never
/// ran the named tests.
const GATE_REASON_PM: &str = "finish_task rejected: the task prompt or project context names a \
runnable test command (a pytest/cargo/npm/pnpm/go test invocation), but the delegated \
engineer's transcript shows no matching invocation yet. Delegate a follow-up task instructing \
the engineer to run the named test suite and reconcile any failures before finishing.";

/// Build the regex alternation matching the six test-invocation shapes named
/// in #2279's finalized trigger heuristic.
///
/// Why: A single compiled pattern, rather than six separate `Regex`es, keeps
/// [`names_test_command`] and [`is_test_command`] identical in shape — "does
/// this text contain a recognizable test-command invocation" — the only
/// difference between the two call sites is WHICH text they scan. Compiled
/// fresh per call rather than cached in a `once_cell`/`lazy_static` global
/// (project convention reserves those for the tracing subscriber only); this
/// is not a hot path — at most once per `finish_task` dispatch attempt, or
/// once per recorded turn — so the recompilation cost is immaterial next to
/// the LLM round-trip it sits beside.
/// What: `pytest\s+\S+` (a pytest invocation naming a path/module),
/// `cargo test`, `npm test`, `pnpm test`, `go test`, `python -m pytest` —
/// exactly the six patterns #2279's finalized decision comment named.
/// Test: covered indirectly by every `tests::*` case below.
fn test_command_pattern() -> Regex {
    Regex::new(
        r"(?:pytest\s+\S+)|(?:cargo\s+test)|(?:npm\s+test)|(?:pnpm\s+test)|(?:go\s+test)|(?:python\s+-m\s+pytest)",
    )
    .expect("test_command_pattern: hardcoded regex literal must compile")
}

/// Whether `text` textually names a runnable test command (the gate's
/// TRIGGER half).
///
/// Why: The gate must stay inert when nothing in the task/project context
/// ever asked for a specific test command.
/// What: `true` iff [`test_command_pattern`] finds a match anywhere in
/// `text`.
/// Test: `tests::names_test_command_detects_each_pattern`,
/// `tests::names_test_command_false_on_unrelated_text`.
pub fn names_test_command(text: &str) -> bool {
    test_command_pattern().is_match(text)
}

/// Whether `command` (a `bash` tool call's shell command) IS a matching test
/// invocation (the gate's VERIFY half).
///
/// Why: Reused by both [`default_finish_gate`]'s own-transcript scan and
/// `run_task::recorder`'s per-turn `ran_test_command` signal, so the two
/// gate sites can never independently drift on what counts as "the named
/// tests actually ran".
/// What: Same predicate as [`names_test_command`], applied to a single
/// command string rather than free-form prose.
/// Test: `tests::is_test_command_matches_each_pattern`,
/// `tests::is_test_command_false_on_unrelated_command`.
pub fn is_test_command(command: &str) -> bool {
    test_command_pattern().is_match(command)
}

/// Extract the shell command from a `bash` tool call's arguments, if `call`
/// names the `bash` tool at all.
///
/// Why: Shared by [`default_finish_gate`]'s own-transcript scan and
/// `run_task::recorder::RecordingLlmClient`'s per-turn signal, so the two
/// gate sites parse the SAME wire shape (`{"command": "..."}`) identically.
/// What: `None` when `call.function.name != BASH_TOOL_NAME` or the
/// arguments do not parse as a JSON object with a string `command` field.
/// Test: `tests::bash_command_from_call_extracts_command`,
/// `tests::bash_command_from_call_ignores_other_tools`.
pub fn bash_command_from_call(call: &ToolCall) -> Option<String> {
    if call.function.name != BASH_TOOL_NAME {
        return None;
    }
    let parsed: Value = serde_json::from_str(&call.function.arguments).ok()?;
    parsed.get("command")?.as_str().map(str::to_string)
}

/// Whether any `bash` call in `messages` invoked a matching test command.
///
/// Why: The core predicate [`default_finish_gate`] applies to a delegated
/// engineer's own `Transcript` — factored out as a free function so it is
/// independently unit-testable against a plain `&[ChatMessage]` without
/// constructing a full `Transcript`.
/// What: Flattens every assistant turn's `tool_calls`, extracts each `bash`
/// call's command via [`bash_command_from_call`], and checks
/// [`is_test_command`].
/// Test: `tests::transcript_ran_test_command_detects_match`,
/// `tests::transcript_ran_test_command_false_without_bash`.
fn transcript_ran_test_command(messages: &[ChatMessage]) -> bool {
    messages
        .iter()
        .filter_map(|m| m.tool_calls.as_ref())
        .flatten()
        .filter_map(bash_command_from_call)
        .any(|cmd| is_test_command(&cmd))
}

/// Join the seeded `system` + `user` message text into one haystack for
/// [`names_test_command`].
///
/// Why: `Transcript::seed` is the ONLY place raw history ever gets a
/// `system`- or `user`-role entry, and it seeds exactly one of each — the
/// assembled system prompt (BASE + agent prompt + project `CLAUDE.md`
/// context) and the task text. Scanning by role (not position) stays
/// correct even if `Transcript`'s internal layout ever changes.
/// What: Concatenates the `content` of every `system`/`user` entry with
/// newlines.
/// Test: `tests::seed_text_joins_system_and_user`.
fn seed_text(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .filter(|m| m.role == "system" || m.role == "user")
        .filter_map(|m| m.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build the default verify-before-finish gate: scans the SAME loop's own
/// transcript for both the trigger and the verification.
///
/// Why: This is the shape a delegated engineer's sub-agent loop needs — its
/// own `bash` calls land directly in its own `Transcript`, so no external
/// state is required.
/// What: Returns a [`FinishGate`] closure that is inert unless
/// [`seed_text`] of the transcript [`names_test_command`]; when it does,
/// trips (returning [`GATE_REASON_ENGINEER`]) unless
/// [`transcript_ran_test_command`] finds a match.
/// Test: `tests::default_gate_trips_when_named_and_unrun`,
/// `tests::default_gate_inert_when_not_named`,
/// `tests::default_gate_satisfied_when_run`,
/// `agent_loop::tests::finish_gate_trips_and_recovers`.
pub fn default_finish_gate() -> FinishGate {
    Arc::new(|transcript: &Transcript| {
        let messages = transcript.messages();
        if !names_test_command(&seed_text(&messages)) {
            return None;
        }
        if transcript_ran_test_command(&messages) {
            return None;
        }
        Some(GATE_REASON_ENGINEER.to_string())
    })
}

/// Build the delegating PM's verify-before-finish gate: the trigger comes
/// from the PM's own transcript, but the verification scans the externally
/// shared engineer transcript instead.
///
/// Why: The PM's own tool registry never includes `bash` (`run_task::
/// execute_run_task` registers only `delegate_to_agent` and `finish_task`),
/// so scanning the PM's own `Transcript` for a `bash` call would ALWAYS
/// report "not run" — wrongly tripping the gate even when the delegated
/// engineer ran the named tests correctly. `shared` is the SAME
/// `run_task::SharedTranscript` the engineer's `RecordingLlmClient` records
/// into, so this closure sees the engineer's turns too.
/// What: Returns a [`FinishGate`] closure inert unless the PM transcript's
/// [`seed_text`] [`names_test_command`]; when it does, trips (returning
/// [`GATE_REASON_PM`]) unless any `TurnRecord` in `shared` has
/// `ran_test_command == true`. A poisoned lock is treated as "not run" —
/// fail toward asking the model to verify, not toward silently finishing.
/// Test: `tests::pm_gate_trips_when_engineer_never_ran_tests`,
/// `tests::pm_gate_satisfied_when_engineer_ran_tests`,
/// `tests::pm_gate_inert_when_not_named`.
pub fn pm_finish_gate(shared: SharedTranscript) -> FinishGate {
    Arc::new(move |transcript: &Transcript| {
        let messages = transcript.messages();
        if !names_test_command(&seed_text(&messages)) {
            return None;
        }
        let ran = shared
            .lock()
            .map(|turns| turns.iter().any(|t| t.ran_test_command))
            .unwrap_or(false);
        if ran {
            return None;
        }
        Some(GATE_REASON_PM.to_string())
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::llm::FunctionCall;
    use crate::run_task::TurnRecord;

    fn bash_call(id: &str, command: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: FunctionCall {
                name: BASH_TOOL_NAME.into(),
                arguments: serde_json::json!({"command": command}).to_string(),
            },
        }
    }

    fn assistant_with_tool_calls(calls: Vec<ToolCall>) -> ChatMessage {
        ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(calls),
            tool_call_id: None,
            name: None,
            cache_control: None,
        }
    }

    /// [`names_test_command`] detects each of the six finalized patterns.
    ///
    /// Why: Guards the exact pattern set #2279's decision comment named.
    /// What: One assertion per pattern, embedded in surrounding prose.
    /// Test: this test.
    #[test]
    fn names_test_command_detects_each_pattern() {
        for text in [
            "please run pytest tests/test_basic.py -v before submitting",
            "then run cargo test",
            "run npm test after building",
            "use pnpm test to check",
            "run go test ./... first",
            "invoke python -m pytest as the final step",
        ] {
            assert!(names_test_command(text), "expected a match in {text:?}");
        }
    }

    /// [`names_test_command`] is `false` on prose naming no test command.
    ///
    /// Why: The gate must stay inert for ordinary tasks that never mention a
    /// runnable test suite.
    /// What: Free-form task text with no recognizable pattern.
    /// Test: this test.
    #[test]
    fn names_test_command_false_on_unrelated_text() {
        assert!(!names_test_command(
            "implement a git log parser and make sure it builds cleanly"
        ));
    }

    /// [`is_test_command`] matches each pattern as a standalone command
    /// string (not embedded in prose).
    ///
    /// Why: This is the shape a `bash` tool call's `command` argument
    /// actually takes.
    /// What: One assertion per pattern.
    /// Test: this test.
    #[test]
    fn is_test_command_matches_each_pattern() {
        for cmd in [
            "pytest challenges/level-2-git-analyzer/test_suite/ -v",
            "cargo test -p trusty-code",
            "npm test",
            "pnpm test --run",
            "go test ./...",
            "python -m pytest -x",
        ] {
            assert!(is_test_command(cmd), "expected a match in {cmd:?}");
        }
    }

    /// [`is_test_command`] is `false` on an unrelated shell command.
    ///
    /// Why: Ordinary bash calls (`ls`, `git status`, a build command) must
    /// not be mistaken for a test invocation.
    /// What: A handful of common non-test commands.
    /// Test: this test.
    #[test]
    fn is_test_command_false_on_unrelated_command() {
        for cmd in ["ls -la", "git status", "cargo build --release", "echo ok"] {
            assert!(!is_test_command(cmd), "unexpected match in {cmd:?}");
        }
    }

    /// [`bash_command_from_call`] extracts the command from a real `bash`
    /// call's JSON arguments.
    ///
    /// Why: This is the exact wire shape `BashTool::schema` declares.
    /// What: Build a `bash` call, assert the extracted command.
    /// Test: this test.
    #[test]
    fn bash_command_from_call_extracts_command() {
        let call = bash_call("c1", "cargo test -p trusty-code");
        assert_eq!(
            bash_command_from_call(&call).as_deref(),
            Some("cargo test -p trusty-code")
        );
    }

    /// [`bash_command_from_call`] returns `None` for a non-`bash` tool call.
    ///
    /// Why: The gate must only ever consider `bash` calls a test invocation
    /// — a `write_file` or `delegate_to_agent` call must never match.
    /// What: A `write_file`-named call with an unrelated argument shape.
    /// Test: this test.
    #[test]
    fn bash_command_from_call_ignores_other_tools() {
        let call = ToolCall {
            id: "c1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "write_file".into(),
                arguments: serde_json::json!({"path": "x", "content": "y"}).to_string(),
            },
        };
        assert_eq!(bash_command_from_call(&call), None);
    }

    /// [`seed_text`] joins the `system` and `user` entries, skipping any
    /// `assistant`/`tool` entries in between.
    ///
    /// Why: The trigger check must only ever consider the ORIGINAL task
    /// prompt/project context, never text that later shows up in tool
    /// output.
    /// What: Build a small message list; assert the join.
    /// Test: this test.
    #[test]
    fn seed_text_joins_system_and_user() {
        let messages = vec![
            ChatMessage::system("SYSTEM_TEXT"),
            ChatMessage::user("USER_TEXT"),
            ChatMessage {
                role: "assistant".into(),
                content: Some("mentions cargo test in passing".into()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                cache_control: None,
            },
        ];
        let joined = seed_text(&messages);
        assert!(joined.contains("SYSTEM_TEXT"));
        assert!(joined.contains("USER_TEXT"));
        assert!(!joined.contains("mentions cargo test"));
    }

    /// [`transcript_ran_test_command`] finds a match inside an assistant
    /// turn's `bash` tool calls.
    ///
    /// Why: This is the exact shape the engineer's own transcript takes
    /// after it runs the named suite.
    /// What: An assistant turn with one matching `bash` call.
    /// Test: this test.
    #[test]
    fn transcript_ran_test_command_detects_match() {
        let messages = vec![assistant_with_tool_calls(vec![bash_call(
            "c1",
            "pytest tests/ -v",
        )])];
        assert!(transcript_ran_test_command(&messages));
    }

    /// [`transcript_ran_test_command`] is `false` when no `bash` call is
    /// present at all.
    ///
    /// Why: Guards the common "nothing ran yet" case.
    /// What: An empty message list.
    /// Test: this test.
    #[test]
    fn transcript_ran_test_command_false_without_bash() {
        assert!(!transcript_ran_test_command(&[]));
    }

    /// [`default_finish_gate`] trips when the task names a test command and
    /// no matching `bash` call appears in the transcript.
    ///
    /// Why: This is the core acceptance behaviour — a `finish_task` call
    /// must be rejected with a recoverable reason under exactly this
    /// condition.
    /// What: Seed a transcript whose task names `pytest`, with no tool
    /// calls at all; assert the gate returns `Some`.
    /// Test: this test.
    #[test]
    fn default_gate_trips_when_named_and_unrun() {
        let transcript = Transcript::seed("system prompt", "run pytest tests/ -v before finishing");
        let gate = default_finish_gate();
        assert!(gate(&transcript).is_some());
    }

    /// [`default_finish_gate`] is inert when the task never names a test
    /// command.
    ///
    /// Why: #2279 explicitly defers general polyglot test-suite discovery —
    /// the gate must not invent a requirement that was never stated.
    /// What: Seed a transcript with an unrelated task; assert `None`.
    /// Test: this test.
    #[test]
    fn default_gate_inert_when_not_named() {
        let transcript = Transcript::seed("system prompt", "implement a widget");
        let gate = default_finish_gate();
        assert!(gate(&transcript).is_none());
    }

    /// [`default_finish_gate`] is satisfied once a matching `bash` call has
    /// been pushed onto the transcript.
    ///
    /// Why: Proves the "recover" half of the recoverable-retry contract —
    /// after the agent runs the named tests, a second gate check must pass.
    /// What: Seed, push an assistant turn with a matching `bash` call;
    /// assert `None`.
    /// Test: this test.
    #[test]
    fn default_gate_satisfied_when_run() {
        let mut transcript = Transcript::seed("system prompt", "run cargo test before finishing");
        transcript.push_assistant(None, &[bash_call("c1", "cargo test -p trusty-code")]);
        let gate = default_finish_gate();
        assert!(gate(&transcript).is_none());
    }

    /// [`pm_finish_gate`] trips when the PM's task names a test command but
    /// the shared (engineer) transcript never records a matching invocation.
    ///
    /// Why: This is the PM-symmetric half of the acceptance criterion — the
    /// PM must not be able to `finish_task` while its delegated engineer's
    /// transcript shows no verification either.
    /// What: An empty `SharedTranscript`; assert the gate trips.
    /// Test: this test.
    #[test]
    fn pm_gate_trips_when_engineer_never_ran_tests() {
        let shared: SharedTranscript = Arc::new(Mutex::new(Vec::new()));
        let transcript = Transcript::seed("pm system prompt", "run pytest tests/ -v");
        let gate = pm_finish_gate(shared);
        assert!(gate(&transcript).is_some());
    }

    /// [`pm_finish_gate`] is satisfied when a `TurnRecord` in the shared
    /// transcript (recorded from the delegated engineer's own turns) has
    /// `ran_test_command == true`.
    ///
    /// Why: Proves the PM-side gate correctly consults EXTERNAL state
    /// (the engineer's recorded turns) rather than its own (bash-less)
    /// transcript.
    /// What: Seed the shared transcript with one `python-engineer` turn
    /// carrying `ran_test_command: true`; assert `None`.
    /// Test: this test.
    #[test]
    fn pm_gate_satisfied_when_engineer_ran_tests() {
        let shared: SharedTranscript = Arc::new(Mutex::new(vec![TurnRecord {
            role: "python-engineer".into(),
            model: "test-model".into(),
            text: String::new(),
            tool_calls: vec!["bash".into()],
            ran_test_command: true,
            usage: crate::perf::TokenUsage::default(),
        }]));
        let transcript = Transcript::seed("pm system prompt", "run pytest tests/ -v");
        let gate = pm_finish_gate(shared);
        assert!(gate(&transcript).is_none());
    }

    /// [`pm_finish_gate`] is inert when the PM's own task never names a test
    /// command, regardless of the shared transcript's contents.
    ///
    /// Why: Mirrors [`default_gate_inert_when_not_named`] for the PM path.
    /// What: An unrelated PM task with an empty shared transcript.
    /// Test: this test.
    #[test]
    fn pm_gate_inert_when_not_named() {
        let shared: SharedTranscript = Arc::new(Mutex::new(Vec::new()));
        let transcript = Transcript::seed("pm system prompt", "coordinate the widget build");
        let gate = pm_finish_gate(shared);
        assert!(gate(&transcript).is_none());
    }
}
