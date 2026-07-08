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

/// Default Claude Code output style applied to launched sessions.
///
/// Why: the Claude Code status bar renders `style:<outputStyle>`; when no style
/// is configured, launched trusty-mpm sessions advertise the professional
/// default `trusty-mpm`. HR-4 lets the operator select a different bundled style
/// (teaching/research); [`write_output_style`] takes the resolved active id and
/// falls back to this constant when none is supplied. `pub(crate)` (rather than
/// `pub(super)`) so `core::standalone::settings_defaults::ensure_settings_defaults`
/// can seed the SAME default id into the tm-owned `CLAUDE_CONFIG_DIR` settings.json
/// (issue #2214), re-exported via `session_launch::OUTPUT_STYLE`.
pub(crate) const OUTPUT_STYLE: &str = crate::core::bundle::DEFAULT_OUTPUT_STYLE_ID;

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
    "tmux ls shows all active tm-<project>-NN sessions",
    "tm session list shows daemon-managed sessions",
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

/// Build the `trusty-memory` MCP server definition injected into a project's
/// `.mcp.json`, optionally pinned to a project palace (issue #1605).
///
/// Why (issue #1270): Claude Code reads MCP servers from `<project>/.mcp.json`;
/// a launched trusty-mpm session needs the `trusty-memory` server registered
/// there so the memory tools (`memory_recall`, `memory_remember`, …) are
/// available. The canonical stdio MCP invocation (per `trusty-memory setup`) is
/// `serve --stdio`. Issue #1605: a bare stub leaves palace selection to
/// trusty-memory's cwd-based derivation, which for a repo_url-cloned managed
/// session resolves to the session-id directory basename — the WRONG palace.
/// Pinning `env.TRUSTY_MEMORY_PALACE` to the project's derived `owner-repo` slug
/// makes the spawned trusty-memory resolve the correct project-scoped palace
/// regardless of the throwaway workspace path (the env var is the
/// highest-precedence palace override read by `derive_palace_id`). No
/// trusty-memory CLI/protocol change — the env var is the agreed seam.
/// What: returns the JSON `Value` for a stdio MCP server running
/// `trusty-memory serve --stdio` — with `"env": { "TRUSTY_MEMORY_PALACE": <slug> }`
/// when `palace_slug` is `Some` (non-empty), else the bare stub (no `env` key,
/// byte-identical to the pre-#1605 block). Pure: callers resolve the slug.
/// Test: `trusty_memory_mcp_value_pins_palace`, `trusty_memory_mcp_value_bare`.
pub(super) fn trusty_memory_mcp_value(palace_slug: Option<&str>) -> serde_json::Value {
    let mut server = serde_json::json!({
        "type": "stdio",
        "command": "trusty-memory",
        "args": ["serve", "--stdio"],
    });
    if let Some(slug) = palace_slug
        && !slug.trim().is_empty()
    {
        server["env"] = serde_json::json!({
            trusty_common::PALACE_OVERRIDE_ENV: slug,
        });
    }
    server
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

/// Deploy ALL bundled output styles under `<root>/.claude/output-styles/`.
///
/// Why: [`write_output_style`] sets `"outputStyle": "<active>"` in the project
/// settings; Claude Code honours that name only when a matching style file
/// exists under a `.claude/output-styles/` directory that is actually loaded
/// by the running session's `--setting-sources`. HR-4 bundles three styles
/// (professional / teaching / research) and the operator may select any of
/// them, so ALL of them must be on disk for the selection to resolve. `root`
/// is passed in (rather than resolved here) so callers can target either the
/// operator's `$HOME`/`CLAUDE_CONFIG_DIR` (the `user` tier) or, since issue
/// #2125 item 2, the PROJECT directory itself (the `project` tier) — the
/// daemon managed-spawn path launches `claude --setting-sources project,local`,
/// which EXCLUDES the `user` tier, so without a project-tier copy the
/// `outputStyle` id written into the project's `settings.json` cannot resolve
/// and Claude Code silently falls back to its own default. `prepare_session`
/// calls this for BOTH roots; it is a plain additive deploy either way.
/// What: creates `<root>/.claude/output-styles/` if absent, then writes every
/// entry in [`crate::core::bundle::OUTPUT_STYLES`] to its `file_name`, always
/// overwriting so framework upgrades to the styles propagate on the next launch.
/// Returns the path of the default (professional) style for the [`PrepReport`].
/// Test: `deploy_output_style_writes_file`, `deploy_output_style_overwrites`,
/// `deploy_output_style_writes_all_styles`,
/// `deploy_output_style_writes_under_project_root`.
pub(super) fn deploy_output_style(root: &Path) -> Result<PathBuf, PrepError> {
    let style_dir = root.join(".claude").join("output-styles");
    std::fs::create_dir_all(&style_dir).map_err(|source| PrepError::Io {
        path: style_dir.clone(),
        source,
    })?;

    let mut default_path: Option<PathBuf> = None;
    for style in crate::core::bundle::OUTPUT_STYLES {
        let style_path = style_dir.join(style.file_name);
        std::fs::write(&style_path, style.content).map_err(|source| PrepError::Io {
            path: style_path.clone(),
            source,
        })?;
        if style.id == crate::core::bundle::DEFAULT_OUTPUT_STYLE_ID {
            default_path = Some(style_path);
        }
    }

    // The default style is always in the registry; fall back to its conventional
    // path defensively so the return type never has to be optional.
    Ok(default_path.unwrap_or_else(|| style_dir.join("trusty-mpm.md")))
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
/// other keys), sets `outputStyle` to `active_style` (the resolved active id;
/// `None` → [`OUTPUT_STYLE`] default), enables `spinnerTipsEnabled`, sets
/// `spinnerTipsOverride.tips` to [`SPINNER_TIPS`], and writes it back
/// pretty-printed. Creates the file and `.claude/` directory when absent. The
/// caller resolves+validates the id (via
/// [`crate::core::output_style::resolve_active_style`]); an empty/whitespace id
/// is treated as "unset" and falls back to the default here defensively.
/// Test: `prepare_session_sets_output_style`,
/// `write_output_style_preserves_existing_keys`,
/// `write_output_style_sets_spinner_tips`,
/// `write_output_style_sets_active_style`.
pub(super) fn write_output_style(
    project_dir: &Path,
    active_style: Option<&str>,
) -> Result<(), PrepError> {
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

    let style = active_style
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(OUTPUT_STYLE);
    settings["outputStyle"] = serde_json::Value::String(style.to_string());
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
/// other keys), *replaces* the entire `hooks` key with [`TRUSTY_MEMORY_HOOKS`]
/// **plus** a `PreToolUse` PM-enforcement guard entry (see
/// [`pm_guard_hook_value`], issue #1977), and writes it back pretty-printed.
/// Replacing — rather than merging — the `hooks` key avoids double-firing if
/// this runs twice. Creates the file and `.claude/` directory when absent.
/// Test: `write_project_hooks_writes_all_event_types`,
/// `write_project_hooks_registers_pm_guard`,
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
    let mut hooks: serde_json::Value =
        serde_json::from_str(TRUSTY_MEMORY_HOOKS).expect("bundled hook block is valid JSON");
    // Register the PM-enforcement PreToolUse guard (issue #1977) alongside the
    // trusty-memory hooks so the PM is blocked from editing code directly and
    // steered to delegate. The bundled block is an object, so this insert
    // always succeeds; guarded defensively regardless.
    if let Some(obj) = hooks.as_object_mut() {
        obj.insert("PreToolUse".to_string(), pm_guard_hook_value());
    }
    settings["hooks"] = hooks;

    let serialized = serde_json::to_string_pretty(&settings)
        .map_err(|err| PrepError::Deploy(err.to_string()))?;
    std::fs::write(&settings_path, serialized).map_err(|source| PrepError::Io {
        path: settings_path.clone(),
        source,
    })?;
    Ok(())
}

/// Build the `PreToolUse` PM-enforcement guard hook entry (issue #1977).
///
/// Why: the deployed PM instructions' P1–P11 prohibitions ("delegate all file
/// edits", "never run builds/tests yourself") are prompt-only and unenforced.
/// Claude Code's `PreToolUse` hook is the one seam that can *block* a tool call
/// before it runs, so managed PM sessions register `tm hook --pm-guard` there
/// (see `crate::bin::tm::commands::pm_guard`) to turn those prohibitions into
/// enforcement. The command is resolved to an ABSOLUTE binary path via
/// [`resolve_statusline_binary`] — the same PATH-robustness fix #1914 applied to
/// the statusLine command — because a bare `tm hook --pm-guard` would silently
/// no-op under Claude Code's minimal `PATH`, leaving the guard un-fired.
/// What: returns the `PreToolUse` handler-group array with `matcher: ""` (fires
/// for every tool) invoking `<abs-path> hook --pm-guard` with a short timeout.
/// The guard itself fails open (ALLOW) on any error, so a slow/absent binary
/// never blocks the PM.
/// Test: `write_project_hooks_registers_pm_guard`.
pub(super) fn pm_guard_hook_value() -> serde_json::Value {
    serde_json::json!([
        {
            "matcher": "",
            "hooks": [
                {
                    "type": "command",
                    "command": format!("{} hook --pm-guard", resolve_statusline_binary()),
                    "timeout": 10
                }
            ]
        }
    ])
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
///
/// Visibility: `pub(super)` (rather than private) because the sibling
/// `search_index` module's `inject_trusty_search_mcp` also calls this shared
/// read-merge-write helper (issue #610 split).
pub(super) fn inject_mcp_server(
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

/// Inject the `trusty-memory` MCP server into the project's `.mcp.json`,
/// pinned to the project's palace when one can be derived (issue #1605).
///
/// Why: `prepare_session` configures hooks and instructions but, without this,
/// the launched `claude` process has no access to the memory tools because the
/// `trusty-memory` MCP server is never registered in `<project>/.mcp.json`.
/// Issue #1605: the stub must additionally PIN the session to its project's
/// palace via `env.TRUSTY_MEMORY_PALACE`, because a repo_url-cloned managed
/// session lives under a throwaway `<owner>/<repo>/<session-id>/` workspace
/// whose basename is the session-id — so trusty-memory's cwd-based derivation
/// would resolve the wrong palace. The slug is derived from project identity
/// (operator override > git `owner/repo` > parent/dir) so it matches the slug
/// trusty-memory itself would compute for the canonical project root.
/// What: resolves the palace slug via [`resolve_palace_slug`] (passing the
/// explicit `git_remote` from `LaunchParams`/`SessionRecord` when known, else
/// best-effort `git remote get-url origin` in `project_path`), builds the
/// (optionally pinned) server value via [`trusty_memory_mcp_value`], and
/// registers it under the key `trusty-memory`. When no slug can be derived the
/// bare stub is injected — byte-identical to the pre-#1605 behaviour, so the
/// session still gets the memory tools (no regression).
/// Test: `inject_trusty_memory_mcp_adds_server`,
/// `inject_trusty_memory_mcp_preserves_existing`,
/// `inject_trusty_memory_mcp_is_idempotent`,
/// `inject_trusty_memory_mcp_uses_serve_stdio`,
/// `inject_trusty_memory_mcp_pins_palace_from_repo_url`,
/// `inject_trusty_memory_mcp_pins_palace_from_git_remote`.
pub(super) fn inject_trusty_memory_mcp(
    project_path: &Path,
    git_remote: Option<&str>,
) -> Result<(), PrepError> {
    let slug = resolve_palace_slug(project_path, git_remote);
    // Issue #1939: before pinning, heal the claude-mpm split-brain — if the
    // derived `owner-repo` palace does not exist but the BARE repo-name palace
    // does, register a palace-level alias so the pinned `owner-repo` name resolves
    // to the existing bare store. Best-effort and side-effect-only; never fails
    // the launch.
    super::palace_alias::maybe_register_palace_alias(project_path, git_remote);
    inject_mcp_server(
        project_path,
        "trusty-memory",
        trusty_memory_mcp_value(slug.as_deref()),
    )
}

/// Resolve the trusty-memory palace slug to pin for a managed session (#1605).
///
/// Why: the slug must match what trusty-memory itself would derive for the
/// project's canonical identity, so memory written/read in the managed session
/// lands in the project-scoped palace rather than a session-id-named one. The
/// explicit `git_remote` (threaded from `LaunchParams`/`SessionRecord.repo_url`)
/// is the authoritative identity for repo_url-cloned sessions; for local-path
/// sessions where it is `None` we fall back to the workspace's own
/// `git remote get-url origin`, exactly as trusty-memory's `cwd_palace_slug_at`
/// does. The operator `TRUSTY_MEMORY_PALACE` override still wins over both, per
/// `derive_palace_id` precedence.
/// What: reads the `TRUSTY_MEMORY_PALACE` env override (highest precedence),
/// resolves the git remote (explicit arg, else best-effort `git -C
/// <project_path> config --get remote.origin.url`), and returns
/// `trusty_common::derive_palace_id(project_path, git_remote, override)`. Returns
/// `None` when nothing usable can be derived (no override, no remote, root-less
/// path) so the caller injects the bare stub. Best-effort and side-effect-free
/// beyond the read-only `git config` probe; never panics.
/// Test: covered via `inject_trusty_memory_mcp_pins_palace_from_repo_url`
/// (explicit remote) and `resolve_palace_slug_*` (override / git-fallback / none).
fn resolve_palace_slug(project_path: &Path, git_remote: Option<&str>) -> Option<String> {
    let override_value = trusty_common::palace_override_from_env();
    // Prefer the explicit (cloned-from) remote; otherwise probe the workspace's
    // own origin remote so a local-path session still pins by repo identity.
    let probed = match git_remote {
        Some(_) => None,
        None => git_remote_origin(project_path),
    };
    let effective_remote = git_remote.or(probed.as_deref());
    trusty_common::derive_palace_id(project_path, effective_remote, override_value.as_deref())
}

/// Read `remote.origin.url` for the repo containing `start` (best-effort).
///
/// Why: a local-path managed session (no `repo_url` in `LaunchParams`) should
/// still pin its palace by the repo's GitHub identity rather than the directory
/// basename. Shelling out to `git config` (rather than parsing `.git/config`)
/// transparently handles worktrees, mirroring trusty-memory's own
/// `git_remote_origin` so the two derive the same slug.
/// What: runs `git -C <start> config --get remote.origin.url`; returns the
/// trimmed URL on success, `None` when there is no origin remote, git is
/// absent, or `start` is not in a repo. No network.
/// Test: exercised indirectly via `resolve_palace_slug_falls_back_to_git_remote`
/// (a temp git repo) and the daemon-less paths in `inject_trusty_memory_mcp_*`.
pub(super) fn git_remote_origin(start: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(start)
        .arg("config")
        .arg("--get")
        .arg("remote.origin.url")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if url.is_empty() { None } else { Some(url) }
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

/// Inject `"statusLine": {"type":"command","command":"<abs-path> statusline","padding":0}`
/// into `<project>/.claude/settings.json` when the key is absent, or upgrade a
/// stale bare `tm`/`trusty-mpm statusline` default already on disk to the
/// resolved absolute path.
///
/// Why (#1914, same risk class as #1298's `claude_code.rs::resolve_claude`): a
/// bare `tm statusline` command relies on the spawning shell's `PATH`
/// containing wherever `tm` is installed (e.g. `~/.cargo/bin`); under launchd
/// or Claude Code's minimal environment that PATH omission silently drops the
/// statusline segment with no error surfaced anywhere. Resolving an absolute
/// path via [`resolve_statusline_command`] closes that gap the same way
/// `crate::runtime::claude_code::ClaudeCodeAdapter::resolve_claude` already
/// does for the `claude` runtime binary. Because every prior version
/// of this function wrote the exact literal bare string, [`ensure_status_line`]'s
/// resume self-heal (#1913) can recognise that EXACT fingerprint via
/// [`is_stale_bare_statusline_command`] and safely upgrade it in place —
/// without ever touching a genuinely user-customized `statusLine.command`.
/// What: reads the existing `.claude/settings.json` (or `{}` when absent/invalid).
/// When `statusLine` is absent, inserts it with the resolved absolute command.
/// When present and it matches [`is_stale_statusline_command`] (a bare default,
/// OR a `"<binary> statusline"` command whose absolute binary is ephemeral or
/// no longer on disk — #2229), rewrites ONLY its `command` field in place. Any
/// other existing `statusLine` value — a genuine user customization pointing at
/// an existing binary — is left completely untouched. The file is written back
/// pretty-printed only when a change was actually made.
/// Test: `write_status_line_injects_when_absent`,
/// `write_status_line_skips_when_already_set`,
/// `write_status_line_preserves_user_config`,
/// `write_status_line_heals_stale_tm_default`,
/// `write_status_line_heals_stale_trusty_mpm_default`.
pub(super) fn write_status_line(project_dir: &Path) -> Result<(), PrepError> {
    let claude_dir = project_dir.join(".claude");
    std::fs::create_dir_all(&claude_dir).map_err(|source| PrepError::Io {
        path: claude_dir.clone(),
        source,
    })?;
    let settings_path = claude_dir.join("settings.json");
    let mut settings: serde_json::Value = std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    let obj = match settings.as_object_mut() {
        Some(o) => o,
        None => return Ok(()),
    };

    let changed = match obj.get("statusLine") {
        None => {
            obj.insert(
                "statusLine".to_string(),
                serde_json::json!({
                    "type": "command",
                    "command": resolve_statusline_command(),
                    "padding": 0
                }),
            );
            true
        }
        Some(existing) if is_stale_statusline_command(existing) => {
            let resolved = resolve_statusline_command();
            match obj
                .get_mut("statusLine")
                .and_then(serde_json::Value::as_object_mut)
            {
                Some(entry) => {
                    entry.insert("command".to_string(), serde_json::json!(resolved));
                }
                // Defensive (#1914 review finding 2): `is_stale_bare_statusline_command`
                // only returns `true` for a `serde_json::Value::Object` with matching
                // `type`/`command` fields, so `as_object_mut` succeeding here is
                // expected on every real call. If that invariant is ever violated
                // (e.g. a future refactor of the match guard), replace the entry
                // wholesale instead of silently leaving the stale/broken value in
                // place — a silent no-op would defeat the whole point of the heal.
                None => {
                    obj.insert(
                        "statusLine".to_string(),
                        serde_json::json!({
                            "type": "command",
                            "command": resolved,
                            "padding": 0
                        }),
                    );
                }
            }
            true
        }
        // A genuinely user-customized statusLine — never clobber it.
        Some(_) => false,
    };

    if !changed {
        return Ok(());
    }

    let serialized = serde_json::to_string_pretty(&settings)
        .map_err(|err| PrepError::Deploy(err.to_string()))?;
    std::fs::write(&settings_path, serialized).map_err(|source| PrepError::Io {
        path: settings_path,
        source,
    })?;
    Ok(())
}

/// The literal `statusLine.command` strings this module has ever auto-injected.
///
/// Why: a single source of truth for [`is_stale_bare_statusline_command`] and
/// its tests keeps the "what counts as our own stale default" fingerprint from
/// drifting between the two call sites.
/// What: the bare (no absolute path) commands written before #1914 introduced
/// path resolution — one per `[[bin]]` name the `trusty-mpm` crate produces.
pub(super) const KNOWN_BARE_STATUSLINE_COMMANDS: &[&str] =
    &["tm statusline", "trusty-mpm statusline"];

/// Whether an existing `statusLine` entry is our own previously-injected bare
/// default, safe for [`write_status_line`] to upgrade in place.
///
/// Why (#1914): [`write_status_line`] must never clobber a genuinely
/// user-customized `statusLine.command`, but a bare `tm statusline` /
/// `trusty-mpm statusline` written by a pre-#1914 build of this module is not
/// a user customization — it is stale output of this exact function that
/// silently breaks under a minimal PATH. Matching the EXACT literal fingerprint
/// (rather than e.g. "starts with tm") keeps the heuristic conservative: any
/// command with extra flags, an already-absolute path, or a different binary
/// name is left alone.
/// What: returns `true` when `entry.type == "command"` and `entry.command` is
/// exactly one of [`KNOWN_BARE_STATUSLINE_COMMANDS`] (case-sensitive, no extra
/// arguments).
/// Test: `is_stale_bare_statusline_command_matches_known_defaults`,
/// `is_stale_bare_statusline_command_ignores_custom_command`,
/// `is_stale_bare_statusline_command_ignores_non_command_type`.
pub(super) fn is_stale_bare_statusline_command(entry: &serde_json::Value) -> bool {
    entry.get("type").and_then(serde_json::Value::as_str) == Some("command")
        && entry
            .get("command")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|cmd| KNOWN_BARE_STATUSLINE_COMMANDS.contains(&cmd))
}

/// Whether an existing `statusLine` entry is a broken/stale trusty-mpm default
/// that [`write_status_line`] / `ensure_settings_defaults` may safely re-resolve
/// — a strict superset of [`is_stale_bare_statusline_command`].
///
/// Why (#2229): the original heal recognised ONLY the exact pre-#1914 bare
/// literals; every other absolute `statusLine.command` was treated as an
/// untouchable user customization. But a command baked from an ephemeral
/// `current_exe()` (`.../target/debug/deps/tm statusline`, or a worktree path)
/// is NOT a customization — it is our own output pointing at a build artifact
/// that no longer exists on disk, so the statusline silently breaks and never
/// self-heals. This predicate additionally flags a `"<binary> statusline"`
/// command whose absolute binary path is ephemeral or missing on disk, while
/// still leaving a genuine customization that points at an EXISTING,
/// non-ephemeral binary completely alone.
/// What: returns `true` when `entry.type == "command"` and either (a) the
/// command is one of [`KNOWN_BARE_STATUSLINE_COMMANDS`], or (b) the command has
/// our managed `"<binary> statusline"` shape AND the binary token is an
/// absolute path that is either an ephemeral build path
/// ([`trusty_common::bin_resolve::is_ephemeral_build_path`]) or does not exist.
/// A command not ending in ` statusline`, or whose absolute binary still exists
/// and is non-ephemeral, is left untouched.
/// Test: `is_stale_statusline_command_flags_missing_and_ephemeral`,
/// `is_stale_statusline_command_respects_existing_custom_binary`.
pub(crate) fn is_stale_statusline_command(entry: &serde_json::Value) -> bool {
    // Reuse the strict bare-default fingerprint for the pre-#1914 literals so
    // that predicate stays the single source of truth for "our own bare
    // default", then extend it with the ephemeral/missing absolute-path check.
    if is_stale_bare_statusline_command(entry) {
        return true;
    }
    if entry.get("type").and_then(serde_json::Value::as_str) != Some("command") {
        return false;
    }
    let Some(cmd) = entry.get("command").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let Some(binary) = cmd.strip_suffix(" statusline") else {
        return false;
    };
    let path = Path::new(binary);
    path.is_absolute()
        && (trusty_common::bin_resolve::is_ephemeral_build_path(path) || !path.exists())
}

/// Resolve the absolute `<binary> statusline` command to write into
/// `statusLine.command`.
///
/// Why (#1914): see [`write_status_line`]'s doc comment for the full
/// PATH-resolution motivation. `pub(crate)` (rather than private) so
/// `core::standalone::settings_defaults::ensure_settings_defaults` can reuse the
/// SAME absolute-path resolution when seeding `statusLine` into the tm-owned
/// `CLAUDE_CONFIG_DIR` settings.json (issue #2214), re-exported via
/// `session_launch::resolve_statusline_command`.
/// What: appends the `statusline` subcommand to [`resolve_statusline_binary`].
/// Test: exercised indirectly by `write_status_line_injects_when_absent`
/// (asserts the written command is an absolute path ending in ` statusline`).
pub(crate) fn resolve_statusline_command() -> String {
    format!("{} statusline", resolve_statusline_binary())
}

/// The `[[bin]]` names this crate's `Cargo.toml` produces, in fallback order.
///
/// Why (#1914 review, trusty-review PR #1925 finding 1): a machine that only
/// has `trusty-mpm` on `PATH` (not the `tm` alias) must still resolve when
/// `current_exe()` is unavailable — falling back to a single hardcoded "tm"
/// PATH lookup reproduced the exact bug this module fixes for that installed
/// name. Both entries are tried, in order, before degrading to the bare
/// literal.
const STATUSLINE_BIN_NAMES: &[&str] = &["tm", "trusty-mpm"];

/// Resolve the absolute path to the running `tm`/`trusty-mpm` binary.
///
/// Why (#1914): [`write_status_line`] always runs INSIDE the `tm`/`trusty-mpm`
/// process itself — the CLI launch path (`tm session start`) and the daemon
/// resume path (`ensure_status_line`) are both compiled into that same binary
/// — so `std::env::current_exe()` is the single most reliable source of truth:
/// no PATH search needed, the running binary IS the answer. `current_exe()`
/// does not guarantee a canonical (symlink-free) path, so the result is
/// canonicalized best-effort — falling back to the raw path when
/// canonicalization fails (e.g. the exe was deleted/moved since spawn) rather
/// than losing the resolution entirely. This mirrors, but is even more direct
/// than, the `bin_resolve::resolve_binary("claude")` pattern `claude_code.rs`
/// uses for a DIFFERENT binary it does not control.
/// What: thin wrapper over [`resolve_statusline_binary_with`] using the real
/// `std::env::current_exe` (canonicalized best-effort) and
/// [`trusty_common::bin_resolve::resolve_binary`].
/// Test: exercised indirectly by `write_status_line_injects_when_absent`; the
/// resolution priority itself is covered by the `resolve_statusline_binary_with_*`
/// unit tests, which inject fake sources.
fn resolve_statusline_binary() -> String {
    resolve_statusline_binary_with(
        || std::env::current_exe().map(|exe| exe.canonicalize().unwrap_or(exe)),
        trusty_common::bin_resolve::resolve_binary,
    )
}

/// Testable core of [`resolve_statusline_binary`] with its I/O sources injected.
///
/// Why: separating the resolution PRIORITY from the actual `current_exe()` /
/// PATH syscalls lets each branch (current-exe hit, PATH-lookup fallback, bare
/// last resort) be exercised deterministically, mirroring the
/// `has_prior_conversation_in` injected-I/O-root pattern used elsewhere in
/// this crate (`crate::runtime::claude_code`).
/// What: returns `current_exe()`'s path as a `String` when it resolves to
/// valid UTF-8 AND is not an ephemeral build/worktree path (#2229 — a
/// `target/debug` or worktree exe would bake a path that 404s after a rebuild);
/// else the first hit of `path_lookup` over [`STATUSLINE_BIN_NAMES`]
/// (`"tm"` then `"trusty-mpm"` — #1914 review finding 1: a bare single-name
/// PATH lookup reproduces the original bug on a `trusty-mpm`-only install);
/// else the literal `"tm"` so the statusline segment degrades to pre-#1914
/// behaviour rather than disappearing outright.
/// Test: `resolve_statusline_binary_with_prefers_current_exe`,
/// `resolve_statusline_binary_with_rejects_ephemeral_current_exe`,
/// `resolve_statusline_binary_with_falls_back_to_path_lookup`,
/// `resolve_statusline_binary_with_falls_back_to_trusty_mpm_name`,
/// `resolve_statusline_binary_with_falls_back_to_bare_name`.
pub(super) fn resolve_statusline_binary_with(
    current_exe: impl Fn() -> std::io::Result<PathBuf>,
    path_lookup: impl Fn(&str) -> Option<PathBuf>,
) -> String {
    if let Ok(exe) = current_exe()
        && !trusty_common::bin_resolve::is_ephemeral_build_path(&exe)
        && let Some(s) = exe.to_str()
    {
        return s.to_string();
    }
    if let Some(p) = STATUSLINE_BIN_NAMES
        .iter()
        .find_map(|name| path_lookup(name))
        && let Some(s) = p.to_str()
    {
        return s.to_string();
    }
    "tm".to_string()
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
