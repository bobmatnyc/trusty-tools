//! Claude Code runtime adapter.
//!
//! Why: the session manager must have a concrete adapter that launches the
//! `claude` CLI inside a tmux session without leaking `ANTHROPIC_API_KEY` into
//! the pane environment; `env -u ANTHROPIC_API_KEY claude` achieves that.
//! What: [`ClaudeCodeAdapter`] wraps a [`ManagedTmuxDriver`] and implements
//! [`RuntimeAdapter`]; `spawn` sends the env-scrubbed command to the tmux pane
//! after verifying the `claude` binary is on PATH.
//! Test: `claude_code_adapter_spawn_sends_env_scrub_command`,
//! `claude_code_adapter_identifies`.

use std::path::Path;
use std::sync::Arc;

use tracing::debug;

use crate::session_manager::ManagedTmuxDriver;

use super::RuntimeAdapter;
use super::RuntimeError;

/// Single-quote a string for safe interpolation into the pane shell command.
///
/// Why: [`env_bin_prefix`] interpolates the resolved `CLAUDE_CONFIG_DIR` path into
/// a command string that `send_line` types into the tmux pane's live shell. An
/// UNQUOTED path containing a space — e.g. a macOS home `/Users/John Doe/…` —
/// word-splits, so `env` sees `CLAUDE_CONFIG_DIR=/Users/John` plus a stray
/// `Doe/…` argv entry; `claude` never execs and the pane silently dies with no
/// error surfaced anywhere. POSIX single-quoting disables ALL word-splitting,
/// globbing, and expansion, so any path survives intact.
/// What: wraps `s` in single quotes, escaping any embedded single quote with the
/// canonical close-reopen sequence `'\''` (a macOS home never contains a `'`, but
/// the escape keeps the quoting correct for arbitrary paths and is cheap).
/// Test: `env_bin_prefix_quotes_config_dir_with_space`,
/// `shell_single_quote_escapes_embedded_quote`.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Build the `export TM_MANAGED_SESSION_ID=…;` prefix injected into the pane
/// shell ahead of every managed launch/resume command (#2023 component B).
///
/// Why: `tmux set-environment` only updates the tmux *session* environment —
/// the pane's login shell was already forked (and its environment snapshotted)
/// at session creation, BEFORE this command is ever sent, so it never picks up
/// a `set-environment` change. Exporting the variable as part of the literal
/// shell line sent via `send_line` is the only way the assignment lands in
/// THAT shell's live environment, which is what makes it survive `claude`
/// exiting and dropping back to the pane's shell — the exact case the
/// in-place-relaunch feature (#2023 component C) depends on: a command run in
/// the pane after Claude exits needs to identify which managed session it is.
/// What: `export TM_MANAGED_SESSION_ID='<session_id>'; ` (single-quoted via
/// [`shell_single_quote`], trailing space so it concatenates cleanly with the
/// `env …` prefix that follows). `session_id` is a UUID and never contains a
/// single quote, but the same escaping used for `CLAUDE_CONFIG_DIR` is applied
/// for defense in depth.
/// Test: `spawn_command_exports_managed_session_id`,
/// `resume_command_exports_managed_session_id`.
fn session_id_export_prefix(session_id: &str) -> String {
    format!(
        "export TM_MANAGED_SESSION_ID={}; ",
        shell_single_quote(session_id)
    )
}

/// The one-line hint printed to the pane AFTER `claude` exits (#2023 component D).
///
/// Why: component A leaves the pane alive as a bare shell when the runtime
/// exits, and component C lets a bare `tm` run from inside that pane relaunch
/// the same session in place — but neither is discoverable unless the pane
/// itself says so. Backticks are used around the literal command name; they
/// have no special meaning inside the single-quoted `echo` argument this is
/// embedded in (see [`on_exit_hint_suffix`]), so no escaping is needed.
/// What: a short, single-line, non-panicking string.
/// Test: `spawn_command_prints_relaunch_hint_after_claude_exits`,
/// `resume_command_prints_relaunch_hint_after_claude_exits`.
const RELAUNCH_HINT: &str = "tm: run `tm` to relaunch this session";

/// Build the `; echo '<hint>'` suffix appended AFTER every managed
/// launch/resume command (#2023 component D).
///
/// Why: `;` sequences the `echo` to run only once the preceding `claude`
/// invocation exits (successfully or not) and control returns to the pane's
/// shell — exactly the moment component A leaves the pane idle at, and
/// exactly what the in-place relaunch (component C) needs advertised.
/// What: `; echo '<RELAUNCH_HINT>'`, single-quoted via [`shell_single_quote`]
/// for the same reason [`env_bin_prefix`] quotes `CLAUDE_CONFIG_DIR` — a
/// literal constant with no shell metacharacters, but quoting defensively
/// costs nothing and matches this file's established convention.
/// Test: `spawn_command_prints_relaunch_hint_after_claude_exits`,
/// `resume_command_prints_relaunch_hint_after_claude_exits`.
fn on_exit_hint_suffix() -> String {
    format!("; echo {}", shell_single_quote(RELAUNCH_HINT))
}

/// Compose the `env …` prefix + resolved binary that starts every managed
/// `claude`, wiring in the tm-owned `CLAUDE_CONFIG_DIR` when available (DOC-34).
///
/// Why: two invariants must hold on the pane command. (1) `-u ANTHROPIC_API_KEY`
/// strips the API key so Claude Code falls back to OAuth, preventing key leakage
/// through pane history. (2) When a managed `CLAUDE_CONFIG_DIR` is resolved,
/// `env -u ANTHROPIC_API_KEY CLAUDE_CONFIG_DIR='<dir>'` points the session at the tm-owned config home
/// (`~/.trusty-tools/trusty-mpm/claude-config/`). Under the retained
/// `--setting-sources project,local` flag (see [`spawn_command`]) this config
/// home does NOT supply the agent roster/skills/hooks — those load from the
/// project layer — but it DOES isolate the session's auth: with the API key
/// scrubbed, `claude` authenticates via the keychain/`.credentials.json` keyed to
/// this config-dir path (the tm-managed login), never touching the operator's
/// `~/.claude`. The path is SINGLE-QUOTED via [`shell_single_quote`] so a home
/// dir with a space does not word-split and break the pane. Both settings are
/// applied via a single `env` prefix, consistent with the scrub-only prefix.
/// What: `env -u ANTHROPIC_API_KEY CLAUDE_CONFIG_DIR='<dir>' <claude_bin>` (path
/// single-quoted) when `config_dir` is `Some`; the legacy
/// `env -u ANTHROPIC_API_KEY <claude_bin>` when `None` (home unresolved — no
/// config dir to point at). The `-u NAME` option MUST precede any `NAME=VALUE`
/// assignment per POSIX `env` grammar (`env [OPTION]... [NAME=VALUE]...
/// [COMMAND]...`); putting `CLAUDE_CONFIG_DIR=<dir>` before `-u` makes `env`
/// stop parsing options at the first `NAME=VALUE` and try to exec `-u` as a
/// command (`env: -u: No such file or directory`), which fatally kills every
/// managed session spawn.
/// Test: `spawn_command_contains_env_scrub`, `spawn_command_sets_claude_config_dir`,
/// `spawn_command_without_config_dir_omits_it`,
/// `env_bin_prefix_quotes_config_dir_with_space`,
/// `env_bin_prefix_orders_unset_flag_before_config_dir_assignment`.
fn env_bin_prefix(claude_bin: &str, config_dir: Option<&Path>) -> String {
    match config_dir {
        Some(dir) => {
            let quoted = shell_single_quote(&dir.display().to_string());
            format!("env -u ANTHROPIC_API_KEY CLAUDE_CONFIG_DIR={quoted} {claude_bin}")
        }
        None => format!("env -u ANTHROPIC_API_KEY {claude_bin}"),
    }
}

/// Build the `--append-system-prompt-file <path>` flag fragment for a spawn
/// command, when a prompt file was written (issue #2125 item 3).
///
/// Why: the flag must be single-quoted for the same reason
/// [`env_bin_prefix`] quotes `CLAUDE_CONFIG_DIR` — the prompt file lives under
/// `std::env::temp_dir()`, which is not attacker-controlled, but quoting
/// defensively costs nothing and matches this file's established convention.
/// What: `" --append-system-prompt-file '<path>'"` when `prompt_file` is
/// `Some`; an empty string when `None` (no prompt was built — e.g. the write
/// failed — so the flag is simply omitted rather than passing a bad path).
/// Test: `spawn_command_with_prompt_file_contains_flag`,
/// `spawn_command_without_prompt_file_omits_flag`.
fn prompt_file_flag(prompt_file: Option<&Path>) -> String {
    match prompt_file {
        Some(p) => format!(
            " --append-system-prompt-file {}",
            shell_single_quote(&p.display().to_string())
        ),
        None => String::new(),
    }
}

/// The shell command sent to the tmux pane to start Claude Code.
///
/// Why: the env prefix (see [`env_bin_prefix`]) strips `ANTHROPIC_API_KEY` and,
/// when available, injects the tm-owned `CLAUDE_CONFIG_DIR` for AUTH isolation.
/// `--setting-sources project,local` is RETAINED and is LOAD-BEARING for where
/// the roster comes from: it restricts every setting source Claude Code loads —
/// settings.json, subagents (`agents/*.md`), and skills (`skills/`) — to the
/// `project` + `local` tiers, EXCLUDING the `user` tier. Because
/// `CLAUDE_CONFIG_DIR` relocates the `user` tier, the agents/skills/hooks the
/// daemon provisions INTO that config home are NOT read while this flag is in
/// force (empirically verified against `claude` 2.1.201, DOC-34 review). The
/// full framework roster, skills, project hooks (trusty-memory + PM-guard) and
/// MCP servers are instead delivered by the PROJECT layer — `<workspace>/.claude/
/// {agents,skills,settings.json,.mcp.json}` — which `session_launch::
/// prepare_session` (run on every daemon spawn path) deploys and which
/// `project,local` DOES load. The flag also excludes the operator's global
/// `~/.claude/settings.json` (#1269). `--dangerously-skip-permissions` keeps the
/// unattended orchestration session from blocking on per-tool prompts (#1269).
/// Both flags reuse the shared [`crate::core::model_inject::SETTING_SOURCES_FLAG`]
/// / [`crate::core::model_inject::PERMISSION_MODE_FLAG`] constants so this spawn
/// path and the CLI launch path can never drift. `prompt_file` (issue #2125
/// item 3) carries the PM system prompt via `--append-system-prompt-file` — the
/// same mechanism the CLI `tm launch` / client `/connect` paths already use —
/// so this, previously the one driver missing it, can no longer silently spawn
/// vanilla Claude Code.
/// What: [`session_id_export_prefix`] followed by [`env_bin_prefix`], the
/// optional [`prompt_file_flag`], the two shared flag constants, followed by
/// [`on_exit_hint_suffix`] (#2023 component D — `; echo '<hint>'`, which only
/// runs once `claude` exits and control returns to the pane); piped to
/// `tmux send-keys … Enter`. `claude_bin` is the resolved binary — an absolute
/// path under launchd so the pane (which inherits the daemon's minimal `PATH`)
/// does not need `claude` on its own `PATH` (#1298). `session_id` is the
/// managed session's UUID (#2023 component B), exported so in-pane commands
/// can identify the session after `claude` exits.
/// Test: `spawn_command_contains_env_scrub`,
/// `spawn_command_contains_isolation_flags`,
/// `spawn_command_uses_resolved_binary`,
/// `spawn_command_sets_claude_config_dir`,
/// `spawn_command_exports_managed_session_id`,
/// `spawn_command_prints_relaunch_hint_after_claude_exits`,
/// `spawn_command_with_prompt_file_contains_flag`,
/// `spawn_command_without_prompt_file_omits_flag`.
fn spawn_command(
    claude_bin: &str,
    config_dir: Option<&Path>,
    session_id: &str,
    prompt_file: Option<&Path>,
) -> String {
    format!(
        "{}{}{} {} {}{}",
        session_id_export_prefix(session_id),
        env_bin_prefix(claude_bin, config_dir),
        prompt_file_flag(prompt_file),
        crate::core::model_inject::SETTING_SOURCES_FLAG,
        crate::core::model_inject::PERMISSION_MODE_FLAG,
        on_exit_hint_suffix(),
    )
}

/// Build and write the PM system-prompt file for `project_dir`, for injection
/// into the daemon managed-spawn command via `--append-system-prompt-file`
/// (issue #2125 item 3 — the daemon-adapter carrier).
///
/// Why: this module's own DOC-34 audit trail established that under
/// `--setting-sources project,local` the framework roster/instructions load
/// from the PROJECT layer, not the `CLAUDE_CONFIG_DIR` [`prepare_managed_config`]
/// provisions — but `spawn` never actually passed `--append-system-prompt-file`
/// at all, so the one thing that turns a bare `claude` process into a
/// trusty-mpm PM never reached the daemon's default managed-spawn path,
/// leaving every bare-`tm` session running vanilla Claude Code (#2125). This
/// reuses the exact same seam
/// ([`crate::core::session_launch::build_system_prompt_for_with_style_and_native`])
/// the CLI `tm launch` / client `/connect` paths already use, so all three
/// drivers build byte-identical prompts for the same project.
/// What: resolves live native-output-style support (fail-safe to injection via
/// [`crate::core::output_style::claude_supports_native_output_style`]), builds
/// the override-resolved + style-injected prompt for `project_dir`, and writes
/// it to a fresh temp file via [`crate::core::model_inject::write_prompt_file`].
/// Returns `None` (logged) on any write failure so `spawn` still proceeds —
/// matching the non-fatal pattern every other `prepare_managed_config` step in
/// this file follows. There is no CLAUDE.md-carrier fallback: #2173 made
/// "trusty-mpm must never modify the target project's CLAUDE.md" a hard
/// constraint, so a write failure here means the session runs without the
/// injected PM system prompt rather than falling back to a different carrier.
/// Test: `build_prompt_file_writes_resolved_prompt_for_project`.
fn build_prompt_file(project_dir: &Path) -> Option<std::path::PathBuf> {
    let native = crate::core::output_style::claude_supports_native_output_style();
    let prompt = crate::core::session_launch::build_system_prompt_for_with_style_and_native(
        project_dir,
        None,
        native,
    );
    let file = crate::core::model_inject::write_prompt_file(&prompt);
    if file.is_none() {
        tracing::warn!(
            project = %project_dir.display(),
            "failed to write PM system-prompt file; spawning without --append-system-prompt-file"
        );
    }
    file
}

/// Build a resume-aware Claude Code command (#1744, #1840).
///
/// Why: `resume_managed` must restore the prior conversation when one is known.
/// `--resume <id>` restores the exact Claude Code conversation identified by
/// `claude_session_id`; `--continue` resumes the most-recent conversation in
/// the workspace when the id is absent AND prior conversation history exists;
/// neither flag is passed when there is no prior conversation, preventing the
/// "No conversation found to continue" error that would otherwise drop the
/// session to a bare shell (#1840).
/// What: `Some(id)` → `--resume <id>`. `None, has_prior_conv=true` → `--continue`.
/// `None, has_prior_conv=false` → plain spawn (no resume flag), so the session
/// starts a fresh conversation instead of erroring. All three share the same
/// env prefix (scrub + `CLAUDE_CONFIG_DIR`) and isolation flags as
/// [`spawn_command`], and all three get [`on_exit_hint_suffix`] appended
/// (#2023 component D) so the pane always prints the relaunch hint once
/// `claude` exits, whichever branch fired. `prompt_file` (#2230) carries the
/// PM system prompt via `--append-system-prompt-file` the same way
/// [`spawn_command`] does, so resumed/guided-resume/crash-recovery sessions
/// get the identical carrier a fresh spawn gets instead of falling back to a
/// vanilla `claude` invocation.
/// Test: `resume_command_with_id_uses_resume_flag`,
/// `resume_command_without_id_with_prior_conv_uses_continue`,
/// `resume_command_without_id_no_prior_conv_uses_plain_spawn`,
/// `resume_command_sets_claude_config_dir`,
/// `resume_command_exports_managed_session_id`,
/// `resume_command_prints_relaunch_hint_after_claude_exits`,
/// `resume_command_with_prompt_file_contains_flag`.
fn resume_command(
    claude_bin: &str,
    config_dir: Option<&Path>,
    claude_session_id: Option<&str>,
    has_prior_conv: bool,
    session_id: &str,
    prompt_file: Option<&Path>,
) -> String {
    let base = format!(
        "{}{}{} {} {}",
        session_id_export_prefix(session_id),
        env_bin_prefix(claude_bin, config_dir),
        prompt_file_flag(prompt_file),
        crate::core::model_inject::SETTING_SOURCES_FLAG,
        crate::core::model_inject::PERMISSION_MODE_FLAG,
    );
    let cmd = match claude_session_id {
        Some(id) => format!("{base} --resume {id}"),
        None if has_prior_conv => format!("{base} --continue"),
        None => base, // No prior conversation: start fresh to avoid "no conversation found".
    };
    format!("{cmd}{}", on_exit_hint_suffix())
}

/// Encode a workspace path the same way Claude Code names its project dir.
///
/// Why: both the `--continue`-eligibility check ([`has_prior_conversation_in`])
/// and the `--resume <id>`-existence check ([`session_id_exists_in`]) must
/// derive the SAME on-disk project directory name for a given `cwd` — if the
/// two ever computed the encoding differently (e.g. one gets tweaked and the
/// other doesn't), `--continue` detection and `--resume` existence detection
/// would silently disagree about which conversations exist. Sharing one
/// helper makes that impossible.
/// What: replaces every `/` in the path with `-` (Claude Code's project-dir
/// naming scheme). A leading `/` becomes `-`, so `/private/tmp/foo` →
/// `-private-tmp-foo`.
/// Test: `encode_project_dir_replaces_slashes`,
/// `has_prior_conversation_returns_true_when_jsonl_exists`,
/// `session_id_exists_true_for_real_jsonl_file`,
/// `session_id_exists_finds_hardcoded_dir_name_for_known_cwd` (non-circular
/// regression guard against encoding-scheme drift).
fn encode_project_dir(cwd: &Path) -> String {
    cwd.to_string_lossy().replace('/', "-")
}

/// Detect whether Claude Code has prior conversation history for `cwd` (#1840).
///
/// Why: `claude --continue` exits with "No conversation found to continue" when
/// no prior conversation exists for the workspace. This guard prevents that error.
/// What: delegates to [`has_prior_conversation_in`] with the standard
/// `~/.claude/projects/` directory derived from `dirs::home_dir()`.
/// Test: `has_prior_conversation_returns_false_for_fresh_workspace` exercises
/// the inner function directly via `has_prior_conversation_in`.
fn has_prior_conversation(cwd: &Path) -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    has_prior_conversation_in(cwd, &home.join(".claude").join("projects"))
}

/// Inner implementation of conversation-history detection, testable with injected dir.
///
/// Why: allows unit tests to pass a temp directory as `projects_dir` without
/// mutating the `HOME` environment variable (which is thread-unsafe under parallel
/// test execution). Separating the I/O root from the detection logic keeps the
/// detection pure and mockable.
/// What: returns `true` when `<projects_dir>/<encoded-cwd>/` exists and contains
/// at least one `.jsonl` file. The encoded path comes from
/// [`encode_project_dir`] (every `/` replaced with `-`); a leading `/` becomes
/// `-`, so `/private/tmp/foo` → `-private-tmp-foo`.
/// Test: `has_prior_conversation_returns_false_for_fresh_workspace`,
/// `has_prior_conversation_returns_true_when_jsonl_exists`.
fn has_prior_conversation_in(cwd: &Path, projects_dir: &Path) -> bool {
    if !projects_dir.is_dir() {
        return false;
    }
    let project_dir = projects_dir.join(encode_project_dir(cwd));
    project_dir.is_dir()
        && std::fs::read_dir(&project_dir)
            .map(|d| {
                d.flatten()
                    .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
            })
            .unwrap_or(false)
}

/// Resolve the Claude Code `projects` directory used for session storage.
///
/// Why: `CLAUDE_CONFIG_DIR` relocates the ENTIRE config home for a managed
/// session — not just settings/agents/skills but also the conversation-history
/// store under `<config_dir>/projects/` (verified empirically: this daemon's
/// own tool-result artifacts land under
/// `~/.trusty-tools/trusty-mpm/claude-config/projects/<encoded-cwd>/...`). So
/// the existence check for a stored `claude_session_id` (#2013) must look under
/// the SAME `config_dir` the session was/will be launched with, not always
/// `~/.claude/projects` — otherwise every id would appear "missing" for managed
/// sessions and resume would never use `--resume`.
/// What: `<config_dir>/projects` when a managed config dir is resolved; falls
/// back to `~/.claude/projects` when `config_dir` is `None` (home-unresolved
/// legacy path, matching [`has_prior_conversation`]'s fallback).
/// Test: `session_id_exists_prefers_config_dir_projects_when_present`,
/// `session_id_exists_falls_back_to_home_claude_when_no_config_dir`.
fn projects_dir_for(config_dir: Option<&Path>) -> Option<std::path::PathBuf> {
    if let Some(dir) = config_dir {
        return Some(dir.join("projects"));
    }
    dirs::home_dir().map(|h| h.join(".claude").join("projects"))
}

/// Existence-check a stored `claude_session_id` against Claude Code's local
/// session store before trusting `--resume <id>` (#2013).
///
/// Why: a `claude_session_id` persisted from a prior run can go stale (the
/// session file was pruned, moved, or never made it to disk after a crash).
/// `claude --resume <missing-id>` fails hard with no graceful recovery, which
/// turns `tm` resume into a dead end. Checking first lets the caller fall back
/// to `--continue` or a plain spawn instead of a hard failure.
/// What: best-effort filesystem check — `true` only when
/// `<projects_dir>/<encoded-cwd>/<id>.jsonl` exists as a regular file, where
/// `projects_dir` is resolved via [`projects_dir_for`] and the cwd is encoded
/// via the shared [`encode_project_dir`] helper (every `/` becomes `-`) — the
/// same encoding [`has_prior_conversation_in`] uses, so the two checks can
/// never silently disagree.
/// Never panics: an unresolvable projects dir, a missing file, or any I/O
/// error all conservatively resolve to `false` (safest outcome — it only ever
/// causes an extra fallback, never a hard failure or a wrong `--resume`).
/// Test: `session_id_exists_true_for_real_jsonl_file`,
/// `session_id_exists_false_for_missing_id`,
/// `session_id_exists_false_when_projects_dir_absent`.
fn session_id_exists(cwd: &Path, config_dir: Option<&Path>, id: &str) -> bool {
    match projects_dir_for(config_dir) {
        Some(projects_dir) => session_id_exists_in(cwd, &projects_dir, id),
        None => false,
    }
}

/// Inner implementation of the session-id existence check, testable with an
/// injected `projects_dir` (mirrors [`has_prior_conversation_in`]'s pattern).
///
/// Why: unit tests need to point at a temp directory rather than mutating
/// `HOME`/`CLAUDE_CONFIG_DIR` process-globals.
/// What: encodes `cwd` via [`encode_project_dir`] (every `/` → `-`) and checks
/// `<projects_dir>/<encoded-cwd>/<id>.jsonl` is a regular file.
/// Test: `session_id_exists_true_for_real_jsonl_file`,
/// `session_id_exists_false_for_missing_id`,
/// `session_id_exists_finds_hardcoded_dir_name_for_known_cwd`.
fn session_id_exists_in(cwd: &Path, projects_dir: &Path, id: &str) -> bool {
    projects_dir
        .join(encode_project_dir(cwd))
        .join(format!("{id}.jsonl"))
        .is_file()
}

/// Resolve, provision, and trust-seed the managed `CLAUDE_CONFIG_DIR` for a spawn.
///
/// Why (DOC-34): every managed spawn points `claude` at the tm-owned config home
/// primarily for AUTH + TRUST isolation — with the API key scrubbed the session
/// authenticates via the keychain/`.credentials.json` keyed to this config-dir
/// path, and its per-workspace trust is seeded into `<config_dir>/.claude.json`
/// (NEVER `~/.claude.json`), so a managed session never reads or writes the
/// operator's `~/.claude`. NOTE (DOC-34 review): under the retained
/// `--setting-sources project,local` flag the config home's provisioned
/// agents/skills/settings.json are NOT loaded (that flag excludes the `user`
/// tier this dir relocates); the framework roster/skills/hooks are delivered by
/// the PROJECT layer via `session_launch::prepare_session`. The full-roster
/// provisioning here is therefore belt-and-suspenders (it would load only if the
/// flag were ever dropped) — the load-bearing effect of the config dir is auth +
/// trust isolation. This centralises the three coupled steps — resolve the path,
/// provision it, and seed workspace trust — so `spawn` and `spawn_resume` stay
/// identical and cannot drift.
/// What: resolves [`crate::core::trusty_tools_config::managed_claude_config_dir`].
/// When `Some`: provisions it via
/// [`crate::core::managed_config::ensure_managed_config_dir`] and seeds managed
/// trust via [`crate::core::standalone::preseed_managed_trust`] (both non-fatal —
/// a failure logs a warning but the dir is still returned so the session never
/// silently falls back to the project's `.claude/`), returning `Some(dir)`. When
/// `None` (home unresolved): falls back to the legacy
/// [`crate::core::home_trust_seed::preseed_home_trust`] and returns `None`
/// (no `CLAUDE_CONFIG_DIR` to inject).
/// Test: exercised via `spawn_sends_env_scrub_when_binary_available` and the
/// `spawn_command`/`resume_command` config-dir tests (the command-string layer);
/// the provisioning itself is covered in `core::managed_config`.
fn prepare_managed_config(tmux_name: &str, cwd: &Path) -> Option<std::path::PathBuf> {
    let Some(config_dir) = crate::core::trusty_tools_config::managed_claude_config_dir() else {
        // Home unresolved (stripped env): no config dir to point at. Fall back
        // to the legacy home-trust seed so startup prompts are still dismissed.
        if let Err(e) = crate::core::home_trust_seed::preseed_home_trust(cwd) {
            tracing::warn!(
                session = %tmux_name,
                cwd = %cwd.display(),
                "home trust pre-seed failed (non-fatal): {e}"
            );
        }
        return None;
    };

    // Provision the tm-owned config dir with the full framework roster. Non-fatal:
    // even on a partial provisioning error we still point CLAUDE_CONFIG_DIR at it,
    // because that is strictly safer than falling back to the project's committed
    // `.claude/` (the #1996 regression this whole change exists to prevent).
    if let Err(e) = crate::core::managed_config::ensure_managed_config_dir(&config_dir) {
        tracing::warn!(
            session = %tmux_name,
            config_dir = %config_dir.display(),
            "managed config dir provisioning failed (non-fatal): {e}"
        );
    }

    // Seed workspace trust into <config_dir>/.claude.json (isolation invariant:
    // NEVER ~/.claude.json) so the session starts without the trust/MCP dialogs.
    if let Err(e) = crate::core::standalone::preseed_managed_trust(&config_dir, cwd) {
        tracing::warn!(
            session = %tmux_name,
            cwd = %cwd.display(),
            "managed trust pre-seed failed (non-fatal): {e}"
        );
    }

    Some(config_dir)
}

/// Owned pieces of an in-place `claude` relaunch command, built for direct
/// process `exec` rather than a tmux `send_line` (#2023 component C).
///
/// Why: [`spawn_command`]/[`resume_command`] build single shell STRINGS meant
/// for `tmux send-keys` into a pane whose shell does the quoting/splitting.
/// The bare-`tm` in-pane relaunch instead replaces the CURRENT process image
/// via `std::os::unix::process::CommandExt::exec` — no shell involved, so
/// there is no command string to build (or parse back). This struct carries
/// exactly what the caller (the `tm` CLI binary) needs to construct that
/// [`std::process::Command`] itself.
/// What: `claude_bin` (resolved absolute path), `args` (the isolation flags
/// plus `--resume <id>`/`--continue`/neither, mirroring [`resume_command`]'s
/// selection — see [`compose_inplace_args`]), and `config_dir` (the tm-owned
/// `CLAUDE_CONFIG_DIR`, when resolved). The caller is expected to
/// `env_remove("ANTHROPIC_API_KEY")` and, when `config_dir` is `Some`, set
/// `CLAUDE_CONFIG_DIR` to it — the same two invariants [`env_bin_prefix`]
/// encodes into the shell-string commands.
/// Test: exercised via [`build_inplace_resume_command`]'s tests.
#[derive(Debug)]
pub struct InPlaceResumeCommand {
    /// Resolved `claude` binary (absolute path).
    pub claude_bin: String,
    /// Full argv (isolation flags + resume/continue selection), EXCLUDING the
    /// binary itself.
    pub args: Vec<String>,
    /// The tm-owned `CLAUDE_CONFIG_DIR`, when resolved (`None` when home is
    /// unresolvable — mirrors [`prepare_managed_config`]'s fallback).
    pub config_dir: Option<std::path::PathBuf>,
}

/// Pure argv composition shared by [`build_inplace_resume_command`] (#2023 C).
///
/// Why: separating the resume/continue/fresh SELECTION from claude-binary
/// resolution (which needs a real `claude` install to exercise end-to-end)
/// keeps the decision itself testable in every CI environment — mirroring how
/// [`resume_command`]'s selection tests pass a fake `claude_bin` string rather
/// than depending on [`ClaudeCodeAdapter::resolve_claude`].
/// What: the isolation flags ([`crate::core::model_inject::SETTING_SOURCES_FLAG`]
/// / [`crate::core::model_inject::PERMISSION_MODE_FLAG`], whitespace-split
/// into argv tokens since both constants are simple space-separated flags with
/// no embedded quoting) followed by `--resume <id>` (id exists under
/// `config_dir`, per [`session_id_exists`]), `--continue` (no usable id but
/// [`has_prior_conversation`] is true), or neither (fresh start) — the exact
/// same three-way selection [`resume_command`] makes.
/// Test: `compose_inplace_args_uses_resume_for_existing_id`,
/// `compose_inplace_args_falls_back_for_missing_id`,
/// `compose_inplace_args_uses_continue_when_no_id_but_prior_conv`.
fn compose_inplace_args(
    cwd: &Path,
    config_dir: Option<&Path>,
    claude_session_id: Option<&str>,
) -> Vec<String> {
    let effective_id = claude_session_id.filter(|id| session_id_exists(cwd, config_dir, id));

    let mut args: Vec<String> = crate::core::model_inject::SETTING_SOURCES_FLAG
        .split_whitespace()
        .chain(crate::core::model_inject::PERMISSION_MODE_FLAG.split_whitespace())
        .map(str::to_owned)
        .collect();

    match effective_id {
        Some(id) => {
            args.push("--resume".to_owned());
            args.push(id.to_owned());
        }
        None if has_prior_conversation(cwd) => args.push("--continue".to_owned()),
        None => {}
    }
    args
}

/// Build an [`InPlaceResumeCommand`] for the bare-`tm` in-pane relaunch path
/// (#2023 component C).
///
/// Why: the in-place relaunch must use the SAME `--resume <id>`
/// existence-check → `--continue`/fresh-spawn fallback semantics as the
/// tmux-pane resume path (#2013) — reusing [`session_id_exists`] /
/// [`has_prior_conversation`] / [`prepare_managed_config`] directly (via
/// [`compose_inplace_args`]), rather than re-deriving them, means the two
/// paths can never silently drift.
/// What: resolves the `claude` binary (`Err(RuntimeError::BinaryNotFound)` if
/// missing), provisions/trust-seeds the managed `CLAUDE_CONFIG_DIR` via
/// [`prepare_managed_config`] (logged under the synthetic session name
/// `"in-place-relaunch"` — there is no tmux session name in this context),
/// then delegates argv composition to [`compose_inplace_args`].
/// Test: `build_inplace_resume_command_resolves_claude_binary`.
pub fn build_inplace_resume_command(
    cwd: &Path,
    claude_session_id: Option<&str>,
) -> Result<InPlaceResumeCommand, RuntimeError> {
    let claude_bin = ClaudeCodeAdapter::resolve_claude().ok_or_else(|| {
        RuntimeError::BinaryNotFound(
            "claude binary not found on PATH or in well-known dirs \
             (e.g. ~/.local/bin) — install Claude Code first"
                .into(),
        )
    })?;
    let config_dir = prepare_managed_config("in-place-relaunch", cwd);
    let args = compose_inplace_args(cwd, config_dir.as_deref(), claude_session_id);
    Ok(InPlaceResumeCommand {
        claude_bin,
        args,
        config_dir,
    })
}

/// Runtime adapter that launches Claude Code CLI inside a tmux session.
///
/// Why: Claude Code is the primary agent runtime for MPM sessions; coupling the
/// launch sequence (binary check, env scrub, tmux send) to a typed adapter keeps
/// the session manager free of runtime-specific knowledge.
/// What: holds a [`ManagedTmuxDriver`] reference; `spawn` verifies the `claude`
/// binary exists, then sends `env -u ANTHROPIC_API_KEY claude` to the named pane.
/// Test: `claude_code_adapter_spawn_sends_env_scrub_command`,
/// `claude_code_adapter_identifies`.
pub struct ClaudeCodeAdapter {
    tmux: Arc<dyn ManagedTmuxDriver + Send + Sync>,
}

impl ClaudeCodeAdapter {
    /// Construct an adapter backed by the given tmux driver.
    ///
    /// Why: the session manager injects the tmux driver via `Arc<dyn …>` so
    /// the adapter is testable without a real tmux binary.
    /// What: stores the driver reference.
    /// Test: used in every `ClaudeCodeAdapter` test.
    pub fn new(tmux: Arc<dyn ManagedTmuxDriver + Send + Sync>) -> Self {
        Self { tmux }
    }

    /// Resolve the `claude` binary to an absolute path, or `None` if missing.
    ///
    /// Why: under launchd the daemon (and the tmux pane it spawns) inherits a
    /// minimal `PATH` that omits `~/.local/bin` where Claude Code installs, so a
    /// bare `claude` on the pane would fail to launch (spawn `[errored]`, #1298).
    /// Resolving to an absolute path here lets the spawn command invoke claude
    /// by full path, independent of the pane's `PATH`.
    /// What: delegates to [`trusty_common::bin_resolve::resolve_binary`] which
    /// checks the live `PATH` first then the well-known daemon dirs (Homebrew +
    /// `~/.local/bin` + `~/.cargo/bin`); returns the resolved path as a `String`.
    /// Test: `claude_code_adapter_binary_check_returns_option`.
    fn resolve_claude() -> Option<String> {
        trusty_common::bin_resolve::resolve_binary("claude")
            .and_then(|p| p.to_str().map(str::to_owned))
    }

    /// Durably publish `TM_MANAGED_SESSION_ID` (and `CLAUDE_CONFIG_DIR` when
    /// resolved) into the tmux SESSION environment (#2157 item 1).
    ///
    /// Why: [`session_id_export_prefix`] only lands in the ONE pane shell that
    /// runs the spawn/resume command line — a sibling pane/window in the same
    /// tmux session, or a pane spawned by a pre-#2157 build, never sees it. This
    /// is belt-and-suspenders alongside that export: `tmux set-environment`
    /// writes into the session's own environment table, which
    /// `tmux show-environment` can read from ANY pane in the session — the
    /// fallback the in-place-relaunch gate (`bin/tm/commands/guided_inplace.rs`)
    /// now uses when the process environment is empty.
    /// What: best-effort — a failure is logged at `warn` and never propagated;
    /// the pane-shell export remains the primary mechanism, so a tmux driver
    /// that cannot support `set_environment` must not fail the spawn/resume it
    /// is attached to.
    /// Test: `spawn_publishes_session_id_via_set_environment`,
    /// `spawn_resume_publishes_session_id_via_set_environment`.
    fn publish_session_env(&self, tmux_name: &str, session_id: &str, config_dir: Option<&str>) {
        if let Err(e) = self
            .tmux
            .set_environment(tmux_name, "TM_MANAGED_SESSION_ID", session_id)
        {
            tracing::warn!(
                session = %tmux_name,
                "tmux set-environment TM_MANAGED_SESSION_ID failed (in-place relaunch \
                 fallback impaired, non-fatal): {e}"
            );
        }
        if let Some(dir) = config_dir
            && let Err(e) = self
                .tmux
                .set_environment(tmux_name, "CLAUDE_CONFIG_DIR", dir)
        {
            tracing::warn!(
                session = %tmux_name,
                "tmux set-environment CLAUDE_CONFIG_DIR failed (non-fatal): {e}"
            );
        }
    }
}

impl RuntimeAdapter for ClaudeCodeAdapter {
    /// Launch Claude Code in the named tmux session.
    ///
    /// Why: the session manager calls this after creating the tmux pane so the
    /// actual agent process starts inside it.
    /// What: resolves `claude` to an absolute path (returns `BinaryNotFound`
    /// if it cannot be found on `PATH` or in the well-known daemon dirs),
    /// provisions + trust-seeds the tm-owned `CLAUDE_CONFIG_DIR` via
    /// [`prepare_managed_config`], builds the PM system-prompt file via
    /// [`build_prompt_file`] (issue #2125 item 3), then sends [`spawn_command`]
    /// (`env -u ANTHROPIC_API_KEY CLAUDE_CONFIG_DIR=<dir> <abs-claude>
    /// --append-system-prompt-file <prompt>` plus the isolation/permission
    /// flags) to the pane; the task is logged for observability but not passed
    /// to the command.
    /// Test: `spawn_sends_env_scrub_when_binary_available`.
    fn spawn(
        &self,
        tmux_name: &str,
        cwd: &Path,
        task: &str,
        session_id: &str,
    ) -> Result<(), RuntimeError> {
        let claude_bin = Self::resolve_claude().ok_or_else(|| {
            RuntimeError::BinaryNotFound(
                "claude binary not found on PATH or in well-known dirs \
                 (e.g. ~/.local/bin) — install Claude Code first"
                    .into(),
            )
        })?;
        debug!(
            session = %tmux_name,
            cwd = %cwd.display(),
            task = %task,
            claude = %claude_bin,
            "spawning claude-code in tmux pane"
        );
        // Point the session at the tm-owned CLAUDE_CONFIG_DIR for auth + trust
        // isolation and seed trust there — never at `~/.claude.json` (DOC-34).
        // The framework roster/skills/hooks load from the PROJECT layer under
        // `--setting-sources project,local`, not from this config dir (see
        // `prepare_managed_config`). Non-fatal throughout (closes #1696).
        let config_dir = prepare_managed_config(tmux_name, cwd);
        // Build and inject the PM system prompt (issue #2125 item 3) so this,
        // the default daemon on-ramp, can no longer silently spawn vanilla
        // Claude Code. Non-fatal: a write failure omits the flag (#2173 ruled
        // out a CLAUDE.md-carrier fallback, so there is no other carrier).
        let prompt_file = build_prompt_file(cwd);
        self.tmux
            .send_line(
                tmux_name,
                &spawn_command(
                    &claude_bin,
                    config_dir.as_deref(),
                    session_id,
                    prompt_file.as_deref(),
                ),
            )
            .map_err(|e| RuntimeError::TmuxUnavailable(e.to_string()))?;
        // #2157 item 1: durable publish, belt-and-suspenders alongside the
        // pane-shell export baked into spawn_command above.
        self.publish_session_env(
            tmux_name,
            session_id,
            config_dir.as_deref().and_then(|p| p.to_str()),
        );
        Ok(())
    }

    /// Resume Claude Code with conversation continuity (#1744, #1840, #2013).
    ///
    /// Why: `resume_managed` must restore the prior conversation rather than
    /// starting fresh. If the stored `claude_session_id` is available AND
    /// still resolves to a real session on disk (checked via
    /// [`session_id_exists`], #2013), `--resume <id>` restores the exact
    /// conversation. A stale id (the session was pruned, moved, or never
    /// reached disk) is NOT passed to `--resume` — `claude --resume <missing>`
    /// fails hard with no recovery — instead it is treated like "no id" and
    /// falls back to the existing #1840 semantics: `--continue` when prior
    /// conversation history is detected (via [`has_prior_conversation`]), else
    /// a plain spawn, avoiding the "No conversation found to continue" error
    /// that would otherwise drop the session to a bare shell.
    /// What: resolves the claude binary, provisions + trust-seeds the tm-owned
    /// `CLAUDE_CONFIG_DIR` via [`prepare_managed_config`], existence-checks
    /// `claude_session_id` against the resolved config dir, falls back when it
    /// is missing, checks for prior conversation when no usable id remains,
    /// builds the PM system-prompt file via [`build_prompt_file`] (#2230 —
    /// same carrier `spawn` uses, previously missing from every resume path),
    /// then sends the appropriate [`resume_command`] to the tmux pane.
    /// Test: `spawn_resume_with_id_uses_resume_flag`,
    /// `spawn_resume_without_id_no_prior_conv_sends_plain_spawn`,
    /// `spawn_resume_sends_prompt_file_when_binary_available`.
    fn spawn_resume(
        &self,
        tmux_name: &str,
        cwd: &Path,
        task: &str,
        claude_session_id: Option<&str>,
        session_id: &str,
    ) -> Result<(), RuntimeError> {
        let claude_bin = Self::resolve_claude().ok_or_else(|| {
            RuntimeError::BinaryNotFound(
                "claude binary not found on PATH or in well-known dirs \
                 (e.g. ~/.local/bin) — install Claude Code first"
                    .into(),
            )
        })?;
        let config_dir = prepare_managed_config(tmux_name, cwd);
        // #2230: build the PM system prompt for the resume path too — before
        // this fix only spawn() passed --append-system-prompt-file, so every
        // resumed/guided-resume/crash-recovery session silently ran vanilla
        // Claude Code. Non-fatal: a write failure omits the flag.
        let prompt_file = build_prompt_file(cwd);

        // #2013: a stored id can go stale — existence-check it before trusting
        // `--resume <id>` so a missing session falls back gracefully instead
        // of a hard `claude` failure.
        let effective_id = claude_session_id.filter(|id| {
            let exists = session_id_exists(cwd, config_dir.as_deref(), id);
            if !exists {
                tracing::warn!(
                    session = %tmux_name,
                    claude_session_id = %id,
                    "stored claude_session_id no longer resolves to a session on \
                     disk; falling back instead of passing --resume (#2013)"
                );
            }
            exists
        });
        if let Some(id) = effective_id {
            tracing::warn!(
                session = %tmux_name,
                claude_session_id = %id,
                "resuming with --resume <id>; if conversation no longer exists on \
                 disk, Claude Code will error — the reap loop will detect and mark \
                 Stopped within ~60 s (#1744)"
            );
        }
        // #1840: only use --continue when prior conversation history exists for cwd.
        // Without this guard, --continue fails with "No conversation found" for
        // sessions that were stopped before claude ever ran (e.g. provisioning error,
        // or a fresh worktree that was never used).
        // Compute file_history separately so the debug log reflects the actual
        // filesystem check result rather than the combined (id || file) value.
        let file_history = effective_id.is_none() && has_prior_conversation(cwd);
        let prior = effective_id.is_some() || file_history;
        debug!(
            session = %tmux_name,
            cwd = %cwd.display(),
            task = %task,
            claude = %claude_bin,
            resume = effective_id.is_some(),
            has_prior_conv = file_history, // reflects actual .jsonl file check, not the combined value
            "resuming claude-code in tmux pane"
        );
        self.tmux
            .send_line(
                tmux_name,
                &resume_command(
                    &claude_bin,
                    config_dir.as_deref(),
                    effective_id,
                    prior,
                    session_id,
                    prompt_file.as_deref(),
                ),
            )
            .map_err(|e| RuntimeError::TmuxUnavailable(e.to_string()))?;
        // #2157 item 1: durable publish for the RESUME path too — a fresh tmux
        // session is created on resume, so it needs the same belt-and-suspenders
        // set-environment call as spawn().
        self.publish_session_env(
            tmux_name,
            session_id,
            config_dir.as_deref().and_then(|p| p.to_str()),
        );
        Ok(())
    }

    /// Return `"claude-code"` as the adapter's identifier.
    ///
    /// Why: logs and status responses must identify this adapter by name so
    /// operators can distinguish it from future runtimes.
    /// What: static string, no I/O.
    /// Test: `claude_code_adapter_identifies`.
    fn identify(&self) -> &str {
        "claude-code"
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::FakeTmux;
    use super::*;

    /// Fixed managed-session UUID string reused across command-builder tests
    /// (#2023 component B) — a representative id, not a real session.
    const TEST_SESSION_ID: &str = "11111111-2222-3333-4444-555555555555";

    /// RAII guard that redirects `$HOME` to a temp dir and restores it on drop
    /// (including panic).
    ///
    /// Why: the adapter's `spawn`/`spawn_resume` now provision the real managed
    /// `CLAUDE_CONFIG_DIR` (resolved under `$HOME`). Tests that drive them must
    /// redirect `$HOME` so the provisioning/trust-seed side effects land in a
    /// throwaway dir instead of the developer's real `~/.trusty-tools`. Pair with
    /// `#[serial_test::serial]` since it mutates process-global env.
    struct HomeGuard {
        prev: Option<String>,
        _tmp: tempfile::TempDir,
    }
    impl HomeGuard {
        fn set() -> Self {
            let tmp = tempfile::tempdir().expect("tempdir");
            let prev = std::env::var("HOME").ok();
            // SAFETY: callers are #[serial], so no other test thread reads HOME
            // concurrently; Drop restores the prior value even on panic.
            unsafe { std::env::set_var("HOME", tmp.path()) };
            Self { prev, _tmp: tmp }
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.prev {
                Some(ref p) => unsafe { std::env::set_var("HOME", p) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        }
    }

    #[test]
    fn claude_code_adapter_identifies() {
        let fake = FakeTmux::new();
        let adapter = ClaudeCodeAdapter::new(fake);
        assert_eq!(adapter.identify(), "claude-code");
    }

    #[test]
    fn spawn_command_contains_env_scrub() {
        // The spawn command must always strip the API key from the environment
        // so the session falls back to OAuth (keyed by CLAUDE_CONFIG_DIR).
        let cmd = spawn_command("claude", None, TEST_SESSION_ID, None);
        assert!(
            cmd.contains("env -u ANTHROPIC_API_KEY"),
            "spawn command must contain env scrub: {cmd}"
        );
        assert!(
            cmd.contains(" claude "),
            "spawn command must invoke claude: {cmd}"
        );
    }

    #[test]
    fn spawn_command_exports_managed_session_id() {
        // #2023 component B: the spawn command sent to the pane must export
        // TM_MANAGED_SESSION_ID (single-quoted, terminated by `;`) BEFORE the
        // claude invocation, so a command run in the pane after claude exits
        // can identify which managed session it belongs to. tmux
        // set-environment alone would NOT reach the pane's already-running
        // shell — the export must be part of the literal command line.
        let cmd = spawn_command("claude", None, TEST_SESSION_ID, None);
        let expected_prefix = format!("export TM_MANAGED_SESSION_ID='{TEST_SESSION_ID}'; ");
        assert!(
            cmd.starts_with(&expected_prefix),
            "spawn command must start with the session-id export: {cmd}"
        );
        let export_pos = cmd
            .find("export TM_MANAGED_SESSION_ID")
            .expect("export present");
        let claude_pos = cmd.find(" claude ").expect("claude invocation present");
        assert!(
            export_pos < claude_pos,
            "export must precede the claude invocation: {cmd}"
        );
    }

    #[test]
    fn spawn_command_sets_claude_config_dir() {
        // DOC-34 / #1996: when a managed config dir is available the spawn
        // command MUST export it so the framework roster comes from tm's config
        // home, not the project's committed `.claude/`. It must co-exist with the
        // API-key scrub.
        let dir = Path::new("/home/bob/.trusty-tools/trusty-mpm/claude-config");
        let cmd = spawn_command("claude", Some(dir), TEST_SESSION_ID, None);
        assert!(
            cmd.contains(
                "env -u ANTHROPIC_API_KEY \
                 CLAUDE_CONFIG_DIR='/home/bob/.trusty-tools/trusty-mpm/claude-config' claude"
            ),
            "spawn command must scrub the key (via -u, BEFORE the NAME=VALUE assignment per \
             POSIX env grammar) then set (single-quoted) CLAUDE_CONFIG_DIR: {cmd}"
        );
        assert!(
            cmd.contains("--setting-sources project,local"),
            "isolation flag must still be present with a config dir: {cmd}"
        );
    }

    #[test]
    fn env_bin_prefix_orders_unset_flag_before_config_dir_assignment() {
        // Regression guard (fatal bug): POSIX `env`'s grammar is
        // `env [OPTION]... [NAME=VALUE]... [COMMAND]...` — option flags like
        // `-u NAME` MUST precede any `NAME=VALUE` assignment. Putting
        // `CLAUDE_CONFIG_DIR=<dir>` before `-u` makes `env` stop parsing options
        // at the first NAME=VALUE and try to exec `-u` as a command
        // (`env: -u: No such file or directory`), fatally killing every managed
        // session spawn.
        let dir = Path::new("/home/bob/.trusty-tools/trusty-mpm/claude-config");
        let cmd = spawn_command("claude", Some(dir), TEST_SESSION_ID, None);
        let u_pos = cmd
            .find("-u ANTHROPIC_API_KEY")
            .expect("cmd must contain -u ANTHROPIC_API_KEY");
        let config_pos = cmd
            .find("CLAUDE_CONFIG_DIR=")
            .expect("cmd must contain CLAUDE_CONFIG_DIR=");
        assert!(
            u_pos < config_pos,
            "-u ANTHROPIC_API_KEY must appear BEFORE CLAUDE_CONFIG_DIR= per POSIX env \
             option-then-assignment grammar: {cmd}"
        );
    }

    #[test]
    fn spawn_command_without_config_dir_omits_it() {
        // When home is unresolvable there is no config dir to point at; the
        // command must fall back to the legacy scrub-only prefix (no bare
        // `CLAUDE_CONFIG_DIR=` token).
        let cmd = spawn_command("claude", None, TEST_SESSION_ID, None);
        assert!(
            !cmd.contains("CLAUDE_CONFIG_DIR"),
            "no config dir → must not reference CLAUDE_CONFIG_DIR: {cmd}"
        );
    }

    #[test]
    fn env_bin_prefix_quotes_config_dir_with_space() {
        // CRITICAL (DOC-34 review): a home dir with a space must NOT word-split
        // the pane command. The path must appear single-quoted and INTACT so
        // `env` receives one CLAUDE_CONFIG_DIR value, not two argv tokens.
        let dir = Path::new("/Users/John Doe/.trusty-tools/trusty-mpm/claude-config");
        let cmd = spawn_command("claude", Some(dir), TEST_SESSION_ID, None);
        assert!(
            cmd.contains(
                "env -u ANTHROPIC_API_KEY \
                 CLAUDE_CONFIG_DIR='/Users/John Doe/.trusty-tools/trusty-mpm/claude-config' claude"
            ),
            "config dir with a space must be single-quoted and intact: {cmd}"
        );
        // The bare unquoted form (which would word-split) must NOT appear.
        assert!(
            !cmd.contains("CLAUDE_CONFIG_DIR=/Users/John Doe"),
            "config dir must never be interpolated unquoted: {cmd}"
        );
        // Resume path shares env_bin_prefix, so it must quote identically.
        let resume = resume_command("claude", Some(dir), None, false, TEST_SESSION_ID, None);
        assert!(
            resume.contains(
                "CLAUDE_CONFIG_DIR='/Users/John Doe/.trusty-tools/trusty-mpm/claude-config'"
            ),
            "resume command must also single-quote a spaced config dir: {resume}"
        );
    }

    #[test]
    fn shell_single_quote_escapes_embedded_quote() {
        // A literal single quote in the path must be escaped via the POSIX
        // close-reopen sequence '\'' so the quoting stays balanced. (A macOS
        // home never contains one, but the escape must be correct regardless.)
        assert_eq!(shell_single_quote("/a/b"), "'/a/b'");
        assert_eq!(
            shell_single_quote("/Users/o'brien/cfg"),
            r"'/Users/o'\''brien/cfg'"
        );
    }

    #[test]
    fn spawn_command_contains_isolation_flags() {
        // Why: the session_manager spawn path must isolate from the user's global
        // settings and run fully unattended (bypass all permission prompts) so
        // multi-agent orchestration never stalls on interactive approval dialogs.
        let cmd = spawn_command("claude", None, TEST_SESSION_ID, None);
        assert!(
            cmd.contains("--setting-sources project,local"),
            "spawn command must isolate settings: {cmd}"
        );
        assert!(
            cmd.contains("--dangerously-skip-permissions"),
            "spawn command must bypass permissions for unattended orchestration: {cmd}"
        );
        // Must not carry the old acceptEdits flag.
        assert!(
            !cmd.contains("acceptEdits"),
            "old acceptEdits flag must not be present: {cmd}"
        );
    }

    #[test]
    fn spawn_command_uses_resolved_binary() {
        // Why (#1298): under launchd the pane inherits a minimal PATH, so the
        // spawn command must invoke claude by the resolved (absolute) path
        // rather than a bare `claude` that the pane's PATH cannot find.
        let cmd = spawn_command("/Users/me/.local/bin/claude", None, TEST_SESSION_ID, None);
        assert!(
            cmd.contains("env -u ANTHROPIC_API_KEY /Users/me/.local/bin/claude "),
            "spawn command must invoke the resolved absolute claude path: {cmd}"
        );
    }

    #[test]
    fn claude_code_adapter_binary_check_returns_option() {
        // resolve_claude returns Some(path) or None without panicking; when it
        // resolves, the path must be a non-empty absolute-ish string.
        if let Some(p) = ClaudeCodeAdapter::resolve_claude() {
            assert!(!p.is_empty(), "resolved claude path must be non-empty");
        }
    }

    #[serial_test::serial]
    #[test]
    fn spawn_sends_env_scrub_when_binary_available() {
        // Patch: if claude is not available this test is a no-op (we cannot
        // install it in CI). We only assert the send when the binary exists.
        // HOME is redirected so the spawn's config-dir provisioning + trust seed
        // land in a throwaway dir, not the developer's real ~/.trusty-tools.
        let _home = HomeGuard::set();
        let Some(_claude_bin) = ClaudeCodeAdapter::resolve_claude() else {
            return;
        };
        let fake = FakeTmux::new();
        let adapter = ClaudeCodeAdapter::new(fake.clone());
        adapter
            .spawn("tmpm-test", Path::new("/tmp"), "some task", TEST_SESSION_ID)
            .expect("spawn");
        let sends = fake.sends.lock().unwrap();
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].0, "tmpm-test");
        let cmd = &sends[0].1;
        // Must carry the env scrub regardless (the option must precede any
        // intervening `CLAUDE_CONFIG_DIR=` assignment per POSIX env grammar).
        assert!(cmd.contains("-u ANTHROPIC_API_KEY"));
        // Issue #2125 item 3: the spawn command must inject the PM system
        // prompt via --append-system-prompt-file — the third fail-closed
        // carrier — so this, the default daemon on-ramp, can no longer
        // silently spawn vanilla Claude Code. The prompt file path itself is
        // non-deterministic (fresh UUID per call), so this asserts the flag's
        // presence rather than an exact command match.
        assert!(
            cmd.contains("--append-system-prompt-file"),
            "spawn command must inject the PM system prompt file: {cmd}"
        );
    }

    #[test]
    fn publish_session_env_sets_id_and_config_dir() {
        // #2157 item 1: exercises publish_session_env directly (no HOME
        // redirection or real `claude` binary needed) so this call-shape
        // assertion runs unconditionally in CI, unlike the full-spawn tests
        // below which are gated on a real `claude` binary being present.
        let fake = FakeTmux::new();
        let adapter = ClaudeCodeAdapter::new(fake.clone());
        adapter.publish_session_env("tmpm-test", TEST_SESSION_ID, Some("/tmp/config-dir"));
        let env_sets = fake.env_sets.lock().unwrap();
        assert_eq!(
            env_sets.len(),
            2,
            "expected id + config-dir sets: {env_sets:?}"
        );
        assert!(env_sets.contains(&(
            "tmpm-test".to_string(),
            "TM_MANAGED_SESSION_ID".to_string(),
            TEST_SESSION_ID.to_string()
        )));
        assert!(env_sets.contains(&(
            "tmpm-test".to_string(),
            "CLAUDE_CONFIG_DIR".to_string(),
            "/tmp/config-dir".to_string()
        )));
    }

    #[test]
    fn publish_session_env_omits_config_dir_when_absent() {
        let fake = FakeTmux::new();
        let adapter = ClaudeCodeAdapter::new(fake.clone());
        adapter.publish_session_env("tmpm-test", TEST_SESSION_ID, None);
        let env_sets = fake.env_sets.lock().unwrap();
        assert_eq!(
            env_sets.len(),
            1,
            "expected only the session-id set: {env_sets:?}"
        );
        assert_eq!(env_sets[0].1, "TM_MANAGED_SESSION_ID");
    }

    #[serial_test::serial]
    #[test]
    fn spawn_publishes_session_id_via_set_environment() {
        // #2157 item 1: spawn must durably publish TM_MANAGED_SESSION_ID via
        // `tmux set-environment`, not just the pane-shell export baked into the
        // command line — belt-and-suspenders so a sibling pane/window in the
        // same tmux session (which never ran the export line) can still resolve
        // it via `tmux show-environment`.
        let _home = HomeGuard::set();
        if ClaudeCodeAdapter::resolve_claude().is_none() {
            return;
        }
        let fake = FakeTmux::new();
        let adapter = ClaudeCodeAdapter::new(fake.clone());
        adapter
            .spawn("tmpm-test", Path::new("/tmp"), "some task", TEST_SESSION_ID)
            .expect("spawn");
        let env_sets = fake.env_sets.lock().unwrap();
        assert!(
            env_sets.iter().any(|(name, key, value)| name == "tmpm-test"
                && key == "TM_MANAGED_SESSION_ID"
                && value == TEST_SESSION_ID),
            "spawn must call tmux set-environment with TM_MANAGED_SESSION_ID: {env_sets:?}"
        );
    }

    #[test]
    fn spawn_command_with_prompt_file_contains_flag() {
        // #2125 item 3: when a prompt file is supplied, spawn_command must
        // inject --append-system-prompt-file pointing at it, alongside the
        // existing isolation flags — the third fail-closed carrier.
        let path = Path::new("/tmp/trusty-mpm-system-prompt-test.txt");
        let cmd = spawn_command("claude", None, TEST_SESSION_ID, Some(path));
        assert!(
            cmd.contains("--append-system-prompt-file '/tmp/trusty-mpm-system-prompt-test.txt'"),
            "spawn command must pass the prompt file via --append-system-prompt-file: {cmd}"
        );
        assert!(
            cmd.contains("--setting-sources project,local"),
            "isolation flags must still be present alongside the prompt file: {cmd}"
        );
    }

    #[test]
    fn spawn_command_without_prompt_file_omits_flag() {
        // #2125 item 3: no prompt file (e.g. the write failed) → the flag must
        // be omitted entirely rather than passed with a bad/empty path.
        let cmd = spawn_command("claude", None, TEST_SESSION_ID, None);
        assert!(
            !cmd.contains("--append-system-prompt-file"),
            "no prompt file → flag must be absent: {cmd}"
        );
    }

    #[test]
    fn build_prompt_file_writes_resolved_prompt_for_project() {
        // #2125 item 3: build_prompt_file must reuse the SAME
        // build_system_prompt_for_with_style_and_native seam the CLI/client
        // launch paths use, so the daemon adapter's injected prompt is never a
        // divergent copy — proven here by asserting the written file carries
        // the bundled PM_INSTRUCTIONS heading.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = build_prompt_file(tmp.path()).expect("prompt file written");
        let content = std::fs::read_to_string(&path).expect("prompt file readable");
        assert!(
            content.contains("# PM Agent -- Trusty MPM"),
            "prompt file must contain the resolved PM system prompt: {content}"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn resume_command_with_id_uses_resume_flag() {
        // Why (#1744): --resume <id> restores the exact prior conversation;
        // the test pins the contract so accidental regressions are caught early.
        let cmd = resume_command(
            "claude",
            None,
            Some("abc-123"),
            false,
            TEST_SESSION_ID,
            None,
        );
        assert!(
            cmd.contains("--resume abc-123"),
            "resume command must include --resume <id>: {cmd}"
        );
        assert!(
            cmd.contains("env -u ANTHROPIC_API_KEY"),
            "resume command must still scrub API key: {cmd}"
        );
        assert!(
            !cmd.contains("--continue"),
            "resume command must NOT contain --continue when id is set: {cmd}"
        );
    }

    #[test]
    fn resume_command_exports_managed_session_id() {
        // #2023 component B: the resume command must ALSO export
        // TM_MANAGED_SESSION_ID before the claude invocation, for the same
        // reason as the spawn path — the pane's shell must retain the id after
        // claude exits, whichever launch path (fresh spawn or resume) started it.
        let cmd = resume_command(
            "claude",
            None,
            Some("abc-123"),
            false,
            TEST_SESSION_ID,
            None,
        );
        let expected_prefix = format!("export TM_MANAGED_SESSION_ID='{TEST_SESSION_ID}'; ");
        assert!(
            cmd.starts_with(&expected_prefix),
            "resume command must start with the session-id export: {cmd}"
        );
        let export_pos = cmd
            .find("export TM_MANAGED_SESSION_ID")
            .expect("export present");
        let resume_pos = cmd.find("--resume").expect("--resume flag present");
        assert!(
            export_pos < resume_pos,
            "export must precede the --resume flag: {cmd}"
        );
    }

    #[test]
    fn resume_command_sets_claude_config_dir() {
        // DOC-34: the resume path must also carry CLAUDE_CONFIG_DIR so resumed
        // sessions read the same tm-owned roster as fresh spawns.
        let dir = Path::new("/home/bob/.trusty-tools/trusty-mpm/claude-config");
        let cmd = resume_command(
            "claude",
            Some(dir),
            Some("abc-123"),
            false,
            TEST_SESSION_ID,
            None,
        );
        assert!(
            cmd.contains(
                "env -u ANTHROPIC_API_KEY \
                 CLAUDE_CONFIG_DIR='/home/bob/.trusty-tools/trusty-mpm/claude-config' claude"
            ),
            "resume command must export (single-quoted) CLAUDE_CONFIG_DIR after the -u option: {cmd}"
        );
        assert!(
            cmd.contains("--resume abc-123"),
            "resume command must still include --resume <id>: {cmd}"
        );
    }

    #[test]
    fn resume_command_without_id_with_prior_conv_uses_continue() {
        // Why (#1744 / #1840): when no claude_session_id is stored but prior
        // conversation history exists, --continue resumes the most-recent
        // conversation in the workspace rather than starting fresh.
        let cmd = resume_command("claude", None, None, true, TEST_SESSION_ID, None);
        assert!(
            cmd.contains("--continue"),
            "resume command without id + prior conv must use --continue: {cmd}"
        );
        assert!(
            !cmd.contains("--resume"),
            "resume command without id must NOT contain --resume: {cmd}"
        );
    }

    #[test]
    fn resume_command_without_id_no_prior_conv_uses_plain_spawn() {
        // Why (#1840): when no claude_session_id is stored AND no prior
        // conversation exists, --continue would error with "No conversation
        // found to continue". The plain spawn starts a fresh session instead.
        let cmd = resume_command("claude", None, None, false, TEST_SESSION_ID, None);
        assert!(
            !cmd.contains("--continue"),
            "resume command without id + no prior conv must NOT use --continue: {cmd}"
        );
        assert!(
            !cmd.contains("--resume"),
            "resume command without id must NOT use --resume: {cmd}"
        );
        assert!(
            cmd.contains("env -u ANTHROPIC_API_KEY"),
            "plain spawn command must still scrub API key: {cmd}"
        );
    }

    #[test]
    fn has_prior_conversation_returns_false_for_fresh_workspace() {
        // Why (#1840): a fresh worktree has no Claude conversation history;
        // has_prior_conversation must return false to avoid "No conversation found".
        // Uses has_prior_conversation_in with a temp projects_dir — no HOME env
        // mutation, making this test safe for parallel execution.
        let tmp = tempfile::tempdir().expect("tempdir");
        let projects_dir = tmp.path().join("projects");
        // projects_dir does not exist → always returns false.
        assert!(
            !has_prior_conversation_in(tmp.path(), &projects_dir),
            "no projects dir → false"
        );
        // Create the projects dir but with no entry for this workspace → still false.
        std::fs::create_dir_all(&projects_dir).unwrap();
        assert!(
            !has_prior_conversation_in(tmp.path(), &projects_dir),
            "projects dir exists but no entry for this workspace → false"
        );
    }

    #[test]
    fn has_prior_conversation_returns_true_when_jsonl_exists() {
        // Why (#1840): verify the positive path — a workspace with a .jsonl file
        // in its encoded project dir must return true.
        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path().join("my-workspace");
        std::fs::create_dir_all(&cwd).unwrap();
        let projects_dir = tmp.path().join("projects");
        // Encode the cwd path as Claude does: replace '/' with '-'.
        let encoded = cwd.to_string_lossy().replace('/', "-");
        let project_dir = projects_dir.join(&encoded);
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("session.jsonl"), "{}").unwrap();
        assert!(
            has_prior_conversation_in(&cwd, &projects_dir),
            "workspace with .jsonl must return true"
        );
    }

    #[serial_test::serial]
    #[test]
    fn spawn_resume_with_id_uses_resume_flag() {
        // Why (#1744, #2013): ClaudeCodeAdapter::spawn_resume must send
        // --resume <id> to the pane when the claude_session_id is known AND
        // still resolves to a real session on disk. HOME is redirected so the
        // config-dir provisioning is hermetic; a matching session jsonl file
        // is seeded under the resolved CLAUDE_CONFIG_DIR/projects/ so the
        // #2013 existence check passes.
        let _home = HomeGuard::set();
        if ClaudeCodeAdapter::resolve_claude().is_none() {
            return;
        };
        let cwd = Path::new("/tmp");
        let config_dir = crate::core::trusty_tools_config::managed_claude_config_dir()
            .expect("config dir resolves under redirected HOME");
        let encoded = cwd.to_string_lossy().replace('/', "-");
        let project_dir = config_dir.join("projects").join(&encoded);
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("my-session-id.jsonl"), "{}").unwrap();

        let fake = FakeTmux::new();
        let adapter = ClaudeCodeAdapter::new(fake.clone());
        adapter
            .spawn_resume(
                "tmpm-test",
                cwd,
                "task",
                Some("my-session-id"),
                TEST_SESSION_ID,
            )
            .expect("spawn_resume");
        let sends = fake.sends.lock().unwrap();
        assert_eq!(sends.len(), 1);
        assert!(
            sends[0].1.contains("--resume my-session-id"),
            "spawn_resume with an existing id must use --resume: {}",
            sends[0].1
        );
        // #2157 item 1: the RESUME path must ALSO durably publish the id — a
        // fresh tmux session is created on resume, so it starts with no
        // set-environment history at all.
        let env_sets = fake.env_sets.lock().unwrap();
        assert!(
            env_sets.iter().any(|(name, key, value)| name == "tmpm-test"
                && key == "TM_MANAGED_SESSION_ID"
                && value == TEST_SESSION_ID),
            "spawn_resume must call tmux set-environment with TM_MANAGED_SESSION_ID: {env_sets:?}"
        );
    }

    #[serial_test::serial]
    #[test]
    fn spawn_resume_sends_prompt_file_when_binary_available() {
        // #2230: spawn_resume must inject --append-system-prompt-file just
        // like spawn() does — before this fix, every resume path (including
        // guided-resume and crash-recovery, which both funnel through this
        // adapter method) silently omitted the PM system prompt carrier.
        // HOME is redirected so build_prompt_file's write lands in a
        // throwaway dir, not the developer's real ~/.trusty-tools.
        let _home = HomeGuard::set();
        let Some(_claude_bin) = ClaudeCodeAdapter::resolve_claude() else {
            return;
        };
        let fake = FakeTmux::new();
        let adapter = ClaudeCodeAdapter::new(fake.clone());
        adapter
            .spawn_resume(
                "tmpm-test",
                Path::new("/tmp/does-not-exist-2230"),
                "task",
                None,
                TEST_SESSION_ID,
            )
            .expect("spawn_resume");
        let sends = fake.sends.lock().unwrap();
        assert_eq!(sends.len(), 1);
        assert!(
            sends[0].1.contains("--append-system-prompt-file"),
            "spawn_resume must inject the PM system prompt file: {}",
            sends[0].1
        );
    }

    #[serial_test::serial]
    #[test]
    fn spawn_resume_with_missing_id_falls_back_gracefully() {
        // Why (#2013): a stale/unknown claude_session_id must NOT be passed to
        // `--resume` (which would fail hard) — it must fall back the same way
        // a `None` id does. HOME is redirected so the config dir is hermetic
        // and no session jsonl is seeded, so the id is guaranteed missing.
        let _home = HomeGuard::set();
        if ClaudeCodeAdapter::resolve_claude().is_none() {
            return;
        };
        let fake = FakeTmux::new();
        let adapter = ClaudeCodeAdapter::new(fake.clone());
        adapter
            .spawn_resume(
                "tmpm-test",
                Path::new("/tmp/does-not-exist-2013"),
                "task",
                Some("stale-session-id"),
                TEST_SESSION_ID,
            )
            .expect("spawn_resume must not hard-fail on a missing id");
        let sends = fake.sends.lock().unwrap();
        assert_eq!(sends.len(), 1);
        assert!(
            !sends[0].1.contains("--resume"),
            "a missing id must NOT be passed to --resume: {}",
            sends[0].1
        );
        assert!(
            !sends[0].1.contains("--continue"),
            "a missing id with no prior conversation history must fall back to a \
             plain spawn, not --continue: {}",
            sends[0].1
        );
        assert!(
            sends[0].1.contains("env -u ANTHROPIC_API_KEY"),
            "fallback command must still scrub the API key: {}",
            sends[0].1
        );
    }

    #[test]
    fn session_id_exists_true_for_real_jsonl_file() {
        // Why (#2013): the positive path — a session file present under the
        // encoded project dir must resolve as existing.
        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path().join("my-workspace");
        std::fs::create_dir_all(&cwd).unwrap();
        let projects_dir = tmp.path().join("projects");
        let encoded = cwd.to_string_lossy().replace('/', "-");
        let project_dir = projects_dir.join(&encoded);
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("abc-123.jsonl"), "{}").unwrap();
        assert!(
            session_id_exists_in(&cwd, &projects_dir, "abc-123"),
            "existing session file must resolve as present"
        );
    }

    #[test]
    fn session_id_exists_false_for_missing_id() {
        // Why (#2013): a stale id — no matching .jsonl for this id, even
        // though the workspace has OTHER conversation history — must resolve
        // as absent so the caller falls back instead of hard-failing.
        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path().join("my-workspace");
        std::fs::create_dir_all(&cwd).unwrap();
        let projects_dir = tmp.path().join("projects");
        let encoded = cwd.to_string_lossy().replace('/', "-");
        let project_dir = projects_dir.join(&encoded);
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("other-session.jsonl"), "{}").unwrap();
        assert!(
            !session_id_exists_in(&cwd, &projects_dir, "stale-id-not-present"),
            "id with no matching file must resolve as absent"
        );
    }

    #[test]
    fn session_id_exists_false_when_projects_dir_absent() {
        // Why (#2013): a fresh workspace / unresolved config dir must never
        // panic or hard-fail — best-effort false is the safe default.
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing_projects_dir = tmp.path().join("does-not-exist");
        assert!(
            !session_id_exists_in(tmp.path(), &missing_projects_dir, "any-id"),
            "missing projects dir must resolve as absent, not panic"
        );
        assert!(
            !session_id_exists(tmp.path(), None, "any-id"),
            "session_id_exists with no config dir falls back to home resolution \
             and must never panic even if HOME is unusual in the test env"
        );
    }

    #[test]
    fn encode_project_dir_replaces_slashes() {
        // Why (#2013 cleanup): pins the shared encoding helper's contract
        // directly, independent of either call site.
        assert_eq!(
            encode_project_dir(Path::new("/private/tmp/foo")),
            "-private-tmp-foo"
        );
    }

    #[test]
    fn session_id_exists_finds_hardcoded_dir_name_for_known_cwd() {
        // Why (#2013 cleanup, MEDIUM): the other session_id_exists tests build
        // their expected path via the SAME formula the implementation uses
        // (`cwd.to_string_lossy().replace('/', "-")` / `encode_project_dir`),
        // so they cannot catch a future drift in the encoding scheme — the
        // test and the code would drift together. This test instead types the
        // expected directory name BY HAND as a literal, so if the encoding
        // scheme ever changes (e.g. Claude Code starts hashing paths instead
        // of dash-joining them), this assertion breaks independently of the
        // implementation.
        let tmp = tempfile::tempdir().expect("tempdir");
        let projects_dir = tmp.path().join("projects");
        // Hand-typed literal for cwd "/tmp/my-workspace" — NOT derived by
        // calling encode_project_dir/replace('/', "-") here.
        let project_dir = projects_dir.join("-tmp-my-workspace");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("known-id.jsonl"), "{}").unwrap();
        assert!(
            session_id_exists_in(Path::new("/tmp/my-workspace"), &projects_dir, "known-id"),
            "session_id_exists_in must find the seeded session under the \
             hand-typed expected dir name '-tmp-my-workspace'"
        );
    }

    #[test]
    fn projects_dir_for_prefers_config_dir_when_present() {
        // Why (#2013): CLAUDE_CONFIG_DIR relocates the entire config home,
        // including session storage — the projects dir must be resolved
        // UNDER it, not always under ~/.claude, or the existence check would
        // never find managed sessions.
        let config_dir = Path::new("/tmp/some-managed-config-dir");
        assert_eq!(
            projects_dir_for(Some(config_dir)),
            Some(config_dir.join("projects")),
            "projects_dir_for must nest under the given config dir"
        );
    }

    #[serial_test::serial]
    #[test]
    fn spawn_resume_without_id_no_prior_conv_sends_plain_spawn() {
        // Why (#1840, #1845 item 3): when no claude_session_id is available AND the
        // workspace has no prior conversation, spawn_resume must send a plain spawn
        // (no --continue, no --resume) to avoid "No conversation found to continue".
        //
        // The command construction is tested via resume_command() directly with a fake
        // binary so assertions ALWAYS run even when the `claude` binary is absent in CI.
        // The adapter merely calls resume_command() with the same arguments; testing the
        // function directly proves the selection logic without CI depending on claude.
        let cmd = resume_command("__fake_claude__", None, None, false, TEST_SESSION_ID, None);
        assert!(
            !cmd.contains("--continue"),
            "plain-spawn path must NOT use --continue: {cmd}"
        );
        assert!(
            !cmd.contains("--resume"),
            "plain-spawn path must NOT use --resume: {cmd}"
        );
        assert!(
            cmd.contains("env -u ANTHROPIC_API_KEY"),
            "plain-spawn path must still scrub API key: {cmd}"
        );

        // Additionally verify the adapter-level plumbing when the binary is present.
        // HOME is redirected so the config-dir provisioning is hermetic.
        let _home = HomeGuard::set();
        let Some(_claude_bin) = ClaudeCodeAdapter::resolve_claude() else {
            return; // core assertions above already ran; adapter path requires binary
        };
        let tmp = tempfile::tempdir().expect("tempdir");
        let fake = FakeTmux::new();
        let adapter = ClaudeCodeAdapter::new(fake.clone());
        adapter
            .spawn_resume(
                "test-tmux-session",
                tmp.path(),
                "task",
                None,
                TEST_SESSION_ID,
            )
            .expect("spawn_resume without id");
        let sends = fake.sends.lock().unwrap();
        assert_eq!(sends.len(), 1);
        assert!(
            !sends[0].1.contains("--continue"),
            "adapter plain-spawn must NOT use --continue: {}",
            sends[0].1
        );
        assert!(
            !sends[0].1.contains("--resume"),
            "adapter plain-spawn must NOT use --resume: {}",
            sends[0].1
        );
    }

    // ── #2023 component D: on-exit relaunch hint ────────────────────────────

    #[test]
    fn spawn_command_prints_relaunch_hint_after_claude_exits() {
        // The hint must appear AFTER the claude invocation, separated by `;` so
        // it only runs once claude exits and control returns to the pane shell.
        let cmd = spawn_command("claude", None, TEST_SESSION_ID, None);
        assert!(
            cmd.contains("; echo 'tm: run `tm` to relaunch this session'"),
            "spawn command must print the relaunch hint after claude exits: {cmd}"
        );
        let claude_pos = cmd.find(" claude ").expect("claude invocation present");
        let hint_pos = cmd.find("; echo").expect("relaunch hint present");
        assert!(
            claude_pos < hint_pos,
            "relaunch hint must come AFTER the claude invocation: {cmd}"
        );
    }

    #[test]
    fn resume_command_prints_relaunch_hint_after_claude_exits() {
        // Same invariant for the resume path (--resume branch here; the
        // --continue/plain-spawn branches share the same trailing suffix).
        let cmd = resume_command(
            "claude",
            None,
            Some("abc-123"),
            false,
            TEST_SESSION_ID,
            None,
        );
        assert!(
            cmd.contains("; echo 'tm: run `tm` to relaunch this session'"),
            "resume command must print the relaunch hint after claude exits: {cmd}"
        );
        let resume_pos = cmd.find("--resume").expect("--resume flag present");
        let hint_pos = cmd.find("; echo").expect("relaunch hint present");
        assert!(
            resume_pos < hint_pos,
            "relaunch hint must come AFTER --resume: {cmd}"
        );
    }

    #[test]
    fn resume_command_with_prompt_file_contains_flag() {
        // #2230: the resume path must carry the same --append-system-prompt-file
        // carrier as spawn_command — before this fix, resumed / guided-resume /
        // crash-recovery sessions had no way to inject the PM system prompt at all.
        let path = Path::new("/tmp/trusty-mpm-system-prompt-resume-test.txt");
        let cmd = resume_command(
            "claude",
            None,
            Some("abc-123"),
            false,
            TEST_SESSION_ID,
            Some(path),
        );
        assert!(
            cmd.contains(
                "--append-system-prompt-file '/tmp/trusty-mpm-system-prompt-resume-test.txt'"
            ),
            "resume command must pass the prompt file via --append-system-prompt-file: {cmd}"
        );
        assert!(
            cmd.contains("--resume abc-123"),
            "--resume must still be present alongside the prompt file: {cmd}"
        );
    }

    // ── #2023 component C: in-place relaunch command builder ───────────────

    #[test]
    fn compose_inplace_args_uses_resume_for_existing_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path().join("workspace");
        std::fs::create_dir_all(&cwd).unwrap();
        let config_dir = tmp.path().join("config");
        let encoded = cwd.to_string_lossy().replace('/', "-");
        let project_dir = config_dir.join("projects").join(&encoded);
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("existing-id.jsonl"), "{}").unwrap();

        let args = compose_inplace_args(&cwd, Some(&config_dir), Some("existing-id"));
        assert!(
            args.windows(2).any(|w| w == ["--resume", "existing-id"]),
            "must select --resume <id> for an id that exists on disk: {args:?}"
        );
        assert!(
            !args.contains(&"--continue".to_owned()),
            "must not ALSO pass --continue when --resume is used: {args:?}"
        );
    }

    #[test]
    fn compose_inplace_args_falls_back_for_missing_id() {
        // #2013 parity: a stale id (no matching .jsonl) with no prior
        // conversation history falls back to a fresh spawn — neither
        // --resume nor --continue.
        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path().join("workspace-missing");
        std::fs::create_dir_all(&cwd).unwrap();
        let config_dir = tmp.path().join("config");

        let args = compose_inplace_args(&cwd, Some(&config_dir), Some("stale-id"));
        assert!(
            !args.contains(&"--resume".to_owned()),
            "a stale id must not be passed to --resume: {args:?}"
        );
        assert!(
            !args.contains(&"--continue".to_owned()),
            "no prior conversation history: must not fall back to --continue: {args:?}"
        );
        assert!(
            args.contains(&"--setting-sources".to_owned()),
            "isolation flags must still be present: {args:?}"
        );
    }

    #[serial_test::serial]
    #[test]
    fn compose_inplace_args_uses_continue_when_no_id_but_prior_conv() {
        // has_prior_conversation (unlike session_id_exists) always looks under
        // `~/.claude/projects` regardless of `config_dir` — mirroring exactly
        // what `resume_command`'s own `has_prior_conv` computation does — so
        // this test redirects $HOME rather than seeding under a config dir.
        let _home = HomeGuard::set();
        let home = dirs::home_dir().expect("home resolves under redirected HOME");
        let cwd = std::path::PathBuf::from("/tmp/inplace-continue-test");
        // Seed prior conversation history (a .jsonl under the encoded cwd dir)
        // WITHOUT seeding the specific stale id, so has_prior_conversation is
        // true but session_id_exists for "stale-id" is false.
        let encoded = cwd.to_string_lossy().replace('/', "-");
        let project_dir = home.join(".claude").join("projects").join(&encoded);
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("some-other-session.jsonl"), "{}").unwrap();

        let args = compose_inplace_args(&cwd, None, Some("stale-id"));
        assert!(
            !args.contains(&"--resume".to_owned()),
            "a stale id must not be passed to --resume: {args:?}"
        );
        assert!(
            args.contains(&"--continue".to_owned()),
            "prior conversation history exists: must fall back to --continue: {args:?}"
        );
    }

    #[serial_test::serial]
    #[test]
    fn build_inplace_resume_command_resolves_claude_binary() {
        // Integration-style check that build_inplace_resume_command wires
        // resolve_claude + prepare_managed_config + compose_inplace_args
        // together; the pure selection logic is covered exhaustively above
        // without needing a real claude binary.
        let _home = HomeGuard::set();
        let Some(claude_bin) = ClaudeCodeAdapter::resolve_claude() else {
            return;
        };
        let tmp = tempfile::tempdir().expect("tempdir");
        let result =
            build_inplace_resume_command(tmp.path(), Some("some-id")).expect("build succeeds");
        assert_eq!(result.claude_bin, claude_bin);
        assert!(
            result.args.contains(&"--setting-sources".to_owned()),
            "isolation flags must be present: {:?}",
            result.args
        );
    }
}
