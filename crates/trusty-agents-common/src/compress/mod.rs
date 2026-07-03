//! Portable tool-output compression surface, shared across harnesses.
//!
//! Why: `trusty-agents`'s own LLM tool loop and `trusty-mpm`'s `tm compress`
//! subcommand (Option 0 spike, issue #1956) both need
//! `compress_tool_output_async` but must not pull in the full `trusty-agents`
//! binary crate to get it. Hoisted here in issue #1959, mirroring the
//! `OutputStyle`-style hoists already established for this crate (Wave 1/2,
//! issues #862/#867).
//! What: Re-exports the `tool_output` dispatch/filter module. `trusty-agents`
//! re-exports the same symbols via `trusty_agents::compress` for source-level
//! compatibility with existing call sites.
//! Test: `cargo test -p trusty-agents-common` runs `tool_output::tests` in
//! place; `cargo test -p trusty-agents` confirms the re-export still compiles
//! and passes for the `tool_loop` call site.

pub mod tool_output;

pub use tool_output::{
    CompressionPath, compress_tool_output, compress_tool_output_async,
    compress_tool_output_async_with_path,
};
