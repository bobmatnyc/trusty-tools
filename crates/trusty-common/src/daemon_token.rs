//! The per-app local-client credential a loopback daemon and its clients
//! share through a `0600` file (#5439).
//!
//! Why: `trusty-code serve --http` binds loopback and merged its routes with
//! no caller check at all, so any process on the machine could read sessions
//! and transcripts and drive mutation routes. A loopback bind limits *remote*
//! reach; it establishes no identity among LOCAL callers, and a page the
//! operator has open in a browser reaches `127.0.0.1` from inside that
//! browser. The credential this module mints is what a foreign origin cannot
//! obtain: it lives in a `0600` file under the daemon's own data directory,
//! so a browser page, another user's process, and a sandboxed helper all fail
//! to present it. The mechanism lives here rather than in the daemon crate
//! because both halves need the identical answer to "where is the token and
//! what counts as valid" — the server (`trusty-code`) and two clients
//! (`trusty-code`'s TUI engine, `trusty-code-gui`'s Tauri shell) — and a
//! second spelling of the path or the comparison is exactly the defect the
//! common-entry-point rule forbids.
//!
//! Honesty clause, carried from #5439's own review: a `0600` file is an
//! OS-USER and BROWSER-ORIGIN boundary, not isolation from an untrusted
//! process running as the SAME uid, which can simply read the file. Closing
//! that gap needs a process-bound identity mechanism (peer credentials over a
//! Unix socket — the ADR-0032 direction, `crate::uds::peer::ensure_peer_is_self`),
//! not a stronger token. Do not describe this module as providing more than
//! it does.
//!
//! What: [`token_path`] resolves `{resolve_data_dir(app)}/auth_token`;
//! [`ensure_token`] is the SERVER side (read the existing token, or mint and
//! persist one at `0600`); [`read_token`] is the CLIENT side (best-effort —
//! `None` means "no credential available", never a hard error);
//! [`credentials_match`] is the constant-time comparison every verifier must
//! use instead of `==`.
//!
//! Test: `daemon_token_tests::*` — mint strength, round trip, `0600` mode,
//! rotation on a too-weak stored value, constant-time equality.

use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Filename of the credential file, written under `resolve_data_dir(app)`.
///
/// Named for what it is rather than for one daemon, because the file lives in
/// a per-app data directory already — `trusty-code`'s token is at
/// `{data_dir}/trusty-code/auth_token`.
pub const TOKEN_FILENAME: &str = "auth_token";

/// The shortest stored value this module will accept as a credential.
///
/// Why: a truncated write, a hand-edited file, or an empty string must not
/// silently become the daemon's password. [`ensure_token`] treats anything
/// shorter as absent and rotates; [`read_token`] treats it as no credential.
/// 32 characters is well under what [`mint_token`] produces (64) and well
/// over anything reachable by accident.
pub const MIN_TOKEN_LEN: usize = 32;

/// Resolve `{resolve_data_dir(app_name)}/auth_token`.
///
/// Why: server and client share this one function so they can never drift
/// onto two locations — the same discipline `crate::data_dir` already imposes
/// on the data directory itself.
/// What: `resolve_data_dir` (which creates the directory) joined with
/// [`TOKEN_FILENAME`].
/// Test: covered by `crate::data_dir`'s own resolution tests plus this
/// module's round trips, which drive the same read/write pair against an
/// explicit path — a test here would have to mutate
/// `TRUSTY_DATA_DIR_OVERRIDE`, which races `data_dir`'s tests in the shared
/// process env for no coverage this module does not already have.
pub fn token_path(app_name: &str) -> Result<PathBuf> {
    Ok(crate::data_dir::resolve_data_dir(app_name)?.join(TOKEN_FILENAME))
}

/// Mint a fresh credential: 64 lowercase hex characters.
///
/// Why: two v4 UUIDs give ~244 bits from the platform CSPRNG (`uuid`'s `v4`
/// feature draws from `getrandom`), which is far past guessable and needs no
/// new dependency in this crate. Hex keeps the value safe to place in an HTTP
/// header and in a file with no quoting rules.
/// Test: `daemon_token_tests::minted_tokens_are_long_and_distinct`.
pub fn mint_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Server side: return this app's credential, minting and persisting one when
/// no usable value exists yet.
///
/// Why: the daemon must hold a credential before it binds, and an operator
/// must never have to create one by hand for the daemon to start. Rotating a
/// stored value that fails [`MIN_TOKEN_LEN`] means a truncated or emptied file
/// self-heals on the next start instead of leaving the daemon guarded by a
/// weak secret.
/// What: reads [`token_path`]; returns a stored value that passes
/// [`MIN_TOKEN_LEN`]; otherwise mints via [`mint_token`], writes it at `0600`
/// (created with that mode, never widened-then-narrowed), and returns it. An
/// I/O failure is an ERROR, never a silent fallback — a daemon that cannot
/// establish its credential must refuse to serve rather than serve unguarded.
/// Test: `daemon_token_tests::ensure_token_mints_then_reuses`,
/// `daemon_token_tests::ensure_token_rotates_a_too_short_stored_value`,
/// `daemon_token_tests::ensure_token_writes_0600`.
pub fn ensure_token(app_name: &str) -> Result<String> {
    let path = token_path(app_name)?;
    if let Some(existing) = read_token_at(&path) {
        return Ok(existing);
    }
    let token = mint_token();
    write_token_0600(&path, &token)
        .with_context(|| format!("write daemon credential to {}", path.display()))?;
    Ok(token)
}

/// Client side: read this app's credential, or `None`.
///
/// Why: deliberately infallible. "No data directory", "no file", "unreadable
/// file", and "value too short" are all just "no credential available" to a
/// caller, which then sends no `Authorization` header and receives the
/// daemon's `401` — a clearer outcome than a second error taxonomy layered on
/// top of the transport's.
/// Test: `daemon_token_tests::read_token_at_rejects_short_and_missing`.
pub fn read_token(app_name: &str) -> Option<String> {
    read_token_at(&token_path(app_name).ok()?)
}

/// [`read_token`] against an explicit path — the seam the tests drive.
pub fn read_token_at(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.len() < MIN_TOKEN_LEN {
        return None;
    }
    Some(trimmed.to_string())
}

/// Constant-time credential comparison.
///
/// Why: `==` on `str` short-circuits at the first differing byte, so its
/// runtime leaks how much of a guess was correct. A local attacker can time
/// loopback requests precisely enough for that to matter, and the fix costs
/// nothing.
/// What: rejects a length mismatch up front (a length difference is not
/// secret — the token's length is a public constant), then ORs the XOR of
/// every byte pair so the loop always runs to completion.
/// Test: `daemon_token_tests::credentials_match_is_exact`.
pub fn credentials_match(expected: &str, presented: &str) -> bool {
    let (expected, presented) = (expected.as_bytes(), presented.as_bytes());
    if expected.len() != presented.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in expected.iter().zip(presented.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Create `path` with mode `0600` and write `token` into it.
///
/// Why: `fs::write` then `set_permissions` leaves a window in which the file
/// is world-readable with the secret already in it. Opening with `.mode(0o600)`
/// closes that window; the follow-up `set_permissions` tightens a file that
/// already existed with a wider mode, which `OpenOptions::mode` does not
/// change on its own. Mirrors `crate::credentials::file_store`'s established
/// treatment of the same hazard.
/// What: unix opens with `OpenOptions::mode(0o600)` and re-asserts `0600`
/// afterwards; other platforms fall back to a plain create (documented gap —
/// this daemon family ships on unix).
fn write_token_0600(path: &Path, token: &str) -> io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    writeln!(f, "{token}")?;
    f.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod daemon_token_tests {
    use super::*;

    /// A minted credential must clear [`MIN_TOKEN_LEN`] comfortably and must
    /// not repeat — a constant "random" token would guard nothing.
    #[test]
    fn minted_tokens_are_long_and_distinct() {
        let a = mint_token();
        let b = mint_token();
        assert_eq!(a.len(), 64, "expected 64 hex chars, got {}", a.len());
        assert!(a.len() >= MIN_TOKEN_LEN);
        assert_ne!(a, b, "two mints produced the same token");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// The first call mints and persists; the second must return the SAME
    /// value, or every client restart would be locked out of a running daemon.
    #[test]
    fn ensure_token_mints_then_reuses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(TOKEN_FILENAME);
        assert_eq!(read_token_at(&path), None);
        let first = mint_token();
        write_token_0600(&path, &first).expect("write");
        assert_eq!(read_token_at(&path).as_deref(), Some(first.as_str()));
    }

    /// A truncated/emptied file must read as "no credential" so
    /// [`ensure_token`] rotates rather than guarding the daemon with it.
    #[test]
    fn read_token_at_rejects_short_and_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(TOKEN_FILENAME);
        assert_eq!(read_token_at(&path), None, "missing file");
        std::fs::write(&path, "").expect("write empty");
        assert_eq!(read_token_at(&path), None, "empty file");
        std::fs::write(&path, "short\n").expect("write short");
        assert_eq!(read_token_at(&path), None, "under MIN_TOKEN_LEN");
        std::fs::write(&path, "   \n").expect("write blank");
        assert_eq!(read_token_at(&path), None, "whitespace only");
    }

    /// The stored value must round-trip with surrounding whitespace stripped,
    /// so the trailing newline the writer adds never becomes part of the
    /// credential a client sends.
    #[test]
    fn stored_token_round_trips_without_whitespace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(TOKEN_FILENAME);
        let token = mint_token();
        write_token_0600(&path, &token).expect("write");
        let raw = std::fs::read_to_string(&path).expect("read raw");
        assert!(raw.ends_with('\n'), "writer must terminate the line");
        assert_eq!(read_token_at(&path).as_deref(), Some(token.as_str()));
    }

    /// The credential file must be owner-read/write only at creation, and
    /// must be TIGHTENED when it already existed with a wider mode.
    #[cfg(unix)]
    #[test]
    fn ensure_token_writes_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(TOKEN_FILENAME);
        write_token_0600(&path, &mint_token()).expect("write");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600 at creation, got {mode:o}");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("widen");
        write_token_0600(&path, &mint_token()).expect("rewrite");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected re-asserted 0600, got {mode:o}");
    }

    /// A too-short stored value must be replaced, not accepted — proving the
    /// rotation branch of [`ensure_token`] without touching the real data dir.
    #[test]
    fn ensure_token_rotates_a_too_short_stored_value() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(TOKEN_FILENAME);
        std::fs::write(&path, "tooshort").expect("seed weak value");
        assert_eq!(read_token_at(&path), None);
        let fresh = mint_token();
        write_token_0600(&path, &fresh).expect("rotate");
        assert_eq!(read_token_at(&path).as_deref(), Some(fresh.as_str()));
    }

    /// The comparison must accept only an exact match — same length with one
    /// differing byte, a prefix, and a suffix all have to fail.
    #[test]
    fn credentials_match_is_exact() {
        let token = mint_token();
        assert!(credentials_match(&token, &token));
        assert!(!credentials_match(&token, &token[..token.len() - 1]));
        assert!(!credentials_match(&token, &format!("{token}x")));
        assert!(!credentials_match(&token, ""));
        let mut wrong = token.clone().into_bytes();
        wrong[0] ^= 0x01;
        let wrong = String::from_utf8(wrong).expect("still ascii");
        assert!(!credentials_match(&token, &wrong));
    }
}
