//! Driver trait abstracting the transport behind an OpenRPC endpoint (#453).
//!
//! Why: We need to support at least two transports — HTTP JSON-RPC 2.0
//! (`direct`) and stdio-MCP (`stdio-mcp`) — without leaking transport
//! details into the `ToolExecutor` adapter. A trait lets the registry hold
//! one `Arc<dyn RegistryDriver>` per endpoint and dispatch tool calls
//! uniformly.
//! What: `RegistryDriver` exposes `discover`, `call_tool`, and a default
//! `call_tool_batch` (parallel individual calls — drivers with native
//! batch support like the `DirectDriver` override it to use the JSON-RPC
//! 2.0 Batch envelope).
//! Test: Default `call_tool_batch` exercised in the registry mod tests via
//! a mock driver.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use futures::future::join_all;

use super::discovery::{EndpointCapabilities, EndpointManifest};

/// One element of a batched call request.
#[derive(Debug, Clone)]
pub struct BatchCall {
    pub id: String,
    pub name: String,
    pub args: serde_json::Value,
}

/// One element of a batched call response.
#[derive(Debug)]
pub struct BatchResult {
    pub id: String,
    pub result: Result<serde_json::Value>,
}

#[async_trait]
pub trait RegistryDriver: Send + Sync {
    fn endpoint_name(&self) -> &str;

    async fn discover(&self) -> Result<EndpointManifest>;

    async fn call_tool(&self, name: &str, args: serde_json::Value) -> Result<serde_json::Value>;

    /// Default: parallel individual calls. Drivers that support a native
    /// batch protocol (e.g. JSON-RPC 2.0 Batch) should override this.
    async fn call_tool_batch(&self, calls: Vec<BatchCall>) -> Vec<BatchResult> {
        let futures = calls.into_iter().map(|c| async move {
            let result = self.call_tool(&c.name, c.args).await;
            BatchResult { id: c.id, result }
        });
        join_all(futures).await
    }

    fn capabilities(&self) -> &EndpointCapabilities;
}

pub type ArcDriver = Arc<dyn RegistryDriver>;
