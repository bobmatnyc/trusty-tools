//! Compatibility recall alias; the assistant registry binds it to trusty-memory (#7360).
use crate::tools::{ToolExecutor, ToolResult};
use async_trait::async_trait;
use serde_json::Value;
#[derive(Default)]
pub struct SearchMemoryTool;
impl SearchMemoryTool {
    pub fn new() -> Self {
        Self
    }
}
#[async_trait]
impl ToolExecutor for SearchMemoryTool {
    fn name(&self) -> &str {
        "search_memory"
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
