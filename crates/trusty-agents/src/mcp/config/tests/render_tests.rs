//! Tests for role gating, prompt rendering, and the local-inference section
//! of `GlobalConfig`.
//!
//! Why: keeps the pure rendering + parsing assertions separate from the
//! disk-mutating load/save tests so each file stays under the 500-line cap.
//! What: `servers_for_role`, `render_prompt_section`, the `[local_inference]`
//! checks, and the `DEFAULT_CONFIG_TOML` validity guard — which since #7454
//! asserts against what the asset MIGRATES to, because `GlobalConfig` no
//! longer models MCP servers at all.
//! Test: this file is itself the test coverage.

use trusty_mcp::config::{McpServerConfig, McpTransport};

use crate::mcp::config::defaults::DEFAULT_CONFIG_TOML;
use crate::mcp::config::{GlobalConfig, LocalInferenceConfig};
use crate::mcp::extensions::{self, ToolDescriptor};

/// One enabled stdio server with a description and a static tool list.
fn server(name: &str, tools: &[&str]) -> McpServerConfig {
    let mut out = McpServerConfig::new(
        name,
        McpTransport::Stdio {
            command: format!("{name}-bin"),
            args: Vec::new(),
            env: Default::default(),
        },
    );
    extensions::set(
        &mut out.extensions,
        extensions::DESCRIPTION,
        &format!("{name} service"),
    );
    let declared: Vec<ToolDescriptor> = tools
        .iter()
        .map(|t| ToolDescriptor {
            name: (*t).to_string(),
            description: format!("{t} description"),
        })
        .collect();
    if !declared.is_empty() {
        extensions::set(&mut out.extensions, extensions::TOOLS, &declared);
    }
    out
}

/// The servers the migration produces from the shipped default asset.
///
/// Why: `DEFAULT_CONFIG_TOML` still carries the retired tables ON PURPOSE —
/// they are the seed `crate::mcp::shared::migrate` drains into the shared
/// file, which is how an install keeps the connectors it always shipped. The
/// assertions below therefore read the MIGRATED list, not a `GlobalConfig`
/// field that no longer exists.
fn default_servers() -> Vec<McpServerConfig> {
    crate::mcp::shared::migrate::convert(DEFAULT_CONFIG_TOML).0
}

fn find(servers: &[McpServerConfig], name: &str) -> McpServerConfig {
    servers
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("{name} present in the shipped defaults"))
        .clone()
}

#[test]
fn servers_for_role_gating() {
    let cfg = GlobalConfig::default();
    let servers = vec![server("alpha", &["alpha_op"]), server("beta", &[])];

    assert_eq!(cfg.servers_for_role("ctrl", &servers).len(), 2);
    assert_eq!(cfg.servers_for_role("pm", &servers).len(), 2);
    assert!(cfg.servers_for_role("engineer", &servers).is_empty());
    assert!(cfg.servers_for_role("coder", &servers).is_empty());
}

#[test]
fn servers_for_role_drops_disabled() {
    let cfg = GlobalConfig::default();
    let mut off = server("off", &["off_op"]);
    off.enabled = false;
    let servers = vec![server("on", &["on_op"]), off];
    let visible = cfg.servers_for_role("ctrl", &servers);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].name, "on");
}

#[test]
fn render_prompt_section_includes_tool_names() {
    let cfg = GlobalConfig::default();
    let servers = vec![server("granola", &["granola_search", "granola_get"])];
    let rendered = cfg
        .render_prompt_section("ctrl", &servers)
        .expect("non-empty");
    assert!(rendered.contains("granola"), "got: {rendered}");
    assert!(rendered.contains("granola_search"), "got: {rendered}");
    assert!(rendered.contains("granola_get"), "got: {rendered}");
}

/// A disabled server is RENDERED, marked — hiding it would tell the model a
/// connector is absent when it is one flag away (DOC-57 §4.4 C-03.2).
#[test]
fn render_prompt_section_marks_disabled_servers() {
    let cfg = GlobalConfig::default();
    let mut off = server("granola", &["granola_search"]);
    off.enabled = false;
    let rendered = cfg
        .render_prompt_section("ctrl", &[off])
        .expect("non-empty");
    assert!(rendered.contains("(disabled)"), "got: {rendered}");
    assert!(
        !rendered.contains("granola_search"),
        "a disabled server must not advertise callable tools: {rendered}"
    );
}

#[test]
fn render_prompt_section_names_a_server_with_no_declared_tools() {
    let cfg = GlobalConfig::default();
    let rendered = cfg
        .render_prompt_section("ctrl", &[server("bare", &[])])
        .expect("non-empty");
    assert!(rendered.contains("no tools declared"), "got: {rendered}");
}

#[test]
fn render_prompt_section_empty_for_excluded_role() {
    let cfg = GlobalConfig::default();
    let servers = vec![server("alpha", &["alpha_op"])];
    assert!(cfg.render_prompt_section("engineer", &servers).is_none());
    assert!(cfg.render_prompt_section("ctrl", &[]).is_none());
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
fn local_inference_enabled_defaults_true_when_key_omitted() {
    // audit 2026-08-19: `enabled` carried `#[serde(default)]`, so a
    // `[local_inference]` section that omits the key parsed as `false` while
    // both the doc comment and `LocalInferenceConfig::default()` said `true`.
    // A hand-edited config that only changed `model` silently turned the
    // fast-path off.
    let cfg = GlobalConfig::from_toml_str(
        r#"
[local_inference]
model = "ollama/qwen3:8b"
"#,
    )
    .expect("section without `enabled` parses");
    assert!(
        cfg.local_inference.enabled,
        "a [local_inference] section omitting `enabled` must inherit the \
         documented default (true), not serde's bool default (false)"
    );
    assert_eq!(cfg.local_inference.model, "ollama/qwen3:8b");
}

#[test]
fn local_inference_explicit_false_still_wins() {
    // The default only fills an ABSENT key — an operator who wrote
    // `enabled = false` keeps the fast-path off.
    let cfg = GlobalConfig::from_toml_str(
        r#"
[local_inference]
enabled = false
"#,
    )
    .expect("explicit false parses");
    assert!(!cfg.local_inference.enabled);
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

/// #7454: the shipped defaults are asserted through the migration, because
/// that is now the only path from this asset to a configured server.
#[test]
fn default_config_is_valid_toml() {
    // The rest of the file must still parse as `GlobalConfig` — the retired
    // tables are ignored by it, not rejected.
    let cfg = GlobalConfig::from_toml_str(DEFAULT_CONFIG_TOML).expect("default parses");
    assert!(cfg.mcp.inject_for_roles.iter().any(|r| r == "ctrl"));

    let servers = default_servers();
    // ADR-0014 + #3203/#3204: five services after the slack-user-proxy retire
    // and the gworkspace-mcp → trusty-mpm swap, plus trusty-memory and
    // trusty-search which moved off the dead OpenRPC stubs, plus the one
    // surviving registry endpoint (gworkspace).
    assert_eq!(servers.len(), 6, "shipped connector count drifted");

    let tm = find(&servers, "trusty-mpm");
    assert!(tm.enabled);
    let (command, args, _) = extensions::stdio_parts(&tm).expect("stdio");
    assert_eq!(command, "trusty-mpm");
    assert_eq!(args, ["serve", "--stdio"]);
    let tm_tools = extensions::tools(&tm);
    assert!(tm_tools.iter().any(|t| t.name == "session_list"));
    assert!(tm_tools.iter().any(|t| t.name == "agent_delegate"));

    let mem = find(&servers, "trusty-memory");
    assert!(mem.enabled, "trusty-memory must ship enabled, not DISABLED");
    let (command, args, _) = extensions::stdio_parts(&mem).expect("stdio");
    assert_eq!(command, "trusty-memory");
    // #5267: bare `serve` IS MCP stdio for trusty-memory, matching trusty-search.
    assert_eq!(args, ["serve"]);
    assert!(
        extensions::discover(&mem),
        "trusty-memory should live-discover its tools"
    );

    let search = find(&servers, "trusty-search");
    assert!(
        search.enabled,
        "trusty-search must ship enabled, not DISABLED"
    );
    let (command, args, _) = extensions::stdio_parts(&search).expect("stdio");
    assert_eq!(command, "trusty-search");
    assert_eq!(args, ["serve"]);
    assert!(
        extensions::discover(&search),
        "trusty-search should live-discover its tools"
    );

    assert!(
        !servers.iter().any(|s| s.name == "slack-user-proxy"),
        "slack-user-proxy retired per ADR-0014"
    );
    assert!(
        !servers.iter().any(|s| s.name == "gworkspace-mcp"),
        "gworkspace-mcp service removed (#3204); the live path is the OpenRPC \
         `gworkspace` endpoint"
    );
    // Native local integrations stay out of the registry.
    assert!(!servers.iter().any(|s| s.name == "kuzu-memory"));
    assert!(!servers.iter().any(|s| s.name == "mcp-vector-search"));
}

/// Drift guard (#3203): freeze the curated `trusty-mpm` static tool list as a
/// checked-in expected set so a silent typo, addition, or removal in
/// `default-config.toml` fails CI.
///
/// Why: the ideal guard would assert directly against
/// `trusty_mpm::mcp::tools::TOOL_CATALOG`, but `trusty-agents` does not depend
/// on `trusty-mpm`, and adding it as a dev-dependency purely for this one
/// assertion would pull that crate's full daemon/tui/telegram/slack graph into
/// every `cargo test -p trusty-agents` run. This checked-in list is the
/// cheaper option #3203 named — it catches drift in this file, though not a
/// rename inside `trusty-mpm` itself (a human cross-checks that against
/// `crates/trusty-mpm/src/mcp/tools/mod.rs::TOOL_CATALOG`).
/// What: migrates `DEFAULT_CONFIG_TOML`, finds `trusty-mpm`, and asserts its
/// tool names equal `EXPECTED_TRUSTY_MPM_TOOLS` verbatim (order-sensitive).
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
    let servers = default_servers();
    let svc = find(&servers, "trusty-mpm");
    let tools = extensions::tools(&svc);
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        EXPECTED_TRUSTY_MPM_TOOLS.to_vec(),
        "the trusty-mpm static tool list drifted from the checked-in expected \
         list; update EXPECTED_TRUSTY_MPM_TOOLS here AND cross-check against \
         crates/trusty-mpm/src/mcp/tools/mod.rs::TOOL_CATALOG"
    );

    // Safety-relevant substring check: `mcp_service_tool_executors()` feeds
    // this description to the LLM VERBATIM as the tool's function description
    // (crates/trusty-agents/src/tools/mcp_service_tools.rs), so it is
    // behavior-relevant, not decorative. `agent_delegate`'s source description
    // (crates/trusty-mpm/src/mcp/tools/core.rs) clarifies the tool is
    // TRACKING/GATING only and does NOT execute the agent — without it the LLM
    // could believe calling `agent_delegate` runs the agent and skip the native
    // Agent/Task call entirely.
    let agent_delegate_desc = tools
        .iter()
        .find(|t| t.name == "agent_delegate")
        .expect("agent_delegate tool present")
        .description
        .clone();
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
