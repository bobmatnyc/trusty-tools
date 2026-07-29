//! Call-site regression test for `[[plugins.python]]` wiring (#446, epic #3052).
//!
//! Why: the unit tests in `ctrl::pm_task::dispatch::persona_plugins`' test
//! module prove the registration HELPER works, but the defect this PR fixes
//! was never in a helper — it was that `run_pm_task_with_persona` never called
//! one. A helper-only test would have passed just as happily against the
//! broken tree. This test therefore drives the REAL dispatch entry point (the
//! same `trusty_agents::ctrl::run_pm_task_with_persona` the REPL `/agent`
//! command, the HTTP API, Slack and Telegram all funnel persona chat into) and
//! asserts on what that function actually built. Delete the call site and this
//! test goes red while everything else stays green.
//! What: writes a throwaway agent package declaring one `[[plugins.python]]`
//! tool with a package-relative `script`, points `TAGENT_CONFIG_DIR` and
//! `$HOME` at it, captures `trusty_agents` tracing output, and calls the
//! dispatch function. The LLM leg is deliberately routed at a local (absent)
//! Ollama endpoint via `SessionOverrides::provider = "local"`, so the turn
//! fails AFTER the registry is built and no external network call is made; a
//! placeholder `OPENROUTER_API_KEY` satisfies `llm::create_client`'s
//! before-the-registry credential check without ever being transmitted. The
//! assertions are on the captured registry, not on a model reply. Two things
//! are asserted: the plugin was
//! registered, and it SURVIVED the `[tools].allow` / RBAC / scope gate into the
//! set advertised to the model — a plugin registered but gated away would be
//! just as useless as one never registered.
//! Test: `persona_python_plugin_is_registered_by_the_dispatch_path` — this
//! file IS the test.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use trusty_agents::ctrl::{SessionOverrides, run_pm_task_with_persona};

/// `tracing_subscriber` writer that appends every formatted event to a shared
/// buffer, so the test can assert on what the production code logged.
#[derive(Clone)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .write(buf)
            .map(|_| buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// A persona declaring `[[plugins.python]]` gets that tool registered AND
/// advertised by the production dispatch path (#446).
///
/// Why: see the module doc — this is the only assertion in the suite that
/// fails if the `register_python_plugins` CALL in `run_pm_task_with_persona`
/// is removed while the helper itself survives.
/// What: drives the real entry point against a sandboxed agent package and
/// asserts on the two tracing events the registry build emits. The dispatch
/// result is intentionally ignored: the turn is expected to fail at the LLM
/// leg (no model server), which happens long after the registry is built.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persona_python_plugin_is_registered_by_the_dispatch_path() {
    let buffer = Arc::new(Mutex::new(Vec::<u8>::new()));
    let _ = tracing_subscriber::fmt()
        .with_writer(CaptureWriter(Arc::clone(&buffer)))
        .with_env_filter(tracing_subscriber::EnvFilter::new("trusty_agents=info"))
        .with_ansi(false)
        .try_init();

    let project = tempfile::tempdir().expect("temp project dir");
    let agents_dir = project.path().join(".trusty-agents").join("agents");
    std::fs::create_dir_all(&agents_dir).expect("create agents dir");

    // The bundled script, referenced below by a PACKAGE-RELATIVE path — the
    // co-located-with-its-package layout the plugin model is built around.
    std::fs::write(
        agents_dir.join("echo_tool.py"),
        "import sys, json\n\
call = json.loads(sys.stdin.readline())\n\
print(json.dumps({'type': 'tool_result', 'id': call.get('id'), \
'status': 'success', 'content': 'ok'}))\n",
    )
    .expect("write plugin script");

    std::fs::write(
        agents_dir.join("pyplug-probe.toml"),
        r#"
[agent]
name = "pyplug-probe"
role = "assistant"
model = "llama3"
description = "throwaway persona for the [[plugins.python]] wiring test"

[llm]
temperature = 0.0
max_tokens = 64

[tools]
allow = ["demo_python_echo"]

[[plugins.python]]
name = "demo_python_echo"
description = "Echo tool bundled with this agent package."
script = "echo_tool.py"
timeout_secs = 5

[system_prompt]
content = "Probe persona. Call demo_python_echo."
"#,
    )
    .expect("write agent toml");

    // SAFETY: this integration test is the ONLY test in its binary, so nothing
    // else in this process can observe the mutation. `TAGENT_CONFIG_DIR` makes
    // `AgentConfig::by_name_async` resolve our throwaway package (and nothing
    // from the developer's real `~/.trusty-agents`); `HOME` sandboxes the
    // second resolution tier and the global MCP config for the same reason.
    //
    // `OPENROUTER_API_KEY` is a PLACEHOLDER and is never transmitted: the
    // `provider = "local"` override below routes the chat leg at a local
    // Ollama endpoint, which sends no auth header. It is set because
    // `llm::create_client` runs BEFORE the registry is built and hard-fails
    // when no credential of any kind resolves — which is exactly how the first
    // CI run of this test failed while passing on a developer box that happened
    // to have a real key exported. Set unconditionally (not only when unset) so
    // the test behaves identically in both environments.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", &agents_dir);
        std::env::set_var("HOME", project.path());
        std::env::set_var("OPENROUTER_API_KEY", "placeholder-never-transmitted");
    }

    // `provider = "local"` forces the Ollama routing branch, so the eventual
    // chat call targets localhost rather than any paid or external endpoint. It
    // cannot short-circuit to the claude-cli path, so the registry below is
    // always built.
    let overrides = SessionOverrides {
        provider: Some("local".to_string()),
        ..Default::default()
    };

    // The turn is EXPECTED to fail (no local model server). The registry is
    // built well before that, which is all this test observes. A generous
    // timeout keeps a hung network probe from wedging CI instead of failing.
    let _ = tokio::time::timeout(
        Duration::from_secs(120),
        run_pm_task_with_persona(
            project.path(),
            "pyplug-probe",
            "call the echo tool",
            &[],
            None,
            overrides,
        ),
    )
    .await;

    let logs =
        String::from_utf8_lossy(&buffer.lock().unwrap_or_else(|e| e.into_inner())).to_string();

    assert!(
        logs.contains("registered [[plugins.python]] tools") && logs.contains("demo_python_echo"),
        "run_pm_task_with_persona must register the agent's declared \
         [[plugins.python]] tools; captured logs:\n{logs}"
    );
    assert!(
        logs.contains("persona tool registry built")
            && logs.lines().any(
                |l| l.contains("persona tool registry built") && l.contains("demo_python_echo")
            ),
        "the registered python plugin must survive the [tools].allow / RBAC / \
         scope gate into the advertised tool set; captured logs:\n{logs}"
    );
}
