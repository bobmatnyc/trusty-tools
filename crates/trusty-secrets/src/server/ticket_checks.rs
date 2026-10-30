//! The mint checks and the redemption match for main-thread tickets
//! (#10459 PR1, DOC-86 §2).
//!
//! Why: a ticket is worth only the checks behind it. A mint must come from
//! the registered hook, run by the installed `tm`, as a fresh direct child
//! of the launched Architect's `claude`; a redemption must be the call the
//! hook hashed, started after the mint, under that same `claude`.
//! What: [`check_mint_peer`] (everything but the launch record, which
//! `architect::check_architect` reads), [`check_redeemer`], the argv hash
//! [`argv_sha256`], and the wire-field validators. Every read error refuses.
//! Test: `ticket_tests.rs` beside `wire_tests.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::ancestry::ProcessTable;
use super::architect::{ArchitectRefusal, same_dir};
use super::grant::hex;
use super::ticket::{Ticket, TicketOp, TicketRefusal};
use crate::api::SecretKey;

/// The registered hook argv after argv\[0\], the absolute `tm` path.
// #10459 PR1: `tm hook --pm-guard` in exec form (DOC-86 §0, §8 PR 2).
pub(crate) const HOOK_ARGV_TAIL: [&str; 2] = ["hook", "--pm-guard"];

/// The oldest a minting peer may be, in whole seconds (DOC-86 §2).
pub(crate) const MAX_PEER_AGE_SECS: u64 = 2;

/// The installed binary's file name.
const TM_EXE: &str = "tm";

/// The longest accepted session, prompt or tool-use id.
const MAX_ID_LEN: usize = 128;

/// The cargo bin directory under the account's home, `None` without one.
// #10459 PR1: from the password database, never `$HOME` or `CARGO_HOME`,
// so a spawner cannot name the directory the check trusts.
pub(crate) fn tm_bin_dir(account_home: Option<&Path>) -> Option<PathBuf> {
    account_home.map(|home| home.join(".cargo/bin"))
}

/// The SHA-256 of `argv`, lowercase hex: each argument's bytes and a NUL.
///
/// Why: a ticket binds the exact argv the hook parsed, and the redeemer's
/// argv is read from the process table in the same form.
/// Test: `argv_hash_separates_arguments`.
pub(crate) fn argv_sha256(argv: &[String]) -> String {
    let mut hash = Sha256::new();
    for arg in argv {
        hash.update(arg.as_bytes());
        hash.update([0u8]);
    }
    hex(&hash.finalize())
}

/// A Claude Code id the server may log: 1 to 128 of `[A-Za-z0-9._:-]`.
pub(crate) fn valid_id(id: &str) -> bool {
    (1..=MAX_ID_LEN).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// 64 lowercase hex digits.
pub(crate) fn valid_sha256(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The mint checks on the socket peer; `Ok` is its parent `claude`'s pid.
///
/// Why: DOC-86 §2 — only the Architect main thread's PreToolUse hook mints.
/// What: in order, each fail-closed: the peer's executable canonicalizes to
/// `tm` directly in `bin_dir`, and neither file nor directory is group- or
/// other-writable ([`TicketRefusal::NotInstalledTm`]); its argv is
/// `<tm> hook --pm-guard` ([`TicketRefusal::NotHookArgv`]); its parent is a
/// Claude Code process ([`TicketRefusal::ParentNotClaude`]); it started at
/// most [`MAX_PEER_AGE_SECS`] before `now` ([`TicketRefusal::PeerTooOld`];
/// a start after `now` is [`TicketRefusal::Clock`]). A read error is
/// [`TicketRefusal::ProcessLookup`].
/// Test: `mint_from_a_non_tm_exe_is_refused`, `mint_from_a_grandchild_is_refused`,
/// `mint_from_an_old_process_is_refused`, `process_table_error_at_mint_refuses`.
pub(crate) fn check_mint_peer(
    table: &dyn ProcessTable,
    bin_dir: Option<&Path>,
    peer: u32,
    now: Duration,
) -> Result<u32, TicketRefusal> {
    let lookup = |_| TicketRefusal::ProcessLookup;
    let bin_dir = bin_dir.ok_or(TicketRefusal::NotInstalledTm)?;
    let exe = table.executable(peer).map_err(lookup)?;
    if !is_installed_tm(&exe, bin_dir) {
        return Err(TicketRefusal::NotInstalledTm);
    }
    let argv = table.argv(peer).map_err(lookup)?;
    if argv.len() != 3 || argv[1..] != HOOK_ARGV_TAIL {
        return Err(TicketRefusal::NotHookArgv);
    }
    let parent = table.parent(peer).map_err(lookup)?;
    if parent <= 1 || !table.is_agent(parent).map_err(lookup)? {
        return Err(TicketRefusal::ParentNotClaude);
    }
    let started = table.start_unix_secs(peer).map_err(lookup)?;
    let age = now
        .as_secs()
        .checked_sub(started)
        .ok_or(TicketRefusal::Clock)?;
    if age > MAX_PEER_AGE_SECS {
        return Err(TicketRefusal::PeerTooOld);
    }
    Ok(parent)
}

/// Whether `exe` is the installed `tm`: see [`check_mint_peer`].
fn is_installed_tm(exe: &Path, bin_dir: &Path) -> bool {
    let (Ok(exe), Ok(dir)) = (std::fs::canonicalize(exe), std::fs::canonicalize(bin_dir)) else {
        return false;
    };
    exe.parent() == Some(dir.as_path())
        && exe.file_name().is_some_and(|name| name == TM_EXE)
        && owner_writable_only(&exe)
        && owner_writable_only(&dir)
}

/// No group or other user can write `path`; `false` when it does not stat.
#[cfg(unix)]
fn owner_writable_only(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).is_ok_and(|meta| meta.mode() & 0o022 == 0)
}

#[cfg(not(unix))]
fn owner_writable_only(_path: &Path) -> bool {
    false
}

/// The call a redemption must match (DOC-86 §2, "Redemption").
#[derive(Debug, Clone, Copy)]
pub(crate) struct TicketCall<'a> {
    /// `set` or a local `copy`.
    pub(crate) op: TicketOp,
    /// The resolved project root.
    pub(crate) project: &'a Path,
    /// The one key written; `None` for a copy of several or of all keys.
    pub(crate) key: Option<&'a SecretKey>,
}

/// Whether `peer` may redeem `ticket` for `call`.
///
/// What: in order — the peer's argv hashes to the ticket's
/// ([`ArchitectRefusal::TicketArgvMismatch`]); it started in a whole second
/// after the mint's ([`ArchitectRefusal::TicketPeerStartedBeforeMint`]; the
/// same second refuses, fail closed); the op, the project (canonically) and
/// the one key equal the ticket's ([`ArchitectRefusal::TicketOtherCall`]).
/// A read error is [`ArchitectRefusal::ProcessLookup`]. The caller already
/// matched the `claude` (pid, start time) and removed the ticket.
/// Test: `ticket_argv_mismatch_is_refused`,
/// `ticket_peer_started_before_mint_is_refused`, `ticket_for_another_key_is_refused`,
/// `multi_key_copy_never_redeems`.
pub(crate) fn check_redeemer(
    table: &dyn ProcessTable,
    ticket: &Ticket,
    peer: u32,
    call: &TicketCall<'_>,
) -> Result<(), ArchitectRefusal> {
    let lookup = |_| ArchitectRefusal::ProcessLookup;
    let argv = table.argv(peer).map_err(lookup)?;
    if argv_sha256(&argv) != ticket.argv_sha256 {
        return Err(ArchitectRefusal::TicketArgvMismatch);
    }
    let started = table.start_unix_secs(peer).map_err(lookup)?;
    if started <= ticket.minted_at.as_secs() {
        return Err(ArchitectRefusal::TicketPeerStartedBeforeMint);
    }
    let same_call = call.op == ticket.op
        && call.key == Some(&ticket.key)
        && same_dir(call.project, &ticket.project);
    if !same_call {
        return Err(ArchitectRefusal::TicketOtherCall);
    }
    Ok(())
}
