//! Output-style, spinner-tip, hook, and MCP-injection helpers.
//!
//! Why: `prepare_session` must configure several Claude Code settings files
//! before launching a session. Grouping all the settings-file mutations here
//! keeps `mod.rs` focused on the top-level preparation sequence and makes
//! each individual helper easy to test in isolation.
//! What: constants and functions for writing the output style, spinner tips,
//! `trusty-memory` project hooks, `trusty-memory` MCP server injection, and
//! global-hook cleanup.
//! Test: each public(super) function is covered by a dedicated test in `tests.rs`.

use std::path::{Path, PathBuf};

use super::PrepError;

/// Claude Code output style applied to launched sessions.
///
/// Why: the Claude Code status bar renders `style:<outputStyle>`; launched
/// trusty-mpm sessions should advertise themselves as `trusty-mpm`.
pub(super) const OUTPUT_STYLE: &str = "trusty-mpm";

/// trusty-mpm-specific spinner tips shown during Claude Code loading.
///
/// Why: Claude Code's loading spinner renders tips from
/// `spinnerTipsOverride.tips`; the operator's global settings carry generic
/// claude-mpm tips, so trusty-mpm sessions override them with project-relevant
/// guidance (the `tm` CLI, the `make check` gate, the API-first layering rule).
pub(super) const SPINNER_TIPS: &[&str] = &[
    "tm launch — start a configured claude session for this project",
    "make check = cargo test + clippy + fmt — must pass before any PR",
    "API → CLI → TUI: implement at the lowest layer first",
    "Delegate Rust code to rust-engineer — PM never edits .rs files",
    "gh issue create to track work; commits include Closes #N",
    "tmux ls shows all active tmpm-<folder> sessions",
    "tm sessions list shows daemon-managed sessions",
    "/compact at ~50% context to stay focused",
    "Layer new features behind the HTTP API before wiring CLI or TUI",
];

/// The `trusty-memory` hook block written into the project's
/// `.claude/settings.json`.
///
/// Why (issue #1270): the previous block invoked `trusty-memory hooks fire
/// <event>`, but the installed `trusty-memory` binary has **no `hooks`
/// subcommand** — that invocation exits non-zero with "unrecognized subcommand
/// 'hooks'". A non-zero `UserPromptSubmit` hook *hard-blocks the prompt*, so
/// every trusty-mpm-spawned session was dead on arrival: the injected task was
/// rejected before Claude ever saw it. This block now mirrors the canonical
/// hooks that `trusty-memory setup` installs (see
/// `crates/trusty-memory/src/commands/setup.rs`): `UserPromptSubmit` runs
/// `trusty-memory prompt-context` (documented to always exit 0 so it can never
/// block a prompt) and `SessionStart` runs `trusty-memory inbox-check` (also
/// fail-silent). There is intentionally no `PostToolUse` or `Stop` hook because
/// `trusty-memory` ships no corresponding CLI surface; memory writes during a
/// session flow through the MCP tools instead.
/// What: a `UserPromptSubmit` → `trusty-memory prompt-context` hook and a
/// `SessionStart` → `trusty-memory inbox-check` hook, scoped to the project so
/// they fire only for trusty-mpm sessions.
/// Test: `write_project_hooks_uses_canonical_commands`,
/// `write_project_hooks_writes_all_event_types`.
pub(super) const TRUSTY_MEMORY_HOOKS: &str = r#"{
  "UserPromptSubmit": [
    {
      "matcher": "",
      "hooks": [
        {
          "type": "command",
          "command": "trusty-memory prompt-context",
          "timeout": 60
        }
      ]
    }
  ],
  "SessionStart": [
    {
      "matcher": "",
      "hooks": [
        {
          "type": "command",
          "command": "trusty-memory inbox-check",
          "timeout": 60
        }
      ]
    }
  ]
}"#;

/// The `trusty-memory` MCP server definition injected into a project's
/// `.mcp.json`.
///
/// Why (issue #1270): Claude Code reads MCP servers from `<project>/.mcp.json`;
/// a launched trusty-mpm session needs the `trusty-memory` server registered
/// there so the memory tools (`memory_recall`, `memory_remember`, …) are
/// available. The previous definition used `args: ["mcp", "serve"]`, but
/// `trusty-memory` has **no `mcp` subcommand** — the server would have failed
/// to start. The canonical stdio MCP invocation (per `trusty-memory setup`) is
/// `serve --stdio`.
/// What: a stdio MCP server entry running `trusty-memory serve --stdio`.
/// Test: `inject_trusty_memory_mcp_uses_serve_stdio`.
pub(super) const TRUSTY_MEMORY_MCP_SERVER: &str = r#"{
  "type": "stdio",
  "command": "trusty-memory",
  "args": ["serve", "--stdio"]
}"#;

/// Build the `trusty-search` MCP server definition injected into a project's
/// `.mcp.json`, optionally pinned to a project index (issue #1373).
///
/// Why (issue #1270 / step 4): trusty-mpm-spawned sessions need the code-search
/// tools (`search`, `grep`, `get_call_chain`, …). Issue #1373: without pinning,
/// the contextless `trusty-search serve` stub left index selection to the LLM,
/// which routinely queried the WRONG index (usually the persistent `claude-mpm`
/// one) instead of the session's own project. Passing `--index <derived-id>`
/// pins the session to its project index so a bare `search` always resolves
/// correctly and fan-out never sweeps every index. The canonical stdio MCP
/// invocation is bare `serve` (stdio is the default transport; HTTP is off
/// unless `--with-http`).
/// What: returns the JSON `Value` for a stdio MCP server running
/// `trusty-search serve` — with `["serve", "--index", "<id>"]` when `index_id`
/// is `Some` (non-empty), else the unpinned `["serve"]`. Index *creation* is
/// handled separately by [`register_project_index`].
/// Test: `trusty_search_mcp_value_pins_index`, `trusty_search_mcp_value_unpinned`.
pub(super) fn trusty_search_mcp_value(index_id: Option<&str>) -> serde_json::Value {
    let args: Vec<serde_json::Value> = match index_id {
        Some(id) if !id.trim().is_empty() => vec![
            serde_json::Value::String("serve".to_string()),
            serde_json::Value::String("--index".to_string()),
            serde_json::Value::String(id.to_string()),
        ],
        _ => vec![serde_json::Value::String("serve".to_string())],
    };
    serde_json::json!({
        "type": "stdio",
        "command": "trusty-search",
        "args": args,
    })
}

/// Hook event types the global `trusty-memory` entries may have been registered
/// under (across current and legacy trusty-mpm builds).
///
/// Why: [`clean_global_trusty_memory_hooks`] strips trusty-mpm's
/// `trusty-memory` hook entries from `~/.claude/settings.json` so they stop
/// firing for unrelated sessions. It must cover every event trusty-mpm has ever
/// written globally: the new canonical pair (`UserPromptSubmit`, `SessionStart`)
/// AND the legacy `hooks fire` events (`PostToolUse`, `Stop`) so an upgrade
/// cleans up the old entries too.
/// What: the union of current and legacy global hook event keys.
pub(super) const GLOBAL_TRUSTY_MEMORY_EVENTS: &[&str] =
    &["UserPromptSubmit", "SessionStart", "PostToolUse", "Stop"];

/// Deploy the bundled `trusty-mpm` output style under `<home>/.claude/output-styles/`.
///
/// Why: [`write_output_style`] only sets `"outputStyle": "trusty-mpm"` in the
/// project settings; Claude Code honours that name only when a matching style
/// file exists in `~/.claude/output-styles/`. This places that file. `home` is
/// passed in (rather than resolved here) so tests can target a temp directory
/// instead of the operator's real home.
/// What: creates `<home>/.claude/output-styles/` if absent, then writes the
/// bundled [`crate::core::bundle::OUTPUT_STYLE`] asset, always overwriting so
/// framework upgrades to the style propagate on the next launch. Returns the
/// path written.
/// Test: `deploy_output_style_writes_file`, `deploy_output_style_overwrites`.
pub(super) fn deploy_output_style(home: &Path) -> Result<PathBuf, PrepError> {
    let style_dir = home.join(".claude").join("output-styles");
    std::fs::create_dir_all(&style_dir).map_err(|source| PrepError::Io {
        path: style_dir.clone(),
        source,
    })?;
    let style_path = style_dir.join("trusty-mpm.md");
    std::fs::write(&style_path, crate::core::bundle::OUTPUT_STYLE).map_err(|source| {
        PrepError::Io {
            path: style_path.clone(),
            source,
        }
    })?;
    Ok(style_path)
}

/// Merge trusty-mpm output-style and spinner-tip settings into the project's
/// `.claude/settings.json`.
///
/// Why: Claude Code reads the output style from `.claude/settings.json` under
/// the `outputStyle` key (there is no `--style` CLI flag); writing it in the
/// project directory makes every `claude` launched there show
/// `style:trusty-mpm` without disturbing the operator's global settings. The
/// same file drives the loading-spinner tips, so trusty-mpm-specific tips are
/// written alongside to override the operator's generic claude-mpm tips.
/// What: reads an existing `<project>/.claude/settings.json` (preserving all
/// other keys), sets `outputStyle` to [`OUTPUT_STYLE`], enables
/// `spinnerTipsEnabled`, sets `spinnerTipsOverride.tips` to [`SPINNER_TIPS`],
/// and writes it back pretty-printed. Creates the file and `.claude/` directory
/// when absent.
/// Test: `prepare_session_sets_output_style`,
/// `write_output_style_preserves_existing_keys`,
/// `write_output_style_sets_spinner_tips`.
pub(super) fn write_output_style(project_dir: &Path) -> Result<(), PrepError> {
    let claude_dir = project_dir.join(".claude");
    std::fs::create_dir_all(&claude_dir).map_err(|source| PrepError::Io {
        path: claude_dir.clone(),
        source,
    })?;
    let settings_path = claude_dir.join("settings.json");

    // Load existing settings to preserve unrelated keys; tolerate a missing or
    // malformed file by starting from an empty object.
    let mut settings = match std::fs::read_to_string(&settings_path) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
        Err(_) => serde_json::Value::Object(serde_json::Map::new()),
    };

    settings["outputStyle"] = serde_json::Value::String(OUTPUT_STYLE.to_string());
    settings["spinnerTipsEnabled"] = serde_json::Value::Bool(true);
    settings["spinnerTipsOverride"] = serde_json::json!({ "tips": SPINNER_TIPS });

    let serialized = serde_json::to_string_pretty(&settings)
        .map_err(|err| PrepError::Deploy(err.to_string()))?;
    std::fs::write(&settings_path, serialized).map_err(|source| PrepError::Io {
        path: settings_path.clone(),
        source,
    })?;
    Ok(())
}

/// Write the `trusty-memory` hook block into the project's `.claude/settings.json`.
///
/// Why: `trusty-memory` hooks must fire only for trusty-mpm-managed sessions.
/// Scoping them to the project settings (instead of the operator's global
/// `~/.claude/settings.json`) means they no longer run for unrelated Claude
/// Code sessions such as claude-mpm.
/// What: reads an existing `<project>/.claude/settings.json` (preserving all
/// other keys), *replaces* the entire `hooks` key with [`TRUSTY_MEMORY_HOOKS`],
/// and writes it back pretty-printed. Replacing — rather than merging — the
/// `hooks` key avoids double-firing if this runs twice. Creates the file and
/// `.claude/` directory when absent.
/// Test: `write_project_hooks_writes_all_event_types`,
/// `write_project_hooks_replaces_existing`.
pub(super) fn write_project_hooks(project_dir: &Path) -> Result<(), PrepError> {
    let claude_dir = project_dir.join(".claude");
    std::fs::create_dir_all(&claude_dir).map_err(|source| PrepError::Io {
        path: claude_dir.clone(),
        source,
    })?;
    let settings_path = claude_dir.join("settings.json");

    // Load existing settings to preserve unrelated keys; tolerate a missing or
    // malformed file by starting from an empty object.
    let mut settings = match std::fs::read_to_string(&settings_path) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
        Err(_) => serde_json::Value::Object(serde_json::Map::new()),
    };

    // Replace the entire `hooks` key. The bundled block is a constant and is
    // guaranteed to parse.
    let hooks: serde_json::Value =
        serde_json::from_str(TRUSTY_MEMORY_HOOKS).expect("bundled hook block is valid JSON");
    settings["hooks"] = hooks;

    let serialized = serde_json::to_string_pretty(&settings)
        .map_err(|err| PrepError::Deploy(err.to_string()))?;
    std::fs::write(&settings_path, serialized).map_err(|source| PrepError::Io {
        path: settings_path.clone(),
        source,
    })?;
    Ok(())
}

/// Inject (or update) a named stdio MCP server into the project's `.mcp.json`.
///
/// Why: trusty-mpm needs to register multiple MCP servers (`trusty-memory`,
/// `trusty-search`) into the workspace `.mcp.json` with identical merge/idempotency
/// semantics. Factoring the read-merge-write logic into one helper keeps the two
/// public wrappers thin and guarantees they behave consistently (issue #1270).
/// What: reads an existing `<project_path>/.mcp.json` (starting from `{}` when
/// absent or malformed), adds/updates `mcpServers.<name>` to `server`, and
/// writes the merged JSON back pretty-printed — preserving all other MCP
/// servers. Idempotent: if the entry already matches, the file is left
/// untouched and no write occurs. `server` is a `serde_json::Value` object
/// built by the caller (a const for `trusty-memory`, a dynamically-pinned value
/// for `trusty-search`, #1373).
/// Test: exercised via `inject_trusty_memory_mcp_*` and `inject_trusty_search_mcp_*`.
fn inject_mcp_server(
    project_path: &Path,
    name: &str,
    server: serde_json::Value,
) -> Result<(), PrepError> {
    let mcp_path = project_path.join(".mcp.json");

    // Load existing config to preserve unrelated servers; tolerate a missing or
    // malformed file by starting from an empty object.
    let mut config = match std::fs::read_to_string(&mcp_path) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
        Err(_) => serde_json::Value::Object(serde_json::Map::new()),
    };

    // Ensure `mcpServers` is an object we can insert into.
    let servers = config
        .as_object_mut()
        .expect("config starts as an object")
        .entry("mcpServers")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if !servers.is_object() {
        *servers = serde_json::Value::Object(serde_json::Map::new());
    }
    let servers = servers
        .as_object_mut()
        .expect("mcpServers normalized to an object");

    // Idempotent: skip the write when the entry already matches.
    if servers.get(name) == Some(&server) {
        return Ok(());
    }
    servers.insert(name.to_string(), server);

    let serialized =
        serde_json::to_string_pretty(&config).map_err(|err| PrepError::Deploy(err.to_string()))?;
    std::fs::write(&mcp_path, serialized).map_err(|source| PrepError::Io {
        path: mcp_path.clone(),
        source,
    })?;
    Ok(())
}

/// Inject the `trusty-memory` MCP server into the project's `.mcp.json`.
///
/// Why: `prepare_session` configures hooks and instructions but, without this,
/// the launched `claude` process has no access to the memory tools because the
/// `trusty-memory` MCP server is never registered in `<project>/.mcp.json`.
/// What: thin wrapper over [`inject_mcp_server`] registering the
/// [`TRUSTY_MEMORY_MCP_SERVER`] entry under the key `trusty-memory`.
/// Test: `inject_trusty_memory_mcp_adds_server`,
/// `inject_trusty_memory_mcp_preserves_existing`,
/// `inject_trusty_memory_mcp_is_idempotent`,
/// `inject_trusty_memory_mcp_uses_serve_stdio`.
pub(super) fn inject_trusty_memory_mcp(project_path: &Path) -> Result<(), PrepError> {
    let server: serde_json::Value = serde_json::from_str(TRUSTY_MEMORY_MCP_SERVER)
        .expect("bundled MCP server block is valid JSON");
    inject_mcp_server(project_path, "trusty-memory", server)
}

/// Inject the `trusty-search` MCP server into the project's `.mcp.json`,
/// pinned to the project's index when one is known (issue #1373).
///
/// Why (issue #1270 / step 4): spawned sessions must reach the code-search
/// tools, but `trusty-search` was never registered alongside `trusty-memory`.
/// Issue #1373: the stub must additionally PIN the session to the project's own
/// index (`--index <id>`) so queries never resolve to the wrong index. When
/// `index_id` is `None` (derivation failed) the unpinned stub is written so the
/// session still gets the tools — backward-compatible with the pre-#1373 stub.
/// What: builds the (optionally pinned) server value via
/// [`trusty_search_mcp_value`] and registers it under the key `trusty-search`.
/// Test: `inject_trusty_search_mcp_adds_server`,
/// `inject_trusty_search_mcp_preserves_existing`,
/// `inject_trusty_search_mcp_is_idempotent`,
/// `inject_trusty_search_mcp_pins_index`,
/// `inject_both_mcp_servers_coexist`.
pub(super) fn inject_trusty_search_mcp(
    project_path: &Path,
    index_id: Option<&str>,
) -> Result<(), PrepError> {
    inject_mcp_server(
        project_path,
        "trusty-search",
        trusty_search_mcp_value(index_id),
    )
}

/// Find-or-create the trusty-search index for `project_root` and return its id
/// (issue #1373).
///
/// Why: pinning the serve stub to an index id is only useful if that index
/// actually exists in the daemon — otherwise a query against it returns nothing
/// and the LLM falls back to guessing (the very bug #1373 fixes). At session
/// launch we therefore derive the project's canonical index id (the same rule
/// trusty-search's `detect_project` uses, via the shared
/// `trusty_common::derive_index_id`) and best-effort register it with the
/// running daemon. The daemon's `POST /indexes` is idempotent (returns
/// `created: false` for an existing id), so a re-register is safe and cheap.
/// What: resolves the git-root for `project_root`, derives the index id, and —
/// when the id is non-empty AND the trusty-search daemon address is discoverable
/// — POSTs `{id, root_path}` to `/indexes`. ALWAYS returns the derived id
/// (`None` only when derivation yields an empty string) so the caller can pin
/// the stub even if the daemon is unreachable; a failed/skipped registration is
/// logged at warn/debug and never propagates (the session must still launch).
/// Test: `register_project_index_returns_derived_id` (derivation + daemon-down
/// graceful path) in `tests.rs`.
pub(super) fn register_project_index(project_root: &Path) -> Option<String> {
    let root = trusty_common::resolve_project_root(project_root);
    let index_id = trusty_common::derive_index_id(&root);
    if index_id.trim().is_empty() {
        tracing::warn!(
            "skipping trusty-search index registration: empty index id for {}",
            root.display()
        );
        return None;
    }

    // Discover the running daemon's address. Absent / unreadable file ⇒ daemon
    // not started: skip registration (best-effort) but still return the id so
    // the stub is pinned — the daemon will create the index on first reindex.
    match trusty_common::read_daemon_addr("trusty-search") {
        Ok(Some(addr)) if !addr.trim().is_empty() => {
            let base = if addr.starts_with("http://") || addr.starts_with("https://") {
                addr
            } else {
                format!("http://{addr}")
            };
            best_effort_create_index(&base, &index_id, &root);
        }
        _ => {
            tracing::warn!(
                "trusty-search daemon address not found; pinning index '{index_id}' \
                 without pre-registering it (it will be created on first reindex)"
            );
        }
    }

    Some(index_id)
}

/// POST `/indexes` to find-or-create `index_id`; failures are logged, never
/// propagated (issue #1373).
///
/// Why: registration is best-effort — a daemon that is briefly unreachable, or
/// an HTTP hiccup, must NOT abort session launch. Isolating the blocking HTTP
/// call here keeps [`register_project_index`] readable and the error handling
/// in one place.
/// What: issues a short-timeout blocking `POST {base}/indexes` with body
/// `{id, root_path}` ON A DEDICATED OS THREAD. `prepare_session` is synchronous
/// but is frequently invoked from inside a tokio runtime (the daemon/TUI launch
/// paths); creating `reqwest::blocking`'s internal runtime directly there panics
/// with "Cannot drop a runtime in a context where blocking is not allowed".
/// Running the blocking client on a freshly-spawned `std::thread` (joined here)
/// keeps that nested runtime entirely off the async worker, so the call is safe
/// from both sync and async callers. A non-2xx response or transport error is
/// logged at warn/debug and swallowed; the daemon endpoint is idempotent so
/// re-creates are harmless. The client uses a tight ~1s overall timeout
/// (750 ms connect) so the joined thread returns quickly: this call sits on the
/// `prepare_session` hot path and must NOT stall a session launch when the
/// daemon is slow or unreachable. Registration is purely best-effort — the
/// index is also created on the first reindex — so a short cap is acceptable.
/// Test: exercised via `register_project_index_returns_derived_id` (daemon-down
/// path) and `launch_session_errors_when_daemon_unreachable` (async-context
/// safety); the live HTTP path is covered by integration use.
fn best_effort_create_index(base: &str, index_id: &str, root: &Path) {
    let url = format!("{base}/indexes");
    let body = serde_json::json!({ "id": index_id, "root_path": root.to_string_lossy() });
    let index_id = index_id.to_string();
    let root_display = root.display().to_string();

    let result = std::thread::spawn(move || {
        // 1s overall / 750ms connect cap: this runs synchronously on the
        // session-launch hot path, so the worst-case stall must stay small.
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(1))
            .connect_timeout(std::time::Duration::from_millis(750))
            .build()?;
        let resp = client.post(&url).json(&body).send()?;
        Ok::<reqwest::StatusCode, reqwest::Error>(resp.status())
    })
    .join();

    match result {
        Ok(Ok(status)) if status.is_success() => {
            tracing::debug!("registered trusty-search index '{index_id}' (root={root_display})");
        }
        Ok(Ok(status)) => {
            tracing::warn!(
                "trusty-search index registration for '{index_id}' returned HTTP {status}"
            );
        }
        Ok(Err(e)) => {
            tracing::warn!("trusty-search index registration for '{index_id}' failed: {e}");
        }
        Err(_) => {
            tracing::warn!("trusty-search index registration thread for '{index_id}' panicked");
        }
    }
}

/// Collect the MCP server names declared in a workspace's `.mcp.json`.
///
/// Why (issue #1296): Claude Code blocks a freshly-spawned session on a "new MCP
/// servers found" approval dialog whenever `<workspace>/.mcp.json` registers a
/// server the project entry has not yet approved. [`preseed_workspace_trust`]
/// pre-approves them via `enabledMcpjsonServers`, and to do that it needs the
/// list of server names the project actually ships. The list is derived from the
/// on-disk `.mcp.json` (rather than a hard-coded set) so it covers BOTH the
/// servers trusty-mpm injects (`trusty-memory`, `trusty-search`) AND any servers
/// the cloned project shipped of its own — every one of which would otherwise
/// trigger the dialog.
/// What: reads `<workspace>/.mcp.json`, parses it, and returns the sorted keys of
/// its `mcpServers` object. A missing, unreadable, malformed, or server-less file
/// yields an empty vector — this never fails, so a degenerate `.mcp.json` cannot
/// crash launch preparation. Sorting makes the result deterministic so the
/// written settings are stable across runs (supporting idempotency).
/// Test: covered via `preseed_trust_enables_mcp_servers_from_mcp_json` and
/// `preseed_trust_enables_empty_when_no_mcp_json`.
fn mcp_server_names(workspace: &Path) -> Vec<String> {
    let mcp_path = workspace.join(".mcp.json");
    let Ok(text) = std::fs::read_to_string(&mcp_path) else {
        return Vec::new();
    };
    let Ok(config) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let mut names: Vec<String> = config
        .get("mcpServers")
        .and_then(|servers| servers.as_object())
        .map(|servers| servers.keys().cloned().collect())
        .unwrap_or_default();
    names.sort_unstable();
    names
}

/// Pre-seed per-directory trust acceptance for `workspace` in `~/.claude.json`.
///
/// Why (issue #1269): trusty-mpm launches Claude Code inside an *interactive*
/// tmux pane (so the session can be observed and driven). For a directory it has
/// never trusted, Claude Code blocks on the "Do you trust the files in this
/// folder?" dialog before it will accept any input — so the task prompt that tm
/// injects via `send-keys` is swallowed and the session stalls. Trust is keyed
/// by absolute directory path in the config dir (`~/.claude.json`), which
/// `--setting-sources` does NOT govern, so we must seed it directly. Because tm
/// clones each repo into its OWN throwaway workspace under
/// `~/trusty-mpm-projects/<owner>/<repo>/<id>/`, marking that path trusted is
/// safe — no real user project is affected. Issue #1296 extends this: trust
/// acceptance alone is not enough — a project shipping a `.mcp.json` triggers a
/// SECOND blocking "new MCP servers found" dialog, so this also pre-approves the
/// project's MCP servers via `enabledMcpjsonServers`.
/// What: reads `~/.claude.json` (the JSON config; starts from `{}` when absent
/// or malformed — but a malformed *existing* file is left untouched to avoid
/// clobbering the operator's OAuth/login data), ensures
/// `projects.<workspace>` carries `hasTrustDialogAccepted: true`,
/// `hasCompletedProjectOnboarding: true`, `projectOnboardingSeenCount >= 1`, and
/// `enabledMcpjsonServers` listing every server name from
/// `<workspace>/.mcp.json` (see [`mcp_server_names`]; an empty array when the
/// project ships none), then writes it back pretty-printed preserving every
/// other key. Idempotent. The OAuth fields elsewhere in the file are never
/// touched.
/// Test: `preseed_trust_marks_directory`, `preseed_trust_preserves_other_keys`,
/// `preseed_trust_is_idempotent`, `preseed_trust_leaves_malformed_file`,
/// `preseed_trust_enables_mcp_servers_from_mcp_json`,
/// `preseed_trust_enables_empty_when_no_mcp_json`.
pub(super) fn preseed_workspace_trust(
    claude_json: &Path,
    workspace: &Path,
) -> Result<(), PrepError> {
    use serde_json::Value;

    // Read the existing config. If the file exists but is malformed JSON we must
    // NOT overwrite it — it likely holds the operator's OAuth credentials, and
    // clobbering it would force a re-login. Treat that as a soft failure.
    let mut config: Value = match std::fs::read_to_string(claude_json) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(value) if value.is_object() => value,
            Ok(_) => {
                tracing::warn!(
                    "skipping trust pre-seed: {} is valid JSON but not an object",
                    claude_json.display()
                );
                return Ok(());
            }
            Err(err) => {
                tracing::warn!(
                    "skipping trust pre-seed: {} is not valid JSON ({err}); \
                     leaving it untouched to protect OAuth state",
                    claude_json.display()
                );
                return Ok(());
            }
        },
        // Missing file: start fresh. (Rare — claude writes it on first run.)
        Err(_) => Value::Object(serde_json::Map::new()),
    };

    let key = workspace.to_string_lossy().to_string();

    let projects = config
        .as_object_mut()
        .expect("config is an object")
        .entry("projects")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !projects.is_object() {
        *projects = Value::Object(serde_json::Map::new());
    }
    let projects = projects.as_object_mut().expect("projects is an object");

    let entry = projects
        .entry(key)
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(serde_json::Map::new());
    }
    let entry = entry.as_object_mut().expect("project entry is an object");

    // Derive the set of MCP servers the project ships so we can pre-approve them
    // (issue #1296). Sorted for a deterministic, idempotency-friendly result.
    let enabled_mcp = Value::Array(
        mcp_server_names(workspace)
            .into_iter()
            .map(Value::String)
            .collect(),
    );

    // Idempotent: skip the write when trust is already fully accepted AND the MCP
    // approval list already matches what `.mcp.json` declares. The latter check
    // is essential — without it a second prep run (after `.mcp.json` gained the
    // injected servers) would never persist `enabledMcpjsonServers`.
    let already_trusted = entry.get("hasTrustDialogAccepted") == Some(&Value::Bool(true))
        && entry.get("hasCompletedProjectOnboarding") == Some(&Value::Bool(true))
        && entry.get("enabledMcpjsonServers") == Some(&enabled_mcp);
    if already_trusted {
        return Ok(());
    }

    entry.insert("hasTrustDialogAccepted".to_string(), Value::Bool(true));
    entry.insert(
        "hasCompletedProjectOnboarding".to_string(),
        Value::Bool(true),
    );
    // Claude Code shows the onboarding flow until this counter is >= 1.
    entry
        .entry("projectOnboardingSeenCount")
        .or_insert_with(|| Value::from(1));
    // Pre-approve the project's MCP servers so the "new MCP servers found" dialog
    // never blocks the spawned session (issue #1296).
    entry.insert("enabledMcpjsonServers".to_string(), enabled_mcp);

    let serialized =
        serde_json::to_string_pretty(&config).map_err(|err| PrepError::Deploy(err.to_string()))?;
    std::fs::write(claude_json, serialized).map_err(|source| PrepError::Io {
        path: claude_json.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Pre-seed workspace trust in the operator's real `~/.claude.json`.
///
/// Why: thin home-resolving wrapper over [`preseed_workspace_trust`] so
/// `prepare_session` can call it without knowing the config-dir layout; tests
/// target a temp file via the inner function directly.
/// What: resolves `~/.claude.json` and delegates. A missing home directory is a
/// soft failure (logged, non-fatal) so launch still proceeds.
/// Test: covered by the inner `preseed_trust_*` tests.
pub(super) fn preseed_workspace_trust_home(workspace: &Path) -> Result<(), PrepError> {
    let Some(home) = dirs::home_dir() else {
        tracing::warn!("skipping trust pre-seed: home directory unresolved");
        return Ok(());
    };
    preseed_workspace_trust(&home.join(".claude.json"), workspace)
}

/// Remove the `trusty-memory` hook entries from `~/.claude/settings.json`.
///
/// Why: `trusty-memory` hooks were previously registered globally, so they
/// fired for every Claude Code session. Now that [`write_project_hooks`]
/// scopes them to trusty-mpm projects, the global entries must be removed to
/// stop them double-firing (and firing for unrelated sessions like claude-mpm).
/// What: reads `~/.claude/settings.json`, and for each event in
/// [`GLOBAL_TRUSTY_MEMORY_EVENTS`] filters out handler groups whose `hooks`
/// array contains a command matching `trusty-memory hooks fire`. An event key
/// whose array becomes empty is removed entirely. Writes the file back. A
/// missing or malformed file is treated as success (nothing to clean up).
/// Test: `remove_global_hooks_removes_trusty_memory_entries`.
pub(super) fn remove_global_trusty_memory_hooks() -> Result<(), PrepError> {
    let home = match dirs::home_dir() {
        Some(home) => home,
        None => {
            tracing::warn!("skipping global trusty-memory hook removal: home unresolved");
            return Ok(());
        }
    };
    let settings_path = home.join(".claude").join("settings.json");
    clean_global_trusty_memory_hooks(&settings_path)
}

/// Filter `trusty-memory` hook entries out of the settings file at `settings_path`.
///
/// Why: split from [`remove_global_trusty_memory_hooks`] so tests can target a
/// temp file instead of the operator's real `~/.claude/settings.json`.
/// What: see [`remove_global_trusty_memory_hooks`]. A missing or malformed
/// file is a no-op success.
/// Test: `remove_global_hooks_removes_trusty_memory_entries`.
pub(super) fn clean_global_trusty_memory_hooks(settings_path: &Path) -> Result<(), PrepError> {
    let text = match std::fs::read_to_string(settings_path) {
        Ok(text) => text,
        // Missing file: nothing to clean up.
        Err(_) => return Ok(()),
    };
    let mut settings = match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(value) if value.is_object() => value,
        // Malformed or non-object: leave it untouched rather than risk loss.
        _ => return Ok(()),
    };

    let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return Ok(());
    };

    for event in GLOBAL_TRUSTY_MEMORY_EVENTS {
        let Some(groups) = hooks.get_mut(*event).and_then(|g| g.as_array_mut()) else {
            continue;
        };
        groups.retain(|group| !group_is_trusty_memory(group));
        if groups.is_empty() {
            hooks.remove(*event);
        }
    }

    let serialized = serde_json::to_string_pretty(&settings)
        .map_err(|err| PrepError::Deploy(err.to_string()))?;
    std::fs::write(settings_path, serialized).map_err(|source| PrepError::Io {
        path: settings_path.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Whether a hook handler group is a `trusty-memory` entry.
///
/// Why: identifies the groups [`clean_global_trusty_memory_hooks`] must drop.
/// Matching on the broad `trusty-memory` prefix (rather than the old, narrow
/// `trusty-memory hooks fire` substring) cleans up BOTH the legacy global
/// entries and the canonical `prompt-context` / `inbox-check` entries should
/// either ever have been written globally (issue #1270).
/// What: returns `true` when any command in the group's `hooks` array invokes
/// the `trusty-memory` binary (the command string starts with `trusty-memory `
/// or is exactly `trusty-memory`).
/// Test: covered indirectly by `remove_global_hooks_removes_trusty_memory_entries`.
pub(super) fn group_is_trusty_memory(group: &serde_json::Value) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array())
        .is_some_and(|handlers| {
            handlers.iter().any(|handler| {
                handler
                    .get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(|cmd| cmd == "trusty-memory" || cmd.starts_with("trusty-memory "))
            })
        })
}
