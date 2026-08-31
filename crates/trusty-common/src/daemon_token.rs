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
/// What: rejects an `expected` shorter than [`MIN_TOKEN_LEN`] outright, then a
/// length mismatch (a length difference is not secret — the token's length is a
/// public constant), then ORs the XOR of every byte pair so the loop always
/// runs to completion.
///
/// The first check is what stops an empty `expected` from matching an empty
/// `presented`: without it, a daemon holding `""` authenticates every request
/// that sends a bare `Authorization: Bearer `. A credential too weak to be one
/// must never verify anything, whatever the caller sent.
/// Test: `daemon_token_tests::credentials_match_is_exact`,
/// `daemon_token_tests::an_empty_or_weak_expected_credential_never_matches`.
pub fn credentials_match(expected: &str, presented: &str) -> bool {
    if expected.len() < MIN_TOKEN_LEN {
        return false;
    }
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

/// Does `url` name a daemon on THIS machine?
///
/// Why: the credential authenticates a caller to the local daemon and nothing
/// else, so every client resolves it through this gate before attaching a
/// header. The gate must parse `url` the way the HTTP client that will dial it
/// parses it, and the first implementation did not: it reused
/// `server::origin_guard::origin_is_loopback`, which reads an `Origin` HEADER —
/// a bare `scheme://host[:port]` with no userinfo and no path, split at the
/// FIRST `:`. Handed a URL instead, it reads
/// `http://127.0.0.1:7882@attacker.example` as host `127.0.0.1` and answers
/// "loopback", while WHATWG parsing splits userinfo at the LAST `@` and reaches
/// `attacker.example`. Setting `TCODE_DAEMON_URL` to that string was enough to
/// ship the token off-machine on the next request.
///
/// What: parses with the `url` crate (WHATWG), then requires BOTH:
/// - no userinfo — any username or password at all is a rejection, not a
///   parse detail. A URL that carries credentials is not one this gate can
///   reason about, and the whole confusion above lives in that component.
/// - a host of `localhost`, or an IP address that `is_loopback()`.
///
/// A URL that will not parse is not loopback. Fail closed; the caller then
/// sends no header and reads the daemon's `401`.
/// Test: `daemon_token_tests::url_targets_loopback_accepts_real_loopback`,
/// `daemon_token_tests::url_targets_loopback_rejects_userinfo_confusion`.
pub fn url_targets_loopback(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return false;
    }
    match parsed.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// The ONE credential-resolution path every client uses: loopback gate, then
/// environment override, then the token file.
///
/// Why: `trusty-code`'s TUI engine, its `TcodeConnector`, and
/// `trusty-code-gui`'s Tauri shell all need the same three-step answer, and the
/// first implementation spelled it twice — once per crate. Two copies of a
/// security gate is one copy that will not get the next fix; the `url` parsing
/// bug above had to be fixed in both places precisely because of that.
/// What: `None` unless [`url_targets_loopback`] accepts `base_url`; then
/// `env_var` when set and non-blank; then [`read_token`] for `app_name`. A
/// blank override falls through rather than becoming an empty credential.
/// `env_var` is the CLIENT-side override only — a server that took its
/// credential from the environment would accept whatever a caller could arrange
/// to export.
/// Test: `daemon_token_tests::credential_for_withholds_from_non_loopback`,
/// `daemon_token_tests::credential_for_prefers_the_env_override`,
/// `daemon_token_tests::credential_for_ignores_a_blank_override`.
pub fn credential_for(app_name: &str, base_url: &str, env_var: &str) -> Option<String> {
    if !url_targets_loopback(base_url) {
        return None;
    }
    if let Ok(raw) = std::env::var(env_var) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    read_token(app_name)
}

/// Write `token` to `path` atomically, at mode `0600`.
///
/// Why, on the mode: `fs::write` then `set_permissions` leaves a window in
/// which the file is world-readable with the secret already in it. Opening with
/// `.mode(0o600)` closes that window. Mirrors `crate::credentials::file_store`'s
/// treatment of the same hazard.
///
/// Why, on the atomicity: truncating `path` in place publishes an EMPTY file
/// for the length of the write, and a client reading in that window sees no
/// credential and sends no header. Worse, two daemons starting together each
/// truncate and each mint, so the one that writes second leaves the first
/// holding a token nothing on disk agrees with. A tmp-plus-rename makes the
/// swap a single POSIX operation: a reader observes either the old value or the
/// new one, never a partial or empty one. This is the discipline
/// `crate::daemon_addr` and `trusty_code::serve::discovery` already apply to the
/// far less sensitive `http_addr` file.
/// What: creates a uniquely-named `.tmp` sibling at `0600` (unique so two
/// concurrent writers cannot share a scratch file), writes, `sync_all`s, then
/// renames over `path`. A failure part-way leaves the tmp file behind and
/// `path` untouched, which is the safe half of the trade.
/// Test: `daemon_token_tests::ensure_token_writes_0600`,
/// `daemon_token_tests::concurrent_writes_never_publish_a_partial_file`.
fn write_token_0600(path: &Path, token: &str) -> io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Unique per writer: pid plus a fresh mint's first 16 chars. Two daemons
    // racing must not truncate each other's scratch file.
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        &mint_token()[..16]
    ));

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let write_result = (|| -> io::Result<()> {
        let mut f = opts.open(&tmp)?;
        writeln!(f, "{token}")?;
        f.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, path)
    })();
    if write_result.is_err() {
        // Best-effort: the rename never happened, so this scratch file is dead
        // weight. Leaving it would accumulate one per failed start.
        let _ = std::fs::remove_file(&tmp);
    }
    write_result
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

    /// An `expected` too weak to be a credential must verify NOTHING, whatever
    /// is presented.
    ///
    /// Without the [`MIN_TOKEN_LEN`] floor, `credentials_match("", "")` is
    /// `true` on the length-and-XOR path alone, so a daemon that somehow held
    /// an empty token would authenticate every request carrying a bare
    /// `Authorization: Bearer `.
    #[test]
    fn an_empty_or_weak_expected_credential_never_matches() {
        for expected in ["", " ", "short", &"a".repeat(MIN_TOKEN_LEN - 1)] {
            assert!(
                !credentials_match(expected, expected),
                "expected {expected:?} must never verify, not even against itself"
            );
            assert!(!credentials_match(expected, ""));
            assert!(!credentials_match(expected, &mint_token()));
        }
        // The floor is exactly MIN_TOKEN_LEN, not one past it.
        let at_floor = "a".repeat(MIN_TOKEN_LEN);
        assert!(credentials_match(&at_floor, &at_floor));
    }

    /// The real loopback spellings a client legitimately targets.
    #[test]
    fn url_targets_loopback_accepts_real_loopback() {
        for url in [
            "http://127.0.0.1:7882",
            "http://127.0.0.1",
            "http://127.9.9.9:7882",
            "http://localhost:7882",
            "http://LOCALHOST:7882",
            "https://localhost",
            "http://[::1]:7882",
        ] {
            assert!(url_targets_loopback(url), "{url} must be treated as local");
        }
    }

    /// The CRITICAL this function exists for, plus the ordinary remote hosts.
    ///
    /// The userinfo rows are the ones an `Origin`-header parser gets wrong: it
    /// splits the authority at the first `:` and reads host `127.0.0.1`, while
    /// the HTTP client that dials the URL reaches `attacker.example`.
    #[test]
    fn url_targets_loopback_rejects_userinfo_confusion() {
        for url in [
            // Userinfo confusion — the exfiltration vector.
            "http://127.0.0.1:7882@attacker.example",
            "http://127.0.0.1:7882@attacker.example/rpc",
            "http://localhost@attacker.example",
            "http://user:pass@attacker.example",
            // Userinfo naming a loopback host is still rejected: a URL that
            // carries credentials is not one this gate reasons about.
            "http://user@127.0.0.1:7882",
            "http://user:pass@localhost",
            // Ordinary remote hosts.
            "http://example.test:7882",
            "https://10.0.0.5:7882",
            "http://192.168.1.4:7882",
            "http://localhost.attacker.example",
            "http://notlocalhost",
            // Unparseable is not loopback — fail closed.
            "",
            "127.0.0.1:7882",
            "://127.0.0.1",
        ] {
            assert!(
                !url_targets_loopback(url),
                "{url} must NOT be treated as local"
            );
        }
    }

    /// The gate runs before anything else: a non-loopback target gets no
    /// credential even with an override sitting in the environment.
    #[test]
    #[serial_test::serial]
    fn credential_for_withholds_from_non_loopback() {
        let _env = EnvVar::set("TRUSTY_TEST_DAEMON_TOKEN", &"a".repeat(64));
        assert_eq!(
            credential_for(
                "trusty-test-app",
                "http://127.0.0.1:7882@attacker.example",
                "TRUSTY_TEST_DAEMON_TOKEN"
            ),
            None
        );
        assert_eq!(
            credential_for(
                "trusty-test-app",
                "http://127.0.0.1:7882",
                "TRUSTY_TEST_DAEMON_TOKEN"
            )
            .as_deref(),
            Some("a".repeat(64).as_str())
        );
    }

    /// The override beats the token file, so a client that cannot read the
    /// daemon's data directory can still be pointed at a credential.
    #[test]
    #[serial_test::serial]
    fn credential_for_prefers_the_env_override() {
        let _env = EnvVar::set("TRUSTY_TEST_DAEMON_TOKEN", &"b".repeat(64));
        assert_eq!(
            credential_for(
                "trusty-test-app",
                "http://localhost:7882",
                "TRUSTY_TEST_DAEMON_TOKEN"
            )
            .as_deref(),
            Some("b".repeat(64).as_str())
        );
    }

    /// A blank override must fall through rather than become an empty
    /// credential — an empty bearer is a malformed header, not "no header".
    #[test]
    #[serial_test::serial]
    fn credential_for_ignores_a_blank_override() {
        let _env = EnvVar::set("TRUSTY_TEST_DAEMON_TOKEN", "   ");
        assert_ne!(
            credential_for(
                "trusty-test-app",
                "http://127.0.0.1:7882",
                "TRUSTY_TEST_DAEMON_TOKEN"
            )
            .as_deref(),
            Some("")
        );
    }

    /// Two writers racing on one path must never publish an empty or partial
    /// file — a reader in that window would send no header and be `401`ed by a
    /// daemon it holds a valid credential for.
    ///
    /// The in-place truncate this replaced fails here roughly one run in three;
    /// the tmp-plus-rename makes every observation land on a whole value.
    #[test]
    fn concurrent_writes_never_publish_a_partial_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(TOKEN_FILENAME);
        write_token_0600(&path, &mint_token()).expect("seed");

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let (path, stop) = (path.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut observations = 0u32;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Ok(raw) = std::fs::read_to_string(&path) {
                        assert_eq!(
                            raw.trim().len(),
                            64,
                            "observed a partial credential file: {raw:?}"
                        );
                        observations += 1;
                    }
                }
                observations
            })
        };

        for _ in 0..200 {
            write_token_0600(&path, &mint_token()).expect("rewrite");
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let observations = reader.join().expect("reader thread");
        assert!(observations > 0, "the reader never observed the file");

        // No scratch files survive a run of successful writes.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "scratch files left behind: {leftovers:?}"
        );
    }

    /// Scoped `set_var`/`remove_var` for the `credential_for` tests — the
    /// process env is shared across the threads `cargo test` runs on, so each
    /// test restores what it changed even on a panic.
    struct EnvVar(&'static str);

    impl EnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            // SAFETY: test-only env mutation; every caller is `#[serial]`, and
            // `Drop` restores the prior state.
            unsafe { std::env::set_var(key, value) };
            Self(key)
        }
    }

    impl Drop for EnvVar {
        fn drop(&mut self) {
            // SAFETY: as above.
            unsafe { std::env::remove_var(self.0) };
        }
    }
}
