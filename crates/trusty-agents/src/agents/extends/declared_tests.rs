//! `extends` inheritance of the per-key-merged tables (#7901).
//!
//! Why: `merge_extends` kept the base's `[llm]` (beyond four keys),
//! `[compress]`, `[runner_config]`, `[session]`, `[plugins]`, `[rbac]` and
//! `[workstreams]` wholesale and dropped a child's own values in them. These
//! tests parse through the real loader (`AgentConfig::from_toml_str`) and
//! compare every table at once, so a fix that handles one table cannot pass.

use std::path::Path;

use super::merge_extends;
use crate::agents::AgentConfig;

/// A base declaring a non-default value in every per-key-merged table.
const BASE: &str = r#"
[agent]
name = "base"
role = "assistant"
model = "anthropic/claude-sonnet-4-6"
description = "base"
[llm]
temperature = 0.3
max_tokens = 2048
enable_prompt_caching = false
max_turns = 31
persona_max_turns = 9
tool_choice = "any"
use_finish_task = true
stop_sequences = ["BASE-STOP"]
routing_model = "base-router"
[compress]
enabled = false
token_budget = 1111
output_style = "lite"
[runner_config]
max_tool_calls = 7
[session]
enabled = true
compression_threshold = 55
compression_model = "base-summarizer"
[[plugins.python]]
name = "base_tool"
description = "base plugin"
script = "base_tool.py"
[rbac]
allowed_users_env = "BASE_USERS"
[workstreams]
enabled = false
summarize_every = 3
[system_prompt]
content = "base"
"#;

/// A child that declares none of the per-key-merged tables' keys.
const CHILD_OMITS: &str = r#"
[agent]
name = "child"
role = "assistant"
model = "anthropic/claude-sonnet-4-6"
description = "child"
extends = "base"
[llm]
[system_prompt]
content = "child"
"#;

/// A child overriding one key in every table.
const CHILD_OVERRIDES: &str = r#"
[agent]
name = "child"
role = "assistant"
model = "anthropic/claude-sonnet-4-6"
description = "child"
extends = "base"
[llm]
max_turns = 12
stop_sequences = ["CHILD-STOP"]
[compress]
token_budget = 2222
[runner_config]
max_tool_calls = 3
[session]
compression_model = "child-summarizer"
[[plugins.python]]
name = "child_tool"
description = "child plugin"
script = "child_tool.py"
[rbac]
allowed_users_env = "CHILD_USERS"
[workstreams]
recent_window = 4
[system_prompt]
content = "child"
"#;

/// `BASE` with `CHILD_OVERRIDES`' keys applied — the per-key expectation.
const EXPECTED_OVERRIDDEN: &str = r#"
[agent]
name = "expected"
role = "assistant"
model = "anthropic/claude-sonnet-4-6"
description = "expected"
[llm]
temperature = 0.3
max_tokens = 2048
enable_prompt_caching = false
max_turns = 12
persona_max_turns = 9
tool_choice = "any"
use_finish_task = true
stop_sequences = ["CHILD-STOP"]
routing_model = "base-router"
[compress]
enabled = false
token_budget = 2222
output_style = "lite"
[runner_config]
max_tool_calls = 3
[session]
enabled = true
compression_threshold = 55
compression_model = "child-summarizer"
[[plugins.python]]
name = "child_tool"
description = "child plugin"
script = "child_tool.py"
[rbac]
allowed_users_env = "CHILD_USERS"
[workstreams]
enabled = false
summarize_every = 3
recent_window = 4
[system_prompt]
content = "expected"
"#;

fn load(raw: &str) -> AgentConfig {
    AgentConfig::from_toml_str(raw, Path::new("fixture.toml")).expect("fixture parses")
}

/// Every per-key-merged table, rendered, in a fixed order.
fn table_set(cfg: &AgentConfig) -> Vec<String> {
    vec![
        format!("llm {:?}", cfg.llm),
        format!("compress {:?}", cfg.compress),
        format!("runner_config {:?}", cfg.runner_config),
        format!("session {:?}", cfg.session),
        format!("plugins {:?}", cfg.plugins),
        format!("rbac {:?}", cfg.rbac),
        format!("workstreams {:?}", cfg.workstreams),
    ]
}

/// A child that omits every table inherits all of them from the base.
#[test]
fn extends_child_inherits_every_table_it_omits() {
    let merged = merge_extends(load(BASE), load(CHILD_OMITS));
    assert_eq!(table_set(&merged), table_set(&load(BASE)));
}

/// #7901 regression: a child's declared keys win in every table, and every
/// key it omits still comes from the base.
#[test]
fn extends_child_declared_keys_win_in_every_table() {
    let merged = merge_extends(load(BASE), load(CHILD_OVERRIDES));
    assert_eq!(table_set(&merged), table_set(&load(EXPECTED_OVERRIDDEN)));
}

/// The presence record refuses text that is not TOML instead of recording nothing.
#[test]
fn declared_keys_reject_invalid_toml() {
    assert!(super::DeclaredKeys::from_toml("[llm").is_err());
}
