//! Handler for `trusty-search setup`.
//!
//! Why: `trusty-search integrate cursor` was the only one-shot bootstrap path
//! and it only targets Cursor. Claude Code users had to hand-edit
//! `~/.claude/settings.json` to register the MCP server — which is brittle,
//! divergent from `trusty-memory setup`, and easy to get wrong on machines
//! with multiple per-project Claude settings files. `trusty-search setup`
//! mirrors `trusty-memory setup`: it scans `$HOME` for every Claude Code
//! settings file via the shared discovery helpers, idempotently upserts the
//! trusty-search MCP server entry into each one, and falls back to creating
//! `~/.claude/settings.json` when none exist.
//!
//! Note: trusty-search does NOT install hooks. The MCP tools (`search`,
//! `index_status`, etc.) are invoked on-demand by the model; nothing about
//! trusty-search needs to fire on every prompt the way trusty-memory's
//! prompt-context block does, so a hook would just add overhead with no
//! payoff.
//!
//! Test: unit tests cover the patch loop against tempdir-rooted settings
//! files. The discovery walk is exercised by `trusty_common::claude_config`.

use anyhow::Result;
use colored::Colorize;
use std::path::Path;
use trusty_common::claude_config::{
    default_settings_max_depth, discover_claude_settings, mcp_server_entry, patch_mcp_server,
};

/// Canonical MCP server key trusty-search is registered under in Claude
/// settings files.
///
/// Why: keeping the literal in one place prevents the integrate/setup/migrate
/// commands from drifting (e.g. one writing `trusty-search` and another
/// writing `trusty_search`).
/// What: the literal string `"trusty-search"`.
const MCP_SERVER_KEY: &str = "trusty-search";

/// Entry point for `trusty-search setup`.
///
/// Why: a single command that wires trusty-search into every Claude Code
/// settings file on the machine. Idempotent: re-running it produces no
/// further writes and reports "already configured" per file.
/// What: scans `$HOME` via
/// [`trusty_common::claude_config::discover_claude_settings`], upserts the
/// MCP server entry into each discovered file, and falls back to creating
/// `~/.claude/settings.json` when none are found. Per-file failures are
/// non-fatal — they print a red diagnostic and the loop continues.
/// Test: `setup_creates_fallback_settings_file`,
/// `setup_patches_existing_settings_file`, `setup_is_idempotent`.
pub fn handle_setup() -> Result<()> {
    println!(
        "{} Setting up trusty-search for Claude Code…\n",
        "·".dimmed()
    );

    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?;
    println!(
        "{} Scanning for Claude settings under {}…",
        "·".dimmed(),
        home.display()
    );

    let entry = mcp_server_entry(MCP_SERVER_KEY, &["serve"]);
    let files = discover_claude_settings(&home, default_settings_max_depth());

    let changed = if files.is_empty() {
        let fallback = home.join(".claude").join("settings.json");
        println!(
            "{} No Claude settings files found. Creating {}…",
            "·".dimmed(),
            fallback.display()
        );
        patch_one_with_report(&fallback, &entry, /* fresh */ true)?
    } else {
        println!(
            "{} Found {} settings file(s). Patching each…",
            "·".dimmed(),
            files.len()
        );
        let mut n = 0usize;
        for path in &files {
            n += patch_one_with_report(path, &entry, /* fresh */ false)?;
        }
        n
    };

    println!();
    if changed > 0 {
        println!(
            "{} Setup complete — updated {} settings file{}.",
            "✓".green(),
            changed,
            if changed == 1 { "" } else { "s" }
        );
    } else {
        println!(
            "{} Setup complete — all settings files already configured.",
            "✓".green()
        );
    }
    println!(
        "  Restart Claude Code (or reload MCP servers) to pick up `{}`.",
        MCP_SERVER_KEY.cyan()
    );
    Ok(())
}

/// Patch a single Claude settings file and print a one-line status.
///
/// Why: the per-file accounting and the per-file UI line should always agree,
/// so the helper does both at once. Returning `1`/`0` lets `handle_setup`
/// sum the writes across discovered files.
/// What: calls [`patch_mcp_server`] — which is idempotent (skips writes when
/// the entry is already byte-equal) — and prints `✓ written`, `↻ already
/// configured`, or `+ created` depending on outcome and the `fresh` flag.
/// Returns `1` when the file was modified or created and `0` otherwise.
/// Test: `setup_patches_existing_settings_file`,
/// `setup_creates_fallback_settings_file`.
fn patch_one_with_report(path: &Path, entry: &serde_json::Value, fresh: bool) -> Result<usize> {
    match patch_mcp_server(path, MCP_SERVER_KEY, entry) {
        Ok(true) => {
            let label = if fresh { "+ created" } else { "✓ added" };
            println!("  {} {}", label.green(), path.display());
            Ok(1)
        }
        Ok(false) => {
            println!(
                "  {} {} {}",
                "↻".cyan(),
                path.display().to_string().dimmed(),
                "(already configured)".dimmed()
            );
            Ok(0)
        }
        Err(e) => {
            eprintln!(
                "  {} {} {}",
                "✗".red(),
                path.display(),
                format!("({e})").red()
            );
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Why: patching a fresh settings file must produce a valid `mcpServers`
    /// block carrying the canonical `trusty-search` entry.
    /// What: invokes `patch_one_with_report` against a non-existent path,
    /// asserts the file is created with the expected JSON shape.
    #[test]
    fn setup_creates_fallback_settings_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        let entry = mcp_server_entry(MCP_SERVER_KEY, &["serve"]);

        let n = patch_one_with_report(&path, &entry, true).expect("patch ok");
        assert_eq!(n, 1);

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["mcpServers"][MCP_SERVER_KEY]["command"], "trusty-search");
        assert_eq!(v["mcpServers"][MCP_SERVER_KEY]["args"][0], "serve");
    }

    /// Why: an existing file already carrying the trusty-search entry must
    /// be left untouched on a second run.
    /// What: writes a settings file with the entry already present, runs
    /// `patch_one_with_report`, asserts no rewrite happened.
    #[test]
    fn setup_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        let entry = mcp_server_entry(MCP_SERVER_KEY, &["serve"]);
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "mcpServers": { MCP_SERVER_KEY: entry }
            }))
            .unwrap(),
        )
        .unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let n = patch_one_with_report(&path, &entry, false).expect("patch ok");
        assert_eq!(n, 0, "no-op when already configured");
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(before, after, "file must not change on no-op");
    }

    /// Why: the patch must preserve every unrelated MCP server entry and
    /// every unrelated top-level key. Anything else is a regression.
    /// What: seeds a file with other servers and a top-level key, patches,
    /// asserts everything pre-existing survives.
    #[test]
    fn setup_preserves_unrelated_keys() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        let seed = json!({
            "theme": "light",
            "mcpServers": {
                "other": { "command": "other", "args": [] }
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&seed).unwrap()).unwrap();

        let entry = mcp_server_entry(MCP_SERVER_KEY, &["serve"]);
        let n = patch_one_with_report(&path, &entry, false).expect("patch ok");
        assert_eq!(n, 1);

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["theme"], "light");
        let servers = v["mcpServers"].as_object().unwrap();
        assert!(servers.contains_key("other"));
        assert!(servers.contains_key(MCP_SERVER_KEY));
    }
}
