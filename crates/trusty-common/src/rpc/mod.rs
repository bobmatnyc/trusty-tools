//! General-purpose JSON-RPC client + transports.
//!
//! Why: Formerly the library half of the standalone `trusty-rpc` crate.
//! Consolidating into `trusty-common` lets both the `trpc` CLI and any
//! future library consumer (trusty-agents, tests, ops scripts) share one
//! implementation of JSON-RPC envelope construction, stdio-subprocess
//! transport, HTTP transport, and pretty-printers — instead of carrying
//! a separate crate for what amounts to ~700 lines of glue code.
//! What: Re-exports the public surface of the `client`, `output`, and
//! `transport` submodules. The `Transport` trait + `HttpTransport` +
//! `StdioTransport` come from `transport`; `RpcClient`, `new_id`, and
//! `extract_result` from `client`; pretty-printers from `output`.
//! Test: each submodule carries its own unit tests; integration tests
//! continue to live in the `trusty-rpc` crate which now depends on
//! `trusty-common` with the `rpc` feature.

pub mod client;
pub mod output;
pub mod transport;

pub use client::{RpcClient, extract_result, new_id};
pub use output::{print_json, print_server_info, print_tool_result, print_tools_list};
pub use transport::{HttpTransport, StdioTransport, Transport};
