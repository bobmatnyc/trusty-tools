//! Telegram Bot API client layer.
//!
//! Why: Keep the pure HTTP/auth concerns isolated from MCP framing so the same
//! client can be reused outside of MCP (CLI tools, tests, a future daemon),
//! matching the `slack::api` split.
//! What: Re-exports the endpoint constants, the typed [`error::TelegramError`],
//! and the `BaseClient` HTTP wrapper.
//! Test: Constants are smoke-tested in `constants`; the client constructor and
//! request path are tested in `client` + `tests/telegram_client_http.rs`.

pub mod client;
pub mod constants;
pub mod error;
