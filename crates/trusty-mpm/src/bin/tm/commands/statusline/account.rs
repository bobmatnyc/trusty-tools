//! Claude Code account identity for the statusline (#6304).
//!
//! Why: the statusline already names the active `gh` identity (`@<login>`), but
//! never named the Claude Code account the session itself runs under. An
//! operator with a personal account and a work account has no way to tell which
//! one is spending the session's tokens — the same class of silent wrong-identity
//! error the `gh` segment exists to prevent. Claude Code does NOT send the
//! account in the `statusLine` stdin payload (that object carries session, cwd,
//! model, cost, context-window and rate-limit fields only), so the account has
//! to come from the config file Claude Code persists it in.
//!
//! What: [`account_segment_from_path`] reads one JSON file and returns the
//! `oauthAccount.emailAddress` rendered as `✻<email>`. [`claude_json_path`]
//! resolves which file that is: `$CLAUDE_CONFIG_DIR/.claude.json` when
//! `CLAUDE_CONFIG_DIR` is set — which is how every tm-managed session runs, and
//! that file holds a DIFFERENT account block from the home one — else
//! `~/.claude.json`. Every step is fail-soft: an unset home, a missing or
//! unreadable file, malformed JSON, an absent `oauthAccount`, or a blank email
//! all yield `None`, which omits the segment rather than rendering a
//! placeholder. Taking the path as a parameter is what lets the tests drive
//! real files without touching the operator's home directory.
//!
//! Test: `account_segment_renders_email_from_config`,
//! `account_segment_absent_when_file_missing`,
//! `account_segment_absent_when_json_is_malformed`,
//! `account_segment_absent_when_email_is_blank`,
//! `render_claude_account_segment_*`.

use std::path::{Path, PathBuf};

/// Marker prefixed to the account email in the rendered segment.
///
/// Why: the neighbouring `gh` segment renders `@<login>`, and a bare email also
/// contains an `@` — without a distinct lead-in the two identity segments read
/// as one another. `✻` is Claude Code's own glyph, so the segment says whose
/// identity it is without spending width on a word.
/// What: U+273B TEARDROP-SPOKED ASTERISK.
/// Test: `render_claude_account_segment_single`.
const CLAUDE_MARKER: char = '\u{273b}';

/// Resolve the `.claude.json` Claude Code persists this session's account in.
///
/// Why: `CLAUDE_CONFIG_DIR` relocates Claude Code's entire personal tier, and
/// every daemon-managed tm session sets it (`~/.trusty-tools/trusty-mpm/claude-config/`).
/// Reading `~/.claude.json` unconditionally would name the account of a
/// different, possibly stale, login on exactly the sessions tm launches itself.
/// What: reads `CLAUDE_CONFIG_DIR` and the home directory, then defers to
/// [`claude_json_path_from`]. The two inputs are read here and nowhere deeper
/// so the decision itself stays testable without touching the process
/// environment — this bin target bans writing either variable (#5544).
/// Test: `claude_json_path_prefers_config_dir`, `claude_json_path_falls_back_to_home`.
pub(crate) fn claude_json_path() -> Option<PathBuf> {
    // #6304: honour the relocated personal tier, else fall back to $HOME.
    claude_json_path_from(
        std::env::var("CLAUDE_CONFIG_DIR").ok().as_deref(),
        dirs::home_dir().as_deref(),
    )
}

/// Choose the config file from an explicit config dir and home dir.
///
/// Why: taking both as parameters is what lets the precedence be asserted
/// directly; the guard in `env_isolation_tests` bans a test from repointing
/// `CLAUDE_CONFIG_DIR` or `HOME` in this target, because bin-target tests share
/// one process and the write would be visible to every parallel sibling.
/// What: `<config_dir>/.claude.json` when `config_dir` is present and not
/// blank, else `<home>/.claude.json`, else `None`.
/// Test: `claude_json_path_prefers_config_dir`, `claude_json_path_falls_back_to_home`.
fn claude_json_path_from(config_dir: Option<&str>, home: Option<&Path>) -> Option<PathBuf> {
    match config_dir {
        Some(dir) if !dir.trim().is_empty() => Some(PathBuf::from(dir).join(".claude.json")),
        _ => home.map(|h| h.join(".claude.json")),
    }
}

/// The one field of `.claude.json` this module reads.
///
/// Why: `.claude.json` is large (~1 MB on a long-lived install) and its schema
/// is Claude Code's to change; deserializing into a struct with a single
/// optional field skips every other key without allocating for it, and an
/// added or renamed sibling key cannot break the read.
/// What: `oauthAccount`, itself optional (absent before first login).
/// Test: `account_segment_renders_email_from_config`.
#[derive(serde::Deserialize)]
struct ClaudeJson {
    #[serde(default, rename = "oauthAccount")]
    oauth_account: Option<OauthAccount>,
}

/// The logged-in account block Claude Code writes into `.claude.json`.
///
/// Why: the block also carries `organizationName`, but for a personal account
/// that is derived from the email (`"<email>'s Organization"`), so rendering
/// both doubles the segment's width to say the same thing twice. The email is
/// the identity Claude Code authenticates as, so it is the one field kept.
/// What: `emailAddress`, defaulting to `""` when absent.
/// Test: `account_segment_renders_email_from_config`.
#[derive(serde::Deserialize)]
struct OauthAccount {
    #[serde(default, rename = "emailAddress")]
    email_address: String,
}

/// Read `path` and render the Claude Code account segment.
///
/// Why: taking the path as a parameter (rather than resolving it internally)
/// is what makes both branches — account present, account underivable —
/// testable against real files in a temp directory, with no dependency on the
/// operator's real home directory or login state.
/// What: reads the file, parses `oauthAccount.emailAddress`, and renders it via
/// [`render_claude_account_segment`]. Returns `None` for every failure mode
/// (unreadable file, malformed JSON, no account block, blank email) so the
/// segment is omitted rather than shown as a placeholder.
/// Test: `account_segment_renders_email_from_config`,
/// `account_segment_absent_when_file_missing`,
/// `account_segment_absent_when_json_is_malformed`,
/// `account_segment_absent_when_email_is_blank`.
pub(crate) fn account_segment_from_path(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let parsed: ClaudeJson = serde_json::from_str(&raw).ok()?;
    let email = parsed.oauth_account?.email_address;
    render_claude_account_segment(Some(&email))
}

/// Render the `✻<email>` segment from a resolved account email.
///
/// Why: keeping the render pure (no file I/O) pins the present/absent cases
/// without a filesystem, and keeps the marker in one place.
/// What: `Some("✻<email>")` for a non-blank email; `None` for `None` or a blank
/// one, which omits the segment entirely.
/// Test: `render_claude_account_segment_single`,
/// `render_claude_account_segment_absent`.
fn render_claude_account_segment(email: Option<&str>) -> Option<String> {
    let email = email?.trim();
    if email.is_empty() {
        return None;
    }
    Some(format!("{CLAUDE_MARKER}{email}"))
}

/// Probe the Claude Code account at `path`, fail-soft, ≤100 ms.
///
/// Why (#6304): the render path Claude Code calls on every cycle must never
/// block, and `.claude.json` can be a megabyte on a cold page cache. The
/// bounded-thread pattern here matches the sibling `gh` and git/tmux probes:
/// a read that outruns its budget costs the segment, never the statusline.
/// What: spawns a detached thread that reads `path` via
/// [`account_segment_from_path`], waits ≤100 ms, and returns `None` on timeout,
/// on any read failure, or when `path` is `None` (no resolvable config).
/// Test: `render_statusline_shows_claude_account_when_config_is_present` drives
/// it through the renderer with an injected path; the read and render it wraps
/// are unit-tested directly.
pub(crate) fn claude_account_segment_probe(path: Option<&Path>) -> Option<String> {
    use std::sync::mpsc;
    use std::time::Duration;

    let path = path?.to_path_buf();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(account_segment_from_path(&path));
    });
    rx.recv_timeout(Duration::from_millis(100)).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// Write `body` to a fresh temp file and return it (kept alive by the caller).
    fn temp_json(body: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().expect("temp file");
        f.write_all(body.as_bytes()).expect("write");
        f.flush().expect("flush");
        f
    }

    /// Why (#6304, branch b): the account must be rendered when the config file
    /// Claude Code persists it in is present and readable. Uses the real key
    /// spelling observed in `.claude.json` (`oauthAccount.emailAddress`) beside
    /// the sibling keys that must be ignored.
    /// Test: itself.
    #[test]
    fn account_segment_renders_email_from_config() {
        let f = temp_json(
            r#"{
                "numStartups": 412,
                "oauthAccount": {
                    "accountUuid": "0000-1111",
                    "emailAddress": "someone@example.com",
                    "organizationName": "someone@example.com's Organization",
                    "organizationRole": "admin"
                },
                "projects": {}
            }"#,
        );
        assert_eq!(
            account_segment_from_path(f.path()).as_deref(),
            Some("\u{273b}someone@example.com")
        );
    }

    /// Why (#6304, branch c): an absent config file — a first run, or a
    /// `CLAUDE_CONFIG_DIR` that has not been provisioned — must omit the
    /// segment, never render a placeholder.
    /// Test: itself.
    #[test]
    fn account_segment_absent_when_file_missing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let missing = dir.path().join("does-not-exist.json");
        assert_eq!(account_segment_from_path(&missing), None);
    }

    /// Why (#6304, branch c): `.claude.json` is truncated or half-written often
    /// enough that this repo keeps `.corrupt-*` quarantine copies of it. A
    /// malformed file must omit the segment rather than panic on the hot render
    /// path.
    /// Test: itself.
    #[test]
    fn account_segment_absent_when_json_is_malformed() {
        let f = temp_json(r#"{"oauthAccount": {"emailAddress": "trunc"#);
        assert_eq!(account_segment_from_path(f.path()), None);

        // Well-formed JSON with no account block at all (pre-login state).
        let f = temp_json(r#"{"numStartups": 1}"#);
        assert_eq!(account_segment_from_path(f.path()), None);
    }

    /// Why (#6304, branch c): a present-but-blank email must omit the segment
    /// rather than emit a bare `✻`.
    /// Test: itself.
    #[test]
    fn account_segment_absent_when_email_is_blank() {
        let f = temp_json(r#"{"oauthAccount": {"emailAddress": "   "}}"#);
        assert_eq!(account_segment_from_path(f.path()), None);

        let f = temp_json(r#"{"oauthAccount": {"organizationName": "Acme"}}"#);
        assert_eq!(account_segment_from_path(f.path()), None);
    }

    /// Why: the marker distinguishes this segment from the neighbouring
    /// `@<login>` gh segment; pin both the marker and the layout.
    /// Test: itself.
    #[test]
    fn render_claude_account_segment_single() {
        let seg = render_claude_account_segment(Some("someone@example.com")).expect("segment");
        assert_eq!(seg, "\u{273b}someone@example.com");
        assert!(!seg.starts_with('@'), "must not read as a gh login: {seg}");
    }

    /// Why: an absent or blank email omits the segment entirely.
    /// Test: itself.
    #[test]
    fn render_claude_account_segment_absent() {
        assert_eq!(render_claude_account_segment(None), None);
        assert_eq!(render_claude_account_segment(Some("")), None);
        assert_eq!(render_claude_account_segment(Some("  \t ")), None);
    }

    /// Why (#6304): every daemon-managed tm session sets `CLAUDE_CONFIG_DIR`,
    /// and the account block under it differs from the one in `$HOME`. The
    /// resolver must prefer it. Both inputs are passed in rather than exported:
    /// `env_isolation_tests` bans this target from writing either variable,
    /// because its tests share one process (#5544).
    /// Test: itself.
    #[test]
    fn claude_json_path_prefers_config_dir() {
        let resolved = claude_json_path_from(Some("/cfg"), Some(Path::new("/home/user")));
        assert_eq!(resolved.as_deref(), Some(Path::new("/cfg/.claude.json")));
    }

    /// Why (#6304): an unset — or blank — `CLAUDE_CONFIG_DIR` must fall back to
    /// `~/.claude.json`, which is where an unmanaged `claude` session logs in.
    /// A blank one must not resolve to `/.claude.json`.
    /// Test: itself.
    #[test]
    fn claude_json_path_falls_back_to_home() {
        let home = Path::new("/home/user");
        for cfg in [None, Some(""), Some("   ")] {
            assert_eq!(
                claude_json_path_from(cfg, Some(home)).as_deref(),
                Some(Path::new("/home/user/.claude.json")),
                "cfg={cfg:?}"
            );
        }
        // No home and no config dir → nothing to read, segment omitted.
        assert_eq!(claude_json_path_from(None, None), None);
    }
}
