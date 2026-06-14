//! Chat HTTP surface for trusty-memory: OpenRouter/Ollama SSE chat, tool
//! dispatch, chat-session CRUD, and inter-project messaging endpoints.
//!
//! Why: Extracted from `web.rs` to keep the HTTP router thin and isolate the
//! tool-calling loop (which is by far the largest single concern in this
//! crate's HTTP surface) behind its own module. The router still owns wiring,
//! but the chat-specific request/response handlers, the tool dispatcher, and
//! the inter-project messaging handlers all live here.
//! What: Re-exports `chat_handler`, provider/session handlers, the
//! `execute_*` dispatcher set, and the `/api/v1/messages*` handlers. Items
//! kept `pub(crate)` so `web::router()` and the cross-palace recall handler
//! can reference them without enlarging the public crate surface.
//! Test: Behaviour is covered by `web::tests::all_tools_returns_expected_set`
//! and `web::tests::execute_tool_dispatches_known_tools`, which still call
//! into this module via the `pub(crate)` re-exports.

pub mod handler;
pub mod messaging;
pub mod sessions;
pub mod tools;

// Re-export the `pub(crate)` HTTP surface so `web::router()` and the
// cross-palace recall handler keep referencing `crate::chat::X` exactly as
// they did against the former monolithic module.
pub(crate) use handler::chat_handler;
pub(crate) use messaging::{
    list_messages_handler, mark_message_read_handler, send_message_handler,
};
pub(crate) use sessions::{
    create_chat_session, delete_chat_session, get_chat_session, list_chat_sessions,
    list_providers,
};
#[cfg(test)]
pub(crate) use tools::{all_tools, execute_tool};
