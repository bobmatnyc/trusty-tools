//! Pairing state machine, persistence, and REPL-issued code handling for the
//! Slack gateway.
//!
//! Why: Pairing codes are generated in the trusted REPL and validated on
//! Slack, mirroring the Telegram adapter. Keeping the state machine pure and
//! isolated makes it exhaustively unit-testable without a WebSocket.
//! What: `PendingPairs` map type + sentinel key, code issuance/generation,
//! the `PairOutcome` state machine (`verify_pair_attempt`), the
//! `PairedChannels` persistence pair (#4853), and the headless auto-pair
//! decision (#4854).
//! Test: `pair_*`, `repl_issued_code_*`, `sentinel_*`,
//! `slack_paired_state_*`, `auto_pair_*` in `slack::tests`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};

use super::{ChannelId, PairedChannels};

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

/// On-disk record of a single paired channel.
///
/// Why (#4853): `PairedChannels` lived in memory only; `run_slack_bot` built a
/// fresh empty map on every call, so every restart of the launchd-run gateway
/// un-paired every channel. Persisting a minimal record under
/// `~/.trusty-agents/state/` survives restarts without leaking message content.
/// What: the Slack channel id (a string like `"D0A1B2C3"`, unlike Telegram's
/// `i64`) plus the wall-clock pairing time. `Instant` is monotonic and
/// unsuitable for persistence, so we store `DateTime<Utc>` and reconstruct
/// `Instant::now()` on load — the absolute time is only used for diagnostics.
/// Test: `slack_paired_state_round_trip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PairedChannelRecord {
    channel_id: String,
    paired_at: DateTime<Utc>,
}

/// On-disk container for the paired-channels file.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PairedChannelsFile {
    paired_channels: Vec<PairedChannelRecord>,
}

/// Resolve the absolute path of the paired-channels state file.
///
/// Why (#4853): Mirrors `telegram::pairing::paired_chats_state_path` (#467) —
/// the *user-level* `~/.trusty-agents/state/` directory shared across projects,
/// NOT the project-local one, so a gateway started from any cwd sees the same
/// pairings. Falls back to a relative path when `HOME` is unset so we never
/// panic in odd sandboxes.
pub(super) fn paired_channels_state_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".trusty-agents")
        .join("state")
        .join("slack-paired.json")
}

/// Load persisted paired channels from disk.
///
/// Why (#4853): On startup, restore the pairing map so a service restart does
/// not force every user to re-pair.
/// What: Reads `state_path` as JSON. Missing file -> empty map (first run).
/// Parse errors -> warn and return an empty map, never panic: a corrupt state
/// file must not stop the gateway from booting, and the worst case is that
/// users re-pair.
/// Test: `slack_paired_state_round_trip`, `slack_paired_state_missing_file_is_empty`.
pub(super) async fn load_paired_channels(state_path: &Path) -> PairedChannels {
    let bytes = match tokio::fs::read(state_path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Arc::new(RwLock::new(HashMap::new()));
        }
        Err(e) => {
            warn!(path = %state_path.display(), error = %e, "failed to read paired-channels state; starting empty");
            return Arc::new(RwLock::new(HashMap::new()));
        }
    };
    let parsed: PairedChannelsFile = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            warn!(path = %state_path.display(), error = %e, "failed to parse paired-channels state; starting empty");
            return Arc::new(RwLock::new(HashMap::new()));
        }
    };
    let now = Instant::now();
    let mut map: HashMap<ChannelId, Instant> = HashMap::with_capacity(parsed.paired_channels.len());
    for rec in parsed.paired_channels {
        // `Instant` cannot represent a past wall-clock time; `now` is a
        // stand-in consumed only by diagnostic logging.
        map.insert(rec.channel_id, now);
    }
    info!(count = map.len(), path = %state_path.display(), "loaded paired slack channels");
    Arc::new(RwLock::new(map))
}

/// Persist the paired-channels map to disk atomically.
///
/// Why (#4853): Makes a successful pair durable across restarts.
/// What: Snapshots the map under a read lock, serializes to JSON, then writes
/// `<path>.tmp` + `rename` so a crash mid-write can never leave a torn file.
/// Creates the parent directory on demand. Callers intentionally log-and-
/// continue on error: losing persistence is recoverable on the next save and
/// must never block the user's reply.
/// Test: `slack_paired_state_round_trip`.
pub(super) async fn save_paired_channels(paired: &PairedChannels, state_path: &Path) -> Result<()> {
    if let Some(parent) = state_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            anyhow!(
                "failed to create paired-channels state dir {}: {e}",
                parent.display()
            )
        })?;
    }
    let snapshot: Vec<PairedChannelRecord> = {
        let guard = paired.read().await;
        guard
            .keys()
            .map(|cid| PairedChannelRecord {
                channel_id: cid.clone(),
                paired_at: Utc::now(),
            })
            .collect()
    };
    let file = PairedChannelsFile {
        paired_channels: snapshot,
    };
    let json = serde_json::to_vec_pretty(&file)
        .map_err(|e| anyhow!("failed to serialize paired-channels: {e}"))?;
    let tmp_path = state_path.with_extension("json.tmp");
    tokio::fs::write(&tmp_path, &json).await.map_err(|e| {
        anyhow!(
            "failed to write paired-channels tmp file {}: {e}",
            tmp_path.display()
        )
    })?;
    tokio::fs::rename(&tmp_path, state_path)
        .await
        .map_err(|e| {
            anyhow!(
                "failed to rename paired-channels tmp -> {}: {e}",
                state_path.display()
            )
        })?;
    Ok(())
}

/// Is `channel` a 1:1 direct message with the bot?
///
/// Why (#4854): Slack channel ids are prefixed by kind — `D` for an IM, `C`
/// for a public channel, `G` for a private channel / group DM. The prefix is
/// assigned by Slack and arrives over the authenticated Socket Mode socket, so
/// it is not attacker-controlled content. We key on it rather than the
/// message event's `channel_type` field because `channel_type` exists only on
/// message events — slash-command payloads do not carry it, and both inbound
/// paths need the same answer.
/// What: Returns true iff the id starts with `D`.
/// Test: `auto_pair_rejects_public_channel`, `auto_pair_rejects_private_group`.
pub(super) fn is_dm_channel(channel: &str) -> bool {
    channel.starts_with('D')
}

/// Decide whether an unpaired channel may pair itself headlessly (#4854).
///
/// Why: Standalone `--slack` mode has no REPL, so no pairing code can ever be
/// minted and the gate is unpassable. This is the narrowest rule that reopens
/// it without weakening the security boundary: a DM from a user already in the
/// RBAC table is auto-paired, because in that exact case pairing protects
/// nothing that RBAC does not already protect more strongly.
///
/// The argument, precisely: persona access is gated per-USER by
/// `SlackRbacConfig::user` — an unknown user already gets the static
/// `VIRTUAL_CTO_MESSAGE` with no LLM call and no tool dispatch. Pairing is a
/// per-CHANNEL gate whose remaining job is to control *where* an authorized
/// user's (possibly sensitive) assistant output can land. A DM channel is
/// readable only by that one user and the bot, so for a known user it grants
/// exactly the tier they already hold, in a room nobody else can read.
/// Auto-pairing there adds no capability. Auto-pairing a shared channel would
/// — so this deliberately refuses every non-DM channel, and every unknown
/// user, and those remain pairable only by code.
/// What: Pure predicate; true iff `channel` is a DM AND the sender resolved to
/// an RBAC entry. Kept pure and separate from the handlers so the security
/// rule is exhaustively unit-testable without a WebSocket.
/// Test: `auto_pair_allows_known_user_dm`, `auto_pair_rejects_unknown_user_dm`,
/// `auto_pair_rejects_public_channel`, `auto_pair_rejects_private_group`.
pub(super) fn should_auto_pair(channel: &str, sender_is_known_to_rbac: bool) -> bool {
    is_dm_channel(channel) && sender_is_known_to_rbac
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
