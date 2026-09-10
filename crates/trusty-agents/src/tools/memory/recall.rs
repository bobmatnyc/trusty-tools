//! Explicit memory uses trusty-memory after the host binds an assistant (#7360).
use crate::tools::{ToolExecutor, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};
pub(super) const EMBED_DIM: usize = 384;
pub(super) const HIT_MAX_CHARS: usize = 600;
#[derive(Default)]
pub struct MemoryRecallTool;
impl MemoryRecallTool {
    pub fn new() -> Self {
        Self
    }
}
#[async_trait]
impl ToolExecutor for MemoryRecallTool {
    fn name(&self) -> &str {
        "memory_recall"
    }
    fn scope(&self) -> Option<&str> {
        Some("memory.read")
    }
    fn schema(&self) -> Value {
        json!({"type":"function","function":{"name":self.name(),"description":"Recall durable facts from this assistant's trusty-memory namespace. Requires an assistant binding.","parameters":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}}})
    }
    async fn execute(&self, _: Value) -> ToolResult {
        ToolResult::err(
            "Memory requires a configured assistant namespace. Ask Concierge to check this assistant's memory settings.",
        )
    }
}
