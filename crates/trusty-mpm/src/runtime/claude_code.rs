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

use crate::core::oauth_token::OAUTH_TOKEN_ENV_VAR;
use crate::session_manager::ManagedTmuxDriver;

use super::RuntimeAdapter;
use super::RuntimeError;

/// History-safe `GH_TOKEN`/`GH_USER` delivery (#3025 review follow-up) — a
/// sibling submodule so this file (grandfathered at a frozen SLOC budget,
/// #2398) does not have to carry the temp-file plumbing inline.
#[path = "claude_code_gh_env.rs"]
mod claude_code_gh_env;

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

/// Wrap a managed launch/resume command so it is rooted at `cwd` regardless
/// of the tmux pane shell's actual starting directory (#2250).
///
/// Why: `create_session -c <workdir>` is supposed to root the pane at the
/// workspace, and `session_manager::resume_workdir::verify_pane_cwd` now
/// fails loudly when tmux silently falls back to `$HOME` on the fresh-create
/// resume path — but a RESUMED session that reuses an already-alive pane
/// (created by a build that predates that verification, or whose cwd drifted
/// after creation for any other reason) has no such guard. Prepending an
/// explicit `cd` to the command line itself means the pane ends up in the
/// right place independent of whatever directory the shell actually started
/// in, closing that gap for every caller — not just the ones a point-in-time
/// verification could catch.
/// What: `cd '<cwd>' && { <body>; }`. `cwd` is single-quoted via
/// [`shell_single_quote`] for the same reason [`env_bin_prefix`] quotes
/// `CLAUDE_CONFIG_DIR` — a path with a space would otherwise word-split and
/// break the pane. The brace GROUP (not a `( … )` subshell) is load-bearing:
/// `body`'s `export TM_MANAGED_SESSION_ID=…` must land in the SAME shell
/// process so it survives `claude` exiting and dropping back to the pane's
/// shell (#2023 component B) — a subshell would discard that export the
/// instant the group exits, breaking the in-place-relaunch fallback.
/// Test: `spawn_command_prefixes_cd_to_workdir`,
/// `resume_command_prefixes_cd_to_workdir`,
/// `cd_and_group_quotes_workdir_with_space`.
fn cd_and_group(cwd: &Path, body: &str) -> String {
    format!(
        "cd {} && {{ {body}; }}",
        shell_single_quote(&cwd.display().to_string())
    )
}

/// Compose the `env …` prefix + resolved binary that starts every managed
/// `claude`, wiring in the tm-owned `CLAUDE_CONFIG_DIR` and, when available,
/// a `CLAUDE_CODE_OAUTH_TOKEN` (DOC-34; issue #2246).
///
/// Why: three invariants must hold on the pane command. (1) `-u ANTHROPIC_API_KEY`
/// strips the API key so Claude Code falls back to OAuth, preventing key leakage
/// through pane history. (2) When a managed `CLAUDE_CONFIG_DIR` is resolved,
/// `env -u ANTHROPIC_API_KEY CLAUDE_CONFIG_DIR='<dir>'` points the session at the tm-owned config home
/// (`~/.trusty-tools/trusty-mpm/claude-config/`). #4873: this config home DOES
/// supply the agent roster and skills. Every command builder resolves its flag
/// through [`crate::core::model_inject::setting_sources_flag`], which returns
/// `--setting-sources user,project,local` whenever a config dir is present —
/// and `user` is the tier this directory relocates (see [`spawn_command`]'s
/// #4451 note). The older text here claimed `project,local` and "loads from the
/// project layer"; that predates the relocation. It also isolates the session's
/// auth, which was always true: with the API key
/// scrubbed, `claude` authenticates via the keychain/`.credentials.json` keyed to
/// this config-dir path (the tm-managed login), never touching the operator's
/// `~/.claude`. (3) On macOS the Keychain entry Claude Code reads is keyed by a
/// hash of `CLAUDE_CONFIG_DIR` — so a `/login` run inside a managed session
/// diverges from the login stored under the operator's default config dir,
/// producing a "login successful then immediately not-logged-in" loop
/// (#2246). `CLAUDE_CODE_OAUTH_TOKEN`, when [`crate::core::oauth_token::resolve_oauth_token`]
/// resolves one, bypasses the Keychain entirely and sidesteps that divergence.
/// Every value is SINGLE-QUOTED via [`shell_single_quote`] so a home dir (or a
/// token, though tokens do not contain shell metacharacters in practice) with a
/// space does not word-split and break the pane. All settings are applied via a
/// single `env` prefix, consistent with the scrub-only prefix.
/// (4) Issue #4467: Claude Code's own process-local session markers — chiefly
/// `CLAUDE_CODE_CHILD_SESSION` — are unset via
/// [`crate::core::claude_env_scrub::env_unset_flags`]. When `tm` is invoked from
/// inside a Claude Code session it inherits them and passed them straight
/// through, and an inherited `CLAUDE_CODE_CHILD_SESSION` makes the spawned
/// `claude` turn session persistence OFF ("Transcript saving is off — inherited
/// CLAUDE_CODE_CHILD_SESSION marker"), costing the managed session its native
/// `--resume` / `--continue` / `/rewind` recovery. The scrub list deliberately
/// EXCLUDES `CLAUDE_CONFIG_DIR` — see that module's `DELIBERATE_SPAWN_ENV`, and
/// #4455/#4451 for why removing it would re-break the bundled agent roster.
/// (5) Issue #6495: the line always assigns
/// [`crate::core::alt_screen::ALT_SCREEN_SHELL_ASSIGNMENT`], which starts the
/// pane on Claude Code's classic renderer. The fullscreen renderer captures the
/// mouse wheel, so a managed pane loses both native and tmux scrollback. The
/// operand's `${NAME-1}` expansion means a value the pane already exports wins.
/// What: `env -u ANTHROPIC_API_KEY <-u marker…> CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN="${…-1}" [CLAUDE_CONFIG_DIR='<dir>'] [CLAUDE_CODE_OAUTH_TOKEN='<token>'] <claude_bin>`
/// — each bracketed assignment appears only when its value is `Some`; the
/// alternate-screen operand is unconditional. The
/// `-u NAME` option MUST precede any
/// `NAME=VALUE` assignment per POSIX `env` grammar (`env [OPTION]...
/// [NAME=VALUE]... [COMMAND]...`); putting an assignment before `-u` makes
/// `env` stop parsing options at the first `NAME=VALUE` and try to exec `-u`
/// as a command (`env: -u: No such file or directory`), which fatally kills
/// every managed session spawn.
/// Test: `spawn_command_contains_env_scrub`, `spawn_command_sets_claude_config_dir`,
/// `spawn_command_without_config_dir_omits_it`,
/// `env_bin_prefix_quotes_config_dir_with_space`,
/// `env_bin_prefix_orders_unset_flag_before_config_dir_assignment`,
/// `env_bin_prefix_carries_a_non_empty_mcp_env`,
/// `spawn_command_sets_oauth_token_when_available`,
/// `spawn_command_omits_oauth_token_when_absent`,
/// `spawn_command_without_token_pins_the_exact_command`,
/// `spawn_command_scrubs_inherited_session_markers`,
/// `spawn_command_keeps_config_dir_out_of_the_scrub`,
/// `env_bin_prefix_orders_scrub_flags_before_assignments`,
/// `spawn_command_defaults_the_alternate_screen_off`,
/// `resume_command_defaults_the_alternate_screen_off`.
///
/// `GH_TOKEN`/`GH_USER` (issue #3025) are deliberately NOT assignments on
/// this prefix — see [`claude_code_gh_env::gh_env_source_prefix`], applied
/// BEFORE this prefix in [`spawn_command`]/[`resume_command`]'s body, for why
/// (review follow-up: embedding a token literally in this `env` command line
/// would land it in the pane shell's history file and be transiently visible
/// in `ps` output for the `env` process itself).
pub(crate) fn env_bin_prefix(
    claude_bin: &str,
    config_dir: Option<&Path>,
    oauth_token: Option<&str>,
    mcp_env: &[(String, String)],
) -> String {
    let mut assignments = String::new();
    // #6495: default the pane to Claude Code's classic renderer so native and
    // tmux scrollback keep working; the `${NAME-1}` form yields to a value the
    // pane already exports.
    assignments.push(' ');
    assignments.push_str(crate::core::alt_screen::ALT_SCREEN_SHELL_ASSIGNMENT);
    if let Some(dir) = config_dir {
        let quoted = shell_single_quote(&dir.display().to_string());
        assignments.push_str(&format!(" CLAUDE_CONFIG_DIR={quoted}"));
    }
    // #4181: the per-project MCP pins. Claude Code hands its own environment to
    // every stdio MCP server it spawns, so these reach `trusty-memory` and
    // `trusty-search` in place of the arguments the deleted injectors wrote into
    // a workspace `.mcp.json`. See `core::mcp_session_env`.
    for (name, value) in mcp_env {
        let quoted = shell_single_quote(value);
        assignments.push_str(&format!(" {name}={quoted}"));
    }
    if let Some(token) = oauth_token {
        let quoted = shell_single_quote(token);
        assignments.push_str(&format!(" {OAUTH_TOKEN_ENV_VAR}={quoted}"));
    }
    // #4467: strip Claude Code's inherited process-local session markers. An
    // inherited `CLAUDE_CODE_CHILD_SESSION` makes the spawned `claude` disable
    // transcript saving, so the managed session gets no native `--resume` /
    // `--continue` / `/rewind` recovery and never appears in `--resume`. These
    // flags MUST precede `assignments` — POSIX `env` stops parsing options at
    // the first `NAME=VALUE`, so a `-u` after one is exec'd as a command.
    let scrub = crate::core::claude_env_scrub::env_unset_flags();
    format!("env -u ANTHROPIC_API_KEY{scrub}{assignments} {claude_bin}")
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
/// tiers it names. This spawn ALWAYS injects `CLAUDE_CONFIG_DIR`, so it carries
/// [`crate::core::model_inject::SETTING_SOURCES_FLAG_RELOCATED`]
/// (`--setting-sources user,project,local`), selected by
/// [`crate::core::model_inject::setting_sources_flag`].
///
/// Issue #4451 — why `user` is in that list, and why it MUST stay: the earlier
/// `project,local` value (correct for #1269 when the roster lived in the
/// project tier) became wrong the moment #4437 moved bundled agents into
/// `$CLAUDE_CONFIG_DIR/agents`, which IS the `user` tier. A managed session then
/// resolved zero bundled specialists (`Agent type 'rust-engineer' not found`)
/// with all 42 files correctly on disk. Re-admitting `user` does NOT re-admit
/// the operator's global `~/.claude/settings.json` hooks the #1269 exclusion was
/// protecting against: `CLAUDE_CONFIG_DIR` relocates the whole `user` tier to
/// the tm-owned config home, so the isolation now comes from the relocation.
/// Verified live against `claude` 2.1.220 — see the constant's own doc.
/// Project hooks (trusty-memory + PM-guard), workspace skills and MCP servers
/// continue to come from the PROJECT layer — `<workspace>/.claude/
/// {skills,settings.json,.mcp.json}` — which `session_launch::prepare_session`
/// (run on every daemon spawn path) deploys and which `project,local` loads.
/// `--dangerously-skip-permissions` keeps the
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
/// `cwd` (#2250) is where the tmux pane is supposed to be rooted; the
/// entire command is wrapped via [`cd_and_group`] so it is correct
/// independent of the pane shell's actual starting directory.
/// Test: `spawn_command_contains_env_scrub`,
/// `spawn_command_contains_isolation_flags`,
/// `spawn_command_uses_resolved_binary`,
/// `spawn_command_sets_claude_config_dir`,
/// `spawn_command_exports_managed_session_id`,
/// `spawn_command_prints_relaunch_hint_after_claude_exits`,
/// `spawn_command_with_prompt_file_contains_flag`,
/// `spawn_command_without_prompt_file_omits_flag`,
/// `spawn_command_prefixes_cd_to_workdir`,
/// `spawn_command_sets_oauth_token_when_available`,
/// `spawn_command_omits_oauth_token_when_absent`. `gh_env_file` (#3025):
/// `claude_code_gh_env_tests.rs`.
#[allow(clippy::too_many_arguments)]
fn spawn_command(
    cwd: &Path,
    claude_bin: &str,
    config_dir: Option<&Path>,
    session_id: &str,
    prompt_file: Option<&Path>,
    oauth_token: Option<&str>,
    gh_env_file: Option<&Path>,
    mcp_env: &[(String, String)],
) -> String {
    let body = format!(
        "{}{}{}{} {} {}{}",
        session_id_export_prefix(session_id),
        claude_code_gh_env::gh_env_source_prefix(gh_env_file),
        env_bin_prefix(claude_bin, config_dir, oauth_token, mcp_env),
        prompt_file_flag(prompt_file),
        // #4451: the relocated spawn must load the `user` tier — that is where
        // `CLAUDE_CONFIG_DIR/agents` (the bundled roster) lives.
        crate::core::model_inject::setting_sources_flag(config_dir),
        crate::core::model_inject::PERMISSION_MODE_FLAG,
        on_exit_hint_suffix(),
    );
    cd_and_group(cwd, &body)
}

/// Build and write the PM system-prompt file for `project_dir`, for injection
/// into the daemon managed-spawn command via `--append-system-prompt-file`
/// (issue #2125 item 3 — the daemon-adapter carrier).
///
/// Why: `spawn` never actually passed `--append-system-prompt-file`
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
///
/// #4752/#4832: this is also where the session's compiled prompt
/// (`<harness-root>/.trusty-mpm/sessions/<id>/INSTRUCTIONS-COMPILED.md`) is
/// refreshed from the exact bytes handed to `--append-system-prompt-file`.
/// `session_id` selects that directory: `spawn`/`spawn_resume` pass the managed
/// id they already hold, and the in-place relaunch passes `None` so
/// [`crate::core::harness_root::session_scope`] falls back to the pane's
/// `TM_MANAGED_SESSION_ID`. Passing `None` where an id IS known would refresh a
/// different file from the one preparation wrote.
///
/// WHY THIS ONE IS BEST-EFFORT while the same write is FATAL in the
/// provisioning paths — the asymmetry is deliberate: those are PROVISIONING
/// steps, whose job is to establish the session's on-disk state before anything
/// spawns; refusing there is coherent and is what the owner ruled. This call
/// sits INSIDE the spawn, past that gate.
///
/// That holds only because ALL THREE entry points that reach this function have
/// already had a fatal compiled write succeed:
///   * fresh start → `session_launch::prepare_session_inner`
///   * daemon resume → `managed_routes::lifecycle::resume_managed`
///   * bare-`tm` in-place relaunch → `guided_inplace::run_inplace_relaunch`,
///     via `instruction_pipeline::refresh_compiled_prompt`
///
/// The third was missing until round 4 of #4752, which made the earlier version
/// of this comment false: `run_inplace_relaunch` calls neither of the other two,
/// so this best-effort write was its ONLY compiled write. Adding a call here
/// without that fatal upstream write would resurrect the same hole — check the
/// list above before adding a fourth spawn path.
///
/// Making it fatal here would also invert this function's own priority: a
/// failure of the strictly MORE important write below (the actual system-prompt
/// file) already degrades to spawning without it (#2173). Refusing to launch
/// over the inspection copy while shrugging at the real prompt would be exactly
/// backwards.
/// Test: `build_prompt_file_writes_resolved_prompt_for_project`,
/// `build_prompt_file_refreshes_the_compiled_prompt`,
/// `build_prompt_file_compiled_write_failure_does_not_block_the_spawn`.
fn build_prompt_file(project_dir: &Path, session_id: Option<&str>) -> Option<std::path::PathBuf> {
    let native = crate::core::output_style::claude_supports_native_output_style();
    let prompt = crate::core::session_launch::build_system_prompt_for_with_style_and_native(
        project_dir,
        None,
        native,
    );

    // #4752: refresh this PROJECT's compiled prompt from the very string about
    // to be passed to `--append-system-prompt-file`. Best-effort by design —
    // see this function's doc comment for why the same write is fatal in the
    // two provisioning steps and not here.
    let scope = crate::core::harness_root::session_scope(session_id);
    let compiled = crate::core::instruction_pipeline::compiled_prompt_path(project_dir, &scope);
    if let Err(err) =
        crate::core::instruction_pipeline::write_compiled_prompt_to(&compiled, &prompt)
    {
        tracing::warn!(
            project = %project_dir.display(),
            path = %compiled.display(),
            "failed to refresh the compiled PM prompt (non-fatal; the session still \
             launches with the prompt file below): {err}"
        );
    }

    let file = crate::core::model_inject::write_prompt_file(&prompt);
    if file.is_none() {
        tracing::warn!(
            project = %project_dir.display(),
            "failed to write PM system-prompt file; spawning without --append-system-prompt-file"
        );
    }
    file
}

/// Build a resume-aware Claude Code command (#1744, #1840, #6765).
///
/// Why: `resume_managed` must restore the prior conversation when one is known.
/// `--resume <id>` restores the exact Claude Code conversation identified by
/// `claude_session_id`. There is no `--continue` fallback (#6765): the command
/// exports the MANAGED `CLAUDE_CONFIG_DIR`, so a bare `--continue` resolves
/// "most recent conversation" against `<config_dir>/projects` — a store this
/// process cannot reason about, and which may hold a still-live conversation
/// Claude Code refuses to double-attach to (it prints "Your most recent
/// conversation is running in the background" and exits 0, dropping the pane to
/// a bare shell). Resolving the target EXPLICITLY, or launching fresh, keeps
/// the decision independent of Claude Code's undocumented background-job layout.
/// What: `Some(id)` → `--resume <id>`; `None` → plain spawn (no resume flag), so
/// the session starts a fresh conversation. Both share the same env prefix
/// (scrub + `CLAUDE_CONFIG_DIR`) and isolation flags as [`spawn_command`], and
/// both get [`on_exit_hint_suffix`] appended (#2023 component D) so the pane
/// always prints the relaunch hint once
/// `claude` exits, whichever branch fired. `prompt_file` (#2230) carries the
/// PM system prompt via `--append-system-prompt-file` the same way
/// [`spawn_command`] does, so resumed/guided-resume/crash-recovery sessions
/// get the identical carrier a fresh spawn gets instead of falling back to a
/// vanilla `claude` invocation.
/// Test: `resume_command_with_id_uses_resume_flag`,
/// `resume_command_without_id_never_uses_continue`,
/// `resume_command_without_id_no_prior_conv_uses_plain_spawn`,
/// `resume_command_sets_claude_config_dir`,
/// `resume_command_exports_managed_session_id`,
/// `resume_command_prints_relaunch_hint_after_claude_exits`,
/// `resume_command_with_prompt_file_contains_flag`,
/// `resume_command_prefixes_cd_to_workdir`,
/// `resume_command_sets_oauth_token_when_available`,
/// `resume_command_omits_oauth_token_when_absent`.
///
/// `cwd` (#2250) is where the tmux pane is supposed to be rooted; the entire
/// command is wrapped via [`cd_and_group`] — see its doc for why this must be
/// a brace GROUP, not a subshell (the exported `TM_MANAGED_SESSION_ID` has to
/// survive `claude` exiting).
///
/// `#[allow(too_many_arguments)]`: each parameter is an independently-tested,
/// orthogonal command-builder input (cwd/#2250, config_dir/DOC-34, resume
/// selection/#1744+#1840, session_id/#2023, prompt_file/#2230, oauth_token/
/// #2246, gh_env_file/#3025) — bundling them into a struct would only move
/// the field list, not reduce it, and this is a private, single-call-site
/// builder (mirrors the established pattern in `session_manager/create.rs`).
#[allow(clippy::too_many_arguments)]
fn resume_command(
    cwd: &Path,
    claude_bin: &str,
    config_dir: Option<&Path>,
    claude_session_id: Option<&str>,
    session_id: &str,
    prompt_file: Option<&Path>,
    oauth_token: Option<&str>,
    gh_env_file: Option<&Path>,
    mcp_env: &[(String, String)],
) -> String {
    let base = format!(
        "{}{}{}{} {} {}",
        session_id_export_prefix(session_id),
        claude_code_gh_env::gh_env_source_prefix(gh_env_file),
        env_bin_prefix(claude_bin, config_dir, oauth_token, mcp_env),
        prompt_file_flag(prompt_file),
        // #4451: same relocated-tier contract as `spawn_command`.
        crate::core::model_inject::setting_sources_flag(config_dir),
        crate::core::model_inject::PERMISSION_MODE_FLAG,
    );
    let cmd = match claude_session_id {
        Some(id) => format!("{base} --resume {id}"),
        // #6765: never a bare `--continue` — it would resolve against the
        // managed store this process did not choose. No usable id → start fresh.
        None => base,
    };
    let body = format!("{cmd}{}", on_exit_hint_suffix());
    cd_and_group(cwd, &body)
}

/// Encode a workspace path the same way Claude Code names its project dir.
///
/// Why: every on-disk lookup against a Claude Code session store — today only
/// the `--resume <id>`-existence check ([`session_id_exists_in`]) — must derive
/// the SAME project directory name for a given `cwd`. Sharing one helper makes
/// two callers computing the encoding differently impossible.
/// What: replaces every `/` in the path with `-` (Claude Code's project-dir
/// naming scheme). A leading `/` becomes `-`, so `/private/tmp/foo` →
/// `-private-tmp-foo`.
/// Test: `encode_project_dir_replaces_slashes`,
/// `session_id_exists_true_for_real_jsonl_file`,
/// `session_id_exists_finds_hardcoded_dir_name_for_known_cwd` (non-circular
/// regression guard against encoding-scheme drift).
fn encode_project_dir(cwd: &Path) -> String {
    cwd.to_string_lossy().replace('/', "-")
}

// #6765: `has_prior_conversation` / `has_prior_conversation_in` used to live
// here as the `--continue`-eligibility check. They hardcoded
// `~/.claude/projects` — the OPERATOR's store — while every managed relaunch
// exports a managed `CLAUDE_CONFIG_DIR`, so the check answered for one store
// and the flag acted on another. Both relaunch paths now resolve the target
// explicitly (`--resume <id>` verified against the session's OWN store, else a
// fresh launch), leaving neither function a caller. Deleted rather than
// repointed: with no `--continue` branch there is nothing to gate.

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
/// back to `~/.claude/projects` when `config_dir` is `None` — the unmanaged
/// default store, which is where an unrelocated `claude` really does keep its
/// conversations.
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
/// to a plain spawn instead of a hard failure.
/// What: best-effort filesystem check — `true` only when
/// `<projects_dir>/<encoded-cwd>/<id>.jsonl` exists as a regular file, where
/// `projects_dir` is resolved via [`projects_dir_for`] and the cwd is encoded
/// via the shared [`encode_project_dir`] helper (every `/` becomes `-`).
/// #6765: this is now the ONLY conversation-store lookup either relaunch path
/// makes — it is the check that consults the store the spawned process will
/// actually use, which is exactly what the deleted `--continue` gate did not.
/// Never panics: an unresolvable projects dir, a missing file, or any I/O
/// error all conservatively resolve to `false` (safest outcome — it only ever
/// causes an extra fallback, never a hard failure or a wrong `--resume`).
/// Test: `session_id_exists_true_for_real_jsonl_file`,
/// `session_id_exists_false_for_missing_id`,
/// `session_id_exists_false_when_projects_dir_absent`.
///
/// `pub(crate)` (#4337): the daemon's `SessionStart` correlation
/// (`daemon::api::session_start_correlation`) reuses this SAME staleness
/// check to decide whether a managed session's already-stored
/// `claude_session_id` may be replaced by a differently-reported one, so the
/// two call sites can never disagree about what "stale" means.
pub(crate) fn session_id_exists(cwd: &Path, config_dir: Option<&Path>, id: &str) -> bool {
    match projects_dir_for(config_dir) {
        Some(projects_dir) => session_id_exists_in(cwd, &projects_dir, id),
        None => false,
    }
}

/// Inner implementation of the session-id existence check, testable with an
/// injected `projects_dir` (the injected-I/O-root pattern used throughout this
/// module).
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
/// operator's `~/.claude`.
///
/// #4873 CORRECTION — the provisioning here is LOAD-BEARING, not
/// belt-and-suspenders. This doc used to say the config home's provisioned
/// agents/skills/settings.json "are NOT loaded" because `--setting-sources
/// project,local` excludes the `user` tier this dir relocates, and that the
/// framework roster arrived via the PROJECT layer instead. That stopped being
/// true when the flag became conditional: all three command builders
/// (`spawn_command`, `resume_command`, `compose_inplace_args`) resolve it
/// through [`crate::core::model_inject::setting_sources_flag`], which returns
/// `--setting-sources user,project,local` whenever `config_dir` is `Some` — and
/// every managed path reaches here with `Some`. The `user` tier IS read. See
/// `spawn_command`'s #4451 note, which already stated this correctly while
/// this comment contradicted it. Auth + trust isolation remains true and is now
/// one of two load-bearing effects, not the only one.
///
/// This centralises the three coupled steps — resolve the path,
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
///
/// // #4181 (ADR-0042): this function used to re-run the four `.mcp.json`
/// force-overwrite injectors on every spawn and resume, and derive
/// `enabledMcpjsonServers` from whether each write succeeded (#3918→#3950). Both
/// halves are deleted. MCP servers are declared once in
/// `<config_dir>/.claude.json`'s user-scope `mcpServers` map — the map a
/// relocated spawn reads under `--setting-sources user,project,local`, and which
/// Claude Code connects with no approval — so there is no per-run write to prove
/// and no name to pre-approve. `preseed_managed_trust` now seeds the trust dialog
/// only, and strips any approval an older tm left in the file.
/// Test: exercised via `spawn_sends_env_scrub_when_binary_available` and the
/// `spawn_command`/`resume_command` config-dir tests (the command-string layer);
/// the provisioning itself is covered in `core::managed_config`;
/// `prepare_managed_config_writes_no_mcp_json` and
/// `prepare_managed_config_writes_no_mcp_approval` cover the deletions.
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
    // #4880: `cwd` is the workspace, so the same call also refreshes the
    // PROJECT skill tier (`<cwd>/.claude/skills`) when the project manifest
    // moved — the tier that outranks everything this config dir carries.
    if let Err(e) = crate::core::managed_config::ensure_managed_config_dir(&config_dir, cwd) {
        tracing::warn!(
            session = %tmux_name,
            config_dir = %config_dir.display(),
            "managed config dir provisioning failed (non-fatal): {e}"
        );
    }

    // Seed workspace trust into <config_dir>/.claude.json (isolation invariant:
    // NEVER ~/.claude.json) so the session starts without the trust dialog.
    // // #4181: no MCP approval is written, and a stale one is stripped.
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
/// plus `--resume <id>` or neither, mirroring [`resume_command`]'s
/// selection — see [`compose_inplace_args`]), `config_dir` (the tm-owned
/// `CLAUDE_CONFIG_DIR`, when resolved), and `oauth_token` (issue #2246 — the
/// resolved [`crate::core::oauth_token::resolve_oauth_token`] value, when
/// available). The caller is expected to `env_remove("ANTHROPIC_API_KEY")`
/// and, when each field is `Some`, set the matching env var (`CLAUDE_CONFIG_DIR`
/// / `CLAUDE_CODE_OAUTH_TOKEN`) — the same invariants [`env_bin_prefix`]
/// encodes into the shell-string commands.
/// Test: exercised via [`build_inplace_resume_command`]'s tests.
#[derive(Debug)]
pub struct InPlaceResumeCommand {
    /// Resolved `claude` binary (absolute path).
    pub claude_bin: String,
    /// Full argv (isolation flags + resume-or-fresh selection), EXCLUDING the
    /// binary itself.
    pub args: Vec<String>,
    /// The tm-owned `CLAUDE_CONFIG_DIR`, when resolved (`None` when home is
    /// unresolvable — mirrors [`prepare_managed_config`]'s fallback).
    pub config_dir: Option<std::path::PathBuf>,
    /// The resolved `CLAUDE_CODE_OAUTH_TOKEN`, when available (issue #2246;
    /// see [`crate::core::oauth_token::resolve_oauth_token`]'s precedence).
    pub oauth_token: Option<String>,
    /// The per-project MCP pins to export (#4181): `TRUSTY_MEMORY_PALACE` and
    /// `TRUSTY_INDEX`, as [`crate::core::mcp_session_env::session_mcp_env`]
    /// resolved them. Empty when both manifest toggles are off or neither value
    /// could be derived — the servers then fall back to their own cwd
    /// derivation, which is what the unpinned stub used to do.
    pub mcp_env: Vec<(String, String)>,
}

/// Pure argv composition shared by [`build_inplace_resume_command`] (#2023 C).
///
/// Why: separating the resume/fresh SELECTION from claude-binary
/// resolution (which needs a real `claude` install to exercise end-to-end)
/// keeps the decision itself testable in every CI environment — mirroring how
/// [`resume_command`]'s selection tests pass a fake `claude_bin` string rather
/// than depending on [`ClaudeCodeAdapter::resolve_claude`].
/// What: `--append-system-prompt-file <path>` when `prompt_file` is `Some`
/// (#4336 — see below), then the isolation flags
/// ([`crate::core::model_inject::SETTING_SOURCES_FLAG`] /
/// [`crate::core::model_inject::PERMISSION_MODE_FLAG`], whitespace-split
/// into argv tokens since both constants are simple space-separated flags with
/// no embedded quoting) followed by `--resume <id>` (id exists under
/// `config_dir`, per [`session_id_exists`]) or neither flag (fresh start) — the
/// exact same two-way selection [`resume_command`] makes. #6765: there is no
/// `--continue` fallback on either path; see [`resume_command`]'s doc for why a
/// bare `--continue` under a managed `CLAUDE_CONFIG_DIR` is unsafe.
///
/// `prompt_file` (#4336): this path previously omitted the PM system prompt BY
/// DESIGN, so an in-place relaunch silently restored the operator into vanilla
/// Claude Code — the same defect #2125/#2230 fixed for the spawn and resume
/// paths, left unfixed here because the exec seam is not a shell string. The
/// path is pushed as its OWN argv token and is deliberately NOT run through
/// [`shell_single_quote`] the way [`prompt_file_flag`] does: that helper exists
/// because `spawn_command`/`resume_command` build a string a pane SHELL will
/// re-split, whereas these tokens go straight to `execv` with no shell in
/// between, so quoting them would embed literal quote characters in the
/// filename claude then fails to open.
/// Test: `compose_inplace_args_uses_resume_for_existing_id`,
/// `compose_inplace_args_falls_back_for_missing_id`,
/// `compose_inplace_args_never_continues_from_home_store`,
/// `compose_inplace_args_carries_prompt_file_unquoted`,
/// `compose_inplace_args_omits_prompt_flag_when_absent`.
fn compose_inplace_args(
    cwd: &Path,
    config_dir: Option<&Path>,
    claude_session_id: Option<&str>,
    prompt_file: Option<&Path>,
) -> Vec<String> {
    let effective_id = claude_session_id.filter(|id| session_id_exists(cwd, config_dir, id));

    let mut args: Vec<String> = Vec::new();
    if let Some(prompt) = prompt_file {
        args.push("--append-system-prompt-file".to_owned());
        args.push(prompt.display().to_string());
    }
    args.extend(
        // #4451: in-place relaunch inherits the same relocated-tier contract.
        crate::core::model_inject::setting_sources_flag(config_dir)
            .split_whitespace()
            .chain(crate::core::model_inject::PERMISSION_MODE_FLAG.split_whitespace())
            .map(str::to_owned),
    );

    // #6765: an id verified against the session's OWN store, or a fresh launch.
    // Never a bare `--continue` — see `resume_command`'s doc.
    if let Some(id) = effective_id {
        args.push("--resume".to_owned());
        args.push(id.to_owned());
    }
    args
}

/// Build an [`InPlaceResumeCommand`] for the bare-`tm` in-pane relaunch path
/// (#2023 component C).
///
/// Why: the in-place relaunch must use the SAME `--resume <id>`
/// existence-check → fresh-spawn fallback semantics as the tmux-pane resume
/// path (#2013, #6765) — reusing [`session_id_exists`] /
/// [`prepare_managed_config`] directly (via [`compose_inplace_args`]), rather
/// than re-deriving them, means the two paths can never silently drift.
/// What: resolves the `claude` binary (`Err(RuntimeError::BinaryNotFound)` if
/// missing), provisions/trust-seeds the managed `CLAUDE_CONFIG_DIR` via
/// [`prepare_managed_config`] (logged under the synthetic session name
/// `"in-place-relaunch"` — there is no tmux session name in this context),
/// builds the PM system-prompt file via [`build_prompt_file`] (#4336 — the
/// SAME carrier `spawn`/`spawn_resume` use, previously missing from this path
/// alone; non-fatal, a write failure omits the flag), then delegates argv
/// composition to [`compose_inplace_args`].
/// Test: `build_inplace_resume_command_resolves_claude_binary`,
/// `build_inplace_resume_command_carries_prompt_file`.
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
    // #4832: no explicit id here — this path runs INSIDE the managed pane, so
    // `session_scope` reads `TM_MANAGED_SESSION_ID` from the environment.
    let prompt_file = build_prompt_file(cwd, None);
    let args = compose_inplace_args(
        cwd,
        config_dir.as_deref(),
        claude_session_id,
        prompt_file.as_deref(),
    );
    let oauth_token = crate::core::oauth_token::resolve_oauth_token();
    // #4181: same per-project MCP pins the tmux-pane paths export.
    let mcp_env = crate::core::mcp_session_env::session_mcp_env(cwd, None);
    Ok(InPlaceResumeCommand {
        claude_bin,
        args,
        config_dir,
        oauth_token,
        mcp_env,
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
    /// [`build_prompt_file`] (issue #2125 item 3), resolves an optional
    /// `CLAUDE_CODE_OAUTH_TOKEN` via
    /// [`crate::core::oauth_token::resolve_oauth_token`] (issue #2246), then
    /// sends [`spawn_command`] (`env -u ANTHROPIC_API_KEY CLAUDE_CONFIG_DIR=<dir>
    /// [CLAUDE_CODE_OAUTH_TOKEN=<token>] <abs-claude> --append-system-prompt-file
    /// <prompt>` plus the isolation/permission flags) to the pane; the task is
    /// logged for observability but not passed to the command. `gh_env`
    /// (#3025) is caller-RESOLVED (the daemon's spawn handler consults the
    /// `ProjectRegistry` — the actual write target for a pinned `gh_account`
    /// — off the async executor via `tokio::task::spawn_blocking`, since this
    /// trait method itself is synchronous); `spawn` only writes it to a
    /// mode-0600 temp file the pane sources-and-deletes
    /// ([`claude_code_gh_env::write_gh_env_file`]) rather than embedding the
    /// token literally in the command line sent via `send-keys` (history/`ps`
    /// exposure, review follow-up).
    /// Test: `spawn_sends_env_scrub_when_binary_available`.
    fn spawn(
        &self,
        tmux_name: &str,
        cwd: &Path,
        task: &str,
        session_id: &str,
        gh_env: &[(String, String)],
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
        // #4873: the framework roster and skills load FROM this config dir —
        // `setting_sources_flag` yields `user,project,local` whenever it is
        // present, and `user` is the tier it relocates. The older comment here
        // claimed the project layer under `project,local`; see
        // `prepare_managed_config`. Non-fatal throughout (closes #1696).
        let config_dir = prepare_managed_config(tmux_name, cwd);
        // Build and inject the PM system prompt (issue #2125 item 3) so this,
        // the default daemon on-ramp, can no longer silently spawn vanilla
        // Claude Code. Non-fatal: a write failure omits the flag (#2173 ruled
        // out a CLAUDE.md-carrier fallback, so there is no other carrier).
        let prompt_file = build_prompt_file(cwd, Some(session_id));
        // Issue #2246: inject CLAUDE_CODE_OAUTH_TOKEN when one is available
        // (an operator-set env var, else the tm-managed store) to bypass the
        // CLAUDE_CONFIG_DIR-keyed Keychain divergence that causes the
        // managed-session login loop. `None` when neither source has a
        // token — the command is then byte-identical to pre-#2246.
        let oauth_token = crate::core::oauth_token::resolve_oauth_token();
        // Issue #3025: `gh_env` was already resolved by the caller (registry
        // lookup + `gh auth token` both happen off the async executor). Here
        // we only write it to a mode-0600 temp file the pane sources-and-
        // deletes — the token value itself never appears in the command
        // line sent via `send-keys` (history/`ps` exposure, review follow-up).
        // #4181: the per-project MCP pins the shared user-scope declarations
        // cannot carry as arguments. Resolved once per spawn (it touches the
        // trusty-search daemon), never inside the command-string builder.
        let mcp_env = crate::core::mcp_session_env::session_mcp_env(cwd, None);
        let gh_env_file = claude_code_gh_env::write_gh_env_file(gh_env);
        self.tmux
            .send_line(
                tmux_name,
                &spawn_command(
                    cwd,
                    // #2997: route the pane's `claude` through the disclaim-exec
                    // wrapper so tccd attributes its RemovableVolumes/App-Data
                    // access to Claude Code itself, not the shared tmux server
                    // that forks this pane's shell. No-op off macOS / under
                    // TM_DISABLE_SPAWN_DISCLAIM (`claude_bin` stays unwrapped for
                    // the debug log above).
                    &crate::core::spawn_disclaim::disclaim_pane_command(&claude_bin),
                    config_dir.as_deref(),
                    session_id,
                    prompt_file.as_deref(),
                    oauth_token.as_deref(),
                    gh_env_file.as_deref(),
                    &mcp_env,
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

    /// Resume Claude Code with conversation continuity (#1744, #2013, #6765).
    ///
    /// Why: `resume_managed` must restore the prior conversation rather than
    /// starting fresh. If the stored `claude_session_id` is available AND
    /// still resolves to a real session on disk (checked via
    /// [`session_id_exists`], #2013), `--resume <id>` restores the exact
    /// conversation. A stale id (the session was pruned, moved, or never
    /// reached disk) is NOT passed to `--resume` — `claude --resume <missing>`
    /// fails hard with no recovery — instead the pane launches FRESH. #6765:
    /// there is no `--continue` fallback; the target is resolved explicitly or
    /// not at all, so the decision never depends on which conversation the
    /// managed store happens to consider "most recent".
    /// What: resolves the claude binary, provisions + trust-seeds the tm-owned
    /// `CLAUDE_CONFIG_DIR` via [`prepare_managed_config`], existence-checks
    /// `claude_session_id` against the resolved config dir, falls back to a
    /// fresh launch when it is missing,
    /// builds the PM system-prompt file via [`build_prompt_file`] (#2230 —
    /// same carrier `spawn` uses, previously missing from every resume path),
    /// resolves an optional `CLAUDE_CODE_OAUTH_TOKEN` via
    /// [`crate::core::oauth_token::resolve_oauth_token`] (#2246 — same carrier
    /// `spawn` uses), then sends the appropriate [`resume_command`] to the
    /// tmux pane — targeting the SPECIFIC `pane_id` (via
    /// [`ManagedTmuxDriver::send_line_to_pane`]) when the caller supplies one,
    /// rather than the session-scoped [`ManagedTmuxDriver::send_line`], which
    /// tmux resolves to whichever pane/window is currently active (sibling-
    /// window hijack fix, follow-up to #2456). `pane_id: None` (a legacy
    /// record that predates pane-id capture) falls back to the session-scoped
    /// send, preserving prior behavior for that case.
    /// Test: `spawn_resume_with_id_uses_resume_flag`,
    /// `spawn_resume_without_id_no_prior_conv_sends_plain_spawn`,
    /// `spawn_resume_never_sends_bare_continue`,
    /// `spawn_resume_sends_prompt_file_when_binary_available`,
    /// `spawn_resume_sends_oauth_token_when_available`,
    /// `spawn_resume_targets_stored_pane_id_when_known`,
    /// `spawn_resume_falls_back_to_session_target_when_pane_id_unknown`.
    #[allow(clippy::too_many_arguments)]
    fn spawn_resume(
        &self,
        tmux_name: &str,
        pane_id: Option<&str>,
        cwd: &Path,
        task: &str,
        claude_session_id: Option<&str>,
        session_id: &str,
        gh_env: &[(String, String)],
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
        let prompt_file = build_prompt_file(cwd, Some(session_id));
        // #2246: the resume path must ALSO carry CLAUDE_CODE_OAUTH_TOKEN —
        // every resumed/guided-resume/crash-recovery session funnels through
        // here, so omitting it would leave exactly those sessions exposed to
        // the login loop spawn() itself was fixed against.
        let oauth_token = crate::core::oauth_token::resolve_oauth_token();
        // Issue #3025: same caller-resolved `gh_env` → temp-file delivery as
        // `spawn` — every resumed/guided-resume/crash-recovery session must
        // get the same deterministic `gh` identity.
        // #4181: the per-project MCP pins the shared user-scope declarations
        // cannot carry as arguments. Resolved once per spawn (it touches the
        // trusty-search daemon), never inside the command-string builder.
        let mcp_env = crate::core::mcp_session_env::session_mcp_env(cwd, None);
        let gh_env_file = claude_code_gh_env::write_gh_env_file(gh_env);

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
        // #6765: no `--continue` fallback — a usable id resumes by id, anything
        // else launches fresh.
        debug!(
            session = %tmux_name,
            pane_id = pane_id.unwrap_or("<none>"),
            cwd = %cwd.display(),
            task = %task,
            claude = %claude_bin,
            resume = effective_id.is_some(),
            "resuming claude-code in tmux pane"
        );
        let cmd = resume_command(
            cwd,
            // #2997: same disclaim-exec wrapper as `spawn` — every resumed /
            // guided-resume / crash-recovery pane must disclaim `claude` off the
            // shared tmux server too, else exactly those sessions keep
            // re-prompting.
            &crate::core::spawn_disclaim::disclaim_pane_command(&claude_bin),
            config_dir.as_deref(),
            effective_id,
            session_id,
            prompt_file.as_deref(),
            oauth_token.as_deref(),
            gh_env_file.as_deref(),
            &mcp_env,
        );
        // Sibling-window hijack fix (follow-up to #2456): when the caller
        // supplies the record's own `pane_id`, target it directly — tmux's
        // session-scoped `send_line` resolves to whichever pane/window is
        // currently ACTIVE, which is not necessarily (and after this bug,
        // often was not) the pane this resume is actually about. `None`
        // (a legacy record predating pane-id capture) preserves the prior
        // session-scoped behavior — there is no stronger signal available.
        let send_result = match pane_id {
            Some(pane_id) => self.tmux.send_line_to_pane(tmux_name, pane_id, &cmd),
            None => self.tmux.send_line(tmux_name, &cmd),
        };
        send_result.map_err(|e| RuntimeError::TmuxUnavailable(e.to_string()))?;
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

// #3025 GH_TOKEN/GH_USER spawn-env injection tests live in a companion
// `_tests.rs` file purely to keep this file — a production file capped at
// 500 SLOC (grandfathered, #2398) — clear of further growth; see that
// file's module doc.
#[cfg(test)]
#[path = "claude_code_gh_env_tests.rs"]
mod gh_env_tests;

// The command-builder and `RuntimeAdapter` impl tests (#3070) live in a
// sibling `_tests.rs` file for the same reason — see that file's module doc.
#[cfg(test)]
#[path = "claude_code_tests.rs"]
mod tests;
