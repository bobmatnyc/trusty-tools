//! Slack Web API client layer.
//!
//! Why: Keep the pure HTTP/auth concerns isolated from MCP framing so the same
//! client can be reused outside of MCP (CLI tools, tests, a future REST
//! daemon), matching the `trusty-gworkspace` split.
//! What: Re-exports the endpoint constants and the `BaseClient` HTTP wrapper.
//! Test: Constants are smoke-tested in `constants`; the client constructor is
//! smoke-tested in `client`.

pub mod client;
pub mod constants;
