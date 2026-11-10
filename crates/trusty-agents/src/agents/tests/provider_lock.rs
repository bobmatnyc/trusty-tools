//! Provider lock (#10978) through the real agent-load paths.
//!
//! Why: the lock is only as strong as the paths that consult it. These tests
//! drive the loader, a model override, the built-in ctrl default and the two
//! `.md` projections, each with the lock set and unset on this thread.
//! What: [`crate::llm::provider_lock::override_on_this_thread`] sets the lock
//! for one thread, so no locked state leaks into a concurrent test.
//! Test: this file.

use std::path::Path;

use crate::agents::AgentConfig;
use crate::llm::provider_lock::{LockOverride, ProviderLock, override_on_this_thread};

fn locked() -> LockOverride {
    override_on_this_thread(Ok(Some(ProviderLock::Bedrock)))
}

fn unlocked() -> LockOverride {
    override_on_this_thread(Ok(None))
}

/// An agent TOML with `name`, `model` and extra `[agent]` lines.
fn agent_toml(name: &str, model: &str, extra: &str) -> String {
    format!(
        "[agent]\nname = \"{name}\"\nrole = \"assistant\"\nmodel = \"{model}\"\n\
         description = \"x\"\n{extra}\n\n[llm]\ntemperature = 0.0\nmax_tokens = 1024\n\n\
         [system_prompt]\ncontent = \"base\"\n"
    )
}

fn load(name: &str, model: &str, extra: &str) -> anyhow::Result<AgentConfig> {
    AgentConfig::from_toml_str(
        &agent_toml(name, model, extra),
        Path::new("lock-fixture.toml"),
    )
}

/// Why: the issue's named case — with the lock set and `ANTHROPIC_API_KEY`
/// present, a bare-slug agent must be an error naming the agent and slug,
/// not an Anthropic-direct route. Unlocked, the same agent loads as before.
/// Test: itself.
#[test]
#[serial_test::serial]
fn lock_refuses_a_bare_slug_agent_even_with_an_anthropic_key() {
    let _env = crate::test_env::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var_os("ANTHROPIC_API_KEY");
    // SAFETY: ENV_LOCK held for the entire test body.
    unsafe { std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-api03-lock-test") }; // pragma: allowlist secret

    let refused = {
        let _l = locked();
        load("research-agent", "claude-sonnet-4-6", "")
    };
    let admitted = {
        let _u = unlocked();
        load("research-agent", "claude-sonnet-4-6", "")
    };

    // SAFETY: ENV_LOCK still held.
    unsafe {
        match prev {
            Some(v) => std::env::set_var("ANTHROPIC_API_KEY", v),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
    }
    let msg = format!(
        "{:#}",
        refused.expect_err("the lock must refuse a bare slug")
    );
    assert!(
        msg.contains("research-agent") && msg.contains("claude-sonnet-4-6"),
        "{msg}"
    );
    assert_eq!(
        admitted.expect("unlocked load is unchanged").agent.model,
        "claude-sonnet-4-6"
    );
}

/// Why: the lock checks the FINAL slug, after a `bedrock` pin has rewritten
/// a bare one, so a pinned agent passes; a claude-code runner never does.
/// Test: itself.
#[test]
fn lock_checks_the_pinned_slug_and_refuses_claude_code() {
    let _l = locked();
    let pinned = load("pm", "anthropic.claude-sonnet", "provider_id = \"bedrock\"")
        .expect("a bedrock-pinned agent passes the lock");
    assert_eq!(pinned.agent.model, "bedrock/anthropic.claude-sonnet");

    let err = load(
        "claude-code-engineer",
        "bedrock/x",
        "runner = \"claude-code\"",
    )
    .expect_err("claude-code runner under the lock");
    assert!(
        format!("{err:#}").contains("claude-code-engineer"),
        "{err:#}"
    );
}

/// Why: `override_model` is reachable from an HTTP body (`/model`); under the
/// lock it must refuse a slug off Bedrock rather than rebuild the adapter.
/// Test: itself.
#[test]
fn override_model_is_refused_off_bedrock_under_the_lock() {
    let _l = locked();
    let mut cfg = load("pm", "bedrock/anthropic.claude-sonnet", "").expect("loads");
    let err = cfg
        .override_model("claude-haiku-4-5")
        .expect_err("override off Bedrock");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("pm") && msg.contains("claude-haiku-4-5"),
        "{msg}"
    );
}

/// Why: the built-in ctrl default is `anthropic/…`; under the lock the
/// fallible constructor must return the refusal so dispatch can report it.
/// Test: itself.
#[test]
fn ctrl_default_is_refused_under_the_lock() {
    {
        let _u = unlocked();
        AgentConfig::try_ctrl_default().expect("unlocked default loads");
    }
    let _l = locked();
    let err = AgentConfig::try_ctrl_default().expect_err("locked default refused");
    assert!(format!("{err:#}").contains("ctrl"), "{err:#}");
}

/// Why: the `.md` overlay and `.claude/agents` readers build an
/// `AgentConfig` without `apply_provider_pin`; the lock must still hold.
/// Test: itself.
#[test]
fn md_projections_are_refused_under_the_lock() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("writer.md");
    std::fs::write(
        &path,
        "---\nname: writer\nmodel: claude-sonnet-4-6\n---\nbody\n",
    )
    .expect("write md");
    {
        let _u = unlocked();
        crate::agents::registry::parse_md_agent(&path).expect("unlocked md loads");
        crate::agents::mpm_bridge::load_mpm_agent(&path).expect("unlocked mpm loads");
    }
    let _l = locked();
    for err in [
        crate::agents::registry::parse_md_agent(&path).expect_err("md refused"),
        crate::agents::mpm_bridge::load_mpm_agent(&path).expect_err("mpm refused"),
    ] {
        assert!(format!("{err:#}").contains("writer"), "{err:#}");
    }
}
