//! One-shot grants from the pm-guard to `tm env` (#8939 fix round).
//!
//! Why: the owner's 2026-09-29 ruling in #8878 exempts the Architect's MAIN
//! THREAD only. `tm env` has no hook payload, and a Claude Code subagent runs
//! its Bash calls under the Architect's own `claude` with the same
//! environment, so the verb's binding check cannot tell the two apart;
//! `CLAUDE_MPM_SUB_AGENT` marks only trusty-agents' own spawns. Only the
//! pm-guard sees the payload's `agent_id`.
//! What: when the guard exempts a `tm env` call for the main thread, [`mint`]
//! writes the exact call — verb, resolved path, key, Keychain names — to a
//! grant file under `~/.trusty-mpm/architect-launch/envfile-grants/`, inside
//! the sealed #8878 launch-record anchor no PM or agent may write. [`consume`]
//! lets the verb run only after it removes an unexpired grant for the same
//! exact call; the removal is atomic, so one grant runs one call.
//! FAIL-CLOSED: no grant, an expired one, one for another call, an unreadable
//! grant directory or a symlinked one means refuse. So the verb refuses every
//! call that did not pass the guard's exempt shape as the main thread,
//! including a subagent's indirect call (a script, a variable).
//! Residual: a subagent issuing the IDENTICAL call in the [`GRANT_TTL`]
//! window after the main thread's grant is minted can spend that grant. It
//! can only perform the exact call the main thread was granted, and the main
//! thread's own call then fails visibly. A grant whose call a later hook
//! refused lies unspent until it expires.
//! Test: `env_file_tests.rs` (`the_main_thread_grant_lets_the_verb_run_once`,
//! `a_subagent_shaped_caller_without_a_grant_is_refused`,
//! `a_grant_expires_and_binds_its_exact_call`).

use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use trusty_mpm::core::architect_launch::ARCHITECT_DIR;

use crate::commands::pm_guard_architect_envfile::EnvfileCall;
use crate::commands::pm_guard_trust_anchor::ANCHOR_ROOT;

/// The grant directory, under the launch-record anchor.
const GRANT_DIR: &str = "envfile-grants";

/// A grant file's extension; a temp file never carries it.
const GRANT_EXT: &str = "grant";

/// How long a grant stays valid after the guard mints it.
pub(crate) const GRANT_TTL: Duration = Duration::from_secs(60);

/// The largest grant file read.
const MAX_GRANT_BYTES: u64 = 16 << 10;

/// `~/.trusty-mpm/architect-launch/envfile-grants`.
fn grant_dir(home: &Path) -> PathBuf {
    home.join(ANCHOR_ROOT).join(ARCHITECT_DIR).join(GRANT_DIR)
}

/// The exact call, as NUL-terminated fields no field can contain.
fn grant_bytes(call: &EnvfileCall) -> Vec<u8> {
    let mut out = Vec::new();
    let mut field = |bytes: &[u8]| {
        out.extend_from_slice(bytes);
        out.push(0);
    };
    field(call.verb.as_bytes());
    field(call.path.as_os_str().as_bytes());
    field(call.keys.join(",").as_bytes());
    match &call.keychain {
        Some((service, account)) => {
            field(b"1");
            field(service.as_bytes());
            field(account.as_bytes());
        }
        None => field(b"0"),
    }
    out
}

/// Write the grant for `call` under `home`, through a temp file and a rename.
pub(crate) fn mint(home: &Path, call: &EnvfileCall) -> std::io::Result<()> {
    let dir = grant_dir(home);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    if !std::fs::symlink_metadata(&dir)?.is_dir() {
        return Err(std::io::Error::other(
            "the grant directory is not a directory",
        ));
    }
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let stem = format!("{nanos}-{}", std::process::id());
    let tmp = dir.join(format!(".{stem}.tmp"));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)
        .and_then(|mut f| f.write_all(&grant_bytes(call)))
        .and_then(|()| std::fs::rename(&tmp, dir.join(format!("{stem}.{GRANT_EXT}"))));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Spend an unexpired grant for exactly `call`; `true` when one was spent.
pub(crate) fn consume(home: &Path, call: &EnvfileCall) -> bool {
    consume_at(home, call, SystemTime::now())
}

/// [`consume`] at `now`. An expired or future-dated grant is removed.
pub(crate) fn consume_at(home: &Path, call: &EnvfileCall, now: SystemTime) -> bool {
    let dir = grant_dir(home);
    if !std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return false;
    };
    let want = grant_bytes(call);
    for path in entries.flatten().map(|e| e.path()) {
        if path.extension().and_then(|e| e.to_str()) != Some(GRANT_EXT) {
            continue;
        }
        let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
        else {
            continue;
        };
        let Ok(meta) = file.metadata() else { continue };
        let age = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok());
        if !meta.is_file() || !age.is_some_and(|age| age <= GRANT_TTL) {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let mut bytes = Vec::new();
        let read = file.take(MAX_GRANT_BYTES).read_to_end(&mut bytes);
        if read.is_ok() && bytes == want && std::fs::remove_file(&path).is_ok() {
            return true;
        }
    }
    false
}
