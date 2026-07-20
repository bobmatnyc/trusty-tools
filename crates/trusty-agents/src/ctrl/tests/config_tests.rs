//! Tests for ctrl `config` + `claude_cli` prompt/credential helpers.

use crate::agents::AgentConfig;
use crate::llm;

use super::super::claude_cli::{filter_project_index_in_prompt, strip_cli_artifacts};
use super::super::config::{
    apply_credential_routing, build_deployment_footer, build_user_context_prefix,
    render_user_context_block, render_user_datetime, resolve_agent_config,
};

#[test]
fn build_user_context_prefix_appends_base_content_after_block() {
    with_sandboxed_home(|_home| {
        let out = build_user_context_prefix("BASE PROMPT CONTENT");
        assert!(out.contains("## User Context"));
        assert!(out.contains("BASE PROMPT CONTENT"));
        assert!(
            out.find("## User Context").unwrap() < out.find("BASE PROMPT CONTENT").unwrap(),
            "user context block must precede the base content: {out}"
        );
    });
}
