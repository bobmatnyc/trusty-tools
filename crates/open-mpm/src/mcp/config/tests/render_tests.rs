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
    assert_eq!(cfg.mcp.services.len(), 4);
    let gw = cfg
        .mcp
        .services
        .iter()
        .find(|s| s.name == "gworkspace-mcp")
        .expect("gworkspace-mcp present");
    assert!(gw.enabled);
    assert!(gw.tools.iter().any(|t| t.name == "gmail_search"));
    assert!(gw.tools.iter().any(|t| t.name == "calendar_list"));
    let slack = cfg
        .mcp
        .services
        .iter()
        .find(|s| s.name == "slack-user-proxy")
        .expect("slack-user-proxy present");
    assert!(!slack.enabled);
    // Native local integrations stay out of the registry.
    assert!(!cfg.mcp.services.iter().any(|s| s.name == "kuzu-memory"));
    assert!(
        !cfg.mcp
            .services
            .iter()
            .any(|s| s.name == "mcp-vector-search")
    );
}
