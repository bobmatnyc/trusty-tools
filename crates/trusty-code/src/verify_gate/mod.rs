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
//! What (#8206): naming a test command is necessary but no longer
//! sufficient. Both gates additionally require that a test command be
//! DETECTABLE for the bound project ([`detect::detect_test_command`]) and —
//! for the engineer gate — that the agent's own registry carry `bash` to run
//! it with. When either fails, `finish_task` is ACCEPTED and the completion
//! report records that no test command ran and why; only a detectable,
//! runnable, unrun suite still produces a refusal, and that refusal names the
//! manifest or command it detected.
//! Test: `verify_gate::tests::*`.
//!
//! [`names_test_command`]: crate::verify_gate::names_test_command
//! [`is_test_command`]: crate::verify_gate::is_test_command
//! [`default_finish_gate`]: crate::verify_gate::default_finish_gate
//! [`pm_finish_gate`]: crate::verify_gate::pm_finish_gate

pub mod detect;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;

use regex::Regex;
use serde_json::Value;

use crate::agent_loop::{FinishGate, Transcript};
use crate::llm::{ChatMessage, ToolCall};
use crate::run_task::SharedTranscript;
use crate::tools::BASH_TOOL_NAME;

use detect::{TestCommandTarget, UndetectableReason, detect_test_command};

/// Recoverable-retry message fed back to a delegated engineer's own loop
/// when its `finish_task` call is rejected.
///
/// Why (#8206): built per call rather than held as a `const`, so the refusal
/// names the detected manifest or command — an agent told only that "a
/// pytest/cargo/npm/pnpm/go test invocation" was named cannot tell WHICH
/// suite to run.
/// What: `target.evidence()` is the detection clause; the rest is the
/// unchanged #2279 nudge, including the "Run the named test suite" phrase
/// `agent_loop::tests::gate_intercept` asserts on.
/// Test: `tests::default_gate_refusal_names_the_manifest`.
fn gate_reason_engineer(target: &TestCommandTarget) -> String {
    format!(
        "finish_task rejected: {}, but no matching command has been run yet in this session. \
         Run the named test suite via the bash tool, reconcile any failures, and only then \
         call finish_task again.",
        target.evidence()
    )
}

/// Recoverable-retry message fed back to the delegating PM's own loop when
/// its `finish_task` call is rejected because the delegated engineer never
/// ran the named tests.
///
/// Why: The PM-side twin of [`gate_reason_engineer`], carrying the same
/// #8206 evidence clause so the PM's follow-up delegation can name the suite.
/// Test: `tests::pm_gate_trips_when_engineer_never_ran_tests`.
fn gate_reason_pm(target: &TestCommandTarget) -> String {
    format!(
        "finish_task rejected: {}, but the delegated engineer's transcript shows no matching \
         invocation yet. Delegate a follow-up task instructing the engineer to run the named \
         test suite and reconcile any failures before finishing.",
        target.evidence()
    )
}

/// The note appended to an ACCEPTED completion report when the gate was
/// triggered but could not be satisfied here (#8206).
///
/// Why: Closure condition 3 — the accepted finish must record that no test
/// command ran, and why, rather than silently looking like a verified one.
/// What: One sentence naming [`UndetectableReason::explain`]'s clause.
/// Test: `tests::default_gate_accepts_on_empty_project_root`,
/// `tests::default_gate_accepts_without_bash_tool`.
fn unverified_finish_note(reason: UndetectableReason) -> String {
    format!(
        "Note (#8206): no test command was run in this session — {}.",
        reason.explain()
    )
}

/// What a [`FinishGate`] decided about a successful `finish_task` call.
///
/// Why (#8206): before this enum the gate could only accept or reject. An
/// accepted finish that the gate WOULD have policed, but could not (no
/// detectable test suite, no `bash` tool to run one with), must not read as a
/// verified one — so a third outcome carries the reason into the recorded
/// report instead of silently accepting.
/// What: `Accept` leaves the report untouched; `AcceptWithNote` appends the
/// note to the model's own `summary` before it becomes the run's output;
/// `Reject` downgrades the call to a recoverable tool error.
/// Test: `agent_loop::tests::finish_gate_*`,
/// `verify_gate::tests::default_gate_accepts_on_empty_project_root`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishGateOutcome {
    /// Nothing to say — the finish stands as reported.
    Accept,
    /// The finish stands, but the report records this note.
    AcceptWithNote(String),
    /// The finish is refused; the string is the recoverable retry reason.
    Reject(String),
}

/// The project/tooling facts the engineer gate needs beyond the prompt text.
///
/// Why (#8206): the pre-#8206 gate was a pure text match, so it refused a
/// finish on an empty project root, and refused an agent that had no `bash`
/// tool to comply with — the run wedged, because nothing the agent could do
/// would satisfy the refusal.
/// What: `project_root` is the directory the agent's fs/bash tools are scoped
/// to (`None` when nothing is bound); `has_bash` is whether THIS agent's
/// gated registry carries the `bash` tool.
/// Test: `tests::default_gate_accepts_without_bash_tool`,
/// `tests::default_gate_accepts_on_empty_project_root`.
#[derive(Debug, Clone, Default)]
pub struct VerifyGateContext {
    project_root: Option<PathBuf>,
    has_bash: bool,
}

impl VerifyGateContext {
    /// Construct from the bound project root and this agent's `bash`
    /// availability.
    ///
    /// Test: `tests::default_gate_trips_when_named_and_unrun`.
    pub fn new(project_root: Option<PathBuf>, has_bash: bool) -> Self {
        Self {
            project_root,
            has_bash,
        }
    }
}

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
    named_test_command(text).is_some()
}

/// The test command `text` names, as the matched substring (#8206).
///
/// Why: [`names_test_command`]'s boolean answer cannot name the command in a
/// refusal message, and `detect::TestCommandTarget::PromptNamed` needs the
/// literal text the prompt used when no manifest was found.
/// What: The FIRST [`test_command_pattern`] match in `text`, e.g.
/// `"cargo test"` out of "then run cargo test before finishing".
/// Test: `tests::named_test_command_returns_the_matched_text`.
pub fn named_test_command(text: &str) -> Option<String> {
    test_command_pattern()
        .find(text)
        .map(|m| m.as_str().to_string())
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
/// [`seed_text`] of the transcript [`names_test_command`] and
/// [`transcript_ran_test_command`] finds no match. Past that (#8206) it
/// rejects ONLY when the agent's registry carries `bash` AND
/// [`detect_test_command`] finds a runnable suite for `ctx`'s project root;
/// otherwise it accepts the finish with an [`unverified_finish_note`],
/// because no refusal the agent could act on exists.
/// Test: `tests::default_gate_trips_when_named_and_unrun`,
/// `tests::default_gate_inert_when_not_named`,
/// `tests::default_gate_satisfied_when_run`,
/// `tests::default_gate_accepts_without_bash_tool`,
/// `tests::default_gate_accepts_on_empty_project_root`,
/// `agent_loop::tests::finish_gate_trips_and_recovers`.
pub fn default_finish_gate(ctx: VerifyGateContext) -> FinishGate {
    Arc::new(move |transcript: &Transcript| {
        let messages = transcript.messages();
        let Some(named) = named_test_command(&seed_text(&messages)) else {
            return FinishGateOutcome::Accept;
        };
        if transcript_ran_test_command(&messages) {
            return FinishGateOutcome::Accept;
        }
        // #8206: a refusal the agent cannot act on wedges the run — it was
        // refused twice in the live transcript and had no `bash` tool to
        // comply with. Accept and record why instead.
        if !ctx.has_bash {
            return FinishGateOutcome::AcceptWithNote(unverified_finish_note(
                UndetectableReason::NoBashTool,
            ));
        }
        match detect_test_command(ctx.project_root.as_deref(), &named) {
            Ok(target) => FinishGateOutcome::Reject(gate_reason_engineer(&target)),
            Err(reason) => FinishGateOutcome::AcceptWithNote(unverified_finish_note(reason)),
        }
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
/// [`gate_reason_pm`]) unless any `TurnRecord` in `shared` has
/// `ran_test_command == true`. A poisoned lock is treated as "not run" —
/// fail toward asking the model to verify, not toward silently finishing —
/// and (#2857) logs `tracing::warn!` when this happens, since a poisoned
/// lock means some other turn's code already panicked; silently folding
/// that into an ordinary gate trip would hide the real failure.
/// (#8206) `project_root` gets the same detectability precondition the
/// engineer gate has: an empty or unbound root can satisfy no test command,
/// so the PM's finish is accepted with an [`unverified_finish_note`] rather
/// than refused. The bash half of that check is NOT mirrored here — the
/// delegated engineer's registry is built per delegation and is not knowable
/// at PM-loop construction; its own [`default_finish_gate`] applies it.
/// Test: `tests::pm_gate_trips_when_engineer_never_ran_tests`,
/// `tests::pm_gate_satisfied_when_engineer_ran_tests`,
/// `tests::pm_gate_inert_when_not_named`,
/// `tests::pm_gate_accepts_on_empty_project_root`,
/// `tests::pm_gate_poisoned_lock_warns_and_trips`.
pub fn pm_finish_gate(shared: SharedTranscript, project_root: Option<PathBuf>) -> FinishGate {
    Arc::new(move |transcript: &Transcript| {
        let messages = transcript.messages();
        let Some(named) = named_test_command(&seed_text(&messages)) else {
            return FinishGateOutcome::Accept;
        };
        // (#2857) A poisoned lock (some other turn's code panicked while
        // holding it) is treated as "tests not run" — fail toward asking the
        // model to verify, per this function's own doc. That fallback is
        // itself a decision worth surfacing: silently swallowing the
        // poisoning would hide a real prior panic behind an ordinary-looking
        // gate trip.
        let ran = shared
            .lock()
            .map(|turns| turns.iter().any(|t| t.ran_test_command))
            .unwrap_or_else(|_| {
                tracing::warn!(
                    "verify_gate: shared transcript lock poisoned (a prior turn likely \
                     panicked) — treating as tests-not-run and tripping the PM finish gate"
                );
                false
            });
        if ran {
            return FinishGateOutcome::Accept;
        }
        // #8206: same detectability precondition as the engineer gate.
        match detect_test_command(project_root.as_deref(), &named) {
            Ok(target) => FinishGateOutcome::Reject(gate_reason_pm(&target)),
            Err(reason) => FinishGateOutcome::AcceptWithNote(unverified_finish_note(reason)),
        }
    })
}
