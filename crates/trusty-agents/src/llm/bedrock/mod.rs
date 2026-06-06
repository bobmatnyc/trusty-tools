//! AWS Bedrock Converse API client.
//!
//! Why: Bedrock uses AWS SigV4 auth and a native Converse API format, not
//! the OpenAI-compatible format used by OpenRouter/Anthropic direct. This
//! module wraps `aws-sdk-bedrockruntime` so the agent harness can route any
//! `bedrock/<model_id>` agent through AWS instead of OpenRouter, while
//! preserving the `(content, tool_calls, usage)` shape the chat loop expects.
//! What: Builds an authenticated `bedrockruntime::Client` from an optional
//! AWS profile + region (`build_client`), runs single-turn `chat()` /
//! `chat_oneshot()` and multi-turn `chat_with_tools()` against the Converse
//! API (in `client`), and translates OpenAI-format tool definitions into
//! Bedrock `ToolConfiguration` (in `convert`).
//! Test: Conversion units in `convert::tests`; end-to-end smoke test
//! `client::tests::bedrock_smoke_test` (gated `#[ignore]`) requires real AWS
//! credentials.

mod client;
mod convert;

pub use client::{chat, chat_oneshot, chat_with_tools};
pub use convert::BedrockToolUse;

use anyhow::Result;
use aws_config::BehaviorVersion;
use aws_sdk_bedrockruntime::Client as BedrockClient;

/// Build an AWS Bedrock client using the standard credential chain.
///
/// Why: The AWS SDK's default chain handles env vars, `~/.aws/credentials`
/// profiles, instance metadata, and SSO; centralizing client creation here
/// keeps that complexity out of the agent runner and lets us layer
/// per-agent profile/region overrides on top.
/// What: If `profile` is set, selects that named profile; if `region` is
/// set, uses it; otherwise defaults to `us-east-1` (where Bedrock is
/// universally available).
/// Test: Indirectly via `client::tests::bedrock_smoke_test` — unit-testing the
/// AWS config loader requires live credentials.
pub async fn build_client(profile: Option<&str>, region: Option<&str>) -> Result<BedrockClient> {
    let region_str = region.unwrap_or("us-east-1").to_string();
    let region_provider = aws_config::meta::region::RegionProviderChain::first_try(
        aws_types::region::Region::new(region_str),
    );

    let mut loader = aws_config::defaults(BehaviorVersion::latest()).region(region_provider);

    if let Some(p) = profile {
        loader = loader.profile_name(p);
    }

    let config = loader.load().await;
    Ok(BedrockClient::new(&config))
}
