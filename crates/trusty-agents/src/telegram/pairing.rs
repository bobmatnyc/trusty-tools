//! Pairing state machine, persistence, code issuance, and single-instance PID
//! guard for the Telegram gateway.
//!
//! Why (#334/#467/#single-instance): The bot must authorize chats out-of-band
//! (codes issued in the trusted REPL), persist pairings across restarts, and
//! refuse to run two long-pollers at once. These concerns are independent of
//! command dispatch and message formatting, so they live together here.
//! What: `PairedChats` map type + load/save, `PendingPairs` + REPL code
//! issuance, the `PairOutcome` state machine (`verify_pair_attempt`), and
//! `TelegramPidGuard` for single-instance enforcement.
//! Test: `telegram::tests` covers the pure state machine, persistence
//! round-trip, code format, and PID-guard acquire/stale/drop behavior.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use teloxide::types::ChatId;
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};

/// How long a pairing code remains valid after issuance.
///
/// Why: Bound the window where a leaked code from server logs could be used by
/// another chat. 5 minutes is long enough for a human to copy/paste and short
/// enough to limit exposure.
pub(super) const PAIRING_CODE_TTL: Duration = Duration::from_secs(5 * 60);

/// Map of `ChatId` -> instant the chat was paired, shared across handlers.
///
/// Why: #334 introduces a pairing gate. Only chats with an entry in this map
/// may dispatch to ctrl; everyone else gets a "🔒 Not paired" reply. Reads
/// dominate writes (every message reads, only `/pair` writes), so we use
/// `RwLock`.
pub(super) type PairedChats = Arc<RwLock<HashMap<ChatId, Instant>>>;

/// On-disk record of a single paired chat.
///
/// Why (#467): `PairedChats` lives in memory only; every restart loses every
/// pairing, forcing users to re-run `/start` + `/pair` on every harness
/// upgrade. Persisting a minimal record under `~/.trusty-agents/state/` survives
/// restarts without leaking any user content.
/// What: chat-id (Telegram's `i64`) plus the wall-clock timestamp of pairing.
/// `Instant` is monotonic and unsuitable for persistence, so we store
/// `DateTime<Utc>` and reconstruct `Instant::now()` on load — the absolute
/// pairing time is only used for diagnostic logs, not for expiry decisions.
/// Test: `paired_state_round_trip()` exercises save+load with a tempdir.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PairedChatRecord {
    chat_id: i64,
    paired_at: DateTime<Utc>,
}

/// On-disk container for the paired-chats file.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PairedChatsFile {
    paired_chats: Vec<PairedChatRecord>,
}

/// Resolve the absolute path of the paired-chats state file.
///
/// Why: We want the *user-level* `~/.trusty-agents/state/` directory (shared across
/// projects), NOT the project-local `.trusty-agents/state/`. Falls back to a
/// relative path when `HOME` is unset so we never panic on weird sandboxes.
pub(super) fn paired_chats_state_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".trusty-agents")
        .join("state")
        .join("telegram-paired.json")
}

/// Load persisted paired chats from disk.
///
/// Why (#467): On startup, restore the pairing map so users don't have to
/// re-pair every time the harness restarts.
/// What: Reads `state_path` as JSON. Missing file -> empty map (first run).
/// Parse errors -> log a warning and return empty map (never panic). The
/// stored `DateTime<Utc>` is discarded; we use `Instant::now()` as a stand-in
/// since the value is only consumed by diagnostic logging.
/// Test: `paired_state_round_trip` covers happy-path; missing-file and
/// malformed-JSON branches are intentionally fail-open (no panic).
pub(super) async fn load_paired_chats(state_path: &Path) -> PairedChats {
    let bytes = match tokio::fs::read(state_path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Arc::new(RwLock::new(HashMap::new()));
        }
        Err(e) => {
            warn!(path = %state_path.display(), error = %e, "failed to read paired-chats state; starting empty");
            return Arc::new(RwLock::new(HashMap::new()));
        }
    };
    let parsed: PairedChatsFile = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            warn!(path = %state_path.display(), error = %e, "failed to parse paired-chats state; starting empty");
            return Arc::new(RwLock::new(HashMap::new()));
        }
    };
    let now = Instant::now();
    let mut map: HashMap<ChatId, Instant> = HashMap::with_capacity(parsed.paired_chats.len());
    for rec in parsed.paired_chats {
        // `Instant` cannot represent past wall-clock times; use `now` as a
        // stand-in. The exact value is only used by diagnostic logging.
        map.insert(ChatId(rec.chat_id), now);
    }
    info!(count = map.len(), path = %state_path.display(), "loaded paired chats");
    Arc::new(RwLock::new(map))
}

/// Persist the paired-chats map to disk atomically.
///
/// Why (#467): Survives harness restarts so a successful `/pair` is durable.
/// What: Snapshots the in-memory map under a read lock, serializes to JSON,
/// then writes via `<path>.tmp` + `rename` for atomic replacement. Creates
/// the parent directory on demand. Never panics: any IO error is logged and
/// returned; callers in the `/pair` handler intentionally swallow the error
/// because losing persistence on disk-full or perms is recoverable on next
/// successful save.
pub(super) async fn save_paired_chats(paired: &PairedChats, state_path: &Path) -> Result<()> {
    if let Some(parent) = state_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            anyhow!(
                "failed to create paired-chats state dir {}: {e}",
                parent.display()
            )
        })?;
    }
    let snapshot: Vec<PairedChatRecord> = {
        let guard = paired.read().await;
        guard
            .keys()
            .map(|cid| PairedChatRecord {
                chat_id: cid.0,
                paired_at: Utc::now(),
            })
            .collect()
    };
    let file = PairedChatsFile {
        paired_chats: snapshot,
    };
    let json = serde_json::to_vec_pretty(&file)
        .map_err(|e| anyhow!("failed to serialize paired-chats: {e}"))?;
    let tmp_path = state_path.with_extension("json.tmp");
    tokio::fs::write(&tmp_path, &json).await.map_err(|e| {
        anyhow!(
            "failed to write paired-chats tmp file {}: {e}",
            tmp_path.display()
        )
    })?;
    tokio::fs::rename(&tmp_path, state_path)
        .await
        .map_err(|e| {
            anyhow!(
                "failed to rename paired-chats tmp -> {}: {e}",
                state_path.display()
            )
        })?;
    Ok(())
}

/// Resolve the absolute path of the Telegram daemon PID file.
///
/// Why (#single-instance): Multiple `--telegram` processes polling
/// `getUpdates` concurrently trigger Telegram's `TerminatedByOtherGetUpdates`
/// error and fight over updates. A PID file in the user-level
/// `~/.trusty-agents/state/` directory lets a starting daemon detect an
/// already-running peer. Falls back to a relative path when `HOME` is unset
/// so we never panic on weird sandboxes.
/// What: Returns `$HOME/.trusty-agents/state/telegram.pid`.
pub(super) fn telegram_pid_file_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".trusty-agents")
        .join("state")
        .join("telegram.pid")
}

/// Check whether process `pid` is alive by sending signal 0 (no-op signal).
///
/// Why: `kill(pid, 0)` performs the permission/existence checks without
/// delivering a signal — the standard POSIX liveness probe. Used to decide
/// whether an existing PID file represents a live daemon or a stale lock.
/// What: Returns `true` iff `libc::kill(pid, 0)` succeeds.
/// Test: A live process (our own PID) returns true; an absurd PID returns
/// false — covered by `telegram_pid_alive_*` tests.
pub(super) fn telegram_pid_alive(pid: i32) -> bool {
    // SAFETY: `kill` with signal 0 only probes; it never mutates process
    // state. `pid` is a plain integer and any value is a valid argument —
    // the kernel rejects invalid ones via errno, which `== 0` filters out.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// RAII guard that owns the Telegram daemon PID file.
///
/// Why: The PID file must be removed on *every* exit path — normal return,
/// `?` early-return, panic, and SIGINT (the dispatcher's `enable_ctrlc_handler`
/// terminates the long-poll loop, after which `run_telegram_bot` returns and
/// this guard drops). A `Drop` impl is the only construct that fires on all
/// of those without scattering cleanup calls.
/// What: `acquire()` enforces single-instance semantics and writes the PID
/// file; `Drop` removes it. Holds the path so `Drop` needs no recomputation.
/// Test: `telegram_pid_guard_*` tests exercise acquire/stale/drop behavior.
pub(super) struct TelegramPidGuard {
    path: PathBuf,
}

impl TelegramPidGuard {
    /// Acquire the single-instance lock for the Telegram daemon.
    ///
    /// Why: Prevents two `--telegram` processes from racing on `getUpdates`.
    /// What: If a PID file exists and the PID inside is still alive, returns
    /// an error (caller should exit). A stale PID file (process dead) is
    /// overwritten. On success, writes the current PID and returns a guard
    /// whose `Drop` cleans up the file.
    /// Test: `telegram_pid_guard_acquire_writes_file`,
    /// `telegram_pid_guard_stale_is_overwritten`.
    pub(super) fn acquire(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow!("failed to create state dir {}: {e}", parent.display()))?;
        }

        // If a PID file exists, decide live vs. stale.
        if let Ok(contents) = std::fs::read_to_string(&path)
            && let Ok(existing_pid) = contents.trim().parse::<i32>()
            && telegram_pid_alive(existing_pid)
        {
            return Err(anyhow!(
                "Telegram daemon already running (PID {existing_pid}). \
                         Stop it before starting another, or delete {} if you \
                         are sure it is stale.",
                path.display()
            ));
        }
        // Stale lock: the recorded process is gone. Fall through to
        // overwrite it.
        // Unparseable contents are treated as stale too.

        let pid = std::process::id();
        std::fs::write(&path, pid.to_string())
            .map_err(|e| anyhow!("failed to write PID file {}: {e}", path.display()))?;

        Ok(Self { path })
    }
}

impl Drop for TelegramPidGuard {
    /// Best-effort removal of the PID file on daemon exit.
    ///
    /// Why: Leaving a stale file behind would force the next start to treat
    /// it as a stale lock — harmless but noisy. Errors are intentionally
    /// swallowed: there is nothing useful to do during teardown.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Map of pending pairing codes keyed by raw chat-id (`i64`).
///
/// Why (#334): The pairing code is generated **in the REPL** (trusted
/// terminal), not on Telegram. The REPL writes the code under the sentinel
/// key `SENTINEL_PAIRING_CHAT_ID` (= `i64::MAX`). When a `/pair <code>`
/// arrives from Telegram, we look up the sentinel entry; on a match the
/// real `ChatId` is promoted to `paired`. This means an attacker who
/// controls the bot cannot self-authorize — they must also have shell
/// access to the host running the REPL.
/// What: `Arc<tokio::sync::Mutex<HashMap<i64, (String, Instant)>>>`. The
/// raw `i64` (not `ChatId`) keeps the REPL free of teloxide types.
pub type PendingPairs = Arc<Mutex<HashMap<i64, (String, Instant)>>>;

/// Sentinel chat-id under which the REPL stores the next pending code.
///
/// Why: A real Telegram chat-id never equals `i64::MAX` in practice, so this
/// is a safe out-of-band key for "the next /pair attempt from any chat".
pub const SENTINEL_PAIRING_CHAT_ID: i64 = i64::MAX;

/// Construct a fresh, empty `PendingPairs` shared across REPL + bot task.
pub fn new_pending_pairs() -> PendingPairs {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Generate and store a REPL-issued pairing code under the sentinel key.
///
/// Why (#334): Called from `/telegram pair` in the REPL. The next `/pair
/// <code>` arriving on Telegram (from any chat) can claim it. Overwrites
/// any prior pending sentinel entry — only the most recent REPL-issued
/// code is honoured.
/// What: Returns the 6-digit code so the REPL can display it.
/// Test: `repl_issued_code_lands_under_sentinel` exercises the flow.
pub async fn issue_repl_pairing_code(pending: &PendingPairs) -> String {
    let code = generate_pairing_code();
    let mut map = pending.lock().await;
    map.insert(SENTINEL_PAIRING_CHAT_ID, (code.clone(), Instant::now()));
    code
}

/// Generate a random 6-digit pairing code (zero-padded).
///
/// Why: 6 digits gives ~1M codes — plenty for a human-friendly handoff over a
/// log line, while still being short enough to type on a phone.
/// What: Uses `rand::random::<u32>() % 1_000_000` and zero-pads with `{:06}`.
/// Test: `pairing_code_is_six_digits` asserts the format.
pub(super) fn generate_pairing_code() -> String {
    format!("{:06}", rand::random::<u32>() % 1_000_000)
}

/// Outcome of a `/pair <code>` attempt. Pure for unit testing.
///
/// Why: We want to unit-test the state-machine without the teloxide types in
/// the loop. `verify_pair_attempt` returns one of these and the handler turns
/// it into Telegram replies + map mutations.
/// Test: `pair_*` tests in `telegram::tests`.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PairOutcome {
    /// No `/start` was issued for this chat (no pending code).
    NoPending,
    /// The pending code is past its TTL.
    Expired,
    /// The provided code does not match the pending code.
    Mismatch,
    /// The provided code matches and is within TTL — caller must promote the
    /// chat to paired.
    Success,
}

/// Verify a `/pair` attempt against a pending entry.
///
/// Why: Pure function so we can exhaustively test the state machine without
/// spinning up a teloxide bot. The caller is responsible for the side effects
/// (removing the pending entry, inserting into paired, sending the reply).
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
