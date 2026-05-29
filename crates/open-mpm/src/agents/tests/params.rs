//! `[llm]` / `[runner_config]` / `runner` parsing tests for `AgentConfig`.
//!
//! Why: Pins the `LlmParams` field defaults and overrides, the `RunnerKind`
//! kebab-case variants, and the `[runner_config]` block so refactors to the
//! params data shapes (in `agents::params`) can't silently change defaults.
//! What: Pure `toml::from_str` round-trips asserting parsed `llm` / `runner` /
//! `runner_config` / `tools` values.
//! Test: This module IS the test surface.

use crate::agents::{AgentConfig, RunnerKind, ToolChoice};

#[test]
fn llm_params_caching_defaults_true() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert!(cfg.llm.enable_prompt_caching);
}

#[test]
fn llm_params_max_turns_defaults_to_20() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert_eq!(cfg.llm.max_turns, 20);
}

#[test]
fn llm_params_max_turns_parses_override() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024
max_turns = 30

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert_eq!(cfg.llm.max_turns, 30);
}

#[test]
fn llm_params_caching_can_be_disabled() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024
enable_prompt_caching = false

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert!(!cfg.llm.enable_prompt_caching);
}

#[test]
fn persistent_session_defaults_to_false() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert!(!cfg.agent.persistent_session);
}

#[test]
fn persistent_session_parses_when_present() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"
persistent_session = true

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert!(cfg.agent.persistent_session);
}

#[test]
fn llm_params_tool_choice_defaults_auto() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert_eq!(cfg.llm.tool_choice, ToolChoice::Auto);
}

#[test]
fn llm_params_tool_choice_parses_any() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024
tool_choice = "any"

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert_eq!(cfg.llm.tool_choice, ToolChoice::Any);
}

#[test]
fn llm_params_use_finish_task_defaults_false() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert!(!cfg.llm.use_finish_task);
}

#[test]
fn llm_params_use_finish_task_parses_true() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024
use_finish_task = true

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert!(cfg.llm.use_finish_task);
}

#[test]
fn llm_params_use_anthropic_direct_defaults_false() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert!(!cfg.llm.use_anthropic_direct);
}

#[test]
fn llm_params_use_anthropic_direct_parses_true() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024
use_anthropic_direct = true

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert!(cfg.llm.use_anthropic_direct);
}

#[test]
fn runner_defaults_to_subprocess() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert_eq!(cfg.agent.runner, RunnerKind::Subprocess);
}

#[test]
fn runner_parses_claude_code() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"
runner = "claude-code"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert_eq!(cfg.agent.runner, RunnerKind::ClaudeCode);
}

#[test]
fn runner_parses_in_process() {
    // #198 / Phase C: agents opt into the in-process runner via
    // `runner = "in-process"` (kebab-case for the InProcess variant).
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"
runner = "in-process"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert_eq!(cfg.agent.runner, RunnerKind::InProcess);
}

#[test]
fn runner_config_defaults_to_none() {
    // No [runner_config] section -> all fields None.
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert!(cfg.runner_config.max_tool_calls.is_none());
}

#[test]
fn runner_config_parses_max_tool_calls() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[runner_config]
max_tool_calls = 12
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert_eq!(cfg.runner_config.max_tool_calls, Some(12));
}

#[test]
fn runner_parses_inline() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"
runner = "inline"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert_eq!(cfg.agent.runner, RunnerKind::Inline);
}

#[test]
fn tools_config_absent_means_no_restriction() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert!(cfg.tools.allowed.is_none());
}
