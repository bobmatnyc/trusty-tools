//! Init-context injection and pre-plan skill discovery tests (#108/#109, #173).
//!
//! Why: These pin the prompt-assembly contract — the project self-init prefix
//! is prepended to every phase, and pre-plan skill discovery derives
//! `TaskSignals` from the task text, queries the tag-indexed registry, and
//! prepends a "## Available Skills" block to ONLY the plan-agent's prompt.
//! What: Drives `WorkflowEngine` with recording mocks plus the
//! `discover_skills_for_task` API and the shared
//! `temp_tag_registry_with_python_skills` fixture.
//! Test: This file IS the test body.

use super::*;

/// #108/#109: an engine configured with an `InitContext` must prepend
/// the project summary + memories prefix to every phase's rendered task.
/// We use a recording mock that captures the exact task text the runner
/// receives and assert the prefix appears before the task body.
#[tokio::test]
async fn init_context_is_prepended_to_phase_template() {
    struct RecordingRunner {
        tasks: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl AgentRunner for RecordingRunner {
        async fn run(&self, _agent_name: &str, task: &str) -> Result<AgentOutput> {
            self.tasks.lock().unwrap().push(task.to_string());
            Ok(AgentOutput {
                content: "ok".into(),
                summary: None,
                usage: TokenUsage::default(),
            })
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let workflows_dir = tmp.path().join("workflows");
    tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
    let wf_path = workflows_dir.join("init-test.json");
    let wf_json = r#"{
            "name": "init-test",
            "description": "single phase",
            "phases": [
                {"name":"research","agent":"research-agent","context_template":"TASK={{task}}"}
            ]
        }"#;
    tokio::fs::write(&wf_path, wf_json).await.unwrap();

    let tasks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mock = Arc::new(RecordingRunner {
        tasks: tasks.clone(),
    });

    let ic = InitContext {
        project_summary: "# Project: demo\nindex body".into(),
        relevant_memories: vec!["prior fact".into()],
        initialized_at: chrono::Utc::now(),
    };

    let engine = WorkflowEngine::new(mock, workflows_dir.clone()).with_init_context(Some(ic));
    let _ = engine
        .run("init-test", "my-task".into(), None)
        .await
        .expect("workflow ok");

    let recorded = tasks.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    let seen = &recorded[0];
    assert!(
        seen.contains("## Project Context (auto-indexed)"),
        "seen: {seen}"
    );
    assert!(seen.contains("prior fact"), "seen: {seen}");
    assert!(seen.contains("TASK=my-task"), "seen: {seen}");
    // Ordering: prefix must come before task body.
    let pidx = seen.find("Project Context").unwrap();
    let tidx = seen.find("TASK=my-task").unwrap();
    assert!(pidx < tidx, "prefix should appear before task body");
}

// ── #173: pre-plan skill discovery ────────────────────────────────────
//
// Why: The engine should derive `TaskSignals` from the task text, query
// the tag-indexed registry, and prepend a "## Available Skills" block to
// the plan-agent's prompt — without the plan-agent ever calling
// `list_skills`. These tests pin the contract.

/// #173: discovery must pull `python` + `fastapi` skills out of the
/// tag-indexed registry given a task that mentions Python and FastAPI.
/// The Rust-only skill must NOT appear because no rust signals match.
#[test]
fn skill_discovery_extracts_python_fastapi_tags() {
    let (_keep, reg) = temp_tag_registry_with_python_skills();

    struct NopRunner;
    #[async_trait]
    impl AgentRunner for NopRunner {
        async fn run(&self, _: &str, _: &str) -> Result<AgentOutput> {
            Ok(AgentOutput {
                content: String::new(),
                summary: None,
                usage: TokenUsage::default(),
            })
        }
    }

    let engine = WorkflowEngine::new(Arc::new(NopRunner), PathBuf::from("."))
        .with_tag_skill_registry(Some(reg));

    let task = "Build a Python FastAPI service with pytest tests for the REST endpoints";
    let discovered = engine.discover_skills_for_task(task, 8);

    let names: Vec<&str> = discovered.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"fastapi"),
        "fastapi should be discovered: got {names:?}"
    );
    assert!(
        names.contains(&"pytest"),
        "pytest should be discovered: got {names:?}"
    );
    assert!(
        names.contains(&"python"),
        "python should be discovered: got {names:?}"
    );
    assert!(
        !names.contains(&"rust"),
        "rust must not be matched: got {names:?}"
    );

    // Each discovered skill carries a non-empty summary + tags.
    for s in &discovered {
        assert!(!s.summary.is_empty(), "summary empty for {}", s.name);
        assert!(!s.tags.is_empty(), "tags empty for {}", s.name);
    }
}

/// #173: when many skills tie on raw tag-overlap, effectiveness scores
/// drive the top-N ordering — the engine must respect the registry's
/// ranking and only return the top `limit`.
#[test]
fn skill_discovery_returns_top_n_by_effectiveness() {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, desc: &str, tags: &[&str]| {
        let tags_str = tags
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let content =
            format!("---\nname: {name}\ndescription: {desc}\ntags: [{tags_str}]\n---\n\nbody\n",);
        std::fs::write(dir.path().join(format!("{name}.md")), content).unwrap();
    };
    // Five skills, all matching the single "python" tag — effectiveness
    // breaks the tie. Discovery order (insertion) is the secondary
    // tie-breaker so we drive ranking purely via effectiveness.
    write("a", "d", &["python"]);
    write("b", "d", &["python"]);
    write("c", "d", &["python"]);
    write("d", "d", &["python"]);
    write("e", "d", &["python"]);

    let mut reg = TagSkillRegistry::load(&[dir.path().to_path_buf()]);
    // Push c and a to the top via effectiveness boost.
    reg.update_effectiveness("c", 1.0);
    reg.update_effectiveness("c", 1.0);
    reg.update_effectiveness("c", 1.0);
    reg.update_effectiveness("a", 1.0);

    struct NopRunner;
    #[async_trait]
    impl AgentRunner for NopRunner {
        async fn run(&self, _: &str, _: &str) -> Result<AgentOutput> {
            Ok(AgentOutput {
                content: String::new(),
                summary: None,
                usage: TokenUsage::default(),
            })
        }
    }

    let engine = WorkflowEngine::new(Arc::new(NopRunner), PathBuf::from("."))
        .with_tag_skill_registry(Some(Arc::new(reg)));

    let discovered = engine.discover_skills_for_task("Write a python script", 2);
    assert_eq!(discovered.len(), 2, "limit must be honored");
    // The boosted skills should come first; we don't assert exact order
    // beyond "c is first" because effectiveness EMA + tie-breakers can
    // shift across same-effectiveness siblings.
    assert_eq!(
        discovered[0].name,
        "c",
        "highest-effectiveness skill should rank first; got {:?}",
        discovered.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

/// #173: discovery returns empty when the registry is absent or empty —
/// the engine must NOT panic and must NOT inject anything into the
/// plan-agent prompt downstream.
#[test]
fn skill_discovery_returns_empty_when_registry_absent() {
    struct NopRunner;
    #[async_trait]
    impl AgentRunner for NopRunner {
        async fn run(&self, _: &str, _: &str) -> Result<AgentOutput> {
            Ok(AgentOutput {
                content: String::new(),
                summary: None,
                usage: TokenUsage::default(),
            })
        }
    }

    let engine = WorkflowEngine::new(Arc::new(NopRunner), PathBuf::from("."));
    let discovered = engine.discover_skills_for_task("python fastapi", 8);
    assert!(discovered.is_empty(), "no registry → empty discovery");

    let empty_reg = Arc::new(TagSkillRegistry::empty());
    let engine = engine.with_tag_skill_registry(Some(empty_reg));
    let discovered = engine.discover_skills_for_task("python fastapi", 8);
    assert!(discovered.is_empty(), "empty registry → empty discovery");
}

/// #173: end-to-end — when the engine runs a workflow whose `plan` phase
/// matches discovered skills, the runner sees the assembled task text
/// containing the "## Available Skills" header. Other phases must NOT
/// receive that block.
#[tokio::test]
async fn plan_agent_context_includes_skill_summaries() {
    let (_keep, reg) = temp_tag_registry_with_python_skills();

    struct RecordingRunner {
        tasks: Arc<Mutex<Vec<(String, String)>>>,
    }
    #[async_trait]
    impl AgentRunner for RecordingRunner {
        async fn run(&self, agent: &str, task: &str) -> Result<AgentOutput> {
            self.tasks
                .lock()
                .unwrap()
                .push((agent.to_string(), task.to_string()));
            Ok(AgentOutput {
                content: "ok".into(),
                summary: None,
                usage: TokenUsage::default(),
            })
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let workflows_dir = tmp.path().join("workflows");
    tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
    let wf_path = workflows_dir.join("plan-skills.json");
    // Two phases: a research phase (must NOT get the block) and a plan
    // phase (must receive it).
    let wf_json = r#"{
            "name": "plan-skills",
            "phases": [
                {"name":"research","agent":"research-agent","context_template":"R={{task}}"},
                {"name":"plan","agent":"plan-agent","context_template":"P={{task}}"}
            ]
        }"#;
    tokio::fs::write(&wf_path, wf_json).await.unwrap();

    let tasks: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let mock = Arc::new(RecordingRunner {
        tasks: tasks.clone(),
    });

    let engine =
        WorkflowEngine::new(mock, workflows_dir.clone()).with_tag_skill_registry(Some(reg));
    // #196: pin persona to engineer so the persona heuristic doesn't
    // accidentally classify this as "hacker" (the substring `fast` in
    // "FastAPI" matches the hacker keyword) and skip the research phase.
    engine
        .run(
            "plan-skills",
            "[engineer] Build a Python FastAPI service with pytest tests".into(),
            None,
        )
        .await
        .expect("workflow ok");

    let recorded = tasks.lock().unwrap().clone();
    assert_eq!(recorded.len(), 2);

    let (research_agent, research_task) = &recorded[0];
    assert_eq!(research_agent, "research-agent");
    assert!(
        !research_task.contains("## Available Skills"),
        "research phase must not receive the discovery block: {research_task}"
    );

    let (plan_agent, plan_task) = &recorded[1];
    assert_eq!(plan_agent, "plan-agent");
    assert!(
        plan_task.contains("## Available Skills"),
        "plan phase prompt must contain '## Available Skills': {plan_task}"
    );
    // The block must precede the rendered template body.
    let header_idx = plan_task.find("## Available Skills").unwrap();
    let body_idx = plan_task.find("P=Build a Python").expect("body present");
    assert!(
        header_idx < body_idx,
        "skills block must come before the task body"
    );
}
