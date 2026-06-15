//! Real OS-backed seams for STAGE 3 (console URL) and STAGE 4b (Claude Code).
//!
//! Why: `stages.rs` is written against the `ConsoleLocator` and `ClaudeEnv`
//! traits so it can be unit-tested; this file provides the production
//! implementations that actually shell out to `trusty-console port --json`,
//! `which claude`, `claude --version`, `claude update`, and npm.
//!
//! What: `OsConsoleLocator` discovers the console URL via its `port --json`
//! contract verb; `OsClaudeEnv` implements the §6 probes/upgrade/confirm and the
//! interactive `[Y/n]` prompt (reading a line from stdin).
//!
//! Test: Side-effect-only (real subprocesses + stdin); the pure decision logic
//! that consumes these is unit-tested via the trait doubles in `super::tests`.
//! `parse_console_port` is unit-tested directly.

use std::io::Write;
use std::process::Command;

use super::claude_code::{ClaudeEnv, MANAGED_SPAWN_COMMAND};
use super::stages::ConsoleLocator;

/// Production `ConsoleLocator` over `trusty-console port --json`.
///
/// Why: STAGE 3 prints the console URL discovered from the console's own
/// `port --json` contract (DOC-12 §7 — the console owns its port).
///
/// What: Spawns `trusty-console port --json`, parses `{addr,port}`, builds an
/// `http://addr:port` URL.
///
/// Test: `parse_console_port` is unit-tested; the spawn is side-effect-only.
#[derive(Debug, Default)]
pub struct OsConsoleLocator;

/// Parse a `trusty-console port --json` envelope into an `http://…` URL.
///
/// Why: Isolating the parse from the spawn makes the URL shaping unit-testable.
///
/// What: Reads `addr` (default `127.0.0.1`) and `port` from the JSON; returns
/// `http://{addr}:{port}`. Returns `None` if `port` is absent.
///
/// Test: `super::tests::console_url_parses`.
pub fn parse_console_port(json: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    let port = v.get("port").and_then(|p| p.as_u64())?;
    let addr = v
        .get("addr")
        .and_then(|a| a.as_str())
        .unwrap_or("127.0.0.1");
    // If addr already includes a port, prefer it verbatim; else compose.
    if addr.contains(':') {
        Some(format!("http://{addr}"))
    } else {
        Some(format!("http://{addr}:{port}"))
    }
}

impl ConsoleLocator for OsConsoleLocator {
    fn console_url(&self) -> Option<String> {
        let out = Command::new("trusty-console")
            .args(["port", "--json"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        parse_console_port(&out.stdout)
    }
}

/// Production `ClaudeEnv` driving the real `claude` CLI + npm (DOC-12 §6).
///
/// Why: The concrete STAGE 4b environment.
///
/// What: Each method shells out to the corresponding tool; `prompt_upgrade`
/// reads a line from stdin; `is_tty` queries the controlling terminal.
///
/// Test: Side-effect-only; the consuming decision logic is unit-tested via the
/// `ScriptedClaudeEnv` double in `super::tests`.
#[derive(Debug, Default)]
pub struct OsClaudeEnv;

impl ClaudeEnv for OsClaudeEnv {
    fn present(&self) -> bool {
        which::which("claude").is_ok()
    }

    fn installed_version(&self) -> Option<String> {
        let out = Command::new("claude").arg("--version").output().ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout);
        // `claude --version` prints e.g. "1.2.3 (Claude Code)"; take the first token.
        s.split_whitespace().next().map(|t| t.to_owned())
    }

    fn latest_version(&self) -> Option<String> {
        // Prefer the npm-published latest as the canonical comparison channel
        // (the native `claude update` decides its own latest when invoked).
        let out = Command::new("npm")
            .args(["view", "@anthropic-ai/claude-code", "version"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout);
        s.split_whitespace().next().map(|t| t.to_owned())
    }

    fn upgrade(&self) -> anyhow::Result<()> {
        // Prefer the CLI's own native updater; fall back to npm @latest.
        let native = Command::new("claude").arg("update").status();
        if let Ok(st) = native {
            if st.success() {
                return Ok(());
            }
        }
        let st = Command::new("npm")
            .args(["i", "-g", "@anthropic-ai/claude-code@latest"])
            .status()?;
        if st.success() {
            Ok(())
        } else {
            anyhow::bail!("npm upgrade of @anthropic-ai/claude-code exited with {st}")
        }
    }

    fn confirm_managed_path(&self) -> bool {
        // The managed path is `env -u ANTHROPIC_API_KEY claude`; re-verifying
        // reduces to the same presence check the adapter uses (the `env` prefix
        // does not change `claude`'s resolvability).
        let _ = MANAGED_SPAWN_COMMAND;
        which::which("claude").is_ok()
    }

    fn prompt_upgrade(&self, installed: &str, latest: &str) -> bool {
        // Prompt on stderr (stdout is reserved for data), read consent on stdin.
        eprint!("claude {installed} is installed; latest is {latest}. Update now? [Y/n] ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return false;
        }
        let ans = line.trim().to_ascii_lowercase();
        ans.is_empty() || ans == "y" || ans == "yes"
    }

    fn is_tty(&self) -> bool {
        // SAFETY: isatty has no preconditions; STDIN_FILENO is always valid.
        unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
    }
}
