//! Integration tests for `WorkflowEngine` and the engine's phase/wave logic.
//!
//! Why: These tests exercise the full engine via mock `AgentRunner`s — phase
//! ordering, file extraction, the wave loop, retry/elevation, reconciliation,
//! and skill discovery. The suite was split into focused files (#359) to keep
//! each under the 500-line cap; shared mocks and imports live here so the
//! submodules can `use super::*`.
//! What: Re-exports the executor module's items (`use super::super::*`) and
//! defines the mocks reused across multiple test files, then declares the
//! per-topic test submodules.
//! Test: The submodules below ARE the test bodies.

// Re-export everything the executor module brings into scope (WorkflowEngine,
// DiscoveredSkill, RunContext, PathBuf, Arc, InitContext, TagSkillRegistry,
// discover_project_dir, etc.) so the per-topic submodules can `use super::*`.
pub use super::*;

pub use std::sync::Mutex;

pub use anyhow::Result;
pub use async_trait::async_trait;

pub use crate::perf::TokenUsage;
pub use crate::tools::traits::{AgentOutput, AgentRunner, RunContext};
pub use crate::workflow::config::Assignments;
pub(crate) use crate::workflow::engine::helpers::reconcile_code_outputs_against;
pub(crate) use crate::workflow::engine::step_dispatch::discover_project_dir;

mod phase_order;
mod relocate;
mod retry;
mod skill_discovery;
mod wave_loop;

/// Mock runner that:
///   - for the "code" agent, returns a content blob with two `## File:`
///     sections so the engine has something to extract;
///   - for the "qa" agent, snapshots the contents of `out_dir` at the
///     moment it is invoked — this is the heart of the #64 assertion:
///     files must exist BEFORE QA runs, not after the workflow ends.
pub(super) struct PhaseOrderMock {
    pub out_dir: PathBuf,
    pub qa_dir_snapshot: Arc<Mutex<Vec<PathBuf>>>,
}

#[async_trait]
impl AgentRunner for PhaseOrderMock {
    async fn run(&self, agent_name: &str, _task: &str) -> Result<AgentOutput> {
        if agent_name == "qa-mock" {
            // Snapshot the out_dir so the test can assert what QA saw.
            let mut found = Vec::new();
            if self.out_dir.exists() {
                let mut stack = vec![self.out_dir.clone()];
                while let Some(p) = stack.pop() {
                    let mut rd = tokio::fs::read_dir(&p).await?;
                    while let Some(entry) = rd.next_entry().await? {
                        let path = entry.path();
                        if path.is_dir() {
                            stack.push(path);
                        } else {
                            found.push(path);
                        }
                    }
                }
            }
            self.qa_dir_snapshot.lock().unwrap().extend(found);
            return Ok(AgentOutput {
                content: "QA result: ok".to_string(),
                summary: Some("QA ok".to_string()),
                usage: TokenUsage::default(),
            });
        }

        if agent_name == "code-mock" {
            let content = "Here is the code.\n\n\
                    ## File: src/hello.py\n\
                    ```python\n\
                    def greet():\n    return \"hi\"\n\
                    ```\n\n\
                    ## File: tests/test_hello.py\n\
                    ```python\n\
                    from src.hello import greet\n\n\
                    def test_greet():\n    assert greet() == \"hi\"\n\
                    ```\n"
                .to_string();
            return Ok(AgentOutput {
                content,
                summary: Some("code summary".into()),
                usage: TokenUsage::default(),
            });
        }

        // Any other agent: return a harmless stub.
        Ok(AgentOutput {
            content: format!("stub output from {agent_name}"),
            summary: None,
            usage: TokenUsage::default(),
        })
    }
}

/// #153: Mock runner that captures the `working_dir` observed in
/// `RunContext` on each `run_with_context` call. Used to assert the
/// legacy monolithic code path passes `out_dir` as an *absolute* path
/// to the runner (so subprocess-driven runners like claude-code write
/// into out_dir instead of accidentally writing to the parent process
/// CWD — i.e. the project root).
pub(super) struct WorkingDirCapture {
    pub working_dirs: Arc<Mutex<Vec<Option<PathBuf>>>>,
}

#[async_trait]
impl AgentRunner for WorkingDirCapture {
    async fn run(&self, agent_name: &str, _task: &str) -> Result<AgentOutput> {
        // Fallback: record a `None` so tests can detect when the engine
        // dispatched through the plain `run` path (which loses working_dir).
        self.working_dirs.lock().unwrap().push(None);
        Ok(AgentOutput {
            content: format!("done {agent_name}"),
            summary: None,
            usage: TokenUsage::default(),
        })
    }

    async fn run_with_context(
        &self,
        agent_name: &str,
        _task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        self.working_dirs
            .lock()
            .unwrap()
            .push(ctx.working_dir.clone());
        Ok(AgentOutput {
            content: format!("done {agent_name}"),
            summary: None,
            usage: TokenUsage::default(),
        })
    }
}

/// #88: Wave-loop mock that records each per-file task it receives so we
/// can assert the wave loop invokes the code-agent once per assignment
/// in the right order.
pub(super) struct WaveLoopMock {
    pub calls: Arc<Mutex<Vec<(String, String)>>>, // (agent, task)
    /// Snapshots of the `max_turns_override` observed at each call. None
    /// means the context carried no override (non-wave path).
    pub max_turns_snapshots: Arc<Mutex<Vec<Option<u32>>>>,
    pub out_dir: PathBuf,
}

#[async_trait]
impl AgentRunner for WaveLoopMock {
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
        // CRIT-1 / MAJ-1 (#90, #93): Default `run` path is taken when
        // the engine calls us without a RunContext (legacy non-wave).
        // Record the call and log that no override was observed.
        self.calls
            .lock()
            .unwrap()
            .push((agent_name.to_string(), task.to_string()));
        self.max_turns_snapshots.lock().unwrap().push(None);

        Ok(AgentOutput {
            content: format!("done {agent_name}"),
            summary: Some("ok".into()),
            usage: TokenUsage::default(),
        })
    }

    async fn run_with_context(
        &self,
        agent_name: &str,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        self.calls
            .lock()
            .unwrap()
            .push((agent_name.to_string(), task.to_string()));

        // CRIT-1 / MAJ-1 (#90, #93): Snapshot the context-provided turn
        // cap so tests can prove the wave loop plumbed it through the
        // `RunContext` instead of mutating parent env vars.
        self.max_turns_snapshots
            .lock()
            .unwrap()
            .push(ctx.max_turns_override);

        // Simulate the agent writing its assigned file to disk using the
        // context-supplied path (previously came from an env var).
        if let Some(path) = &ctx.assigned_file {
            let dest = self.out_dir.join(path);
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await.ok();
            }
            tokio::fs::write(&dest, b"# generated\n").await.ok();
        }

        Ok(AgentOutput {
            content: format!("done {agent_name}"),
            summary: Some("ok".into()),
            usage: TokenUsage::default(),
        })
    }
}

/// Mock runner for the "plan writes assignments.json then code uses
/// wave-loop" regression test. The plan agent writes assignments.json
/// during its run (simulating what the real plan-agent's write_file tool
/// does). The code agent records each call it receives.
pub(super) struct PlanThenCodeMock {
    /// (agent, task) pairs recorded in order.
    pub calls: Arc<Mutex<Vec<(String, String)>>>,
    /// Directory where the "plan" step will write assignments.json and
    /// where the "code" step expects to find it.
    pub out_dir: PathBuf,
    /// Body to write as assignments.json when the plan agent runs.
    pub assignments_body: String,
}

#[async_trait]
impl AgentRunner for PlanThenCodeMock {
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
        // Non-wave calls (plan phase) land here; wave-loop calls go
        // through `run_with_context` below.
        self.calls
            .lock()
            .unwrap()
            .push((agent_name.to_string(), task.to_string()));

        if agent_name == "plan-mock" {
            // Simulate the plan-agent's write_file("assignments.json", ...)
            // that happens DURING plan phase execution. The engine's
            // wave-loop decision must observe this write when it later
            // processes the code phase.
            tokio::fs::write(
                self.out_dir.join("assignments.json"),
                &self.assignments_body,
            )
            .await
            .ok();
            return Ok(AgentOutput {
                content: "plan done".into(),
                summary: Some("planned".into()),
                usage: TokenUsage::default(),
            });
        }

        Ok(AgentOutput {
            content: format!("done {agent_name}"),
            summary: Some("ok".into()),
            usage: TokenUsage::default(),
        })
    }

    async fn run_with_context(
        &self,
        agent_name: &str,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        self.calls
            .lock()
            .unwrap()
            .push((agent_name.to_string(), task.to_string()));

        // plan-mock goes through non-wave path which may or may not set
        // working_dir; still simulate its assignments.json write.
        if agent_name == "plan-mock" {
            tokio::fs::write(
                self.out_dir.join("assignments.json"),
                &self.assignments_body,
            )
            .await
            .ok();
            return Ok(AgentOutput {
                content: "plan done".into(),
                summary: Some("planned".into()),
                usage: TokenUsage::default(),
            });
        }

        // code-agent: wave-loop writes each per-file output using the
        // context-supplied assigned_file.
        if let Some(path) = &ctx.assigned_file {
            let dest = self.out_dir.join(path);
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await.ok();
            }
            tokio::fs::write(&dest, b"# generated\n").await.ok();
        }

        Ok(AgentOutput {
            content: format!("done {agent_name}"),
            summary: Some("ok".into()),
            usage: TokenUsage::default(),
        })
    }
}

/// Build a `FileAssignment` from raw parts for pre-create tests (#150).
pub(super) fn fa_raw(
    path: &str,
    stub: Option<&str>,
    purpose: &str,
) -> crate::workflow::config::FileAssignment {
    crate::workflow::config::FileAssignment {
        path: path.to_string(),
        stub: stub.map(String::from),
        purpose: purpose.to_string(),
        depends_on: Vec::new(),
        max_lines: None,
    }
}

/// Build a tag-indexed registry from a fresh temp dir containing a few
/// `python` / `fastapi` / `pytest` skills (#173).
pub(super) fn temp_tag_registry_with_python_skills() -> (tempfile::TempDir, Arc<TagSkillRegistry>) {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, desc: &str, tags: &[&str]| {
        let tags_str = tags
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let content = format!(
            "---\nname: {name}\ndescription: {desc}\ntags: [{tags_str}]\n---\n\n# {name}\nbody\n",
        );
        std::fs::write(dir.path().join(format!("{name}.md")), content).unwrap();
    };
    write(
        "fastapi",
        "FastAPI application patterns, TestClient usage, module-level state",
        &["python", "fastapi", "api"],
    );
    write(
        "pytest",
        "async fixtures, parametrize, conftest patterns",
        &["python", "testing", "pytest"],
    );
    write(
        "python",
        "type hints, dataclasses, NLP setup",
        &["python", "packaging"],
    );
    write("rust", "Rust patterns", &["rust", "tokio"]);

    let reg = TagSkillRegistry::load(&[dir.path().to_path_buf()]);
    (dir, Arc::new(reg))
}
