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
/// universally available). Before returning, resolves credentials once
/// (`preflight_credentials`, #10978): a broken chain is an error naming
/// the profile and region instead of an SDK error at the first turn.
/// Test: `preflight_failure_names_profile_region_and_no_fallback`; the live
/// path via `client::tests::bedrock_smoke_test` (`#[ignore]`, real AWS).
pub async fn build_client(profile: Option<&str>, region: Option<&str>) -> Result<BedrockClient> {
    let region_str = region.unwrap_or("us-east-1").to_string();
    let region_provider = aws_config::meta::region::RegionProviderChain::first_try(
        aws_types::region::Region::new(region_str.clone()),
    );

    let mut loader = aws_config::defaults(BehaviorVersion::latest()).region(region_provider);

    if let Some(p) = profile {
        loader = loader.profile_name(p);
    }

    let config = loader.load().await;
    // #10978: fail here, naming profile and region, not at the first turn.
    match config.credentials_provider() {
        Some(provider) => {
            use aws_sdk_bedrockruntime::config::ProvideCredentials;
            preflight_credentials(provider.provide_credentials(), profile, &region_str).await?;
        }
        None => anyhow::bail!(
            "{}",
            credential_failure_message(
                profile,
                &region_str,
                "no AWS credential provider is configured"
            )
        ),
    }
    Ok(BedrockClient::new(&config))
}

/// Await one credential resolution and turn a failure into the #10978 message.
///
/// Why: a Bedrock-only setup must stop with a message an operator can act on,
/// and must say that nothing else is tried. Taking the resolution as a future
/// lets a test inject a failure without AWS or the network.
/// What: `Ok` when `attempt` yields credentials; otherwise an error naming
/// `profile`, `region`, the full source chain of the failure, and
/// "no other provider is tried". The credentials themselves are dropped.
/// Test: `preflight_failure_names_profile_region_and_no_fallback`,
/// `preflight_success_passes`.
pub(crate) async fn preflight_credentials<T, E>(
    attempt: impl std::future::Future<Output = std::result::Result<T, E>>,
    profile: Option<&str>,
    region: &str,
) -> Result<()>
where
    E: std::error::Error,
{
    match attempt.await {
        Ok(_) => Ok(()),
        Err(e) => {
            let detail = aws_smithy_types::error::display::DisplayErrorContext(e).to_string();
            anyhow::bail!("{}", credential_failure_message(profile, region, &detail))
        }
    }
}

/// The #10978 credential-failure text: profile, region, cause, no fallback.
fn credential_failure_message(profile: Option<&str>, region: &str, detail: &str) -> String {
    let profile = profile.map_or_else(
        || "the default AWS credential chain (no profile set)".to_string(),
        |p| format!("AWS profile '{p}'"),
    );
    format!(
        "Bedrock credentials did not resolve for {profile} in region '{region}': {detail}. \
         Fix the AWS credential chain (for SSO: `aws sso login`); no other provider is tried."
    )
}

#[cfg(test)]
mod preflight_tests {
    use super::*;

    /// Why: #10978 — a broken AWS chain must be reported before the first
    /// turn, naming profile and region and stating that nothing else is tried.
    /// The failure is injected; no AWS call or network is made.
    /// Test: itself.
    #[tokio::test]
    async fn preflight_failure_names_profile_region_and_no_fallback() {
        let failing = async { Err::<(), _>(std::io::Error::other("the SSO session has expired")) };
        let err = preflight_credentials(failing, Some("cto"), "us-west-2")
            .await
            .expect_err("a failed resolution must be an error");
        let msg = err.to_string();
        for needle in [
            "'cto'",
            "'us-west-2'",
            "SSO session has expired",
            "no other provider is tried",
        ] {
            assert!(msg.contains(needle), "missing {needle:?}: {msg}");
        }
        let default_chain = credential_failure_message(None, "us-east-1", "x");
        assert!(
            default_chain.contains("default AWS credential chain"),
            "{default_chain}"
        );
    }

    /// Why: a resolving chain must not be refused.
    /// Test: itself.
    #[tokio::test]
    async fn preflight_success_passes() {
        let ok = async { Ok::<_, std::io::Error>(()) };
        preflight_credentials(ok, None, "us-east-1")
            .await
            .expect("resolved credentials pass");
    }
}
