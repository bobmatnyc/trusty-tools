//! Tests for role gating, prompt/list rendering, and the local-inference
//! section of `GlobalConfig`.
//!
//! Why: Keeps the pure rendering + parsing assertions separate from the
//! disk-mutating load/save tests so each file stays under the 500-line cap.
//! What: `services_for_role`, `render_prompt_section`, `render_list`, and the
//! `[local_inference]` / `DEFAULT_CONFIG_TOML` validity checks.
//! Test: This file is itself the test coverage.

use crate::mcp::config::defaults::DEFAULT_CONFIG_TOML;
use crate::mcp::config::{GlobalConfig, LocalInferenceConfig};

#[test]
fn services_for_role_gating() {
    let cfg = GlobalConfig::from_toml_str(
        r#"
[mcp]
inject_for_roles = ["ctrl", "pm"]

[[mcp.services]]
name = "gworkspace-mcp"
description = "Google Workspace"
command = "gworkspace-mcp"
args = ["mcp"]
transport = "stdio"
enabled = true

[[mcp.services]]
name = "slack-user-proxy"
description = "Slack"
command = "slack-user-proxy"
transport = "stdio"
enabled = false
"#,
    )
    .unwrap();

    // Roles in inject list see the enabled service only (gworkspace-mcp);
    // slack-user-proxy is disabled by default and excluded.
    let ctrl_services = cfg.services_for_role("ctrl");
    assert_eq!(ctrl_services.len(), 1);
    assert_eq!(ctrl_services[0].name, "gworkspace-mcp");
    let pm_services = cfg.services_for_role("pm");
    assert_eq!(pm_services.len(), 1);
    assert_eq!(pm_services[0].name, "gworkspace-mcp");
    // Roles outside the list see nothing.
    assert!(cfg.services_for_role("engineer").is_empty());
    assert!(cfg.services_for_role("coder").is_empty());
}

#[test]
fn render_prompt_section_includes_tool_names() {
    let cfg = GlobalConfig::from_toml_str(
        r#"
[mcp]
inject_for_roles = ["pm"]

[[mcp.services]]
name = "gworkspace-mcp"
description = "Google Workspace"
command = "gworkspace-mcp"
args = ["mcp"]
transport = "stdio"
enabled = true

[[mcp.services.tools]]
name = "gmail_search"
description = "Search Gmail"
"#,
    )
    .unwrap();

    let rendered = cfg.render_prompt_section("pm").expect("non-empty");
    assert!(rendered.contains("gworkspace-mcp"));
    assert!(rendered.contains("gmail_search"));
    assert!(rendered.contains("## Available External Services (MCP)"));
}

#[test]
fn render_prompt_section_marks_disabled_services() {
    let cfg = GlobalConfig::from_toml_str(
        r#"
[mcp]
inject_for_roles = ["ctrl"]

[[mcp.services]]
name = "gworkspace-mcp"
description = "Google Workspace"
command = "gworkspace-mcp"
transport = "stdio"
enabled = true

[[mcp.services.tools]]
name = "gmail_search"
description = "Search Gmail"

[[mcp.services]]
name = "slack-user-proxy"
description = "Slack messaging"
command = "slack-user-proxy"
transport = "stdio"
enabled = false
"#,
    )
    .unwrap();

    let rendered = cfg.render_prompt_section("ctrl").expect("non-empty");
    // Enabled service appears with its tools.
    assert!(rendered.contains("gworkspace-mcp"));
    assert!(rendered.contains("gmail_search"));
    // Disabled service is listed with the disabled marker.
    assert!(rendered.contains("slack-user-proxy"));
    assert!(
        rendered.contains("(disabled"),
        "expected '(disabled' marker in rendered output, got:\n{rendered}"
    );
    assert!(
        rendered.contains("not available"),
        "expected 'not available' marker in rendered output, got:\n{rendered}"
    );
}

#[test]
fn render_prompt_section_empty_for_excluded_role() {
    let cfg = GlobalConfig::from_toml_str(
        r#"
[mcp]
inject_for_roles = ["ctrl"]

[[mcp.services]]
name = "gworkspace-mcp"
description = "Google Workspace"
command = "gworkspace-mcp"
transport = "stdio"
enabled = true
"#,
    )
    .unwrap();

    assert!(cfg.render_prompt_section("engineer").is_none());
}

#[test]
fn render_list_format() {
    let cfg = GlobalConfig::from_toml_str(
        r#"
[mcp]
inject_for_roles = ["ctrl"]

[[mcp.services]]
name = "gworkspace-mcp"
description = "Google Workspace"
command = "gworkspace-mcp"
args = ["mcp"]
transport = "stdio"
enabled = true

[[mcp.services.tools]]
name = "gmail_search"
description = "Search Gmail"

[[mcp.services]]
name = "slack-user-proxy"
description = "Slack messaging"
command = "slack-user-proxy"
transport = "stdio"
enabled = false

[[mcp.services.tools]]
name = "slack_post"
description = "Post"
"#,
    )
    .unwrap();
    let rendered = cfg.render_list();
    assert!(rendered.contains("Registered MCP services (2):"));
    assert!(rendered.contains("✓ gworkspace-mcp [stdio]"));
    assert!(rendered.contains("✗ slack-user-proxy [stdio]"));
    assert!(rendered.contains("(disabled)"));
    assert!(rendered.contains("Tools: gmail_search"));
    assert!(rendered.contains("Tools: slack_post"));
}

#[test]
fn render_list_empty() {
    let cfg = GlobalConfig::default();
    assert_eq!(cfg.render_list(), "No MCP services registered.");
}

#[test]
fn local_inference_defaults_apply() {
    // (#319, #345) LocalInferenceConfig::default must match the documented
    // shipping defaults — enabled, qwen3:30b, fallback on, localhost.
    let li = LocalInferenceConfig::default();
    assert!(li.enabled, "local inference must be enabled by default");
    assert_eq!(li.model, "ollama/qwen3:30b");
    assert!(li.fallback_on_error);
    assert_eq!(li.ollama_host, "http://localhost:11434");
    assert_eq!(li.max_tokens, 2048);
}

#[test]
fn local_inference_section_round_trips() {
    // (#319) Round-trip the [local_inference] section so the documented
    // TOML shape stays parseable as the codebase evolves.
    let cfg = GlobalConfig::from_toml_str(
        r#"
[local_inference]
enabled = true
model = "ollama/qwen3:8b"
fallback_on_error = false
ollama_host = "http://192.168.1.10:11434"
max_tokens = 4096
"#,
    )
    .unwrap();
    assert!(cfg.local_inference.enabled);
    assert_eq!(cfg.local_inference.model, "ollama/qwen3:8b");
    assert!(!cfg.local_inference.fallback_on_error);
    assert_eq!(cfg.local_inference.ollama_host, "http://192.168.1.10:11434");
    assert_eq!(cfg.local_inference.max_tokens, 4096);
}

#[test]
fn default_config_includes_local_inference_section() {
    // (#319, #345) The DEFAULT_CONFIG_TOML literal must include a usable
    // [local_inference] block so users have an obvious place to flip
    // the flag without needing to know the schema.
    let cfg = GlobalConfig::from_toml_str(DEFAULT_CONFIG_TOML).unwrap();
    assert!(cfg.local_inference.enabled);
    assert_eq!(cfg.local_inference.model, "ollama/qwen3:30b");
}

#[test]
fn default_config_is_valid_toml() {
    let cfg = GlobalConfig::from_toml_str(DEFAULT_CONFIG_TOML).expect("default parses");
    // ADR-0014 + #3203/#3204: 3 services after slack-user-proxy retire and
    // the gworkspace-mcp → trusty-mpm swap (gworkspace-mcp's static tool list
    // had drifted from the real binary; trusty-mpm's is fresh, see #3203).
    assert_eq!(cfg.mcp.services.len(), 3);
    let tm = cfg
        .mcp
        .services
        .iter()
        .find(|s| s.name == "trusty-mpm")
        .expect("trusty-mpm present");
    assert!(tm.enabled);
    assert_eq!(tm.command, "trusty-mpm");
    assert_eq!(tm.args, vec!["serve".to_string(), "--stdio".to_string()]);
    assert!(tm.tools.iter().any(|t| t.name == "session_list"));
    assert!(tm.tools.iter().any(|t| t.name == "agent_delegate"));
    assert!(
        !cfg.mcp
            .services
            .iter()
            .any(|s| s.name == "slack-user-proxy"),
        "slack-user-proxy retired per ADR-0014"
    );
    assert!(
        !cfg.mcp.services.iter().any(|s| s.name == "gworkspace-mcp"),
        "gworkspace-mcp mcp.services entry removed (#3204); live path is the \
         OpenRPC tool_registry.endpoints \"gworkspace\" entry"
    );
    // Native local integrations stay out of the registry.
    assert!(!cfg.mcp.services.iter().any(|s| s.name == "kuzu-memory"));
    assert!(
        !cfg.mcp
            .services
            .iter()
            .any(|s| s.name == "mcp-vector-search")
    );
}

/// Drift guard (#3203): freeze the curated `trusty-mpm` static tool list as a
/// checked-in expected set so a silent typo, addition, or removal in
/// `default-config.toml` fails CI.
///
/// Why: the ideal guard would assert directly against
/// `trusty_mpm::mcp::tools::TOOL_CATALOG`, but `trusty-agents` does not
/// currently depend on `trusty-mpm` (and `trusty-mpm` does not depend on
/// `trusty-agents` either — no cycle risk); adding it as a dev-dependency
/// purely for this one assertion would pull `trusty-mpm`'s full
/// daemon/tui/telegram/slack dependency graph into every `cargo test -p
/// trusty-agents` run, which is disproportionate for an Effort:S ticket. This
/// checked-in-list approach is the cheaper option named in #3203 — it still
/// catches accidental drift in this file, just not a rename inside
/// `trusty-mpm` itself (a human cross-checks that case against
/// `crates/trusty-mpm/src/mcp/tools/mod.rs::TOOL_CATALOG` when touching
/// either side).
/// What: parses `DEFAULT_CONFIG_TOML`, finds the `trusty-mpm` service, and
/// asserts its tool names equal `EXPECTED_TRUSTY_MPM_TOOLS` verbatim
/// (order-sensitive, matching the file).
/// Test: this test.
#[test]
fn trusty_mpm_service_tool_names_match_expected_curated_list() {
    const EXPECTED_TRUSTY_MPM_TOOLS: [&str; 6] = [
        "session_list",
        "session_status",
        "session_send",
        "project_list",
        "agent_delegate",
        "console_metrics",
    ];
    let cfg = GlobalConfig::from_toml_str(DEFAULT_CONFIG_TOML).expect("default parses");
    let svc = cfg
        .mcp
        .services
        .iter()
        .find(|s| s.name == "trusty-mpm")
        .expect("trusty-mpm service present in defaults (#3203)");
    let names: Vec<&str> = svc.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        EXPECTED_TRUSTY_MPM_TOOLS.to_vec(),
        "trusty-mpm mcp.services.tools drifted from the checked-in expected \
         list; update EXPECTED_TRUSTY_MPM_TOOLS here AND cross-check against \
         crates/trusty-mpm/src/mcp/tools/mod.rs::TOOL_CATALOG"
    );

    // Safety-relevant substring check: `mcp_service_tool_executors()` feeds
    // this description to the LLM VERBATIM as the tool's function
    // description (crates/trusty-agents/src/tools/mcp_service_tools.rs), so
    // it is behavior-relevant, not decorative. `agent_delegate`'s verbatim
    // source description (crates/trusty-mpm/src/mcp/tools/core.rs) carries a
    // load-bearing correction: it clarifies the tool is TRACKING/GATING only
    // and does NOT execute the agent — without it, the LLM could believe
    // calling `agent_delegate` is sufficient to run the agent and skip the
    // native Agent/Task tool call entirely. Guard against a future paraphrase
    // silently dropping that guidance.
    let agent_delegate_desc = svc
        .tools
        .iter()
        .find(|t| t.name == "agent_delegate")
        .expect("agent_delegate tool present")
        .description
        .as_str();
    assert!(
        agent_delegate_desc.contains("does not spawn"),
        "agent_delegate description must retain 'does not spawn' \
         (mcp_service_tool_executors() feeds this text to the LLM verbatim): {agent_delegate_desc}"
    );
    assert!(
        agent_delegate_desc.contains("Agent/Task tool"),
        "agent_delegate description must retain the pointer to the native \
         Agent/Task tool (mcp_service_tool_executors() feeds this text to \
         the LLM verbatim): {agent_delegate_desc}"
    );
}
