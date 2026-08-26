//! The chat surface: a streaming completion and the tool loop behind it.
//!
//! Why: the tool-calling loop is by far the largest single concern in this
//! crate's caller-facing surface, so it sits behind its own module boundary
//! (issue #607). #6286 shrank the module to those two things: the chat-session
//! CRUD handlers and the three messaging handlers were axum routes duplicating
//! names `transport::rpc::dispatch` already routed — `chat_session_create`,
//! `_list`, `_get`, `_delete` — or folded into
//! `transport::methods::chat`, so neither has a second implementation here any
//! more.
//!
//! What: [`handler::chat_stream`], registered as the streaming `memory.chat`
//! method, and the `tools` submodule it drives.
//! Test: `crate::transport::uds::tests` — `rpc_chat_*`.

pub mod handler;
pub mod tools;

pub use handler::chat_stream;
pub use tools::ChatBody;
