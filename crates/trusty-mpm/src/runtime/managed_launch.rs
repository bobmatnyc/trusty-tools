//! Compose a managed Claude Code launch as parameters, not as a shell script
//! (#8233).
//!
//! Why: `spawn_command`/`resume_command`/`attach_command` used to return one
//! shell STRING that `tmux send-keys` typed into the pane. Every parameter — the
//! cwd, the `env -u` scrub flags, each `NAME=VALUE`, the prompt-file path, the
//! `--mcp-config` path — added bytes to that line, and past the tty's
//! canonical-mode limit (`MAX_CANON`, 1024 on macOS) the line discipline dropped
//! the tail and the session died mid-word. This module composes the SAME
//! parameters into a [`LaunchSpec`] instead, so the typed line carries only a
//! fixed-shape reference to it.
//!
//! What: [`managed_env_unset`]/[`managed_env_set`] are the structured form of
//! the old `env_bin_prefix` — same variables, same order, same reasons (see each
//! function). [`spawn_spec`], [`resume_spec`] and [`attach_spec`] add the argv
//! each path needs. [`pane_line`] renders the one thing the pane is typed.
//!
//! The #2997 disclaim contract is unchanged and slightly stronger: the pane
//! still runs `tm internal-spawn-disclaimed`, and that shim now `posix_spawn`s
//! `claude` ITSELF rather than spawning an `env` that execs it, so `claude` is
//! its own TCC responsible process with one less hop.
//!
//! Test: `crates/trusty-mpm/src/runtime/managed_launch_tests.rs`.

use std::path::Path;

use super::launch_spec::LaunchSpec;

/// The environment variable a managed spawn always strips, ahead of the
/// inherited-marker scrub.
///
/// Why: with `ANTHROPIC_API_KEY` removed, Claude Code falls back to OAuth, so
/// the operator's API key never reaches a managed session (DOC-34). This was the
/// `-u ANTHROPIC_API_KEY` that led every `env` prefix.
/// Test: `env_unset_leads_with_the_api_key`.
const API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// Everything a managed launch removes from the inherited pane environment.
///
/// Why: three independent removals, in one list because `LaunchSpec` applies
/// them before any assignment — the same ordering POSIX `env` forced when this
/// was a `-u` flag run. (1) [`API_KEY_ENV`], so the session authenticates via
/// the tm-owned config dir rather than the operator's key. (2)
/// [`crate::core::claude_env_scrub::INHERITED_SESSION_MARKERS`] — an inherited
/// `CLAUDE_CODE_CHILD_SESSION` makes the spawned `claude` turn transcript saving
/// OFF, costing the session its native `--resume`/`--continue`/`/rewind`
/// recovery (#4467). (3) #6668: the `gh` identity variables the pane already
/// exports, when this launch pins a `gh` account — `gh` reads an env token
/// before a config dir, so leaving an inherited `GH_TOKEN` in place would make
/// every `gh` call in the session use the wrong account. Which ones to clear is
/// decided by [`crate::core::gh_identity::inherited_identity_to_clear`], the
/// same function `resolve_gh_env` uses, so the precedence rule has one
/// definition. (Before #8233 those were `unset NAME` lines in a sourced temp
/// file; they are the same names, applied to the child instead of to the pane
/// shell.)
/// What: the names, in that order.
/// Test: `env_unset_leads_with_the_api_key`,
/// `env_unset_carries_every_inherited_session_marker`,
/// `env_unset_clears_an_inherited_gh_token`,
/// `env_unset_clears_nothing_without_a_gh_identity_binding`.
pub(crate) fn managed_env_unset(gh_env: &[(String, String)]) -> Vec<String> {
    let mut unset = vec![API_KEY_ENV.to_owned()];
    unset.extend(
        crate::core::claude_env_scrub::INHERITED_SESSION_MARKERS
            .iter()
            .map(|name| (*name).to_string()),
    );
    unset.extend(crate::core::gh_identity::inherited_identity_to_clear(
        gh_env,
    ));
    unset
}

/// Every variable a managed launch ASSIGNS, in the order the `env` prefix
/// emitted them.
///
/// Why, per entry — each rationale is the one the shell prefix carried:
///   * #8066 `CLAUDE_CODE_ENABLE_TODO_TOOLS=1`, unconditional: Claude Code
///     2.1.260 gates `TodoWrite` and the `TaskCreate` family to a fixed model
///     list, so a managed session on any other model has no progress tracking.
///   * #7685 `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`, only when `memory_reachable`:
///     auto memory is the FALLBACK, so with trusty-memory down a session that
///     also lost auto memory would have no memory at all (owner ruling
///     2026-09-12).
///   * DOC-34 / #2246 `CLAUDE_CONFIG_DIR`: points the session at the tm-owned
///     config home, which relocates the `user` settings tier the bundled agent
///     roster lives in (#4451/#4455) and keys its auth away from `~/.claude`.
///   * #4181 `mcp_env`: the per-project MCP pins (`TRUSTY_MEMORY_PALACE`,
///     `TRUSTY_INDEX`) Claude Code hands on to every stdio MCP server it spawns.
///   * #2246 `CLAUDE_CODE_OAUTH_TOKEN`: bypasses the `CLAUDE_CONFIG_DIR`-keyed
///     Keychain divergence that otherwise produces the managed-session login
///     loop.
///   * #3025/#6668 `gh_env` (`GH_TOKEN`/`GH_USER`/`GH_CONFIG_DIR`): the pinned
///     `gh` identity. Before #8233 these were `export` lines in a mode-0600 temp
///     file the pane sourced and deleted, expressly so the token never appeared
///     in the typed line. `LaunchSpec`'s own file is mode 0600 inside a mode
///     0700 directory and is consumed-and-deleted, so that property is kept by
///     the same means for one carrier instead of two.
///
/// The #6495/#7160 alternate-screen and mouse defaults are deliberately NOT
/// here: they must yield per-variable to a value the pane already exports, which
/// [`LaunchSpec::to_command`] expresses with
/// [`crate::core::alt_screen::apply_default_to_command`] — the exec-path twin of
/// the `${NAME-1}` shell operand. The configured `alternate_screen: true`
/// override (#8405) is not a default and is added by [`ManagedLaunch::base`].
/// What: `(name, value)` pairs, in the order above.
/// Test: `env_set_enables_todo_tools_unconditionally`,
/// `env_set_disables_auto_memory_when_trusty_memory_answers`,
/// `env_set_keeps_auto_memory_when_trusty_memory_is_unreachable`,
/// `env_set_relocates_the_config_dir`, `env_set_carries_a_non_empty_mcp_env`,
/// `env_set_carries_the_oauth_token_when_available`,
/// `env_set_omits_the_oauth_token_when_absent`,
/// `env_set_carries_the_gh_identity`.
pub(super) fn managed_env_set(
    config_dir: Option<&Path>,
    oauth_token: Option<&str>,
    mcp_env: &[(String, String)],
    memory_reachable: bool,
    gh_env: &[(String, String)],
) -> Vec<(String, String)> {
    let mut set: Vec<(String, String)> = vec![("CLAUDE_CODE_ENABLE_TODO_TOOLS".into(), "1".into())];
    if memory_reachable {
        set.push(("CLAUDE_CODE_DISABLE_AUTO_MEMORY".into(), "1".into()));
    }
    if let Some(dir) = config_dir {
        set.push(("CLAUDE_CONFIG_DIR".into(), dir.display().to_string()));
    }
    set.extend(mcp_env.iter().cloned());
    if let Some(token) = oauth_token {
        set.push((
            crate::core::oauth_token::OAUTH_TOKEN_ENV_VAR.to_owned(),
            token.to_owned(),
        ));
    }
    set.extend(gh_env.iter().cloned());
    set
}

/// The launch inputs shared by the spawn, resume and attach paths.
///
/// Why: the three builders need the same nine values, and passing them as one
/// borrowed struct keeps each builder a single testable call rather than a
/// nine-argument function (the shape `claude_code_agents::RelaunchInputs`
/// already established for the two relaunch builders).
/// What: plain borrows, no ownership and no I/O. `claude_bin` is the RESOLVED
/// absolute binary (#1298) and is NOT disclaim-wrapped — the shim is now the
/// process that reads this spec, so it spawns `claude` directly (#2997).
/// Test: used by every test in `managed_launch_tests.rs`.
pub(super) struct ManagedLaunch<'a> {
    pub cwd: &'a Path,
    pub claude_bin: &'a str,
    pub config_dir: Option<&'a Path>,
    pub session_id: &'a str,
    pub prompt_file: Option<&'a Path>,
    pub oauth_token: Option<&'a str>,
    pub gh_env: &'a [(String, String)],
    pub mcp_env: &'a [(String, String)],
    /// #8233: the composed session-MCP file this launch actually PROVISIONED,
    /// carried rather than re-derived so the `--mcp-config` token can never name
    /// a path under a different home than the one written.
    pub mcp_config: Option<&'a Path>,
    /// #7685: whether trusty-memory answered at launch.
    pub memory_reachable: bool,
    /// #8405: config `tmux.alternate_screen`, read by the caller at launch.
    pub alternate_screen: bool,
}

impl ManagedLaunch<'_> {
    /// The spec shared by every path, before the path-specific argv is added.
    ///
    /// Why: cwd, program and environment are identical for spawn, resume and
    /// attach — only the flags differ — so composing them once is what stops the
    /// three paths drifting the way the four shell builders did.
    /// What: [`managed_env_unset`] + [`managed_env_set`] with an empty argv,
    /// then [`crate::core::alt_screen::configured_env`] for
    /// `alternate_screen` (#8405).
    /// Test: `attach_and_resume_share_an_identical_environment`,
    /// `every_launch_path_carries_the_configured_fullscreen_renderer`,
    /// `no_launch_path_assigns_the_renderer_when_alternate_screen_is_off`.
    fn base(&self) -> LaunchSpec {
        let mut env_set = managed_env_set(
            self.config_dir,
            self.oauth_token,
            self.mcp_env,
            self.memory_reachable,
            self.gh_env,
        );
        // #8405: an explicit assignment, so the tmux server's inherited value
        // cannot decide the renderer the config asked for.
        env_set.extend(crate::core::alt_screen::configured_env(
            self.alternate_screen,
        ));
        LaunchSpec {
            session_id: self.session_id.to_owned(),
            // #8233 review round 2 (finding 3): one id per LAUNCH, minted here
            // so every builder gets a fresh one and no two launches of the same
            // session can share a sentinel.
            launch_id: uuid::Uuid::new_v4().simple().to_string(),
            cwd: self.cwd.to_path_buf(),
            program: self.claude_bin.to_owned(),
            args: Vec::new(),
            env_unset: managed_env_unset(self.gh_env),
            env_set,
        }
    }

    /// The isolation argv every non-`attach` launch carries.
    ///
    /// Why: the tokens and their order are exactly what the shell line emitted,
    /// read from the SAME production constants rather than restated, so a change
    /// to either flag reaches this path automatically.
    /// What: `--append-system-prompt-file <path>` when a prompt was written
    /// (#2125/#2230), then [`crate::core::model_inject::setting_sources_flag`]'s
    /// choice for `config_dir` (#4451 — `user` is the tier
    /// `CLAUDE_CONFIG_DIR/agents` relocates into), then the additive
    /// `--mcp-config` (#7892; no `--strict-mcp-config`, so the operator's
    /// user-scope servers load beside tm's builtins), then
    /// [`crate::core::model_inject::PERMISSION_MODE_FLAG`] (#1269).
    ///
    /// Every token is pushed UNQUOTED: these reach `execv` with no shell in
    /// between, so a `shell_single_quote`d path would name a file `claude`
    /// cannot open — the same reason `compose_inplace_args` leaves its tokens
    /// bare.
    /// Test: `spawn_argv_matches_the_shell_line_it_replaces`,
    /// `spawn_argv_quotes_nothing`.
    fn isolation_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(prompt) = self.prompt_file {
            args.push("--append-system-prompt-file".to_owned());
            args.push(prompt.display().to_string());
        }
        args.extend(
            crate::core::model_inject::setting_sources_flag(self.config_dir)
                .split_whitespace()
                .map(str::to_owned),
        );
        // #8233: the provisioner's own answer, not a second home-derived one.
        args.extend(crate::core::session_mcp_scope::mcp_config_argv(
            self.mcp_config,
        ));
        args.extend(
            crate::core::model_inject::PERMISSION_MODE_FLAG
                .split_whitespace()
                .map(str::to_owned),
        );
        args
    }
}

/// Build the spec for a fresh managed spawn (replaces `spawn_command`).
///
/// Why: the daemon's default on-ramp. See [`ManagedLaunch::isolation_args`] for
/// why each flag is there.
/// What: [`ManagedLaunch::base`] plus the isolation argv.
/// Test: `spawn_argv_matches_the_shell_line_it_replaces`,
/// `spawn_spec_roots_the_child_at_the_workspace`.
pub(super) fn spawn_spec(launch: &ManagedLaunch<'_>) -> LaunchSpec {
    let mut spec = launch.base();
    spec.args = launch.isolation_args();
    spec
}

/// Build the spec for a resume (replaces `resume_command`).
///
/// Why: a resume restores the prior conversation when an id survived the
/// staleness check; there is no `--continue` fallback (#6765), because a bare
/// `--continue` resolves "most recent" against a managed store this process did
/// not choose.
/// What: [`spawn_spec`]'s argv, then `--resume <id>` when `claude_session_id` is
/// `Some`; otherwise the fresh-launch argv unchanged.
/// Test: `resume_spec_appends_the_resume_flag`,
/// `resume_spec_without_an_id_is_a_plain_spawn`.
pub(super) fn resume_spec(
    launch: &ManagedLaunch<'_>,
    claude_session_id: Option<&str>,
) -> LaunchSpec {
    let mut spec = spawn_spec(launch);
    if let Some(id) = claude_session_id {
        spec.args.push("--resume".to_owned());
        spec.args.push(id.to_owned());
    }
    spec
}

/// Build the spec for `claude attach <short-id>` (replaces `attach_command`).
///
/// Why: a session Claude Code is still running as a background job refuses
/// `--resume` and exits 0; `attach` re-enters it with the conversation intact
/// (#6863). It must otherwise look exactly like a resume — same cwd, same
/// environment, same disclaim shim — or the attached session loses whatever the
/// differing piece carried.
/// What: [`ManagedLaunch::base`] with `attach <attach_id>` as the whole argv.
/// The isolation flags are deliberately absent: `claude attach <id>` accepts no
/// options, so passing them would make `claude` reject the invocation, and the
/// system prompt they would carry is already part of the conversation being
/// re-entered.
/// Test: `attach_spec_omits_flags_attach_cannot_take`,
/// `attach_and_resume_share_an_identical_environment`.
pub(super) fn attach_spec(launch: &ManagedLaunch<'_>, attach_id: &str) -> LaunchSpec {
    let mut spec = launch.base();
    spec.args = vec!["attach".to_owned(), attach_id.to_owned()];
    spec
}

/// The whole line typed into the pane for a managed launch (#8233).
///
/// Why: this is the string whose length caused the bug, so its shape is the
/// fix's central claim. It has exactly two variable parts — a 36-character UUID
/// and two tm-owned paths — and grows with NONE of the launch's parameters.
/// What: `export TM_MANAGED_SESSION_ID='<uuid>'; '<wrapper>'
/// internal-spawn-disclaimed --launch-spec '<spec>'`.
///
/// The `export` is kept on the pane line deliberately (#2023 component B): it
/// lands in the pane's own shell, so it survives `claude` exiting and lets a
/// bare `tm` run in that pane afterwards identify the session. It is
/// fixed-length — a UUID — so it costs the line nothing that grows. The spawned
/// `claude` gets the same variable from [`LaunchSpec::to_command`], not from
/// this export.
///
/// Both paths are single-quoted for the same reason every path in this crate's
/// pane commands is: a home directory containing a space would otherwise
/// word-split and silently break the launch.
/// Test: `pane_line_is_short_for_the_worst_case_launch`,
/// `pane_line_exports_the_managed_session_id`,
/// `pane_line_routes_through_the_disclaim_shim`.
pub(super) fn pane_line(session_id: &str, wrapper_bin: &str, spec_path: &Path) -> String {
    let quote = crate::core::spawn_disclaim::pane::shell_single_quote;
    format!(
        "export {}={}; {} {} --launch-spec {}",
        crate::core::harness_root::MANAGED_SESSION_ID_ENV,
        quote(session_id),
        quote(wrapper_bin),
        crate::core::spawn_disclaim::PANE_DISCLAIM_SUBCOMMAND,
        quote(&spec_path.display().to_string()),
    )
}

/// The short line typed into the pane when a launch is ABANDONED (#8233).
///
/// Why: a launch that cannot run must not leave the operator staring at a pane
/// that simply never started anything. Every abort path — the spec could not be
/// written, it could not be read back, the tm binary could not be resolved, the
/// command line was refused as over-length — types this before the error
/// propagates, and the caller marks the session record errored.
/// What: a single `echo` whose length is fixed by construction: a static
/// sentence plus a 36-character UUID. It must stay far under
/// [`crate::core::tmux::MAX_PANE_COMMAND_BYTES`], because the case it exists for
/// includes "the previous line was too long to type".
/// Test: `abort_notice_is_short_enough_to_always_type`,
/// `deliver_announces_a_refused_line_in_the_pane`.
fn abort_notice(session_id: &str) -> String {
    format!(
        "echo 'tm: managed launch aborted (#8233) — nothing was started; \
         run: tm sessions show {session_id}'"
    )
}

/// Write the spec, prove it readable, and type the pane's launch line.
///
/// Why: this is the fail-closed choke point. Each step can fail, and every
/// failure has to end the same way — nothing half-launched, a loud line in the
/// pane, and a `RuntimeError` the caller turns into `mark_errored`. Keeping the
/// four steps in one function is what stops a future caller performing three of
/// them.
/// What: resolves the tm binary ([`crate::core::spawn_disclaim::launch_wrapper_bin`]);
/// CONFIRMS the pane's shell is executing what it is typed
/// ([`super::pane_handshake::confirm_prompt`]) before anything else happens;
/// writes the spec to its mode-0600 file; reads it back ([`LaunchSpec::verify`]);
/// publishes this launch's id ([`LaunchSpec::write_launch_pointer_in`]); types
/// [`pane_line`] through
/// [`crate::session_manager::ManagedTmuxDriver::send_command_line`], which
/// refuses an over-length line rather than letting the tty truncate it. The
/// pane's shim then writes the sentinel, which
/// `daemon::managed_routes::launch_verify::launch_was_delivered` reads — that is
/// the half of the handshake that proves the SHELL, not just tmux, took the
/// line. On any failure it types [`abort_notice`] (best-effort — a pane that
/// cannot be typed into cannot be told either), removes the spec it wrote so no
/// credential is left on disk, and returns the error.
/// Test: `deliver_types_a_short_line_and_leaves_a_readable_spec`,
/// `deliver_announces_a_refused_line_in_the_pane`,
/// `deliver_errors_and_cleans_up_when_the_spec_dir_is_unwritable`,
/// `deliver_resets_the_pane_before_typing` — the wedged-pane handshake, and
/// that it precedes the keystrokes.
///
/// // #8233: the `spec_dir` is an ARGUMENT. This used to be a `deliver()`
/// wrapper resolving `LaunchSpec::root()` from the process home, which put a
/// file carrying `CLAUDE_CODE_OAUTH_TOKEN` and `GH_TOKEN` into the operator's
/// own config home on every test-driven launch. The caller
/// (`ClaudeCodeAdapter::spec_dir`) derives it from the framework root the
/// daemon holds, so the home-unresolvable arm the wrapper carried is gone.
pub(super) fn deliver_in(
    tmux: &dyn crate::session_manager::ManagedTmuxDriver,
    tmux_name: &str,
    pane_id: Option<&str>,
    spec: &LaunchSpec,
    spec_dir: &Path,
) -> Result<(), super::RuntimeError> {
    match deliver_inner(tmux, tmux_name, pane_id, spec, spec_dir) {
        Ok(()) => Ok(()),
        Err(err) => {
            // Best-effort: this notice is short by construction, so the only way
            // it fails is a pane that is gone — in which case the error below is
            // still what the record records.
            let _ = tmux.send_command_line(tmux_name, pane_id, &abort_notice(&spec.session_id));
            Err(err)
        }
    }
}

/// The steps [`deliver`] wraps; see its doc.
fn deliver_inner(
    tmux: &dyn crate::session_manager::ManagedTmuxDriver,
    tmux_name: &str,
    pane_id: Option<&str>,
    spec: &LaunchSpec,
    spec_dir: &Path,
) -> Result<(), super::RuntimeError> {
    let wrapper = crate::core::spawn_disclaim::launch_wrapper_bin().ok_or_else(|| {
        super::RuntimeError::Spawn(
            "could not resolve this `tm` binary's own path, so the pane has nothing to \
             read the launch spec with (#8233)"
                .to_owned(),
        )
    })?;
    // #8233 review round 2 (finding 1): confirm — do not assume — that the
    // shell is executing what it is typed, BEFORE anything is written to disk
    // or typed at the pane. A wedged or not-yet-reading shell refuses the
    // launch here, so no spec carrying credentials is ever left behind for a
    // line the pane was never going to run.
    let state = super::pane_handshake::confirm_prompt(
        tmux,
        tmux_name,
        pane_id,
        super::pane_handshake::PROMPT_PROBE_ROUNDS,
        super::pane_handshake::PROMPT_PROBE_ATTEMPTS,
        super::pane_handshake::PROMPT_PROBE_INTERVAL,
    );
    if !state.may_launch() {
        let reason = state
            .refusal()
            .unwrap_or_else(|| "the pane is not ready for a launch (#8233)".to_owned());
        return Err(super::RuntimeError::Spawn(format!(
            "refusing to launch into pane '{tmux_name}': {reason}"
        )));
    }
    let path = spec
        .write_in(spec_dir)
        .map_err(|e| super::RuntimeError::Spawn(e.to_string()))?;
    if let Err(e) = LaunchSpec::verify(&path) {
        let _ = std::fs::remove_file(&path);
        return Err(super::RuntimeError::Spawn(e.to_string()));
    }
    // #8233 review round 2 (finding 3): publish WHICH launch the checker should
    // wait on. The sentinel is keyed on this launch's own id, so a marker left
    // by an earlier launch of the same session cannot satisfy this one. Fatal:
    // without the pointer the launch would be unverifiable, and an unverifiable
    // launch is exactly what this issue is about.
    if let Err(e) = spec.write_launch_pointer_in(spec_dir) {
        let _ = std::fs::remove_file(&path);
        return Err(super::RuntimeError::Spawn(e.to_string()));
    }
    let line = pane_line(&spec.session_id, &wrapper, &path);
    if let Err(e) = tmux.send_command_line(tmux_name, pane_id, &line) {
        // The spec carries credentials and nothing will consume it now.
        let _ = std::fs::remove_file(&path);
        return Err(super::RuntimeError::TmuxUnavailable(e.to_string()));
    }
    Ok(())
}

#[cfg(test)]
#[path = "managed_launch_tests.rs"]
mod tests;
