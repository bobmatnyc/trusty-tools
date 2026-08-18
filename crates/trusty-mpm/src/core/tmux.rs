//! Thin adapter over `trusty_common::tmux` + the crate's single
//! tmux-spawning entry point (#2398 architecture consolidation, #3004
//! shared-layer extraction).
//!
//! Why: trusty-mpm hosts each Claude Code session inside a named tmux session
//! (the primary control model — see `docs/research/session-control-models.md`).
//! `TmuxTarget`/`TmuxCommand`/`tmux_argv` used to live here, but issue #3004
//! extracted them (plus `scrollback_option_commands` and the
//! options-before-`new-session` ordering guarantee) into `trusty_common::tmux`
//! once a scrollback bug report against trusty-agents' INDEPENDENT tmux
//! implementation revealed the real problem: #2398/#2399 could only fix the
//! crate they lived in. This module re-exports the shared pure layer
//! unchanged (every existing call site in this crate keeps compiling against
//! `crate::core::tmux::{TmuxTarget, TmuxCommand, tmux_argv, ...}`) and keeps
//! everything that is genuinely trusty-mpm-specific: binary resolution
//! glue, TCC-disclaimed process spawning (issue #2819/#2820 — NOT moved to
//! the shared layer, since it is a macOS/trusty-mpm-signing concern, not a
//! tmux concern), and `create_managed_session`, which layers this crate's
//! own `TrustyToolsConfig` resolution on top of the shared
//! `managed_session_commands` recipe.
//!
//! Originally this module was argv-construction only, with each of the
//! daemon (`daemon::tmux::TmuxDriver`), the CLI (`bin/tm`'s launch/connect/
//! session-start commands), the TUI client (`client::http_client`), and the
//! control-plane backend (`control::backend::tmux`) independently spawning
//! `std::process::Command::new("tmux")`. That let FIVE call sites bypass the
//! #2398 scrollback/mouse ergonomics entirely (a QA-caught regression) simply
//! by not knowing about `daemon::tmux::TmuxDriver::create_session`. Per Bob's
//! architecture directive, this module now ALSO owns the actual process
//! spawning for output-capturing tmux commands
//! ([`run_tmux_with_bin`]/[`run_tmux`]) and the single choke point for
//! creating a session with ergonomics applied first
//! ([`create_managed_session`]) — every session-creating call site in the
//! crate routes through it, so nothing can bypass the scrollback/mouse
//! options again. [`daemon::tmux::TmuxDriver`](crate::daemon::tmux::TmuxDriver)
//! wraps [`run_tmux_with_bin`] rather than spawning independently.
//!
//! Scope note (#2414, closing the #2398 follow-up): the four read-only probes
//! that used to shell out directly — `tmux display-message -p '#S'`
//! (current-session-name lookups in `bin/tm/commands/tmux_attach.rs` and
//! `bin/tm/commands/statusline/branch.rs`), `tmux show-environment`
//! (`bin/tm/commands/guided_inplace.rs`), and
//! `tmux display-message -t <s> -p '#{pane_pid}'`
//! (`core::process::tmux_pane_pid`) — now route through
//! [`display_message_argv`]/[`show_environment_argv`] +
//! [`run_tmux_argv_with_bin`]/[`run_tmux_argv`]. Those are a SEPARATE pair of
//! entry points from [`run_tmux_with_bin`]/[`run_tmux`] because
//! `trusty_common::tmux::TmuxCommand` does not model `display-message`/
//! `show-environment`, and extending that shared enum was out of scope for
//! #2414 (a concurrent branch owns `trusty-common/`); they still fold into the
//! SAME [`run_tmux_argv_with_bin`] → [`crate::core::spawn_disclaim::disclaimed_output`]
//! spawn primitive [`run_tmux_with_bin`] itself calls, so binary resolution and
//! TCC-disclaim wrapping stay unified across every tmux call in the crate even
//! though argv construction for these two sub-commands is local. This mirrors
//! `statusline::branch::git_branch`'s existing pattern of calling
//! `disclaimed_output` directly for a probe with no typed command wrapper.
//! `bin/tm/commands/tmux_attach.rs::tmux_attach` (the actual `attach-session`/
//! `switch-client` spawn) DOES route its binary resolution through
//! [`resolve_tmux_binary_or_bare`] even though its interactive,
//! stdio-inheriting `.status()` call shape does not fit [`run_tmux`]'s
//! output-capturing signature.
//! What: re-exports `TmuxTarget`/`TmuxCommand`/`tmux_argv`/
//! `scrollback_option_commands`/the scrollback defaults from
//! `trusty_common::tmux`; [`resolve_tmux_binary`]/[`resolve_tmux_binary_or_bare`]
//! (binary resolution), [`run_tmux_with_bin`]/[`run_tmux`] (the shared spawn
//! primitive for typed `TmuxCommand`s), [`display_message_argv`]/
//! [`show_environment_argv`]/[`run_tmux_argv_with_bin`]/[`run_tmux_argv`] (the
//! same spawn primitive for the two untyped sub-commands above), and
//! [`create_managed_session`] (the session-creation choke point with
//! ergonomics baked in, config-resolved locally).
//! Test: `cargo test -p trusty-mpm-core` covers the mpm-specific spawn/config
//! glue; argv construction and the options-before-new-session ordering
//! guarantee are tested once in `trusty_common::tmux` and reused here without
//! re-testing (`managed_session_command_sequence_matches_shared_layer`
//! asserts THIS crate's config-resolved call routes through it correctly).

use tracing::warn;

pub use trusty_common::tmux::{
    ALTERNATE_SCREEN_OPTION, DEFAULT_TMUX_ALTERNATE_SCREEN, DEFAULT_TMUX_HISTORY_LIMIT,
    DEFAULT_TMUX_MOUSE, HISTORY_LIMIT_OPTION, MOUSE_OPTION, PANE_LIST_FORMAT, SESSION_LIST_FORMAT,
    TmuxCommand, TmuxTarget, WINDOW_LIST_FORMAT, managed_session_commands,
    scrollback_option_commands, tmux_argv,
};

/// Resolve the `tmux` binary, preferring live `PATH` and falling back to
/// well-known daemon dirs (Homebrew, user bins) via `trusty_common::bin_resolve`
/// (#2398 architecture consolidation).
///
/// Why: a daemon process under launchd inherits a minimal `PATH` (#1298);
/// resolving through the SAME helper for every caller (daemon, CLI, TUI
/// client) — rather than a bare `"tmux"` PATH lookup for some callers and a
/// well-known-dirs-aware lookup for others — means resolution behavior is
/// identical everywhere, and there is exactly one place to fix if it ever
/// needs to change.
/// What: delegates to `trusty_common::bin_resolve::resolve_binary("tmux")`.
/// Test: `resolve_tmux_binary_does_not_panic`.
pub fn resolve_tmux_binary() -> Option<std::path::PathBuf> {
    trusty_common::bin_resolve::resolve_binary("tmux")
}

/// [`resolve_tmux_binary`], falling back to the literal `"tmux"` (a plain
/// `PATH` lookup at spawn time) when resolution itself comes up empty.
///
/// Why: callers that do not need [`daemon::tmux::TmuxDriver::discover`](crate::daemon::discover)'s
/// (crate::daemon::tmux) fail-fast-if-truly-absent contract — the CLI and TUI
/// client, which already surface their own "tmux command failed" error on the
/// subsequent spawn — just want a best-effort binary name to run. Falling
/// back to the bare string preserves their pre-#2398 behavior (trust the
/// interactive shell's own `PATH`) as the worst case.
/// What: returns the resolved absolute path as a `String` when found, else
/// `"tmux"`.
/// Test: `resolve_tmux_binary_or_bare_never_empty`.
pub fn resolve_tmux_binary_or_bare() -> String {
    resolve_tmux_binary()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "tmux".to_string())
}

/// Run a typed tmux command against an EXPLICITLY resolved binary path,
/// returning the raw process `Output` (#2398 architecture consolidation).
///
/// Why: this is the ONE place in the crate that actually spawns an
/// output-capturing `tmux` child process — every caller
/// ([`daemon::tmux::TmuxDriver`](crate::daemon::tmux::TmuxDriver), `bin/tm`'s
/// CLI commands, `client::http_client`'s TUI-driving methods, and
/// `control::backend::tmux::TmuxBackend`) routes through here so argv
/// construction and process-spawning can never drift or be re-implemented ad
/// hoc at a call site — the exact drift that let 5 CLI/client call sites
/// bypass the scrollback/mouse ergonomics entirely (the QA finding this
/// consolidation closes). Takes an explicit `tmux_bin` (rather than
/// re-resolving on every call) so callers that already cache a resolved path
/// — like `TmuxDriver`, which resolves once in `discover()` for its
/// fail-fast-if-absent contract — do not pay repeated resolution cost.
/// What: renders `cmd` via [`tmux_argv`] and spawns `tmux` through
/// [`crate::core::spawn_disclaim::disclaimed_output`], which on macOS disclaims
/// TCC responsibility so the forked tmux server (and every `claude`/agent
/// descendant) is its own responsible process rather than rolling attribution up
/// to the signed `trusty-mpm` binary (issue #2819). On non-macOS it is a plain
/// `Command::output`. Every session-creating call site routes through here, so
/// the disclaim covers the one spawn that forks the shared server regardless of
/// which tmux sub-command triggers the fork. This TCC-disclaim wrapping is WHY
/// process spawning stayed in trusty-mpm rather than moving to
/// `trusty_common::tmux` in #3004 — it is a trusty-mpm-signing-identity
/// concern, not a tmux-argv concern, and trusty-agents does not need it (no
/// equivalent signed-binary/TCC-prompt problem reported for it). Callers own
/// interpreting the exit status / stderr — this function does not classify
/// failures.
/// Test: `disclaimed_output_captures_stdout` (and the sibling
/// `disclaimed_output_*` tests) in `crate::core::spawn_disclaim` cover the
/// disclaim/capture behaviour; this thin adapter is exercised transitively by
/// every call site (a live `tmux` binary is required to observe real output —
/// see the `#[ignore]` integration tests).
pub fn run_tmux_with_bin(
    tmux_bin: &str,
    cmd: &TmuxCommand,
) -> std::io::Result<std::process::Output> {
    host_state_guard()?;
    crate::core::spawn_disclaim::disclaimed_output(tmux_bin, &tmux_argv(cmd))
}

/// Refuse a tmux spawn when this process is not running in the operator's
/// real environment (#5784).
///
/// Why: a tmux server is keyed to the OS user, not to `$HOME`, so a daemon or
/// CLI under a throwaway `$HOME` still reaches the operator's live sessions.
/// [`crate::daemon::tmux::TmuxDriver::discover`] gates every path that holds a
/// driver, but `tm launch`/`tm connect` CREATE and KILL sessions through
/// [`create_managed_session`] and [`run_tmux`] without ever constructing one —
/// so the check also belongs at the spawn itself, which this module already
/// owns as the crate's single tmux-spawning point.
/// What: `Ok(())` when [`crate::core::host_state_gate::host_state_access`]
/// allows; otherwise an `io::Error` of kind `PermissionDenied` carrying the
/// reason, so a caller that only prints the error still tells its operator
/// which variable lifts the gate. Logged at `debug!` rather than `warn!`
/// because one operation issues many spawns — the loud line belongs at the
/// entry point ([`create_managed_session`], `TmuxDriver::discover`,
/// `discovery::discover_all`, the daemon's startup banner).
/// Test: `scratch_home_daemon_does_not_spawn_tmux` in
/// `tests/scratch_home_tmux_gate.rs` drives both a `create_managed_session`
/// and a `run_tmux(KillSession)` through this guard and asserts each is
/// refused with `PermissionDenied` and no subprocess.
fn host_state_guard() -> std::io::Result<()> {
    match crate::core::host_state_gate::host_state_access().skip_reason() {
        None => Ok(()),
        Some(reason) => {
            tracing::debug!("#5784: tmux spawn refused — {reason}");
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("tmux spawn refused: {reason}"),
            ))
        }
    }
}

/// [`run_tmux_with_bin`], resolving the tmux binary via
/// [`resolve_tmux_binary_or_bare`] first.
///
/// Why: the common-case convenience for callers (CLI, TUI client) that do not
/// already hold a cached, resolved tmux path.
/// What: resolves then delegates to [`run_tmux_with_bin`].
/// Test: exercised transitively; see [`run_tmux_with_bin`].
pub fn run_tmux(cmd: &TmuxCommand) -> std::io::Result<std::process::Output> {
    run_tmux_with_bin(&resolve_tmux_binary_or_bare(), cmd)
}

/// Render `tmux display-message [-t <target>] -p <format>` argv (#2414).
///
/// Why: `trusty_common::tmux::TmuxCommand` has no `display-message` variant
/// (extending that shared enum is out of scope here — see this module's scope
/// note), so the four read-only probes this issue migrates need a local,
/// pure argv builder to route through, exactly like [`tmux_argv`] does for
/// the typed commands.
/// What: `target: None` renders untargeted (`display-message -p <format>`,
/// which tmux resolves against the invoking CLIENT, not any particular
/// session — the "current client" fallback [`current_tmux_session_name`](
/// crate) and [`tmux_session_name`](crate) both rely on); `Some(target)`
/// renders `-t <target.as_target()> -p <format>`, addressing exactly the
/// session or bare pane id [`TmuxTarget::as_target`] renders (never a
/// `"session:%pane"` compound, which tmux parses as a window spec, not a
/// pane spec).
/// Test: `display_message_argv_untargeted`, `display_message_argv_session_targeted`.
pub fn display_message_argv(target: Option<&TmuxTarget>, format: &str) -> Vec<String> {
    let mut argv = vec!["display-message".to_string()];
    if let Some(t) = target {
        argv.push("-t".to_string());
        argv.push(t.as_target());
    }
    argv.push("-p".to_string());
    argv.push(format.to_string());
    argv
}

/// Render `tmux show-environment [-t <name>] <key>` argv (#2414).
///
/// Why: the [`display_message_argv`] counterpart for
/// `bin/tm/commands/guided_inplace.rs::read_tmux_env_managed_session_id`,
/// which reads a variable's durably-published value from the SESSION
/// environment table (never a per-pane `export`) — see
/// [`TmuxCommand::SetEnvironment`]'s doc for why that table exists.
/// What: `name: None` renders untargeted (`show-environment <key>`, querying
/// the CURRENT session — the only shape this issue's call site uses, since it
/// only ever runs from inside the tmux client whose own session it wants);
/// `Some(name)` renders `-t <name> <key>` for a caller that needs an
/// explicit session.
/// Test: `show_environment_argv_untargeted`, `show_environment_argv_session_targeted`.
pub fn show_environment_argv(name: Option<&str>, key: &str) -> Vec<String> {
    let mut argv = vec!["show-environment".to_string()];
    if let Some(n) = name {
        argv.push("-t".to_string());
        argv.push(n.to_string());
    }
    argv.push(key.to_string());
    argv
}

/// Run raw tmux argv against an EXPLICITLY resolved binary path, through the
/// SAME spawn primitive [`run_tmux_with_bin`] uses (#2414).
///
/// Why: [`display_message_argv`]/[`show_environment_argv`] cover sub-commands
/// `trusty_common::tmux::TmuxCommand` does not model, so they cannot go
/// through [`run_tmux_with_bin`]'s `&TmuxCommand` signature — but the actual
/// spawn (binary resolution already done by the caller, TCC-disclaim
/// wrapping) must still be the ONE place that happens, or these four probes
/// would just re-introduce the ad hoc `Command::new("tmux")` drift #2414
/// exists to close, one layer down. Delegating straight to
/// [`crate::core::spawn_disclaim::disclaimed_output`] — the exact primitive
/// [`run_tmux_with_bin`] itself calls — keeps that one place shared.
/// What: spawns `tmux_bin` with `args` via `disclaimed_output`. Callers own
/// interpreting the exit status / stderr, exactly like [`run_tmux_with_bin`].
/// Test: exercised transitively by every migrated call site (a live `tmux`
/// binary is required to observe real output).
pub fn run_tmux_argv_with_bin(
    tmux_bin: &str,
    args: &[String],
) -> std::io::Result<std::process::Output> {
    host_state_guard()?;
    crate::core::spawn_disclaim::disclaimed_output(tmux_bin, args)
}

/// [`run_tmux_argv_with_bin`], resolving the tmux binary via
/// [`resolve_tmux_binary_or_bare`] first (#2414).
///
/// Why: the common-case convenience mirroring [`run_tmux`] for the untyped
/// argv path.
/// What: resolves then delegates to [`run_tmux_argv_with_bin`].
/// Test: exercised transitively; see [`run_tmux_argv_with_bin`].
pub fn run_tmux_argv(args: &[String]) -> std::io::Result<std::process::Output> {
    run_tmux_argv_with_bin(&resolve_tmux_binary_or_bare(), args)
}

/// Build the exact ordered [`TmuxCommand`] sequence [`create_managed_session`]
/// issues, given already-resolved tmux options (#3004).
///
/// Why: extracted as a pure function (no process spawning, no config
/// loading) purely so a hermetic unit test can assert THIS crate's
/// session-creation call routes through `trusty_common::tmux`'s shared
/// ordering guarantee with the right, config-resolved parameters — the
/// per-consumer "integration assertion" issue #3004 asks for, without
/// re-testing the ordering guarantee itself (already covered once in
/// `trusty_common::tmux`'s own tests).
/// What: delegates straight to
/// [`trusty_common::tmux::managed_session_commands`] with `idempotent: true`
/// (trusty-mpm's `-A`-attach creation semantics) and no initial command.
/// Test: `managed_session_command_sequence_matches_shared_layer`.
fn managed_session_command_sequence(
    name: &str,
    workdir: Option<&str>,
    history_limit: u32,
    mouse: bool,
    alternate_screen: bool,
) -> Vec<TmuxCommand> {
    managed_session_commands(
        name,
        workdir,
        history_limit,
        mouse,
        alternate_screen,
        true,
        None,
    )
}

/// Outcome of [`create_managed_session`] (#3386 review finding).
///
/// Why: a `warn!`-and-continue on a failed scrollback-ergonomics
/// verification is exactly the silent-degrade #3386 was originally filed
/// against — only grep-able, never surfaced to whoever asked for the
/// session. Wrapping the raw `new-session` `Output` together with an
/// explicit `options_verified` flag forces every call site to at least
/// LOOK at whether the pane it just got may be capped at tmux's factory
/// 2000-line history-limit, rather than the flag being available only to a
/// caller who happens to read the log.
/// What: `output` is the `new-session` process output — callers classify
/// session-creation success/failure from it exactly as before
/// (`output.status.success()`); `options_verified` is `true` only when an
/// apply-and-verify cycle (server-up, `set-option -g` x2, `show-options`
/// probe) confirmed `history-limit` landed on the server before this
/// `new-session` call ran.
#[derive(Debug)]
pub struct ManagedSessionOutcome {
    /// Raw `tmux new-session` process output.
    pub output: std::process::Output,
    /// `false` when the scrollback/mouse ergonomics could not be CONFIRMED
    /// to have landed before the pane was created — the pane may be capped
    /// at tmux's factory `history-limit` of 2000. Callers must surface this
    /// to the operator rather than silently discarding it (#3386 review).
    pub options_verified: bool,
}

/// Log an operator-visible `error!` naming `session_name` when `outcome`'s
/// scrollback ergonomics were not verified (#3386 review).
///
/// Why: shared by every `tracing`-based call site
/// ([`daemon::tmux::TmuxDriver::create_session`](crate::daemon::tmux::TmuxDriver::create_session),
/// `client::http_client::session_connect`) so the exact message stays in
/// one place and each call site only pays ONE line, keeping
/// `daemon::tmux` (already near its 500-SLOC production cap) from growing
/// just to inline this check. A no-op when `outcome.options_verified` is
/// `true`.
/// What: `tracing::error!` (not `warn!` — a `warn!`-only log is the exact
/// silent-degrade #3386 was filed against) with a `session` field.
/// Test: exercised transitively by `daemon::tmux`'s and
/// `client::http_client::session_connect`'s own tests; the condition itself
/// is covered by `apply_and_verify_scrollback_options_returns_false_after_exhausting_retries`.
pub fn warn_if_options_unverified(outcome: &ManagedSessionOutcome, session_name: &str) {
    if !outcome.options_verified {
        tracing::error!(
            session = %session_name,
            "#3386: tmux scrollback ergonomics could not be verified before this pane was \
             created — it may be capped at tmux's factory history-limit of 2000 instead of \
             the configured value"
        );
    }
}

/// Create a tmux session with the configured scrollback + mouse ergonomics
/// applied FIRST — THE single choke point for "create a managed session"
/// used by every session-creating call site in the crate (#2398 architecture
/// consolidation).
///
/// Why: `history-limit` is captured into a pane's ring buffer AT CREATION
/// TIME, so the scrollback/mouse `set-option -g` calls MUST run before
/// `new-session` — see [`trusty_common::tmux::scrollback_option_commands`].
/// Baking both steps into one function (rather than asking every caller to
/// remember "apply options, then create") is what makes it structurally
/// impossible for a future call site to bypass the ergonomics again, which
/// is exactly how the #2398 QA finding happened the first time (5 call
/// sites independently shelled out to `tmux new-session`, none aware the
/// daemon-side ergonomics existed) — and, per #3004, exactly how
/// trusty-agents' independent tmux implementation missed the fix entirely.
/// What: resolves the tmux binary (`tmux_bin` if given, else
/// [`resolve_tmux_binary_or_bare`]), loads
/// [`crate::core::trusty_tools_config::TrustyToolsConfig`] and resolves the
/// `tmux:` section, then runs [`apply_and_verify_scrollback_options`] (#3386
/// — retries the WHOLE apply-and-verify cycle, not just server-up, since a
/// successful `start-server` does not prove the following `set-option -g`
/// landed) before issuing `new-session`. Never blocks session creation on a
/// degraded outcome — `new-session` always still runs — but the returned
/// [`ManagedSessionOutcome::options_verified`] tells the caller whether it
/// can trust the ergonomics landed.
/// Test: argv construction and the ordering guarantee are covered once in
/// `trusty_common::tmux`'s own tests; this crate's config-resolved call is
/// covered by `managed_session_command_sequence_matches_shared_layer`. The
/// live-process call itself needs a real `tmux` binary, so it is exercised
/// transitively by every migrated call site (daemon `#[ignore]` integration
/// tests; CLI/TUI paths are exercised end-to-end by the existing `tm
/// launch`/`tm connect` integration coverage); the #3386 apply-and-verify
/// behavior itself is covered by `create_managed_session_confirms_server_before_applying_options`
/// and the `apply_and_verify_scrollback_options_*` tests.
pub fn create_managed_session(
    tmux_bin: Option<&str>,
    name: &str,
    workdir: Option<&str>,
) -> std::io::Result<ManagedSessionOutcome> {
    // #5784: refuse once, loudly, at the entry point. `host_state_guard` also
    // guards every spawn below, but this is the operation an operator asked
    // for by name, so it is the one that earns a `warn!` — and refusing here
    // skips the retried apply-and-verify cycle that would otherwise emit a
    // dozen refusals for one `tm launch`.
    if let Some(reason) = crate::core::host_state_gate::host_state_access().skip_reason() {
        warn!("#5784: refusing to create tmux session '{name}' — {reason}");
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("tmux session creation refused: {reason}"),
        ));
    }

    let owned_bin;
    let bin = match tmux_bin {
        Some(b) => b,
        None => {
            owned_bin = resolve_tmux_binary_or_bare();
            &owned_bin
        }
    };

    let config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
    let opts = crate::core::trusty_tools_config::resolve_tmux_options(&config);
    let commands = managed_session_command_sequence(
        name,
        workdir,
        opts.history_limit,
        opts.mouse,
        opts.alternate_screen,
    );
    let split = commands.len().saturating_sub(1);
    let (options, new_session) = commands.split_at(split);

    let options_verified = apply_and_verify_scrollback_options(
        bin,
        options,
        opts.history_limit,
        opts.alternate_screen,
    );
    if !options_verified {
        warn!(
            "#3386: tmux scrollback ergonomics could not be verified after \
             {APPLY_VERIFY_MAX_ATTEMPTS} attempts for session '{name}' — the pane about to be \
             created may be capped at tmux's factory history-limit of 2000; returning \
             options_verified=false for the caller to surface"
        );
    }

    let output = run_tmux_with_bin(
        bin,
        new_session
            .first()
            .expect("managed_session_command_sequence always ends with NewSession"),
    )?;

    Ok(ManagedSessionOutcome {
        output,
        options_verified,
    })
}

/// Number of attempts [`ensure_server_up`] makes before giving up (#3386).
const START_SERVER_MAX_ATTEMPTS: u8 = 3;

/// Delay between [`ensure_server_up`] retry attempts (#3386).
const START_SERVER_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(25);

/// Number of attempts [`apply_and_verify_scrollback_options`] makes before
/// conceding the ergonomics could not be confirmed (#3386 review finding).
const APPLY_VERIFY_MAX_ATTEMPTS: u8 = 3;

/// Delay between [`apply_and_verify_scrollback_options`] retry attempts.
const APPLY_VERIFY_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(25);

/// Confirm the tmux SERVER exists before any `set-option -g`/`new-session`
/// call relies on it (#3386).
///
/// Why: see [`TmuxCommand::StartServer`]'s doc for the full root-cause
/// explanation — `set-option -g` does NOT auto-start the server the way
/// `new-session` does, so a caller must confirm the server is actually up
/// first rather than assuming the first tmux command of the sequence takes
/// care of it.
/// What: runs `tmux start-server` (idempotent against an already-running
/// server) up to [`START_SERVER_MAX_ATTEMPTS`] times, waiting
/// [`START_SERVER_RETRY_DELAY`] between attempts, returning `Ok(())` on the
/// first success. Exhausting every attempt returns an `Err` describing the
/// last failure — this is the "error loudly rather than proceed silently"
/// signal callers must not swallow into an unconditional success path.
/// Test: `ensure_server_up_retries_then_succeeds`,
/// `ensure_server_up_fails_loudly_after_exhausting_retries` (both drive a
/// scripted fake `tmux` binary — no live tmux server required).
///
/// Visibility (#3823): `pub(crate)` (not private) so
/// [`crate::daemon::tmux::TmuxDriver`] can call it directly to guarantee the
/// server exists before the FIRST tmux call of the session-creation/resume
/// flow (`list-sessions`, issued by name-collision checks well before
/// [`create_managed_session`] is ever reached) — not just before this
/// module's own `set-option -g`/`new-session` sequence.
pub(crate) fn ensure_server_up(bin: &str) -> Result<(), String> {
    let mut last_err = String::new();
    for attempt in 1..=START_SERVER_MAX_ATTEMPTS {
        match run_tmux_with_bin(bin, &TmuxCommand::StartServer) {
            Ok(output) if output.status.success() => return Ok(()),
            Ok(output) => last_err = String::from_utf8_lossy(&output.stderr).into_owned(),
            Err(e) => last_err = e.to_string(),
        }
        if attempt < START_SERVER_MAX_ATTEMPTS {
            std::thread::sleep(START_SERVER_RETRY_DELAY);
        }
    }
    Err(last_err)
}

/// Run [`ensure_server_up`], apply every entry of `options`, then confirm
/// via [`probe_history_limit`] and [`probe_alternate_screen`] — retrying the
/// WHOLE cycle up to
/// [`APPLY_VERIFY_MAX_ATTEMPTS`] times (#3386 review finding): a successful
/// `start-server` does NOT by itself prove the FOLLOWING `set-option -g`
/// landed (the server could still be torn down between the two calls, or
/// the value could be clamped/rejected by a tmux version quirk), so the
/// verification probe — not just the server-up check — must gate whether
/// another attempt is worth making.
///
/// Why: [`create_managed_session`] must never silently proceed to
/// `new-session` believing the ergonomics landed when they did not — the
/// original #3386 bug was exactly a `warn!`-and-continue on a single failed
/// attempt.
/// What: returns `true` as soon as one cycle's probes confirm BOTH
/// `history-limit == expected_history_limit` (global scope) and
/// `alternate-screen == expected_alternate_screen` (window scope, #5151);
/// returns `false` once [`APPLY_VERIFY_MAX_ATTEMPTS`] cycles are exhausted
/// without a confirmed match on both. Every failure within a cycle is logged
/// via `warn!`; never panics, never blocks the caller from proceeding to
/// `new-session` either way — the return value is the caller's signal to
/// surface the degraded outcome, not a reason to abort session creation.
///
/// Each probe reads the SAME tmux option scope its `set-option` wrote —
/// `-g` for `history-limit`, `-wg` for `alternate-screen`. A probe that reads
/// a different scope than it wrote would report success for a `set-option`
/// that silently did nothing (measured: `set-option -pg alternate-screen off`
/// exits 0 and leaves the pane still on the alternate screen), which is the
/// same fail-open shape #3386 was filed against.
/// Test: `apply_and_verify_scrollback_options_succeeds_on_second_attempt`,
/// `apply_and_verify_scrollback_options_returns_false_after_exhausting_retries`,
/// `apply_and_verify_scrollback_options_fails_when_only_alternate_screen_is_wrong`,
/// `apply_and_verify_probes_the_same_scope_it_sets` (all drive a scripted
/// fake `tmux` binary — no live tmux server required).
fn apply_and_verify_scrollback_options(
    bin: &str,
    options: &[TmuxCommand],
    expected_history_limit: u32,
    expected_alternate_screen: bool,
) -> bool {
    for attempt in 1..=APPLY_VERIFY_MAX_ATTEMPTS {
        if let Err(e) = ensure_server_up(bin) {
            warn!(
                "#3386: tmux start-server failed on apply-and-verify attempt \
                 {attempt}/{APPLY_VERIFY_MAX_ATTEMPTS}: {e}"
            );
        } else {
            for cmd in options {
                match run_tmux_with_bin(bin, cmd) {
                    Ok(output) if !output.status.success() => {
                        warn!(
                            "#3386: tmux {cmd:?} exited non-zero on apply-and-verify attempt \
                             {attempt}/{APPLY_VERIFY_MAX_ATTEMPTS}: {}",
                            String::from_utf8_lossy(&output.stderr)
                        );
                    }
                    Err(e) => {
                        warn!(
                            "#3386: failed to run tmux {cmd:?} on apply-and-verify attempt \
                             {attempt}/{APPLY_VERIFY_MAX_ATTEMPTS}: {e}"
                        );
                    }
                    Ok(_) => {}
                }
            }
            let history_ok = match probe_history_limit(bin) {
                Ok(observed) if observed == expected_history_limit => true,
                Ok(observed) => {
                    warn!(
                        "#3386: tmux history-limit verification mismatch on apply-and-verify \
                         attempt {attempt}/{APPLY_VERIFY_MAX_ATTEMPTS} — expected \
                         {expected_history_limit}, observed {observed}"
                    );
                    false
                }
                Err(e) => {
                    warn!(
                        "#3386: tmux history-limit verification probe failed on apply-and-verify \
                         attempt {attempt}/{APPLY_VERIFY_MAX_ATTEMPTS}: {e}"
                    );
                    false
                }
            };
            // #5151: read back the WINDOW scope the alternate-screen set wrote.
            let alternate_screen_ok = match probe_alternate_screen(bin) {
                Ok(observed) if observed == expected_alternate_screen => true,
                Ok(observed) => {
                    warn!(
                        "#5151: tmux alternate-screen verification mismatch on apply-and-verify \
                         attempt {attempt}/{APPLY_VERIFY_MAX_ATTEMPTS} — expected \
                         {expected_alternate_screen}, observed {observed}"
                    );
                    false
                }
                Err(e) => {
                    warn!(
                        "#5151: tmux alternate-screen verification probe failed on \
                         apply-and-verify attempt {attempt}/{APPLY_VERIFY_MAX_ATTEMPTS}: {e}"
                    );
                    false
                }
            };
            if history_ok && alternate_screen_ok {
                return true;
            }
        }
        if attempt < APPLY_VERIFY_MAX_ATTEMPTS {
            std::thread::sleep(APPLY_VERIFY_RETRY_DELAY);
        }
    }
    false
}

/// Read back the tmux server's current `history-limit` global option
/// (#3386).
///
/// Why: the verification counterpart to [`ensure_server_up`] and the
/// `set-option -g` loop in [`create_managed_session`] — confirms the value
/// actually landed on the server rather than trusting a `set-option -g`
/// exit code alone.
/// What: runs `tmux show-options -g -v history-limit` and parses the single
/// line of stdout as `u32`. Any spawn failure, non-zero exit, or unparsable
/// output is returned as `Err` describing why the probe could not confirm
/// the value — callers decide how loudly to react.
/// Test: `probe_history_limit_reads_back_configured_value`,
/// `probe_history_limit_errors_on_unparsable_output`.
fn probe_history_limit(bin: &str) -> Result<u32, String> {
    match run_tmux_with_bin(
        bin,
        &TmuxCommand::ShowGlobalOption {
            name: HISTORY_LIMIT_OPTION.to_string(),
        },
    ) {
        Ok(output) if output.status.success() => {
            let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
            raw.parse::<u32>()
                .map_err(|_| format!("unparsable show-options output: {raw:?}"))
        }
        Ok(output) => Err(String::from_utf8_lossy(&output.stderr).into_owned()),
        Err(e) => Err(e.to_string()),
    }
}

/// Read back the tmux server's current WINDOW-scoped `alternate-screen`
/// option (#5151).
///
/// Why: the verification counterpart to the
/// `set-option -wg alternate-screen` entry
/// [`scrollback_option_commands`](trusty_common::tmux::scrollback_option_commands)
/// emits, and deliberately scope-matched to it. `alternate-screen` is a
/// window/pane option, not a server option like `history-limit`; probing
/// `show-options -g` (or `-pg`) here would read a scope the `set` never
/// wrote, which is how a wrong-scope `set-option` verifies green while the
/// pane still enters the alternate screen.
/// What: runs `tmux show-options -wg -v alternate-screen` and maps the single
/// line of stdout — `on` → `true`, `off` → `false`. Any spawn failure,
/// non-zero exit, or unrecognised value is returned as `Err`; a value tmux
/// does not document (anything but `on`/`off`) is NOT guessed at, because a
/// wrong guess here is a silent pass.
/// Test: `probe_alternate_screen_reads_back_on`,
/// `probe_alternate_screen_reads_back_off`,
/// `probe_alternate_screen_errors_on_unrecognised_value`,
/// `probe_alternate_screen_errors_on_nonzero_exit`.
fn probe_alternate_screen(bin: &str) -> Result<bool, String> {
    match run_tmux_with_bin(
        bin,
        &TmuxCommand::ShowWindowGlobalOption {
            name: ALTERNATE_SCREEN_OPTION.to_string(),
        },
    ) {
        Ok(output) if output.status.success() => {
            let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
            match raw.as_str() {
                "on" => Ok(true),
                "off" => Ok(false),
                _ => Err(format!("unrecognised show-options output: {raw:?}")),
            }
        }
        Ok(output) => Err(String::from_utf8_lossy(&output.stderr).into_owned()),
        Err(e) => Err(e.to_string()),
    }
}

/// Type `text` into a tmux pane, then press Enter (#2398 consolidation).
///
/// Why: every call site that starts `claude` in a freshly-created pane needs
/// this exact idiom — mirrors
/// [`daemon::tmux::TmuxDriver::send_line`](crate::daemon::tmux::TmuxDriver::send_line)'s
/// literal-then-Enter pattern (two `send-keys` invocations: `-l` literal text,
/// then the `Enter` key name), now shared so CLI/TUI callers do not
/// re-implement it as a single bare `send-keys <text> Enter` invocation
/// (functionally equivalent for a non-key-name string, but a second,
/// independently-typed-out version of the same idiom is exactly the kind of
/// drift #2398 closes).
/// What: runs the literal `send-keys -l <text>` via [`run_tmux_with_bin`]
/// (`tmux_bin` if given, else resolved via [`resolve_tmux_binary_or_bare`]);
/// if that invocation itself fails to spawn, the error is returned
/// immediately WITHOUT sending `Enter`. Otherwise sends `Enter` and returns
/// ITS `Output` — callers check `output.status.success()` exactly as they
/// did with the single-invocation form.
/// Test: argv shapes covered by `trusty_common::tmux`'s
/// `send_keys_literal_argv`/`send_keys_keyname_argv`; the live-process call
/// needs a real `tmux` binary, exercised transitively by every migrated call
/// site.
pub fn send_line(
    tmux_bin: Option<&str>,
    target: &TmuxTarget,
    text: &str,
) -> std::io::Result<std::process::Output> {
    let owned_bin;
    let bin = match tmux_bin {
        Some(b) => b,
        None => {
            owned_bin = resolve_tmux_binary_or_bare();
            &owned_bin
        }
    };
    let literal_output = run_tmux_with_bin(
        bin,
        &TmuxCommand::SendKeys {
            target: target.clone(),
            keys: text.to_string(),
            literal: true,
        },
    )?;
    if !literal_output.status.success() {
        // Don't send Enter after a failed literal send — nothing to submit.
        return Ok(literal_output);
    }
    run_tmux_with_bin(
        bin,
        &TmuxCommand::SendKeys {
            target: target.clone(),
            keys: "Enter".to_string(),
            literal: false,
        },
    )
}

#[cfg(test)]
#[path = "tmux_tests.rs"]
mod tmux_tests;
