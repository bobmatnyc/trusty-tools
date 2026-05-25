//! Handler for `trusty-memory setup`.
//!
//! Why: first-time users want a single command that installs the launchd
//! service, creates the data directory, and registers `trusty-memory` as an
//! MCP server in every Claude settings file on the machine. Doing this
//! piecewise (manual plist install, hand-edit settings.json, restart Claude)
//! is brittle and error-prone — `setup` makes it a one-liner that leans on
//! the shared `trusty_common::{launchd, claude_config}` modules so the
//! behaviour stays in lockstep with `trusty-search setup` and any future
//! trusty-* tool.
//! What: orchestrates three phases:
//!   1. Creates `<data_dir>/trusty-memory/` (e.g. `~/Library/Application
//!      Support/trusty-memory` on macOS).
//!   2. On macOS, installs and bootstraps the launchd LaunchAgent via the
//!      shared `LaunchdConfig`. On other platforms, this phase is skipped
//!      with a friendly note.
//!   3. Patches every discovered Claude settings file with an MCP server
//!      entry pointing at `trusty-memory serve`. Falls back to creating
//!      `~/.claude/settings.json` when no settings files were found.
//!
//! Test: unit tests cover the patch phase against tempdir-rooted settings
//! files. The launchd phase is side-effecting (macOS only) and exercised
//! manually via `cargo run -p trusty-memory -- setup`.

use anyhow::{Context, Result};
use colored::Colorize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use trusty_common::claude_config::{
    default_settings_max_depth, discover_claude_settings, mcp_server_entry, merge_hook_entries,
    patch_mcp_server, write_json_atomic,
};

/// Canonical MCP server key used in Claude settings files.
///
/// Why: the same key is used by the migrate command and the patch phase
/// here; defining it once prevents the two from drifting (e.g. one writing
/// `trusty-memory` and the other writing `trusty_memory`).
/// What: the literal string `"trusty-memory"`.
/// Test: covered by every test in this module that asserts the key is
/// present after a patch.
const MCP_SERVER_KEY: &str = "trusty-memory";

/// The Claude Code hook event the UserPromptSubmit hook is registered under.
///
/// Why: Claude Code routes hooks to one of a handful of well-known events;
/// `UserPromptSubmit` fires before every user-typed prompt and is the only
/// event whose stdout is injected into the model's next message as additional
/// context. That makes it the right place to surface the palace's prompt
/// facts on every message without paying the per-call MCP tool tax.
/// What: the literal `"UserPromptSubmit"` string Claude Code expects in the
/// settings JSON.
const HOOK_EVENT: &str = "UserPromptSubmit";

/// The Claude Code hook event the inbox-check hook is registered under
/// (issue #99).
///
/// Why: `SessionStart` fires exactly once at the beginning of a new Claude
/// Code session and Claude Code injects the hook's stdout as context for
/// that session. That makes it the right place to deliver inter-project
/// messages that arrived since the previous session — they appear in the
/// model's working context immediately, without polling.
/// What: the literal `"SessionStart"` string Claude Code expects in the
/// settings JSON.
const SESSION_START_HOOK_EVENT: &str = "SessionStart";

/// Shell command Claude Code invokes for the UserPromptSubmit hook.
///
/// Why: routing through the installed `trusty-memory` binary (rather than a
/// raw curl + jq pipeline) means the hook benefits from the central
/// `CLAUDE_MPM_SUB_AGENT` guard, the soft-failure semantics, and the
/// `read_daemon_addr` discovery — none of which can be replicated in a
/// shell one-liner.
/// What: the bare command. Claude Code resolves it via PATH; if the user has
/// installed trusty-memory via `cargo install`, it will be on PATH.
const HOOK_COMMAND: &str = "trusty-memory prompt-context";

/// Shell command Claude Code invokes for the SessionStart hook (issue #99).
///
/// Why: the receiver's inter-project inbox is delivered exactly once per
/// session; `inbox-check` does the daemon round-trip, prints the messages
/// to stdout (where Claude Code injects them), and atomically marks them
/// read so the next session does not redeliver.
/// What: the bare command, found by Claude Code via PATH.
const INBOX_CHECK_HOOK_COMMAND: &str = "trusty-memory inbox-check";

/// Hook command timeout in milliseconds.
///
/// Why: Claude Code blocks the user's prompt until the hook exits, so the
/// timeout must be larger than the daemon's worst-case response latency
/// (HTTP round-trip + prompt-fact rendering) but small enough that a
/// completely dead daemon still releases the prompt within a few seconds.
/// 3 000 ms is the value used across the rest of the trusty-* setup tooling.
const HOOK_TIMEOUT_MS: u64 = 3_000;

/// Entry point for `trusty-memory setup`.
///
/// Why: a first-time-install command that wires up everything a user needs
/// to run trusty-memory from Claude Code with one invocation.
/// What: runs the three phases (data dir → launchd → Claude settings) in
/// order. A failure in the launchd phase is fatal on macOS (we want to
/// fail loud so the user can fix it), but Claude settings phase failures
/// for individual files are non-fatal — we log and continue.
/// Test: integration via `cargo run -p trusty-memory -- setup`; unit tests
/// cover the patch phase against fixture settings files.
pub fn handle_setup() -> Result<()> {
    println!("{} Setting up trusty-memory…\n", "·".dimmed());

    // Phase 1: data directory.
    let data_dir = ensure_data_dir()?;
    println!("{} Data directory: {}", "✓".green(), data_dir.display());

    // Phase 2: launchd (macOS only).
    install_service_phase()?;

    // Phase 2b: pre-warm the embedder model cache. This downloads ~22 MB
    // of ONNX into `$HOME/.cache/fastembed` before launchd ever starts the
    // daemon, so the first `memory_recall` request does not have to wait
    // for (and the read-only `TMPDIR` does not silently break) the model
    // retrieval (GH #58).
    prewarm_embedder_phase();

    // Phase 3: Claude settings patching (MCP entry + UserPromptSubmit hook).
    let SettingsPatchSummary {
        mcp_changed,
        hooks_changed,
    } = patch_claude_settings_phase()?;

    println!("\n{} Setup complete!", "✓".green());
    if mcp_changed > 0 {
        println!(
            "  Updated {} Claude settings file{} with the MCP server entry.",
            mcp_changed,
            if mcp_changed == 1 { "" } else { "s" }
        );
    }
    if hooks_changed > 0 {
        println!(
            "  Installed UserPromptSubmit hook into {} settings file{}.",
            hooks_changed,
            if hooks_changed == 1 { "" } else { "s" }
        );
    }
    println!(
        "  Try: {} (or restart Claude Code to pick up the new MCP server)",
        "trusty-memory serve".cyan()
    );
    Ok(())
}

/// Create the user data directory for trusty-memory.
///
/// Why: `trusty-memory serve` reads/writes its palace files under this
/// directory; pre-creating it during setup avoids first-run race conditions
/// and lets us surface permission failures up-front.
/// What: resolves `<data_dir>/trusty-memory` via [`dirs::data_dir`] and
/// creates it (and any missing parents). Returns the resolved path.
/// Test: `setup_creates_data_dir_under_override` exercises the happy path
/// with a tempdir-based override of `dirs::data_dir`.
fn ensure_data_dir() -> Result<PathBuf> {
    let base =
        dirs::data_dir().ok_or_else(|| anyhow::anyhow!("could not resolve user data directory"))?;
    let dir = base.join("trusty-memory");
    std::fs::create_dir_all(&dir).with_context(|| format!("create data dir {}", dir.display()))?;
    Ok(dir)
}

/// Install the launchd service (macOS) or skip with a note (other platforms).
///
/// Why: keeps the platform-specific logic in one place so `handle_setup`
/// can read top-to-bottom without `#[cfg]` blocks. On macOS the service is
/// the canonical way to keep the daemon alive across logins; on Linux /
/// Windows we expect operators to use systemd / Task Scheduler directly
/// and don't try to forge a half-working wrapper.
/// What: on macOS, calls `LaunchdConfig::install()` + `.bootstrap()`. On
/// other platforms, prints a one-line skip notice and returns Ok.
/// Test: side-effecting on macOS; covered manually. Other platforms hit the
/// no-op path during `cargo test -p trusty-memory` on Linux CI.
fn install_service_phase() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        use crate::commands::service::{build_launchd_config, launchd_log_dir, LAUNCHD_LABEL};

        let exe = std::env::current_exe()
            .map_err(|e| anyhow::anyhow!("could not resolve current exe: {e}"))?;
        let log_dir = launchd_log_dir()?;
        let cfg = build_launchd_config(exe, log_dir.clone());
        cfg.install().context("install LaunchAgent plist")?;
        println!(
            "{} Installed LaunchAgent: {}",
            "✓".green(),
            cfg.plist_path()?.display()
        );

        cfg.bootstrap()
            .context("bootstrap LaunchAgent into user gui domain")?;
        println!(
            "{} Loaded {} (daemon will auto-start; logs in {}).",
            "✓".green(),
            LAUNCHD_LABEL,
            log_dir.display().to_string().dimmed()
        );
    }
    #[cfg(not(target_os = "macos"))]
    {
        println!(
            "{} Skipping launchd install (not macOS) — use your distro's \
             service manager to run `trusty-memory serve` on demand.",
            "·".dimmed()
        );
    }
    Ok(())
}

/// Pre-warm the fastembed ONNX model cache before launchd ever starts the
/// daemon.
///
/// Why: GH #58 — under launchd, `TMPDIR` is mounted read-only for the
/// agent's UID, so fastembed's first `TextEmbedding::try_new` fails with
/// `EROFS (os error 30)` and the HTTP daemon never becomes ready. Even
/// with `FASTEMBED_CACHE_DIR` correctly set in the plist, downloading the
/// ~22 MB model on the daemon's first request introduces latency and
/// failure modes (network blips, slow ANE compile). Pre-warming during
/// `setup` — which runs in the user's normal shell with full network and
/// HOME access — moves both the download and the ONNX session warmup
/// off the daemon's critical path and surfaces failures up-front where the
/// user can act on them.
/// What: explicitly sets `FASTEMBED_CACHE_DIR` to `$HOME/.cache/fastembed`
/// (the same path the launchd plist will use), then spins up a single-
/// threaded tokio runtime to drive `FastEmbedder::new()`. Failures are
/// reported as warnings — they do not abort `setup` because the daemon
/// will retry on its own startup, but a successful pre-warm is the
/// difference between "instant first recall" and "users see EROFS errors".
/// Test: side-effecting (network + filesystem); covered manually via
/// `cargo run -p trusty-memory -- setup`.
fn prewarm_embedder_phase() {
    let cache_dir = trusty_common::embedder::resolve_fastembed_cache_dir();
    // SAFETY: setup runs single-threaded before any worker spawns.
    unsafe {
        std::env::set_var("FASTEMBED_CACHE_DIR", &cache_dir);
    }
    if let Err(e) = std::fs::create_dir_all(&cache_dir) {
        eprintln!(
            "  {} could not create {} ({e}) — daemon will retry on first request.",
            "·".dimmed(),
            cache_dir.display()
        );
        return;
    }

    println!(
        "\n{} Pre-warming embedder model cache at {}…",
        "·".dimmed(),
        cache_dir.display()
    );

    // `prewarm_embedder_phase` is called from a `#[tokio::main]` context, so
    // we must not call `block_on` on the current thread directly (that panics
    // with "Cannot start a runtime from within a runtime"). `block_in_place`
    // moves the blocking work off the async thread pool so we can build a
    // dedicated single-thread runtime safely.
    let result = tokio::task::block_in_place(|| {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!(
                    "  {} could not build tokio runtime for pre-warm ({e}); skipping.",
                    "·".dimmed()
                );
                return None;
            }
        };
        Some(rt.block_on(trusty_common::embedder::FastEmbedder::new()))
    });

    let result = match result {
        None => return,
        Some(r) => r,
    };

    match result {
        Ok(_e) => {
            println!(
                "{} Embedder model cached. First recall after daemon start will be instant.",
                "✓".green()
            );
        }
        Err(e) => {
            // Non-fatal: daemon will retry on its own. Surface the error
            // loudly so the operator can intervene (e.g. fix offline
            // proxy, free disk space) before launchd hits the same wall.
            eprintln!(
                "  {} pre-warm failed ({e}). The daemon will retry on first request — \
                 if this persists, inspect {} for partial downloads.",
                "✗".red(),
                cache_dir.display()
            );
        }
    }
}

/// Per-file outcome of [`patch_one`].
///
/// Why: the patch phase tracks MCP-server and hook changes separately so
/// the summary banner can report each independently. Returning the two
/// counts together (rather than a single mutated `bool`) keeps idempotency
/// reporting precise.
/// What: `mcp_wrote = true` when the MCP server entry changed on disk;
/// `hook_wrote = true` when the UserPromptSubmit hook block changed.
/// Test: `patch_one_creates_missing_file`, `patch_one_is_idempotent`,
/// `patch_one_installs_hook`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct PatchOutcome {
    mcp_wrote: bool,
    hook_wrote: bool,
}

impl PatchOutcome {
    fn any(&self) -> bool {
        self.mcp_wrote || self.hook_wrote
    }
}

/// Aggregate result of the Claude-settings phase across every discovered file.
///
/// Why: `handle_setup` renders separate "MCP" and "hook" lines in the final
/// summary banner; tracking the two counts independently keeps the line
/// rendering honest about exactly what changed.
/// What: one count per kind of mutation.
/// Test: `setup_phase_counts_mcp_and_hooks_separately` (covered indirectly
/// by `patch_one_*` tests).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct SettingsPatchSummary {
    mcp_changed: usize,
    hooks_changed: usize,
}

/// Patch every discovered Claude settings file (or fall back to
/// `~/.claude/settings.json`) so it registers `trusty-memory` as an MCP
/// server **and** carries the trusty-memory UserPromptSubmit hook.
///
/// Why: Claude Code only loads MCP servers it knows about; without the MCP
/// step `setup` would install the daemon but Claude would never call it.
/// And without the UserPromptSubmit hook, the model would have to invoke a
/// per-message MCP tool to get the prompt-context block — a token-tax that
/// the hook avoids by injecting the block on every prompt. Walking every
/// settings file matters because users frequently have both a global
/// `~/.claude/settings.json` and per-project `<repo>/.claude/settings.local.json`
/// files.
/// What: discovers settings files via
/// [`trusty_common::claude_config::discover_claude_settings`], then calls
/// [`patch_one`] for each — which idempotently upserts the MCP server entry
/// and idempotently merges the UserPromptSubmit hook. Both helpers are
/// idempotent by design (deep equality dedup), so re-running setup is safe.
/// When no files are found, falls back to creating `~/.claude/settings.json`.
/// Test: `setup_patches_existing_settings_file`,
/// `setup_creates_fallback_settings_file`, and the per-file `patch_one_*`
/// tests.
fn patch_claude_settings_phase() -> Result<SettingsPatchSummary> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?;
    println!(
        "\n{} Scanning for Claude settings under {}…",
        "·".dimmed(),
        home.display()
    );

    let entry = mcp_server_entry(MCP_SERVER_KEY, &["serve"]);
    let files = discover_claude_settings(&home, default_settings_max_depth());

    if files.is_empty() {
        let fallback = home.join(".claude").join("settings.json");
        println!(
            "{} No Claude settings files found. Creating {}…",
            "·".dimmed(),
            fallback.display()
        );
        let outcome = patch_one(&fallback, &entry)?;
        return Ok(SettingsPatchSummary {
            mcp_changed: outcome.mcp_wrote as usize,
            hooks_changed: outcome.hook_wrote as usize,
        });
    }

    println!(
        "{} Found {} settings file(s). Patching each…",
        "·".dimmed(),
        files.len()
    );
    let mut summary = SettingsPatchSummary::default();
    for path in &files {
        match patch_one(path, &entry) {
            Ok(outcome) => {
                summary.mcp_changed += outcome.mcp_wrote as usize;
                summary.hooks_changed += outcome.hook_wrote as usize;
                if outcome.any() {
                    let label = match (outcome.mcp_wrote, outcome.hook_wrote) {
                        (true, true) => "(mcp + hook)",
                        (true, false) => "(mcp)",
                        (false, true) => "(hook)",
                        (false, false) => "",
                    };
                    println!("  {} {} {}", "✓".green(), path.display(), label.dimmed());
                } else {
                    println!(
                        "  {} {} {}",
                        "↻".cyan(),
                        path.display().to_string().dimmed(),
                        "(already configured)".dimmed()
                    );
                }
            }
            Err(e) => {
                // Non-fatal: log and continue so one bad file doesn't sink
                // the whole setup run.
                eprintln!(
                    "  {} {} {}",
                    "✗".red(),
                    path.display(),
                    format!("({e})").red()
                );
            }
        }
    }
    Ok(summary)
}

/// Patch a single Claude settings file: upsert the MCP server entry, then
/// merge the UserPromptSubmit hook.
///
/// Why: keeping both edits in one helper lets the surrounding loop report a
/// single `(mcp + hook)` / `(mcp)` / `(hook)` / `(already configured)` line
/// per file. Each edit is idempotent on its own, so running setup twice
/// reports `(already configured)` on the second pass.
/// What: calls [`patch_mcp_server`] to upsert the MCP entry, then loads the
/// resulting file, runs [`merge_hook_entries`] with the trusty-memory hook
/// additions, and writes the merged JSON back atomically when it differs
/// from what is already on disk.
/// Test: `patch_one_creates_missing_file`, `patch_one_is_idempotent`,
/// `patch_one_installs_hook`, `patch_one_preserves_unrelated_keys`.
fn patch_one(path: &Path, entry: &serde_json::Value) -> Result<PatchOutcome> {
    let mcp_wrote = patch_mcp_server(path, MCP_SERVER_KEY, entry)?;
    let hook_wrote = merge_prompt_context_hook(path)?;
    Ok(PatchOutcome {
        mcp_wrote,
        hook_wrote,
    })
}

/// Build the trusty-memory `UserPromptSubmit` + `SessionStart` hook block
/// as Claude Code expects it.
///
/// Why: the live `settings.json` shape is `{"hooks": {"<Event>": [{ "matcher":
/// "*", "hooks": [{ "type": "command", "command": "...", "timeout": ... }]
/// }]}}`. A centralised constructor keeps every call site producing the
/// exact same shape so [`merge_hook_entries`] can dedup by deep equality.
/// Issue #99 added the `SessionStart` block for inter-project inbox
/// delivery; it shares the same shape and timeout as the prompt-context
/// hook so existing operators don't have to reason about two policies.
/// What: returns a JSON object with both the `UserPromptSubmit` event
/// (running `prompt-context`) and the `SessionStart` event (running
/// `inbox-check`).
/// Test: `patch_one_installs_hook`, `patch_one_installs_session_start_hook`.
fn prompt_context_hook_additions() -> Value {
    json!({
        "hooks": {
            HOOK_EVENT: [
                {
                    "matcher": "*",
                    "hooks": [
                        {
                            "type": "command",
                            "command": HOOK_COMMAND,
                            "timeout": HOOK_TIMEOUT_MS,
                        }
                    ],
                }
            ],
            SESSION_START_HOOK_EVENT: [
                {
                    "matcher": "*",
                    "hooks": [
                        {
                            "type": "command",
                            "command": INBOX_CHECK_HOOK_COMMAND,
                            "timeout": HOOK_TIMEOUT_MS,
                        }
                    ],
                }
            ]
        }
    })
}

/// Idempotently merge the trusty-memory `UserPromptSubmit` hook into a
/// Claude Code settings file.
///
/// Why: the MCP server entry by itself just registers the daemon; the hook
/// is what makes Claude Code call `trusty-memory prompt-context` before
/// every user prompt and inject its stdout. Without this merge, the daemon
/// would be reachable but no prompt-context block would ever appear in the
/// model's input.
/// What: reads the existing settings (missing file → `{}`), runs the shared
/// [`merge_hook_entries`] helper to fold in `prompt_context_hook_additions()`,
/// and writes the result back atomically when it differs from the input.
/// Returns `true` when the file was rewritten and `false` when the hook was
/// already present (idempotent re-run).
/// Test: `patch_one_installs_hook`, `patch_one_is_idempotent`.
fn merge_prompt_context_hook(path: &Path) -> Result<bool> {
    let original: Value = match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Value::Object(serde_json::Map::new()),
        Ok(s) => serde_json::from_str(&s)
            .with_context(|| format!("parse settings file {}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Object(serde_json::Map::new()),
        Err(e) => {
            return Err(anyhow::Error::new(e))
                .with_context(|| format!("read settings file {}", path.display()))
        }
    };
    let additions = prompt_context_hook_additions();
    let merged = merge_hook_entries(&original, &additions);
    if merged == original {
        return Ok(false);
    }
    write_json_atomic(path, &merged)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Why: patching a fresh settings file must produce a valid
    /// `mcpServers` block with the canonical `trusty-memory` entry AND a
    /// `UserPromptSubmit` hook pointing at `trusty-memory prompt-context`.
    /// What: writes a minimal settings.json, calls `patch_one`, asserts
    /// both edits landed.
    #[test]
    fn patch_one_creates_missing_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        let entry = mcp_server_entry(MCP_SERVER_KEY, &["serve"]);

        let outcome = patch_one(&path, &entry).expect("patch ok");
        assert!(outcome.mcp_wrote, "first patch writes the MCP entry");
        assert!(outcome.hook_wrote, "first patch installs the hook");

        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let server = &value["mcpServers"][MCP_SERVER_KEY];
        assert_eq!(server["command"], "trusty-memory");
        assert_eq!(server["args"][0], "serve");

        let hook_entries = value["hooks"][HOOK_EVENT].as_array().unwrap();
        assert_eq!(hook_entries.len(), 1, "exactly one matcher block");
        let inner = hook_entries[0]["hooks"].as_array().unwrap();
        assert_eq!(inner[0]["command"], HOOK_COMMAND);
        assert_eq!(inner[0]["type"], "command");
        assert_eq!(inner[0]["timeout"], HOOK_TIMEOUT_MS);

        // Issue #99: SessionStart hook must also land.
        let ss_entries = value["hooks"][SESSION_START_HOOK_EVENT]
            .as_array()
            .expect("SessionStart hooks installed");
        assert_eq!(
            ss_entries.len(),
            1,
            "exactly one SessionStart matcher block"
        );
        let ss_inner = ss_entries[0]["hooks"].as_array().unwrap();
        assert_eq!(ss_inner[0]["command"], INBOX_CHECK_HOOK_COMMAND);
        assert_eq!(ss_inner[0]["type"], "command");
        assert_eq!(ss_inner[0]["timeout"], HOOK_TIMEOUT_MS);
    }

    /// Why: regression for issue #99 — when a user has the UserPromptSubmit
    /// hook from an earlier release but no SessionStart hook, re-running
    /// setup must add the SessionStart block (and not touch any existing
    /// UserPromptSubmit hook).
    /// What: seeds a settings file with the MCP entry + UserPromptSubmit
    /// hook only, patches, and asserts both events are present afterwards.
    #[test]
    fn patch_one_installs_session_start_hook_when_upgrading() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        let entry = mcp_server_entry(MCP_SERVER_KEY, &["serve"]);
        let seed = json!({
            "mcpServers": {
                MCP_SERVER_KEY: { "command": "trusty-memory", "args": ["serve"] }
            },
            "hooks": {
                HOOK_EVENT: [{
                    "matcher": "*",
                    "hooks": [{
                        "type": "command",
                        "command": HOOK_COMMAND,
                        "timeout": HOOK_TIMEOUT_MS,
                    }]
                }]
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&seed).unwrap()).unwrap();

        let outcome = patch_one(&path, &entry).expect("patch ok");
        assert!(!outcome.mcp_wrote);
        assert!(outcome.hook_wrote, "SessionStart hook must be added");

        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // Existing UserPromptSubmit is preserved exactly.
        let ups = value["hooks"][HOOK_EVENT].as_array().unwrap();
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0]["hooks"][0]["command"], HOOK_COMMAND);
        // New SessionStart is present.
        let ss = value["hooks"][SESSION_START_HOOK_EVENT].as_array().unwrap();
        assert_eq!(ss.len(), 1);
        assert_eq!(ss[0]["hooks"][0]["command"], INBOX_CHECK_HOOK_COMMAND);
    }

    /// Why: re-running `setup` must be safe — calling `patch_one` against
    /// an already-configured file must not rewrite it.
    /// What: writes settings.json, patches twice, asserts the second call
    /// reports neither change and the file is byte-identical.
    #[test]
    fn patch_one_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        let entry = mcp_server_entry(MCP_SERVER_KEY, &["serve"]);

        let first = patch_one(&path, &entry).unwrap();
        assert!(first.mcp_wrote && first.hook_wrote, "first patch writes");
        let after_first = std::fs::read_to_string(&path).unwrap();

        let second = patch_one(&path, &entry).unwrap();
        assert!(
            !second.mcp_wrote && !second.hook_wrote,
            "second patch is no-op"
        );
        let after_second = std::fs::read_to_string(&path).unwrap();

        assert_eq!(after_first, after_second, "file must not change on no-op");
    }

    /// Why: patching must preserve unrelated keys (theme, other servers,
    /// other hooks). Anything else is a regression — `setup` would destroy
    /// user config.
    /// What: seeds a settings file with extra keys, patches, asserts every
    /// pre-existing key still exists alongside the new MCP entry and that
    /// pre-existing hooks under other events are left in place.
    #[test]
    fn patch_one_preserves_unrelated_keys() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        let seed = json!({
            "theme": "dark",
            "mcpServers": {
                "some-other-server": { "command": "x", "args": [] }
            },
            "hooks": {
                "Stop": [{ "matcher": "*", "hooks": [
                    { "type": "command", "command": "echo bye" }
                ] }]
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&seed).unwrap()).unwrap();

        let entry = mcp_server_entry(MCP_SERVER_KEY, &["serve"]);
        let outcome = patch_one(&path, &entry).expect("patch ok");
        assert!(outcome.mcp_wrote);
        assert!(outcome.hook_wrote);

        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["theme"], "dark", "unrelated top-level key dropped");
        let servers = value["mcpServers"].as_object().unwrap();
        assert!(servers.contains_key("some-other-server"));
        assert!(servers.contains_key(MCP_SERVER_KEY));
        // Pre-existing Stop hook must be retained.
        let stop = value["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0]["hooks"][0]["command"], "echo bye");
        // And our UserPromptSubmit hook was added.
        let ups = value["hooks"][HOOK_EVENT].as_array().unwrap();
        assert_eq!(ups[0]["hooks"][0]["command"], HOOK_COMMAND);
    }

    /// Why: when the MCP entry is already present but the hook is new (a
    /// user upgrading from an older trusty-memory release), the patch must
    /// install only the hook and report that distinction.
    /// What: seeds a settings file with the MCP entry already present but
    /// no hook, runs `patch_one`, asserts `mcp_wrote = false, hook_wrote
    /// = true`.
    #[test]
    fn patch_one_installs_hook_when_mcp_already_present() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        let entry = mcp_server_entry(MCP_SERVER_KEY, &["serve"]);
        let seed = json!({
            "mcpServers": {
                MCP_SERVER_KEY: { "command": "trusty-memory", "args": ["serve"] }
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&seed).unwrap()).unwrap();

        let outcome = patch_one(&path, &entry).expect("patch ok");
        assert!(!outcome.mcp_wrote, "MCP entry already present");
        assert!(outcome.hook_wrote, "hook freshly installed");
    }
}
