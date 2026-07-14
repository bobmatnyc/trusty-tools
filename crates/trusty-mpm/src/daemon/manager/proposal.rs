//! The chat loop's in-conversation propose→confirm plumbing (WI-9, #2586).
//!
//! Why: DOC-36 §6 phase 2 requires the CHAT LOOP itself (not just `/manager/act`)
//! to "propose a session launch/inject action in-conversation and only execute it
//! after explicit user confirmation" — a conversational realization of DOC-35 §11
//! that reuses the SAME [`super::act::ProposedAction`]/actuator seam `/manager/act`
//! uses, never a parallel implementation. This module owns the three pure/small
//! pieces that make that possible: (1) [`extract_proposed_action`] — parses an
//! LLM reply for an embedded action proposal (a fenced `manager-action` JSON
//! block, never real tool-calling — the request still carries no `tools`, so the
//! phase-1 "no tool-calling surface" invariant holds even though the chat loop is
//! no longer read-only); (2) [`is_confirmation`] — the deterministic, documented
//! confirmation-phrase policy; (3) [`ProposalStore`] — the conversation-keyed
//! pending-proposal store with a NEXT-TURN-ONLY expiry policy (chosen over a
//! wall-clock TTL because chat turns are the natural unit of "the same
//! conversation", and a turn-based expiry is exactly testable without a clock).
//! What: [`ProposalStore::set`]/[`ProposalStore::take`] (unconditional
//! consume-on-read — every turn either executes the pending proposal or drops
//! it, so a proposal NEVER survives past the very next turn on its key);
//! [`is_confirmation`]; [`extract_proposed_action`] plus the sentinel constants
//! [`ACTION_FENCE_OPEN`]/[`ACTION_FENCE_CLOSE`] the chat system prompt documents
//! to the model.
//! Test: `is_confirmation_*`, `extract_proposed_action_*`,
//! `proposal_store_take_is_consume_on_read` in `proposal_tests.rs`.

use std::collections::HashMap;
use std::sync::Mutex;

use super::act::ProposedAction;

/// Opening fence the chat system prompt instructs the model to use when
/// proposing an action.
///
/// Why: a fixed, greppable sentinel lets [`extract_proposed_action`] find a
/// proposal in plain LLM reply text WITHOUT wiring an actual tool-calling
/// `tools` array onto the request — the phase-1 "no tools" invariant
/// (`tests/manager_inference.rs::chat_is_read_only_no_mutation_and_no_tools`)
/// keeps holding even though the chat loop can now act.
/// What: the fenced-code-block language tag marking a proposal.
/// Test: `extract_proposed_action_parses_launch_block`.
pub const ACTION_FENCE_OPEN: &str = "```manager-action";

/// Closing fence for the proposal block (a bare triple-backtick).
pub const ACTION_FENCE_CLOSE: &str = "```";

/// Parse an LLM reply for an embedded action proposal, stripping it from the
/// visible text.
///
/// Why: the chat system prompt (`chat.rs::build_chat_messages`) instructs the
/// model to end its reply with a `manager-action` fenced JSON block — shaped
/// exactly like [`ProposedAction`]'s serde tag — ONLY when the user explicitly
/// asked for a launch/inject/summarize. This is the single parse point so the
/// handler never hand-rolls JSON extraction inline.
/// What: finds [`ACTION_FENCE_OPEN`]; if absent, returns `(reply, None)`
/// unchanged. If present but the fenced body fails to parse as a
/// [`ProposedAction`], ALSO returns `(reply, None)` unchanged — a malformed
/// proposal is treated as "no proposal" rather than a hard error, since the
/// chat loop must never fail a turn over the model's formatting slip. On a
/// successful parse, returns the reply with the fenced block (and its
/// surrounding whitespace) removed, plus `Some(action)`.
/// Test: `extract_proposed_action_parses_launch_block`,
/// `extract_proposed_action_no_block_returns_none`,
/// `extract_proposed_action_malformed_json_returns_none`,
/// `extract_proposed_action_keeps_leading_and_trailing_prose`.
pub fn extract_proposed_action(reply: &str) -> (String, Option<ProposedAction>) {
    let Some(start) = reply.find(ACTION_FENCE_OPEN) else {
        return (reply.to_string(), None);
    };
    let after_open = start + ACTION_FENCE_OPEN.len();
    let Some(rel_end) = reply[after_open..].find(ACTION_FENCE_CLOSE) else {
        return (reply.to_string(), None);
    };
    let json_str = reply[after_open..after_open + rel_end].trim();
    let Ok(action) = serde_json::from_str::<ProposedAction>(json_str) else {
        return (reply.to_string(), None);
    };

    let end = after_open + rel_end + ACTION_FENCE_CLOSE.len();
    let mut remaining = reply[..start].trim().to_string();
    let trailing = reply[end..].trim();
    if !trailing.is_empty() {
        if !remaining.is_empty() {
            remaining.push('\n');
        }
        remaining.push_str(trailing);
    }
    (remaining, Some(action))
}

/// Whether a chat message is an explicit confirmation of a pending proposal.
///
/// Why: DOC-35 §11 requires an EXPLICIT confirmation — this must be a narrow,
/// deterministic, documented predicate (never fuzzy NLU) so the confirm path is
/// hermetically testable and never accidentally triggered by an unrelated
/// message that happens to contain the word "confirm" mid-sentence.
/// What: trims the message, strips one trailing `.`/`!`/`?`, lowercases, and
/// matches the WHOLE remaining string against a fixed confirmation-phrase set:
/// `"confirm"`, `"confirmed"`, `"yes"`, `"y"`. Any other content — including a
/// message that merely mentions "confirm" — is NOT a confirmation.
/// Test: `is_confirmation_accepts_documented_phrases`,
/// `is_confirmation_rejects_partial_or_unrelated_text`.
pub fn is_confirmation(message: &str) -> bool {
    let cleaned = message
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .trim()
        .to_lowercase();
    matches!(cleaned.as_str(), "confirm" | "confirmed" | "yes" | "y")
}

/// Conversation-keyed store of pending action proposals, NEXT-TURN-ONLY TTL.
///
/// Why: DOC-35 §11 forbids a proposal from being executable indefinitely — an
/// operator who proposed an action three turns ago and moved on to an unrelated
/// topic must not have a stray later "confirm" retroactively fire it. Modelling
/// the expiry as "valid for exactly the next turn on this key" (rather than a
/// wall-clock TTL) ties the policy to the natural unit of a conversation and
/// needs no clock/timer machinery to test: [`Self::take`] unconditionally
/// REMOVES the entry on every read, so the caller (chat.rs) either executes it
/// (this turn's message was a confirmation) or discards it (any other message) —
/// either way it cannot survive to a THIRD turn.
/// What: a `Mutex`-guarded `HashMap<String, ProposedAction>`, keyed identically
/// to [`super::chat_store::ChatStore`] and the L2 proxy focus map.
/// Test: `proposal_store_take_is_consume_on_read`,
/// `proposal_store_distinct_keys_are_isolated`.
#[derive(Debug, Default)]
pub struct ProposalStore {
    pending: Mutex<HashMap<String, ProposedAction>>,
}

impl ProposalStore {
    /// Construct an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a pending proposal for `key`, replacing any existing one.
    ///
    /// Why: a new proposal on a conversation supersedes whatever was pending
    /// (there is at most one live proposal per conversation at a time).
    /// What: inserts `action` under `key`.
    /// Test: `proposal_store_take_is_consume_on_read`.
    pub fn set(&self, key: &str, action: ProposedAction) {
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key.to_string(), action);
    }

    /// Consume and return the pending proposal for `key`, if any.
    ///
    /// Why: the sole read path — ALWAYS removes on read (the next-turn-only TTL),
    /// so calling this is itself the expiry mechanism. The caller decides whether
    /// the returned action executes (this turn confirmed it) or is simply
    /// discarded (any other message).
    /// What: removes and returns the entry under `key`, or `None`.
    /// Test: `proposal_store_take_is_consume_on_read`,
    /// `proposal_store_distinct_keys_are_isolated`.
    pub fn take(&self, key: &str) -> Option<ProposedAction> {
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(key)
    }
}

#[cfg(test)]
#[path = "proposal_tests.rs"]
mod tests;
