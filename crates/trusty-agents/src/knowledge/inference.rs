//! Tool-free inference preserving the assistant's configured provider (#4283).
use crate::{agents::AgentConfig, llm, tools::ToolRegistry};
use anyhow::{Result, bail};
use async_openai::types::{
    ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
};

/// Why: background extraction must not inherit another assistant's ambient AWS routing.
/// What: resolve configured credentials, reject tool-capable CLI execution, and issue one tool-free request.
/// Test: `cli_extraction_is_refused_before_inference`.
pub async fn extract(
    mut cfg: AgentConfig,
    text: &str,
) -> Result<(super::extraction::Extraction, String)> {
    if cfg.agent.runner == crate::agents::RunnerKind::ClaudeCode {
        bail!(
            "Automatic extraction requires a tool-free inference provider; ClaudeCode extraction is unavailable"
        );
    }
    let configured = uses_configured_provider(&cfg);
    if !configured {
        let credentials = crate::ctrl::config::resolve_overridden_credentials(&mut cfg, None)?;
        if matches!(credentials, llm::credentials::LlmCredentials::ClaudeCode) {
            bail!("ClaudeCode extraction is unavailable");
        }
        crate::ctrl::config::apply_credential_routing(&mut cfg, &credentials);
    }
    let adapter = llm::adapter::adapter_for_model(&cfg.agent.model);
    let source = serde_json::json!({"source_text":text}).to_string();
    let raw = if adapter.provider() == llm::adapter::Provider::Bedrock {
        let client = llm::bedrock::build_client(
            cfg.llm.aws_profile.as_deref(),
            cfg.llm.aws_region.as_deref(),
        )
        .await?;
        let (content, tools, _) = llm::bedrock::chat_oneshot(
            &client,
            cfg.agent
                .model
                .strip_prefix("bedrock/")
                .unwrap_or(&cfg.agent.model),
            super::extraction::INSTRUCTION,
            &source,
            0.0,
            6000,
            vec![],
        )
        .await?;
        anyhow::ensure!(
            tools.is_empty(),
            "Extraction provider attempted a tool call"
        );
        content.ok_or_else(|| anyhow::anyhow!("Extraction provider returned no content"))?
    } else {
        let messages = vec![
            ChatCompletionRequestSystemMessageArgs::default()
                .content(super::extraction::INSTRUCTION)
                .build()?
                .into(),
            ChatCompletionRequestUserMessageArgs::default()
                .content(source)
                .build()?
                .into(),
        ];
        let (raw, _) = llm::chat_with_tools_gated(
            &llm::create_client_for_model(&cfg.agent.model)?,
            &cfg.agent.model,
            adapter.as_ref(),
            messages,
            std::sync::Arc::new(ToolRegistry::new()),
            Some(vec![]),
            0.0,
            6000,
            1,
            false,
            None,
            false,
            false,
            cfg.llm.use_anthropic_direct,
            &[],
            Some((
                cfg.llm.aws_profile.as_deref(),
                cfg.llm.aws_region.as_deref(),
            )),
        )
        .await?;
        raw
    };
    Ok((super::extraction::validate(&raw, text)?, cfg.agent.model))
}
/// Checkpoint identity contains routing settings, never credentials or unrelated tool/personality settings.
pub fn fingerprint(cfg: &AgentConfig) -> String {
    serde_json::json!({"schema":2,"model":cfg.agent.model,"provider":cfg.agent.provider_id,
        "runner":format!("{:?}",cfg.agent.runner),"aws_profile":cfg.llm.aws_profile,
        "aws_region":cfg.llm.aws_region,"anthropic_direct":cfg.llm.use_anthropic_direct})
    .to_string()
}
fn uses_configured_provider(cfg: &AgentConfig) -> bool {
    cfg.agent.provider_id.is_some()
        || cfg.llm.use_anthropic_direct
        || llm::adapter::adapter_for_model(&cfg.agent.model).requires_raw_http()
        || cfg.agent.model.starts_with("bedrock/")
}
#[cfg(test)]
mod tests {
    #[test]
    fn extraction_fingerprint_ignores_unrelated_settings() {
        let mut cfg = crate::agents::AgentConfig::ctrl_default();
        let before = super::fingerprint(&cfg);
        cfg.tools.allow = Some(vec![]);
        cfg.system_prompt.content = "Changed conversational style".into();
        assert_eq!(super::fingerprint(&cfg), before);
        cfg.agent.model = "ollama/changed".into();
        assert_ne!(super::fingerprint(&cfg), before);
    }
    #[test]
    fn configured_provider_preserves_assistant_aws_context() {
        let mut cfg = crate::agents::AgentConfig::ctrl_default();
        cfg.agent.model = "bedrock/anthropic.claude-sonnet".into();
        cfg.llm.aws_profile = Some("synthetic-profile".into());
        cfg.llm.aws_region = Some("us-east-1".into());
        assert!(super::uses_configured_provider(&cfg));
        assert_eq!(cfg.llm.aws_profile.as_deref(), Some("synthetic-profile"));
        cfg.agent.model = "ollama/fixture".into();
        assert!(super::uses_configured_provider(&cfg));
    }
    #[tokio::test]
    async fn cli_extraction_is_refused_before_inference() {
        let mut cfg = crate::agents::AgentConfig::ctrl_default();
        cfg.agent.runner = crate::agents::RunnerKind::ClaudeCode;
        assert!(
            super::extract(cfg, "Synthetic data")
                .await
                .unwrap_err()
                .to_string()
                .contains("tool-free")
        );
    }
}
