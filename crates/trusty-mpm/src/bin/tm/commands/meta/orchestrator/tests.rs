//! Offline tests for the live metaharness orchestrator (#1030, WI-4).
//!
//! Why: Acceptance criterion #1030 requires the full PM → delegate → engineer
//! cycle to be provable without a live LLM. The scripted `LlmClientTrait` mock
//! (shared between the PM loop and the in-process sub-agent runner, exactly as in
//! production) makes the whole delegation deterministic, so every criterion —
//! both turns captured, both usages rolled up, the engineer's file artifact
//! visible, the structured schema adhered to — is asserted offline.
//! What: A `ScriptedLlm` replays a fixed queue of `ChatResponse`s. The orchestrator
//! is wired over a temp project + a temp agents dir (PM + engineer configs
//! written by `super::super::agents::write_agent_configs`). The script drives the
//! PM to delegate to the engineer, the engineer to call `write_file`, and both to
//! stop — then the combined transcript is asserted.
//! Test: this module is itself the test surface.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};

use super::*;
use crate::commands::meta::agents::write_agent_configs;
use trusty_code::llm::{ChatRequest, ChatResponse, LlmClientTrait, LlmError};

/// A `LlmClientTrait` that replays a fixed JSON script in call order.
///
/// Why: Deterministic, offline substitute for the network client; the same
/// instance is shared by the PM loop and the engineer loop so the script must
/// interleave their turns in the order the orchestrator drives them.
/// What: Holds scripted `ChatResponse`s and an atomic cursor; each `chat`
/// returns the next response, erroring loudly once the script is exhausted.
/// Test: used by every orchestrator test below.
struct ScriptedLlm {
    responses: Vec<ChatResponse>,
    cursor: AtomicUsize,
}

impl ScriptedLlm {
    /// Build a scripted client from JSON response fixtures.
    fn from_json(fixtures: &[Value]) -> Self {
        let responses = fixtures
            .iter()
            .map(|v| serde_json::from_value(v.clone()).expect("valid ChatResponse fixture"))
            .collect();
        Self {
            responses,
            cursor: AtomicUsize::new(0),
        }
    }

    /// Number of `chat` calls made so far.
    fn calls(&self) -> usize {
        self.cursor.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LlmClientTrait for ScriptedLlm {
    async fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let idx = self.cursor.fetch_add(1, Ordering::SeqCst);
        match self.responses.get(idx) {
            Some(resp) => Ok(resp.clone()),
            None => Err(LlmError::MissingConfig(format!(
                "scripted LLM exhausted at call {idx}"
            ))),
        }
    }
}

/// A response in which the assistant calls a named tool with the given JSON args.
fn tool_call(call_id: &str, tool: &str, args: Value, p: u32, c: u32) -> Value {
    json!({
        "id": "gen-tool",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": { "name": tool, "arguments": args.to_string() }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": p, "completion_tokens": c, "total_tokens": p + c }
    })
}

/// A response in which the assistant emits final text and stops.
fn stop(text: &str, p: u32, c: u32) -> Value {
    json!({
        "id": "gen-stop",
        "choices": [{
            "message": { "role": "assistant", "content": text, "tool_calls": [] },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": p, "completion_tokens": c, "total_tokens": p + c }
    })
}

/// The full PM → delegate → engineer cycle yields a combined transcript with
/// both turns, both usages, and the engineer's file artifact.
///
/// Why: This is the core #1030 acceptance test — a stubbed end-to-end run must
/// produce a unified transcript adhering to the structured schema, with the PM
/// and engineer usages both captured and the engineer's file change visible.
/// What: Script: PM delegates → engineer writes `hello_metaharness.txt` →
/// engineer stops → PM stops. Run the orchestrator over temp project + agents
/// dirs; assert the file exists, the transcript carries one delegation, the usage
/// rolls up across all four turns, and the artifact is listed.
/// Test: this test.
#[tokio::test]
async fn orchestrator_runs_full_delegation_cycle() {
    let project = tempfile::tempdir().expect("project tempdir");
    let agents = tempfile::tempdir().expect("agents tempdir");
    write_agent_configs(agents.path()).expect("write agent configs");

    // Call order the orchestrator drives:
    //  1. PM loop call 1   -> delegate_to_agent(python-engineer, task)
    //  2. engineer call 1  -> write_file(hello_metaharness.txt)
    //  3. engineer call 2  -> stop ("wrote the file")
    //  4. PM loop call 2   -> stop ("delegated and done")
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call(
            "d1",
            "delegate_to_agent",
            json!({"agent_name": "python-engineer", "task": "create hello_metaharness.txt"}),
            40,
            10,
        ),
        tool_call(
            "w1",
            "write_file",
            json!({"path": "hello_metaharness.txt", "content": "hello from the metaharness\n"}),
            30,
            8,
        ),
        stop("wrote hello_metaharness.txt", 12, 4),
        stop("Delegated to the engineer; the file was created.", 20, 6),
    ]));

    let orchestrator = Orchestrator::new(
        llm.clone(),
        agents.path().to_path_buf(),
        project.path().to_path_buf(),
    )
    .with_config(InProcessRunnerConfig {
        max_turns: 4,
        timeout_secs: 30,
    });

    let transcript = orchestrator
        .run("create hello_metaharness.txt")
        .await
        .expect("orchestration cycle succeeds");

    // The engineer actually wrote the file (real tool dispatch, not a stub).
    let written = project.path().join("hello_metaharness.txt");
    assert!(written.exists(), "engineer must have written the file");
    let body = std::fs::read_to_string(&written).expect("read written file");
    assert_eq!(body, "hello from the metaharness\n");

    // Exactly four chat calls (two PM, two engineer) over the shared client.
    assert_eq!(llm.calls(), 4, "shared client drove all four turns");

    // PM turn captured.
    assert_eq!(transcript.pm.role, "pm");
    assert!(
        transcript.pm.output.contains("Delegated"),
        "PM output captured: {}",
        transcript.pm.output
    );

    // One delegation captured, to the engineer, with its task and output.
    assert_eq!(transcript.delegations.len(), 1, "one delegation captured");
    let eng = &transcript.delegations[0];
    assert_eq!(eng.role, "python-engineer");
    assert_eq!(eng.task.as_deref(), Some("create hello_metaharness.txt"));
    assert_eq!(eng.output, "wrote hello_metaharness.txt");

    // Both usages captured and rolled up: PM (40+10 + 20+6) + engineer (30+8 + 12+4).
    assert_eq!(transcript.pm.usage.prompt_tokens, 40 + 20);
    assert_eq!(transcript.pm.usage.completion_tokens, 10 + 6);
    assert_eq!(eng.usage.prompt_tokens, 30 + 12);
    assert_eq!(eng.usage.completion_tokens, 8 + 4);
    assert_eq!(transcript.usage.prompt_tokens, 40 + 20 + 30 + 12);
    assert_eq!(transcript.usage.completion_tokens, 10 + 6 + 8 + 4);

    // The engineer's file change is visible in the transcript artifacts.
    let art = transcript
        .artifacts
        .iter()
        .find(|a| a.path == "hello_metaharness.txt")
        .expect("artifact listed for the created file");
    assert_eq!(art.bytes, body.len() as u64);

    // The structured schema is adhered to (round-trips through serde_json).
    let v = serde_json::to_value(&transcript).expect("transcript serializes");
    assert_eq!(
        v["schema_version"],
        super::super::transcript::TRANSCRIPT_SCHEMA_VERSION
    );
    assert!(v["pm"].is_object());
    assert!(v["delegations"].is_array());
    assert!(v["usage"].is_object());
    assert!(v["artifacts"].is_array());
}

/// The recording runner captures each delegation it performs.
///
/// Why: The transcript's sub-agent turns come from the recording runner; a
/// regression that dropped the recording would silently lose delegation turns.
/// What: Wrap a stub runner that returns a fixed output; call `run`; assert one
/// record with the right agent, task, and content.
/// Test: this test.
#[tokio::test]
async fn orchestrator_records_delegation() {
    use trusty_code::tools::{AgentOutput, AgentRunner};

    struct StubRunner;
    #[async_trait]
    impl AgentRunner for StubRunner {
        async fn run(&self, _agent: &str, _task: &str) -> anyhow::Result<AgentOutput> {
            Ok(AgentOutput::from_content("stub engineer output"))
        }
    }

    let records: Arc<Mutex<Vec<DelegationRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let runner = RecordingRunner::new(Arc::new(StubRunner), records.clone());

    let out = runner
        .run("python-engineer", "do the thing")
        .await
        .expect("stub runner succeeds");
    assert_eq!(out.content, "stub engineer output");

    let log = records.lock().expect("records lock");
    assert_eq!(log.len(), 1, "one delegation recorded");
    assert_eq!(log[0].agent, "python-engineer");
    assert_eq!(log[0].task, "do the thing");
    assert_eq!(log[0].output.content, "stub engineer output");
}

/// Artifact detection lists only files created during the run.
///
/// Why: The transcript must attribute *new* files to the run, not pre-existing
/// ones; a snapshot diff is how that attribution works.
/// What: Snapshot an empty dir, create a file, then diff — assert the new file is
/// the sole artifact with the correct byte length.
/// Test: this test.
#[test]
fn snapshot_then_new_artifacts_detects_created_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let before = snapshot_files(dir.path());
    assert!(before.is_empty(), "fresh dir has no files");

    std::fs::write(dir.path().join("created.txt"), b"twelve bytes").expect("write file");
    let arts = new_artifacts(dir.path(), &before);
    assert_eq!(arts.len(), 1, "exactly one new artifact");
    assert_eq!(arts[0].path, "created.txt");
    assert_eq!(arts[0].bytes, 12);
}

/// Live end-to-end: a real OpenRouter model drives the PM → engineer cycle and
/// the engineer writes the demo artifact (#1045 WI-8 live variant).
///
/// Why: The mock test proves the wiring; this proves the harness works against a
/// real LLM. It is `#[ignore]`d so CI stays offline and fast — run it locally
/// with `OPENROUTER_API_KEY` set:
/// `cargo test -p trusty-mpm --bin tm -- --ignored orchestrator_live_demo`.
/// What: Builds a real `LlmClient` from env, writes the bundled agent configs,
/// runs the orchestrator over the bundled demo task, and asserts the demo
/// artifact was created and at least one delegation was captured.
/// Test: this test (network-gated, ignored by default).
#[tokio::test]
#[ignore = "requires OPENROUTER_API_KEY and network access"]
async fn orchestrator_live_demo() {
    use trusty_code::llm::{LlmClient, LlmClientConfig};

    let config = LlmClientConfig::from_env().expect("OPENROUTER_API_KEY must be set for live test");
    let client = LlmClient::from_config(config).expect("build client");
    let llm = Arc::new(client);

    let project = tempfile::tempdir().expect("project tempdir");
    let agents = tempfile::tempdir().expect("agents tempdir");
    write_agent_configs(agents.path()).expect("write agent configs");

    let orchestrator = Orchestrator::new(
        llm,
        agents.path().to_path_buf(),
        project.path().to_path_buf(),
    );

    let task = "Create a file named `hello_metaharness.txt` in the project root containing \
                exactly the line `hello from the metaharness`. Delegate the file creation to \
                the python-engineer agent.";
    let transcript = orchestrator.run(task).await.expect("live run succeeds");

    assert!(
        project.path().join("hello_metaharness.txt").exists(),
        "engineer must have created the demo artifact"
    );
    assert!(
        !transcript.delegations.is_empty(),
        "the PM must have delegated at least once"
    );
    assert!(
        transcript.usage.total_tokens > 0,
        "the run must have consumed tokens"
    );
}
