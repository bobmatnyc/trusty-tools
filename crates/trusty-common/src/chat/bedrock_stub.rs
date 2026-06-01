//! Stub `BedrockProvider` for builds without the `bedrock` cargo feature.
//!
//! Why: the `aws-sdk-bedrockruntime` SDK adds ~10 MB of generated code that
//! users who don't need Bedrock should not pay for. Gating it behind the
//! `bedrock` feature and providing this stub keeps the default binary lean
//! while still allowing callers to reference `BedrockProvider` in code paths
//! that are never reached without the feature.
//! What: exposes the same `BedrockProvider::new` and `chat_stream` signatures
//! as the real implementation, but every method returns a clear error
//! instructing the operator to rebuild with `--features bedrock`.
//! Test: `bedrock_stub_returns_error_without_feature` (no-feature test, no
//! network required).

use super::{ChatEvent, ChatProvider, ToolDef};
use crate::ChatMessage;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use tokio::sync::mpsc::Sender;

/// Stub Bedrock provider returned when the `bedrock` feature is disabled.
///
/// Why: keeps call sites that construct `BedrockProvider` compilable even
/// without the feature, while surfacing an explicit build-guidance error at
/// runtime so operators understand the configuration required.
/// What: every method returns `Err("bedrock feature not compiled in ...")`.
/// Test: `bedrock_stub_returns_error_without_feature`.
#[derive(Debug)]
pub struct BedrockProvider {
    model: String,
}

impl BedrockProvider {
    /// Always returns `Err` with a build-instruction message.
    ///
    /// Why: surfacing the missing-feature condition as an error helps
    /// operators diagnose deployments that attempt to use Bedrock without
    /// enabling the feature flag.
    /// What: returns a clear error with rebuild instructions.
    /// Test: `bedrock_stub_returns_error_without_feature`.
    pub async fn new(model: impl Into<String>, _region: Option<&str>) -> Result<Self> {
        let _ = model.into();
        Err(anyhow!(
            "bedrock feature not compiled in — rebuild with \
             --features bedrock (or enable the 'bedrock' feature in Cargo.toml)"
        ))
    }
}

#[async_trait]
impl ChatProvider for BedrockProvider {
    fn name(&self) -> &str {
        "bedrock-stub"
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn chat_stream(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<ToolDef>,
        _tx: Sender<ChatEvent>,
    ) -> Result<()> {
        Err(anyhow!(
            "bedrock feature not compiled in — rebuild with \
             --features bedrock (or enable the 'bedrock' feature in Cargo.toml)"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without the `bedrock` feature, [`BedrockProvider::new`] must error
    /// with the build-instruction message.
    ///
    /// Why: the message is the operator-facing handle for understanding why a
    /// `bedrock/` model prefix was rejected — drift would break runbooks.
    /// What: calls `BedrockProvider::new` and asserts the error message.
    /// Test: no network; stub path only.
    #[tokio::test]
    async fn bedrock_stub_returns_error_without_feature() {
        let result = BedrockProvider::new("some-model-id", None).await;
        let err = result.expect_err("must error without feature");
        assert!(
            err.to_string().contains("bedrock feature not compiled in"),
            "wrong error: {err}"
        );
    }
}
