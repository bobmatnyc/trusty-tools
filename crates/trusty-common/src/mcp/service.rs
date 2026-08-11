//! Cross-service registration contract for unified OpenRPC documents.
//!
//! Why: The trusty-* ecosystem is moving toward a host process (trusty-agents /
//! a unified daemon) that links several MCP services into one binary and
//! exposes a single `rpc.discover` document covering all of them. Each
//! linked service needs a uniform way to advertise its tools, version, and
//! per-tool scope requirements without the host having to know about each
//! service concretely. A trait object solves this: services register an
//! impl, the host collects `&dyn ServiceDescriptor` and feeds them to
//! `OpenRpcBuilder::from_services` to emit one merged manifest.
//!
//! What: A single trait, `ServiceDescriptor`, with four methods covering
//! identity (`name`, `version`), the tool list (MCP-shape JSON), and a
//! per-tool scope lookup. Trait objects are `Send + Sync` so the host can
//! collect them into a `Vec<Box<dyn ServiceDescriptor>>` and share across
//! tasks.
//!
//! Test: `service_descriptor_trait_object_dispatches` in `lib.rs` tests
//! verifies trait object dispatch and tool enumeration through a mock impl.

/// Why: single registration contract all services implement to contribute
///      tools to the unified OpenRPC document in the host process.
/// What: trait objects collected at startup, passed to
///       `OpenRpcBuilder::from_services`.
/// Test: tests in `lib.rs` verify trait object dispatch and tool enumeration.
pub trait ServiceDescriptor: Send + Sync {
    /// Stable service identifier (e.g. `"trusty-memory"`); also emitted as
    /// the `x-service` extension on every method it contributes.
    fn name(&self) -> &str;

    /// Service version string (semver recommended); surfaced for diagnostic
    /// tooling but not currently merged into the top-level OpenRPC `info`.
    fn version(&self) -> &str;

    /// MCP-style tool definitions: each is a JSON object with `name`,
    /// `description`, and `inputSchema`. The host converts these into
    /// OpenRPC methods via the existing `OpenRpcBuilder::add_tool` path.
    fn tools(&self) -> Vec<serde_json::Value>;

    /// Scope tags for a named tool (e.g. `["memory.read"]`). An empty vec
    /// means no scope restriction. Called once per tool when building the
    /// merged manifest, so impls can use a simple match or lookup table.
    fn scopes_for(&self, tool: &str) -> Vec<String>;
}
