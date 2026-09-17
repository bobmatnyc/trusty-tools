//! Unit tests for the verify-before-finish gate (#2279, #8206).
//!
//! Why: Split out of `verify_gate/mod.rs` when #8206 added the
//! detectable-test-command and bash-availability preconditions, so the
//! production module stays under the 500-SLOC cap and these tests count
//! against the 3000-SLOC test cap instead.
//! What: Covers the regex predicates, the seed/transcript scans, the
//! detection probe, and both gate constructors' accept/note/reject arms.
//! Test: this module is itself the test surface.

use std::path::Path;
use std::sync::Mutex;

use tempfile::TempDir;

use super::*;
use crate::llm::FunctionCall;
use crate::run_task::TurnRecord;

/// A project root holding a `Cargo.toml` — the simplest DETECTABLE project.
fn cargo_project() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir for a cargo project");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\n",
    )
    .expect("write Cargo.toml");
    dir
}

/// A gate context bound to `root` with `bash` present or absent.
fn gate_ctx(root: &Path, has_bash: bool) -> VerifyGateContext {
    VerifyGateContext::new(Some(root.to_path_buf()), has_bash)
}

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
        images: vec![],
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
            images: vec![],
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
/// calls at all, against a DETECTABLE project root and an agent that has
/// `bash` (#8206's two added preconditions); assert the gate rejects.
/// Test: this test.
#[test]
fn default_gate_trips_when_named_and_unrun() {
    let project = cargo_project();
    let transcript = Transcript::seed("system prompt", "run pytest tests/ -v before finishing");
    let gate = default_finish_gate(gate_ctx(project.path(), true));
    assert!(matches!(gate(&transcript), FinishGateOutcome::Reject(_)));
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
    let project = cargo_project();
    let transcript = Transcript::seed("system prompt", "implement a widget");
    let gate = default_finish_gate(gate_ctx(project.path(), true));
    assert_eq!(gate(&transcript), FinishGateOutcome::Accept);
}

/// [`default_finish_gate`] is satisfied once a matching `bash` call has
/// been pushed onto the transcript.
///
/// Why: Proves the "recover" half of the recoverable-retry contract —
/// after the agent runs the named tests, a second gate check must pass.
/// What: Seed against a DETECTABLE (`Cargo.toml`) root with `bash`
/// present, push an assistant turn with a matching `bash` call; assert
/// the gate accepts with no note — #8206's scenario (iv), the
/// pre-existing behaviour that must be preserved.
/// Test: this test.
#[test]
fn default_gate_satisfied_when_run() {
    let project = cargo_project();
    let mut transcript = Transcript::seed("system prompt", "run cargo test before finishing");
    transcript.push_assistant(None, &[bash_call("c1", "cargo test -p trusty-code")]);
    let gate = default_finish_gate(gate_ctx(project.path(), true));
    assert_eq!(gate(&transcript), FinishGateOutcome::Accept);
}

/// [`pm_finish_gate`] trips when the PM's task names a test command but
/// the shared (engineer) transcript never records a matching invocation.
///
/// Why: This is the PM-symmetric half of the acceptance criterion — the
/// PM must not be able to `finish_task` while its delegated engineer's
/// transcript shows no verification either.
/// What: An empty `SharedTranscript` and a DETECTABLE project root
/// (#8206's added precondition); assert the gate trips, naming the
/// manifest it detected.
/// Test: this test.
#[test]
fn pm_gate_trips_when_engineer_never_ran_tests() {
    let project = cargo_project();
    let shared: SharedTranscript = Arc::new(Mutex::new(Vec::new()));
    let transcript = Transcript::seed("pm system prompt", "run pytest tests/ -v");
    let gate = pm_finish_gate(shared, Some(project.path().to_path_buf()));
    let FinishGateOutcome::Reject(reason) = gate(&transcript) else {
        panic!("a detectable, unrun suite must still refuse the PM's finish");
    };
    assert!(reason.contains("Cargo.toml"), "reason was: {reason}");
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
    let project = cargo_project();
    let transcript = Transcript::seed("pm system prompt", "run pytest tests/ -v");
    let gate = pm_finish_gate(shared, Some(project.path().to_path_buf()));
    assert_eq!(gate(&transcript), FinishGateOutcome::Accept);
}

/// [`pm_finish_gate`] treats a poisoned shared-transcript lock as
/// "tests not run" (fail toward verify) AND logs a WARN naming why
/// (#2857) — a poisoned lock means some other turn's code already
/// panicked, which must not be silently folded into an ordinary gate
/// trip.
///
/// Why: This is the audited #2857 site: `unwrap_or(false)` previously
/// swallowed the poisoning distinction entirely.
/// What: Poison `shared`'s mutex by panicking on another thread while
/// holding the lock, then call the gate under
/// `crate::test_support::begin_capture`; assert the gate still trips
/// AND a warning naming "lock poisoned" was captured via
/// `captured_at_least`.
/// Test: this test.
#[test]
fn pm_gate_poisoned_lock_warns_and_trips() {
    let shared: SharedTranscript = Arc::new(Mutex::new(Vec::new()));
    {
        let shared = shared.clone();
        let _ = std::thread::spawn(move || {
            let _guard = shared.lock().expect("lock for poisoning");
            panic!("intentional poison for pm_gate_poisoned_lock_warns_and_trips");
        })
        .join();
    }
    assert!(shared.is_poisoned(), "setup must actually poison the lock");

    crate::test_support::begin_capture();

    let project = cargo_project();
    let transcript = Transcript::seed("pm system prompt", "run pytest tests/ -v");
    let gate = pm_finish_gate(shared, Some(project.path().to_path_buf()));
    let result = gate(&transcript);

    assert!(
        matches!(result, FinishGateOutcome::Reject(_)),
        "a poisoned lock must still trip the gate (fail toward verify)"
    );
    let captured = crate::test_support::captured_at_least(tracing::Level::WARN);
    assert!(
        captured.iter().any(|m| m.contains("lock poisoned")),
        "expected a warn-level poisoned-lock log, got: {captured:?}"
    );
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
    let project = cargo_project();
    let transcript = Transcript::seed("pm system prompt", "coordinate the widget build");
    let gate = pm_finish_gate(shared, Some(project.path().to_path_buf()));
    assert_eq!(gate(&transcript), FinishGateOutcome::Accept);
}

// ── #8206: detectable test command + bash availability ───────────────────────

/// [`named_test_command`] returns the matched command text, not just a
/// boolean.
///
/// Why: The refusal message and `TestCommandTarget::PromptNamed` both need
/// the literal command the prompt used (#8206 closure condition 2).
/// What: A prompt with the command embedded in prose; assert the match.
/// Test: this test.
#[test]
fn named_test_command_returns_the_matched_text() {
    assert_eq!(
        named_test_command("then run cargo test before finishing").as_deref(),
        Some("cargo test")
    );
    assert_eq!(named_test_command("implement a widget"), None);
}

/// [`detect_test_command`] names the manifest and command for each
/// recognised ecosystem.
///
/// Why: #8206 closure condition 2 — the refusal must name what it detected,
/// which means detection must carry both halves.
/// What: One tempdir per ecosystem, each holding only its own manifest;
/// `package.json` is exercised in both its npm and pnpm-lock forms.
/// Test: this test.
#[test]
fn detect_manifest_names_each_ecosystem() {
    /// The files to seed a root with, and the (manifest, command) pair
    /// detection must report for it.
    type ManifestCase<'a> = (&'a [(&'a str, &'a str)], &'a str, &'a str);
    let cases: &[ManifestCase] = &[
        (&[("Cargo.toml", "[package]")], "Cargo.toml", "cargo test"),
        (
            &[("package.json", r#"{"scripts":{"test":"vitest"}}"#)],
            "package.json",
            "npm test",
        ),
        (
            &[
                ("package.json", r#"{"scripts":{"test":"vitest"}}"#),
                ("pnpm-lock.yaml", "lockfileVersion: '9.0'"),
            ],
            "package.json",
            "pnpm test",
        ),
        (
            &[("pyproject.toml", "[project]")],
            "pyproject.toml",
            "pytest",
        ),
        (&[("pytest.ini", "[pytest]")], "pytest.ini", "pytest"),
        (&[("go.mod", "module x")], "go.mod", "go test ./..."),
    ];
    for (files, manifest, command) in cases {
        let dir = tempfile::tempdir().expect("tempdir");
        for (name, body) in *files {
            std::fs::write(dir.path().join(name), body).expect("write manifest");
        }
        assert_eq!(
            detect_test_command(Some(dir.path()), "cargo test"),
            Ok(TestCommandTarget::Manifest {
                manifest: (*manifest).to_string(),
                command: (*command).to_string(),
            }),
            "unexpected detection for {files:?}"
        );
    }
}

/// A `package.json` with no `test` script is not a detectable manifest.
///
/// Why: `npm test` there fails with "Missing script", which is not a suite
/// the agent can reconcile — refusing a finish over it would wedge the run
/// exactly as #8206 describes.
/// What: A root holding only a script-less `package.json`; assert the
/// fallback is the prompt-named command, not a `package.json` manifest.
/// Test: this test.
#[test]
fn detect_package_json_without_test_script_is_not_a_manifest() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("package.json"), r#"{"name":"x"}"#).expect("write package.json");
    assert_eq!(
        detect_test_command(Some(dir.path()), "npm test"),
        Ok(TestCommandTarget::PromptNamed {
            command: "npm test".to_string(),
        })
    );
}

/// [`detect_test_command`] reports the two unsatisfiable states.
///
/// Why: These are the exact conditions the live #8206 run hit — a
/// projectless launch (#8205) against an empty scratch root.
/// What: `None` as the root, then an existing but empty root.
/// Test: this test.
#[test]
fn detect_undetectable_without_root_or_on_empty_root() {
    assert_eq!(
        detect_test_command(None, "cargo test"),
        Err(UndetectableReason::NoProjectBound)
    );
    let empty = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        detect_test_command(Some(empty.path()), "cargo test"),
        Err(UndetectableReason::EmptyProjectRoot)
    );
}

/// A non-empty root with no recognised manifest still detects the
/// prompt-named command.
///
/// Why: #2279's original case — a polyglot challenge directory whose test
/// suite is named only by the prompt — must keep tripping the gate.
/// What: A root holding one unrelated file; assert `PromptNamed`.
/// Test: this test.
#[test]
fn detect_falls_back_to_prompt_named_on_non_empty_root() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("README.md"), "# challenge").expect("write README");
    assert_eq!(
        detect_test_command(Some(dir.path()), "pytest tests/"),
        Ok(TestCommandTarget::PromptNamed {
            command: "pytest tests/".to_string(),
        })
    );
}

/// [`default_finish_gate`] ACCEPTS when the agent's registry carries no
/// `bash` tool, and records why (#8206 scenario (i)).
///
/// Why: The live 2026-09-16 transcript refused such an agent twice; it had
/// no way to comply and the run wedged.
/// What: An empty project root, a prompt naming `cargo test`, `has_bash`
/// false; assert the accepted note names the missing `bash` tool.
/// Test: this test.
#[test]
fn default_gate_accepts_without_bash_tool() {
    let empty = tempfile::tempdir().expect("tempdir");
    let transcript = Transcript::seed("system prompt", "run cargo test before finishing");
    let gate = default_finish_gate(gate_ctx(empty.path(), false));
    let FinishGateOutcome::AcceptWithNote(note) = gate(&transcript) else {
        panic!("an agent with no bash tool must not be refused");
    };
    assert!(note.contains("`bash`"), "note was: {note}");
}

/// [`default_finish_gate`] ACCEPTS on an empty project root even when the
/// agent HAS `bash`, and records why (#8206 scenario (ii)).
///
/// Why: No command can be run against a root with no files, so a refusal
/// there is unsatisfiable by construction.
/// What: An empty root, `has_bash` true; assert the note names the empty
/// project root.
/// Test: this test.
#[test]
fn default_gate_accepts_on_empty_project_root() {
    let empty = tempfile::tempdir().expect("tempdir");
    let transcript = Transcript::seed("system prompt", "run cargo test before finishing");
    let gate = default_finish_gate(gate_ctx(empty.path(), true));
    let FinishGateOutcome::AcceptWithNote(note) = gate(&transcript) else {
        panic!("an empty project root must not produce a refusal");
    };
    assert!(note.contains("empty"), "note was: {note}");
    assert!(note.contains("no test command was run"), "note was: {note}");
}

/// [`default_finish_gate`]'s refusal names the manifest and the command it
/// detected (#8206 scenario (iii), closure condition 2).
///
/// Why: The pre-#8206 message named only the generic pattern set, so an
/// agent could not tell WHICH suite to run.
/// What: A `Cargo.toml` root with `bash` and no test run; assert both the
/// manifest filename and `cargo test` appear in the reason.
/// Test: this test.
#[test]
fn default_gate_refusal_names_the_manifest() {
    let project = cargo_project();
    let transcript = Transcript::seed("system prompt", "run cargo test before finishing");
    let gate = default_finish_gate(gate_ctx(project.path(), true));
    let FinishGateOutcome::Reject(reason) = gate(&transcript) else {
        panic!("a detectable, unrun suite must still be refused");
    };
    assert!(reason.contains("Cargo.toml"), "reason was: {reason}");
    assert!(reason.contains("cargo test"), "reason was: {reason}");
}

/// [`pm_finish_gate`] ACCEPTS on an empty project root (#8206).
///
/// Why: The PM half must not wedge on the same unsatisfiable condition the
/// engineer half now tolerates — a projectless run leaves both with nothing
/// to detect.
/// What: An empty root and an empty shared transcript; assert the note.
/// Test: this test.
#[test]
fn pm_gate_accepts_on_empty_project_root() {
    let empty = tempfile::tempdir().expect("tempdir");
    let shared: SharedTranscript = Arc::new(Mutex::new(Vec::new()));
    let transcript = Transcript::seed("pm system prompt", "run pytest tests/ -v");
    let gate = pm_finish_gate(shared, Some(empty.path().to_path_buf()));
    let FinishGateOutcome::AcceptWithNote(note) = gate(&transcript) else {
        panic!("an empty project root must not produce a PM refusal");
    };
    assert!(note.contains("empty"), "note was: {note}");
}
