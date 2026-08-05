//! End-to-end demo for the bundled `[[plugins.python]]` tool wiring (#446,
//! epic #3052).
//!
//! Why: Task 3 of the plugin-python-wiring work needs an "it actually works"
//! proof — the demo agent, driven through the REAL persona-chat dispatch
//! function, calling its bundled Python tool and returning a live result. This
//! test calls `trusty_agents::ctrl::run_pm_task_with_persona` DIRECTLY — the
//! exact function the REPL (`src/repl/dispatch.rs:143`), the HTTP API, Slack,
//! and Telegram all funnel persona chat into. So it exercises the production
//! runtime path, not a mock or a re-implementation.
//! What: Loads the `demo-assistant` agent (which declares
//! `[[plugins.python]] crypto_price` pointing at
//! `.trusty-agents/skills/crypto-price/crypto_price.py`), sends a Bitcoin price
//! question, and asserts a non-empty answer carrying a numeric price. The tool
//! itself fetches from CoinGecko's keyless endpoint with a deterministic
//! offline fallback, so the run is resilient to venue Wi-Fi.
//! Test: this file IS the test. `#[ignore]` because it needs a live LLM
//! credential + network; run explicitly with
//! `cargo test -p trusty-agents --test plugin_python_demo_e2e -- --ignored --nocapture`.
//! Set `RUST_LOG=trusty_agents=debug` to see the plugin registration and the
//! `python3` subprocess spawn in the transcript.

use std::path::PathBuf;

use trusty_agents::ctrl::{SessionOverrides, run_pm_task_with_persona};

/// Any credential the persona LLM call can route through. Without one, the
/// dispatch cannot reach a model, so the demo is skipped (not failed).
fn has_llm_credential() -> bool {
    ["OPENROUTER_API_KEY", "ANTHROPIC_API_KEY", "OPENAI_API_KEY"]
        .iter()
        .any(|k| std::env::var(k).map(|v| !v.is_empty()).unwrap_or(false))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a live LLM credential + network; run with --ignored"]
async fn demo_assistant_calls_bundled_python_tool() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("trusty_agents=info")),
        )
        .with_test_writer()
        .try_init();

    if !has_llm_credential() {
        eprintln!(
            "SKIP demo_assistant_calls_bundled_python_tool: no LLM credential \
             (set OPENROUTER_API_KEY / ANTHROPIC_API_KEY) — cannot run the live demo."
        );
        return;
    }

    // Cargo runs integration tests with CWD = the package root, so the
    // CWD-relative agent registry (`.trusty-agents/agents`) and the plugin
    // script base dir both resolve to this crate's bundled demo package.
    let project_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let user_input = "What is the current price of Bitcoin in USD?";
    eprintln!("\n=== DEMO: user -> demo-assistant ===\n{user_input}\n");

    let response = run_pm_task_with_persona(
        &project_dir,
        "demo-assistant",
        user_input,
        &[],
        None,
        SessionOverrides::default(),
    )
    .await
    .expect("persona dispatch must succeed");

    eprintln!("\n=== DEMO: demo-assistant -> user ===\n{response}\n=== END DEMO ===\n");

    assert!(
        !response.trim().is_empty(),
        "demo-assistant returned an empty answer"
    );
    assert!(
        response.chars().any(|c| c.is_ascii_digit()),
        "answer should carry a numeric price returned by the crypto_price tool; got: {response}"
    );
}
