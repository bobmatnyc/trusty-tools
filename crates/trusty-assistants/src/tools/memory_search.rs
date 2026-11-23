//! Legacy memory_search alias for trusty-memory; no local history fallback (#7360).
use crate::tools::{ToolExecutor, ToolResult};
use async_trait::async_trait;
use serde_json::Value;
pub struct MemorySearchTool;
impl MemorySearchTool {
    pub fn from_env() -> Self {
        Self
    }
}
#[async_trait]
impl ToolExecutor for MemorySearchTool {
    fn name(&self) -> &str {
        "memory_search"
    }
    fn scope(&self) -> Option<&str> {
        Some("memory.read")
    }
    fn schema(&self) -> Value {
        let mut s = crate::tools::memory::MemoryRecallTool::new().schema();
        s["function"]["name"] = self.name().into();
        s
    }
    async fn execute(&self, args: Value) -> ToolResult {
        crate::tools::memory::MemoryRecallTool::new()
            .execute(args)
            .await
    }
}
