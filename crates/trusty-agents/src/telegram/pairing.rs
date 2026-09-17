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

use super::bot::BotKey;

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

/// The user-level directory holding every per-bot gateway state file.
///
/// Why: we want the *user-level* `~/.trusty-agents/state/` directory (shared
/// across projects), NOT the project-local `.trusty-agents/state/`. Falls back
/// to a relative path when `HOME` is unset so we never panic on weird sandboxes.
/// Test: `telegram_state_file_names_never_contain_the_token`.
pub(crate) fn gateway_state_dir() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".trusty-agents").join("state")
}

/// The paired-chats state file for ONE bot.
///
/// Why (#8190, owner ruling 2026-09-16): each assistant has its own Telegram
/// bot, so a chat paired to izzie's bot must not count as paired on
/// cto-assistant's. One shared `telegram-paired.json` made every pairing
/// machine-wide, which is the grant leak that ruling forbids.
/// What: `~/.trusty-agents/state/telegram-paired-<key>.json`, where `<key>` is
/// [`crate::telegram::BotKey`]'s digest — derived from the token, never the
/// token itself, and never logged.
/// Test: `telegram_state_file_names_never_contain_the_token`,
/// `telegram_pairing_state_is_per_bot`.
pub(super) fn paired_chats_state_path_for(bot: &BotKey) -> PathBuf {
    gateway_state_dir().join(format!("telegram-paired-{}.json", bot.digest()))
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

/// Filename prefix every per-bot gateway lock file shares.
const PID_FILE_PREFIX: &str = "telegram-";
/// Filename suffix every per-bot gateway lock file shares.
const PID_FILE_SUFFIX: &str = ".pid";

/// The gateway lock file for ONE bot.
///
/// Why (#single-instance, #8190): two processes polling `getUpdates` for the
/// SAME bot trigger Telegram's `TerminatedByOtherGetUpdates` and fight over
/// updates. Two processes polling DIFFERENT bots do not conflict at all, so
/// the lock is per bot token — one machine-wide lock would have let izzie's
/// poller lock cto-assistant's out.
/// What: `~/.trusty-agents/state/telegram-<key>.pid`, `<key>` being
/// [`crate::telegram::BotKey`]'s digest of the token. The digest is in the
/// FILE NAME only; nothing renders it.
/// Test: `telegram_state_file_names_never_contain_the_token`.
pub(crate) fn telegram_pid_file_path_for(bot: &BotKey) -> PathBuf {
    gateway_state_dir().join(format!(
        "{PID_FILE_PREFIX}{}{PID_FILE_SUFFIX}",
        bot.digest()
    ))
}

/// A live process holding one bot's gateway lock.
///
/// Why (#8190 code-critic MEDIUM 2): the old probe read a PID out of the file
/// and asked `kill(pid, 0)`, which is check-then-act and PID-identity-blind —
/// two hosts could both read "stale" and both acquire, and a REUSED pid read
/// live forever. The lock is now `flock`, held by an open descriptor, so the
/// kernel answers "is it held" directly. The recorded pid survives only as
/// REPORTING text, which is why it is optional here: a holder that has taken
/// the lock but not yet written its pid is still a holder.
/// Test: `telegram_gateway_lock_holder_reports_a_live_holder`,
/// `telegram_gateway_lock_holder_ignores_an_unlocked_file`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockHolder {
    /// The holder's PID, when it recorded one.
    pub pid: Option<i32>,
}

impl LockHolder {
    /// The holder's PID as report text, never a panic on an unwritten file.
    pub fn label(&self) -> String {
        self.pid
            .map_or_else(|| "unknown".to_string(), |pid| pid.to_string())
    }
}

/// Try to take `file`'s advisory lock without blocking.
///
/// Why: `flock(LOCK_EX|LOCK_NB)` is the whole single-instance mechanism —
/// atomic, released by the kernel when the descriptor closes (so a crashed
/// holder leaves no stale lock), and conflicting across two descriptors in ONE
/// process, which is what makes a two-acquirer test possible in-process.
/// What: `Ok(true)` when this descriptor now holds the lock, `Ok(false)` when
/// another descriptor holds it, `Err` for any other `flock` failure.
/// Test: `telegram_pid_guard_live_conflict_is_rejected`.
fn try_lock(file: &std::fs::File) -> Result<bool> {
    use std::os::fd::AsRawFd;
    // SAFETY: `flock` only manipulates the advisory lock on a descriptor we
    // own and keep alive for the whole call; it never touches memory.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(false),
        _ => Err(anyhow!("failed to lock {err}")),
    }
}

/// The live holder of the gateway lock at `path`, if any.
///
/// Why (#8190): the API host decides BEFORE spawning whether a poller already
/// owns `getUpdates` for this bot, and the supervisor re-asks on every rescan.
/// [`TelegramPidGuard::acquire`] answers the same question but TAKES the lock,
/// which is exactly what a startup decision must not do.
/// What: opens `path` read-only (BSD `flock` needs no write access) and
/// attempts the same exclusive lock. Taking it means nobody held it, so the
/// probe immediately drops the descriptor and answers `None`; a refusal means a
/// live holder, whose recorded pid is then read for reporting. A missing file
/// is `None`. The answer is a snapshot — `acquire` remains the authoritative
/// gate.
/// Test: `telegram_gateway_lock_holder_reports_a_live_holder`,
/// `telegram_gateway_lock_holder_ignores_an_unlocked_file`.
pub fn gateway_lock_holder_at(path: &Path) -> Option<LockHolder> {
    let file = std::fs::File::open(path).ok()?;
    match try_lock(&file) {
        // We took it, so nobody held it. Dropping `file` releases it.
        Ok(true) => None,
        Ok(false) => Some(LockHolder {
            pid: std::fs::read_to_string(path)
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok()),
        }),
        // #8190 fail-open check: a lock we cannot even probe is reported as
        // held. Assuming "free" here is what starts a SECOND poller and takes
        // Telegram down for the first one.
        Err(e) => {
            warn!(
                error = %format!("{e:#}"),
                "telegram gateway: the gateway lock could not be probed; treating it as held"
            );
            Some(LockHolder { pid: None })
        }
    }
}

/// Every bot gateway lock in `dir` that a live process holds.
///
/// Why (#8190 code-critic MEDIUM 5): `tagent system status` runs in a DIFFERENT
/// process from the API host, so it has no in-memory gateway state to read. A
/// directory scan plus a lock probe is the one signal that crosses the process
/// boundary, and it is what makes "something is polling Telegram on this
/// machine" observable without stderr capture.
/// What: probes every `telegram-*.pid` in `dir` and returns the holders' pids,
/// sorted. The digest in each file name is never returned — only pids.
/// Test: `telegram_gateway_status_snapshot_reports_a_live_lock_holder`.
pub(crate) fn live_gateway_lock_holders_in(dir: &Path) -> Vec<i32> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut pids: Vec<i32> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(PID_FILE_PREFIX) && n.ends_with(PID_FILE_SUFFIX))
        })
        .filter_map(|p| gateway_lock_holder_at(&p))
        .filter_map(|h| h.pid)
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// RAII guard holding ONE bot's gateway lock for as long as it polls.
///
/// Why (#8190 code-critic MEDIUM 2): the pre-#8190 guard wrote a PID file and
/// unlinked it on `Drop`, so a loser that started beside a winner removed the
/// WINNER's file on its way out and left the lock apparently free. An advisory
/// `flock` on a held descriptor has neither race: the kernel decides who holds
/// it, and closing the descriptor — on return, on `?`, on panic, on SIGINT, on
/// process death — releases it.
/// What: `acquire()` opens the lock file and takes `LOCK_EX|LOCK_NB`, then
/// records its own pid for reporting. There is deliberately NO `Drop` impl: the
/// file stays on disk (it is a lock, not a liveness record) and the `File`
/// field's own drop releases the lock.
/// Test: `telegram_pid_guard_acquire_writes_and_releases`,
/// `telegram_pid_guard_live_conflict_is_rejected`,
/// `telegram_pid_guard_reclaims_a_lock_no_one_holds`.
pub(crate) struct TelegramPidGuard {
    /// Holding the descriptor IS holding the lock; closing it releases.
    _file: std::fs::File,
}

impl TelegramPidGuard {
    /// Acquire this bot's single-instance lock.
    ///
    /// Why: prevents two processes from racing on `getUpdates` for ONE bot.
    /// What: creates the state dir, opens the lock file, and takes the
    /// exclusive non-blocking `flock`. A refusal is an error naming the holder.
    /// On success the file is truncated and the current pid written, purely so
    /// a probe can report who holds it.
    /// Test: `telegram_pid_guard_acquire_writes_and_releases`,
    /// `telegram_pid_guard_live_conflict_is_rejected`.
    pub(crate) fn acquire(path: PathBuf) -> Result<Self> {
        use std::io::Write;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow!("failed to create state dir {}: {e}", parent.display()))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| anyhow!("failed to open gateway lock {}: {e}", path.display()))?;
        if !try_lock(&file).map_err(|e| anyhow!("gateway lock {}: {e}", path.display()))? {
            let holder = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
                .map_or_else(|| "unknown".to_string(), |pid| pid.to_string());
            return Err(anyhow!(
                "another process (PID {holder}) is already polling this Telegram bot. \
                 Stop it before starting another; the lock at {} releases by itself when \
                 that process exits.",
                path.display()
            ));
        }
        file.set_len(0)
            .map_err(|e| anyhow!("failed to reset gateway lock {}: {e}", path.display()))?;
        write!(file, "{}", std::process::id())
            .map_err(|e| anyhow!("failed to write gateway lock {}: {e}", path.display()))?;
        file.flush()
            .map_err(|e| anyhow!("failed to flush gateway lock {}: {e}", path.display()))?;
        Ok(Self { _file: file })
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
