//! Pairing state machine + REPL-issued code handling for the Slack gateway.
//!
//! Why: Pairing codes are generated in the trusted REPL and validated on
//! Slack, mirroring the Telegram adapter. Keeping the state machine pure and
//! isolated makes it exhaustively unit-testable without a WebSocket.
//! What: `PendingPairs` map type + sentinel key, code issuance/generation, and
//! the `PairOutcome` state machine (`verify_pair_attempt`).
//! Test: `pair_*`, `repl_issued_code_*`, `sentinel_*` in `slack::tests`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

/// How long a pairing code remains valid after issuance.
///
/// Why: Bound the window where a leaked code from REPL logs could be used by
/// another Slack channel. 5 minutes mirrors the Telegram adapter.
pub(super) const PAIRING_CODE_TTL: Duration = Duration::from_secs(5 * 60);

/// Map of pending pairing codes keyed by raw `i64` channel id.
///
/// Why: Pairing codes are generated **in the REPL** (trusted terminal),
/// stored under the sentinel key `SENTINEL_PAIRING_CHANNEL_ID = i64::MAX`.
/// When `/slack-pair <code>` arrives from Slack, we look up the sentinel
/// entry; on a match the channel is promoted to paired. An attacker who
/// owns the Slack bot cannot self-authorize — they'd also need shell
/// access to the host running the REPL.
/// What: `Arc<Mutex<HashMap<i64, (String, Instant)>>>`. The `i64` keeps the
/// REPL free of slack-adapter-specific types and reuses the Telegram API
/// shape exactly so the REPL doesn't have to learn a second pairing API.
pub type PendingPairs = Arc<Mutex<HashMap<i64, (String, Instant)>>>;

/// Sentinel channel-id under which the REPL stores the next pending code.
///
/// Why: A real Slack channel id is a string ("C0123ABC..."), never an
/// integer. We use `i64::MAX` as an out-of-band integer key so the REPL
/// pairing API stays uniform across Telegram + Slack.
pub const SENTINEL_PAIRING_CHANNEL_ID: i64 = i64::MAX;

/// Construct a fresh, empty `PendingPairs` shared across REPL + bot task.
pub fn new_pending_pairs() -> PendingPairs {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Generate and store a REPL-issued pairing code under the sentinel key.
///
/// Why: Called from a future `/slack pair` command in the REPL. The next
/// `/slack-pair <code>` arriving on Slack (from any channel) can claim it.
/// What: Returns the 6-digit code so the REPL can display it.
/// Test: `repl_issued_code_lands_under_sentinel` exercises the flow.
pub async fn issue_repl_pairing_code(pending: &PendingPairs) -> String {
    let code = generate_pairing_code();
    let mut map = pending.lock().await;
    map.insert(SENTINEL_PAIRING_CHANNEL_ID, (code.clone(), Instant::now()));
    code
}

/// Generate a random 6-digit pairing code (zero-padded).
///
/// Why: 6 digits = ~1M codes; plenty for human handoff via a log line, short
/// enough to type easily.
/// What: Uses `rand::random::<u32>() % 1_000_000` and zero-pads with `{:06}`.
/// Test: `pairing_code_is_six_digits` asserts the format.
pub(super) fn generate_pairing_code() -> String {
    format!("{:06}", rand::random::<u32>() % 1_000_000)
}

/// Outcome of a `/slack-pair <code>` attempt. Pure for unit testing.
///
/// Why: We want to unit-test the state-machine without WebSocket types in
/// the loop. `verify_pair_attempt` returns one of these and the handler
/// turns it into Slack replies + map mutations.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PairOutcome {
    /// No pending code registered.
    NoPending,
    /// The pending code is past its TTL.
    Expired,
    /// The provided code does not match the pending code.
    Mismatch,
    /// The provided code matches and is within TTL — caller must promote
    /// the channel to paired.
    Success,
}

/// Verify a pairing attempt against a pending entry.
///
/// Why: Pure function so we can exhaustively test without spinning up Slack.
/// The caller is responsible for the side effects (removing the pending
/// entry, inserting into paired, posting the reply).
pub(super) fn verify_pair_attempt(
    pending_entry: Option<&(String, Instant)>,
    provided_code: &str,
    now: Instant,
    ttl: Duration,
) -> PairOutcome {
    match pending_entry {
        None => PairOutcome::NoPending,
        Some((code, issued_at)) => {
            if now.saturating_duration_since(*issued_at) > ttl {
                PairOutcome::Expired
            } else if code != provided_code {
                PairOutcome::Mismatch
            } else {
                PairOutcome::Success
            }
        }
    }
}
