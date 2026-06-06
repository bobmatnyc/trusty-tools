//! `ToolExecutor` adapter wrapping a registry-discovered tool (#453).
//!
//! Why: The harness already dispatches via `Arc<dyn ToolExecutor>`. Wrapping
//! each `DiscoveredTool` in an adapter that owns an `Arc<dyn RegistryDriver>`
//! lets us register OpenRPC tools alongside in-process tools (git_tools,
//! native_ticketing, etc.) without changing the registry trait.
//! What: `RegistryToolExecutor` translates a `tools/call` into a driver
//! invocation and renders the result as text for the LLM.
//! Test: Round-trip name/schema/execute mocking via `MockDriver` in the
//! parent `mod.rs`.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::discovery::DiscoveredTool;
use super::driver::ArcDriver;
use crate::tools::traits::{ToolExecutor, ToolResult};

pub struct RegistryToolExecutor {
    tool: DiscoveredTool,
    endpoint_name: String,
    driver: ArcDriver,
}

impl RegistryToolExecutor {
    pub fn new(tool: DiscoveredTool, endpoint_name: String, driver: ArcDriver) -> Self {
        Self {
            tool,
            endpoint_name,
            driver,
        }
    }

    pub fn endpoint_name(&self) -> &str {
        &self.endpoint_name
    }
}

#[async_trait]
impl ToolExecutor for RegistryToolExecutor {
    fn name(&self) -> &str {
        &self.tool.name
    }

    fn schema(&self) -> Value {
        // OpenAI function-calling shape. Description gets a small suffix so
        // the LLM and an operator reading logs can tell which endpoint
        // serves the tool.
        let description = match &self.tool.description {
            Some(d) => format!("{d} (via {})", self.endpoint_name),
            None => format!("(via {})", self.endpoint_name),
        };
        json!({
            "type": "function",
            "function": {
                "name": self.tool.name,
                "description": description,
                "parameters": self.tool.input_schema,
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        match self.driver.call_tool(&self.tool.name, args).await {
            Ok(v) => {
                // Prefer raw text payloads; fall back to JSON for structured.
                if let Some(s) = v.as_str() {
                    ToolResult::ok(s.to_string())
                } else {
                    ToolResult::ok(v.to_string())
                }
            }
            Err(e) => ToolResult::err(format!(
                "tool '{}' (endpoint '{}') failed: {e}",
                self.tool.name, self.endpoint_name
            )),
        }
    }
}
