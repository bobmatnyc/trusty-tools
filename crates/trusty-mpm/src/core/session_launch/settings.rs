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

/// Write the project-tier trusty-mpm-owned hooks into the project's
/// `.claude/settings.json`.
///
/// Why (issue #2003): the daemon's managed-launch path spawns `claude
/// --setting-sources project,local`, excluding the `user` tier where
/// [`crate::core::standalone::hooks::ensure_managed_hooks`] provisions the
/// lifecycle triad (circuit breaker / audit log / dashboard). A managed
/// session therefore never fired those events — only the `trusty-memory` +
/// PM-guard entries this function previously wrote (by REPLACING the entire
/// `hooks` key, which also clobbered any pre-existing hooks the project itself
/// carried). [`super::project_hooks::project_managed_hook_additions`] now
/// folds the SAME triad definition the user-tier writer uses into this
/// project-tier write, and the entry-level replace-by-identity strip (issue
/// #2948's [`crate::core::standalone::hooks::strip_hook_entries_matching_for_events`])
/// removes only OUR OWN prior entries before merging, so a foreign/hand-added
/// hook — even one sharing a matcher group with one of ours — survives.
/// Since #2969 this writes the full lifecycle triad on every managed-session
/// launch, so a crash mid-write has a larger blast radius than before; issue
/// #2972 switched the final write from a plain `fs::write` to
/// [`trusty_common::claude_config::write_json_atomic`] (temp-file + rename) for
/// parity with the sibling `tm hooks clean` mutation path, so a crash or `^C`
/// mid-write can never leave a torn/truncated `settings.json`.
/// What: reads an existing `<project>/.claude/settings.json` (preserving all
/// other keys), strips any existing entry recognised by
/// [`super::project_hooks::is_project_managed_hook_command`] for the events
/// about to be (re)written, deep-merges in
/// [`super::project_hooks::project_managed_hook_additions`] via
/// [`trusty_common::claude_config::merge_hook_entries`], and atomically writes
/// the result back pretty-printed via
/// [`trusty_common::claude_config::write_json_atomic`]. Creates the file and
/// `.claude/` directory when absent.
///
/// `inject_prompt_context` carries the `[hooks] prompt_context` config key
/// (#5034). It gates ONE entry — `UserPromptSubmit` → `trusty-memory
/// prompt-context`. The strip is deliberately NOT narrowed with it (see
/// [`super::project_hooks::project_managed_hook_events`]), so turning the key
/// off also removes the entry a prior launch wrote. Note the write is
/// framework-owned either way: a hand-deleted entry is re-added on the next
/// launch when the key is `true`. The config key, not a manual edit, is the
/// supported off switch.
/// Test: `write_project_hooks_writes_all_event_types`,
/// `write_project_hooks_registers_pm_guard`,
/// `write_project_hooks_replaces_existing`,
/// `project_hooks_tests::write_project_hooks_writes_lifecycle_triad`,
/// `project_hooks_tests::write_project_hooks_preserves_foreign_hooks`,
/// `project_hooks_tests::write_project_hooks_writes_via_atomic_path`,
/// `project_hooks_tests::write_project_hooks_omits_prompt_context_when_disabled`,
/// `project_hooks_tests::write_project_hooks_strips_stale_prompt_context_when_disabled`,
/// `project_hooks_tests::write_project_hooks_enabled_output_is_unchanged_by_the_toggle`.
pub(super) fn write_project_hooks(
    project_dir: &Path,
    inject_prompt_context: bool,
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

    let additions = super::project_hooks::project_managed_hook_additions(inject_prompt_context);

    // Replace-by-identity at entry granularity: strip any existing entry we
    // own (trusty-memory / PM-guard / lifecycle-triad), so re-running this on
    // every launch never duplicates our own groups while a foreign entry
    // sharing one of our matcher groups survives untouched.
    //
    // #5034: the strip covers every event we OWN, not just the ones this call
    // writes. Scoping it to the additions instead would leave a
    // previously-written `UserPromptSubmit` entry in place once the operator
    // sets `[hooks] prompt_context = false`, and the opt-out would do nothing.
    let event_keys = super::project_hooks::project_managed_hook_events();
    crate::core::standalone::hooks::strip_hook_entries_matching_for_events(
        &mut settings,
        Some(&event_keys),
        super::project_hooks::is_project_managed_hook_command,
    );

    let merged = trusty_common::claude_config::merge_hook_entries(&settings, &additions);

    trusty_common::claude_config::write_json_atomic(&settings_path, &merged)
        .map_err(|err| PrepError::Deploy(err.to_string()))?;
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

/// Resolve the trusty-memory palace slug for a managed session (#1605).
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
///
/// // #4181: this used to feed the `.mcp.json` `env` block the memory injector
/// wrote. ADR-0042 deleted that injector; the slug now becomes the
/// `TRUSTY_MEMORY_PALACE` variable the spawn exports, resolved through
/// [`crate::core::mcp_session_env::session_mcp_env`].
/// What: reads the `TRUSTY_MEMORY_PALACE` env override (highest precedence),
/// resolves the git remote (explicit arg, else best-effort `git -C
/// <project_path> config --get remote.origin.url`), and returns
/// `trusty_common::derive_palace_id(project_path, git_remote, override)`. Returns
/// `None` when nothing usable can be derived (no override, no remote, root-less
/// path), in which case the spawn exports no palace variable and trusty-memory
/// falls back to its own cwd derivation. Best-effort and side-effect-free
/// beyond the read-only `git config` probe; never panics.
/// Test: `resolve_palace_slug_*` (override / git-fallback / none), plus
/// `session_mcp_env_exports_palace_when_memory_enabled`.
pub(crate) fn resolve_palace_slug(project_path: &Path, git_remote: Option<&str>) -> Option<String> {
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
/// (a temp git repo).
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

/// Pre-seed per-directory trust acceptance for `workspace` in `~/.claude.json`,
/// and strip any MCP approval a prior version left behind.
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
/// safe — no real user project is affected.
///
/// **#4181 / ADR-0042 — why the `enabledMcpjsonServers` key is REMOVED, not
/// merely left unwritten.** Claude Code's `enabledMcpjsonServers` approval is
/// name-based and content-blind, and a name that appears in it makes the
/// workspace `.mcp.json` entry of that name WIN over the operator's own
/// user-scope declaration. That displacement is what made the #3918→#3950
/// name-squatting chain possible, and it is the mechanism ADR-0042 removes:
/// tm no longer writes MCP config into a workspace, so it approves no project-
/// scope name either. Ceasing to write would leave the key in place on every
/// machine a prior tm ever launched, keeping the displacement alive exactly
/// where a repo could exploit it, so the key is deleted from the entry.
/// What: reads `~/.claude.json` (the JSON config; starts from `{}` when absent
/// — but a malformed *existing* file is left untouched to avoid clobbering the
/// operator's OAuth/login data), ensures `projects.<workspace>` carries
/// `hasTrustDialogAccepted: true`, `hasCompletedProjectOnboarding: true` and
/// `projectOnboardingSeenCount >= 1`, removes `enabledMcpjsonServers`, then
/// writes it back pretty-printed (ATOMICALLY, temp file + rename) preserving
/// every other key. Idempotent — an entry already trusted and already free of
/// the approval key is not rewritten. The OAuth fields are never touched.
///
/// **Concurrency (issue #4072):** the whole read → mutate → write cycle runs
/// under [`crate::core::claude_json_guard::lock`], because
/// `core::home_trust_seed::preseed_home_trust` load-mutate-stores the SAME file
/// and the daemon runs both concurrently, once per session it provisions.
/// Unsynchronised, the slower writer stored a snapshot taken before the faster
/// writer's store, silently deleting that session's whole
/// `projects.<workspace>` entry.
/// Test: `preseed_trust_marks_directory`, `preseed_trust_preserves_other_keys`,
/// `preseed_trust_is_idempotent`, `preseed_trust_leaves_malformed_file`,
/// `concurrent_seeds_preserve_every_workspace_entry`,
/// `concurrent_home_and_workspace_seeds_preserve_both_entries`. The two
/// `enabledMcpjsonServers` claims above are proven end to end, through this
/// function, in `tests_launch_trust_3926.rs`:
/// `prepare_session_writes_no_approval_for_a_builtin_name_in_workspace_mcp_json`
/// (nothing is written, even for a name a repo squats) and
/// `prepare_session_strips_a_stale_enabled_mcp_approval` (an approval a prior
/// version left is removed while the entry's other keys survive).
pub(super) fn preseed_workspace_trust(
    claude_json: &Path,
    workspace: &Path,
) -> Result<(), PrepError> {
    use serde_json::Value;

    // Issue #4072: hold the process-wide `~/.claude.json` lock across the WHOLE
    // read → mutate → write cycle below, not just the write. This function and
    // `core::home_trust_seed::preseed_home_trust` both load-mutate-store the
    // same file, and the daemon runs them concurrently (one pair per session it
    // provisions); unsynchronised, the slower writer stores a snapshot taken
    // before the faster writer's store and silently drops that session's whole
    // `projects.<workspace>` entry. See `core::claude_json_guard`.
    let _guard = crate::core::claude_json_guard::lock();

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

    // Idempotent: skip the write when trust is already fully accepted AND no
    // stale MCP approval remains. The second half is what makes the #4181 strip
    // reach machines a prior version already seeded — without it, an entry that
    // is already trusted returns early and keeps its `enabledMcpjsonServers`
    // forever.
    let already_trusted = entry.get("hasTrustDialogAccepted") == Some(&Value::Bool(true))
        && entry.get("hasCompletedProjectOnboarding") == Some(&Value::Bool(true))
        && !entry.contains_key("enabledMcpjsonServers");
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
    // #4181: drop the project-scope MCP approval. A name listed here makes the
    // workspace `.mcp.json` entry of that name override the operator's own
    // user-scope declaration, which is the displacement ADR-0042 removes.
    entry.remove("enabledMcpjsonServers");

    // Issue #4072: write through the shared ATOMIC helper (temp file + rename,
    // the same one `home_trust_seed::preseed_home_trust` already uses) rather
    // than a truncating `std::fs::write`. A truncating write is observable
    // mid-flight: a concurrent reader — the sibling seeder, `tm doctor`, or
    // Claude Code itself — could read the file while it was half-written and
    // see malformed JSON, which every reader in this crate treats as "skip,
    // leave it alone" (silently losing the seed) rather than as an error.
    trusty_common::claude_config::write_json_atomic(claude_json, &config)
        .map_err(|err| PrepError::Deploy(format!("write {}: {err}", claude_json.display())))?;
    Ok(())
}

/// Pre-seed workspace trust in the user's `~/.claude.json`.
///
/// Why: thin home-resolving wrapper over [`preseed_workspace_trust`] so
/// `prepare_session` can call it without knowing the config-dir layout; tests
/// target a temp file via the inner function directly.
///
/// #5544: the home comes from `fw.home_tier_dir()`, NOT `fw.claude_home_dir()`.
/// `claude_home_dir()` walks up from `claude_agents`, which
/// `FrameworkPaths::for_managed_project` rewrites to
/// `<workspace>/.claude/agents` — so for every managed session (`tm launch`,
/// `tm connect`, daemon spawns, `tm register/load/run`) it names the WORKSPACE
/// and this would drop an untracked `<workspace>/.claude.json` into the
/// operator's repo. `~/.claude.json` is a per-USER file; `home_tier` is the
/// field that says so and that no constructor rewrites.
/// What: resolves `<home tier>/.claude.json` and delegates. An unresolved home
/// tier is a soft skip (logged), matching the pre-#5544 `dirs::home_dir()`
/// behaviour — seeding a trust record beside the process's working directory
/// would be worse than not seeding one.
/// Test: `prepare_session_does_not_seed_the_workspace_on_the_managed_path`,
/// `home_writes_are_skipped_when_the_home_tier_is_unresolved`; the seeding
/// behaviour itself by the inner `preseed_trust_*` tests.
pub(super) fn preseed_workspace_trust_home(
    fw: &crate::core::paths::FrameworkPaths,
    workspace: &Path,
) -> Result<(), PrepError> {
    let Some(home) = resolved_home_tier(fw) else {
        return Ok(());
    };
    preseed_workspace_trust(&home.join(".claude.json"), workspace)
}

/// The user-global home tier, or `None` when this layout has none.
///
/// Why (#5544): both callers below touch files that belong to the USER, not to
/// a project — `~/.claude.json` and `~/.claude/settings.json`. Routing them
/// through `fw.claude_home_dir()` pointed them at the workspace on every
/// managed path, because that accessor derives its base from `claude_agents`,
/// which the managed constructors rewrite. `home_tier` is stored, not derived,
/// so it cannot be relocated by a deploy-destination change.
/// What: returns `fw.home_tier_dir()`, logging at `warn!` when absent so the
/// declined write is visible rather than silent.
/// Test: `home_writes_are_skipped_when_the_home_tier_is_unresolved`.
fn resolved_home_tier(fw: &crate::core::paths::FrameworkPaths) -> Option<PathBuf> {
    match fw.home_tier_dir() {
        Some(home) => Some(home.to_path_buf()),
        None => {
            tracing::warn!(
                "skipping user-global session writes: no home directory resolved for this \
                 framework layout"
            );
            None
        }
    }
}

/// Remove the `trusty-memory` hook entries from `~/.claude/settings.json`.
///
/// Why: `trusty-memory` hooks were previously registered globally, so they
/// fired for every Claude Code session. Now that [`write_project_hooks`]
/// scopes them to trusty-mpm projects, the global entries must be removed to
/// stop them double-firing (and firing for unrelated sessions like claude-mpm).
///
/// #5544: resolved from `fw.home_tier_dir()` — see
/// [`preseed_workspace_trust_home`] for why NOT `claude_home_dir()`. Pointing
/// this at a managed workspace was the worse half of that bug: the file does not
/// exist there, a missing file is success by contract, so the operator's REAL
/// global hooks were never removed and kept double-firing with nothing to show
/// for it. No second implementation of this cleanup exists to catch it.
/// What: reads `<home tier>/.claude/settings.json`, and for each event in
/// [`GLOBAL_TRUSTY_MEMORY_EVENTS`] filters out handler groups whose `hooks`
/// array contains a command matching `trusty-memory hooks fire`. An event key
/// whose array becomes empty is removed entirely. Writes the file back. A
/// missing or malformed file is treated as success (nothing to clean up).
/// Test: `remove_global_hooks_removes_trusty_memory_entries`,
/// `prepare_session_confines_home_writes_to_the_framework_base`.
pub(super) fn remove_global_trusty_memory_hooks(
    fw: &crate::core::paths::FrameworkPaths,
) -> Result<(), PrepError> {
    let Some(home) = resolved_home_tier(fw) else {
        return Ok(());
    };
    clean_global_trusty_memory_hooks(&home.join(".claude").join("settings.json"))
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
///
/// #4058: sourced from [`crate::core::own_binary_names::OWN_BINARY_NAMES`]
/// rather than a second hand-copy of the two names, so the SET can't drift
/// out of sync with `daemon::discovery` and the `find_daemon_pids` daemon-PID
/// scan again. `OWN_BINARY_NAMES` is deliberately ordered `tm` before
/// `trusty-mpm` for exactly this call site's fallback order — see that
/// constant's module doc. (`core::standalone::hooks::MPM_BIN_NAMES` needs the
/// opposite order for a different lookup, so it keeps its own array rather
/// than aliasing this one; a test pins the two to the same set.)
const STATUSLINE_BIN_NAMES: &[&str] = crate::core::own_binary_names::OWN_BINARY_NAMES;

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
/// `resolve_statusline_binary_with_rejects_system_temp_current_exe`,
/// `resolve_statusline_binary_with_falls_back_to_path_lookup`,
/// `resolve_statusline_binary_with_falls_back_to_trusty_mpm_name`,
/// `resolve_statusline_binary_with_falls_back_to_bare_name`.
pub(super) fn resolve_statusline_binary_with(
    current_exe: impl Fn() -> std::io::Result<PathBuf>,
    path_lookup: impl Fn(&str) -> Option<PathBuf>,
) -> String {
    // #4485/#4492: the guard now also rejects system temp roots, so a
    // `current_exe()` under an agent harness's temp scratchpad no longer gets
    // persisted into `statusLine.command`. Same guard as the hooks site by
    // design — do not add a second check here.
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
