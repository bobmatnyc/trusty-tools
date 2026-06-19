//! Pairing-handshake command methods.
//!
//! Why: the bot-pairing flow (`/pair`, `tm pair`) does not fit the pure
//! `TrustyCommand → CommandResult` shape — `pair_confirm` needs the confirming
//! chat's id, which the command does not carry. Keeping the pairing methods in
//! their own file isolates that asymmetry and keeps the dispatch facade lean.
//! What: inherent `CommandExecutor` methods for the three pairing operations —
//! [`pair_confirm`](CommandExecutor::pair_confirm) (chat-id-carrying confirm),
//! [`pair_request`](CommandExecutor::pair_request) (`tm pair` code request), and
//! the internal [`pair_state`](CommandExecutor::pair_state) status query that
//! `/start` and `/pair` (no code) dispatch to.
//! Test: the `pair_*` tests in `tests.rs`.

use super::CommandExecutor;
use crate::client::result::CommandResult;

impl CommandExecutor {
    /// Confirm a pairing code on behalf of a specific chat.
    ///
    /// Why: `POST /pair/confirm` needs the confirming chat's id, which is not
    /// carried by [`crate::TrustyCommand::Pair`]; the bot adapter supplies it here.
    /// What: calls the daemon's confirm endpoint and maps the result to
    /// [`CommandResult::PairSuccess`] or [`CommandResult::Error`].
    /// Test: `pair_confirm_unknown_code_errors`.
    pub async fn pair_confirm(&self, code: &str, chat_id: i64) -> CommandResult {
        match self.client().pair_confirm(code, chat_id).await {
            Ok(confirm) if confirm.success => CommandResult::PairSuccess {
                chat_info: format!("chat {}", confirm.chat_id.unwrap_or(chat_id)),
            },
            Ok(confirm) => CommandResult::Error(
                confirm
                    .error
                    .unwrap_or_else(|| "invalid or expired code".to_string()),
            ),
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// Request a one-time pairing code from the daemon.
    ///
    /// Why: `tm pair` asks the local daemon for a code to display.
    /// What: calls `POST /pair/request` and maps it to [`CommandResult::PairCode`].
    /// Test: `pair_request_returns_code`.
    pub async fn pair_request(&self) -> CommandResult {
        match self.client().pair_request().await {
            Ok(req) => CommandResult::PairCode {
                code: req.code,
                expires_in_seconds: req.expires_in_seconds,
            },
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }

    /// `/start` and `/pair` (no code) — query the pairing status.
    pub(super) async fn pair_state(&self) -> CommandResult {
        match self.client().pair_status().await {
            Ok(status) => CommandResult::PairState {
                paired: status.paired,
            },
            Err(e) => CommandResult::Error(format!("daemon unreachable: {e}")),
        }
    }
}
