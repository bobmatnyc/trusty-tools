//! tmux session-control primitives AND the crate's single tmux-spawning
//! entry point (#2398 architecture consolidation).
//!
//! Why: trusty-mpm hosts each Claude Code session inside a named tmux session
//! (the primary control model — see `docs/research/session-control-models.md`).
//! The patterns here are distilled from `ai-commander`'s `commander-tmux` crate
//! and `open-mpm`'s `tm` module: create named detached sessions, send keystrokes
//! with `send-keys`, and capture pane output. `TmuxTarget`/`TmuxCommand`/
//! `tmux_argv` stay PURE (no process spawning) so argv construction is
//! unit-testable without a live tmux server.
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
//! Scope note (#2398 follow-up filed for stragglers): a handful of read-only
//! probes — `tmux display-message -p '#S'` (current-session-name lookups in
//! `bin/tm/commands/tmux_attach.rs` and `bin/tm/commands/statusline/
//! branch.rs`), `tmux show-environment` (`bin/tm/commands/guided_inplace.rs`),
//! and `tmux display-message -t <s> -p '#{pane_pid}'`
//! (`core::process::tmux_pane_pid`) — still shell out directly. They are
//! read-only queries against an EXISTING session, not session-creation, so
//! they cannot bypass the scrollback/mouse ergonomics; migrating them was
//! judged out of scope for this PR (no `TmuxCommand` variant models
//! `display-message`/`show-environment` yet) to keep the diff reviewable.
//! `bin/tm/commands/tmux_attach.rs::tmux_attach` (the actual `attach-session`/
//! `switch-client` spawn) DOES route its binary resolution through
//! [`resolve_tmux_binary_or_bare`] even though its interactive,
//! stdio-inheriting `.status()` call shape does not fit [`run_tmux`]'s
//! output-capturing signature.
//! What: `TmuxTarget` (session\[:pane\] addressing), `TmuxCommand` (a typed
//! tmux sub-command), `tmux_argv` (renders a command to an argv vector),
//! [`resolve_tmux_binary`]/[`resolve_tmux_binary_or_bare`] (binary
//! resolution), [`run_tmux_with_bin`]/[`run_tmux`] (the shared spawn
//! primitive), and [`create_managed_session`] (the session-creation choke
//! point with ergonomics baked in).
//! Test: `cargo test -p trusty-mpm-core` asserts the rendered argv for each
//! command shape, including literal vs. key-name `send-keys`; the spawning
//! functions are exercised transitively by every call site (a live `tmux`
//! binary is needed to observe real output, so those paths are covered by
//! `#[ignore]` integration tests plus this module's own no-panic smoke test).

use tracing::warn;

use serde::{Deserialize, Serialize};

/// Addresses a tmux session, optionally a specific pane within it.
///
/// Why: every tmux I/O command needs a `-t` target; modelling it once avoids
/// re-deriving the target string at each call site.
/// What: a session name plus an optional pane id (`%0`, `%1`, ...).
/// Test: `target_renders_session_and_bare_pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TmuxTarget {
    /// tmux session name (the daemon names these `trusty-mpm-<session-id>`).
    pub session: String,
    /// Optional pane id; `None` addresses the session's active pane.
    #[serde(default)]
    pub pane: Option<String>,
}

impl TmuxTarget {
    /// Address the active pane of a named session.
    pub fn session(name: impl Into<String>) -> Self {
        Self {
            session: name.into(),
            pane: None,
        }
    }

    /// Address a specific pane within a session.
    pub fn pane(name: impl Into<String>, pane: impl Into<String>) -> Self {
        Self {
            session: name.into(),
            pane: Some(pane.into()),
        }
    }

    /// Render the tmux `-t` target string.
    ///
    /// Why (CRITICAL fix, follow-up to #2467): tmux pane ids (`%NNNN`) are
    /// GLOBALLY unique across the whole server, not scoped to a session — but
    /// `-t "<session>:<pane_id>"` still parses everything after the `:` as a
    /// WINDOW spec, not a pane spec. Live tmux 3.6b proof: `send-keys -t
    /// "sess:%6028" ...` fails with `can't find window: %6028`, while `-t
    /// "%6028"` (bare) works. Every resume/restart with a stored `pane_id`
    /// hit this — `spawn_resume` returned `Err` and the record flipped to
    /// `Errored`. The session-qualified form was never valid tmux syntax for
    /// a pane target.
    /// What: a pane target renders as the BARE pane id (`%NNNN`) with no
    /// session qualifier; a session-only target still renders the session
    /// name.
    /// Test: `target_renders_session_and_bare_pane`;
    /// `send_keys_pane_target_is_bare_id`.
    pub fn as_target(&self) -> String {
        match &self.pane {
            Some(p) => p.clone(),
            None => self.session.clone(),
        }
    }
}

/// A typed tmux sub-command the daemon's session manager can execute.
///
/// Why: enumerating the small set of tmux operations trusty-mpm needs
/// (vs. building ad-hoc argv vectors) keeps the daemon's tmux usage auditable
/// and the argv rendering testable without spawning processes.
/// What: covers session lifecycle, keystroke injection, and output capture.
/// Test: see the per-variant tests in this module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TmuxCommand {
    /// `new-session -A -d -s <name> [-c <dir>]` — create a detached session,
    /// or attach to it if a session with the same name already exists (the
    /// `-A` flag makes the command idempotent rather than failing on a
    /// duplicate session name).
    NewSession {
        /// Session name.
        name: String,
        /// Optional working directory for the session's first pane.
        workdir: Option<String>,
    },
    /// `kill-session -t <name>` — destroy a session.
    KillSession {
        /// Session name to kill.
        name: String,
    },
    /// `has-session -t <name>` — probe whether a session exists.
    HasSession {
        /// Session name to probe.
        name: String,
    },
    /// `list-sessions -F <fmt>` — enumerate sessions.
    ListSessions,
    /// `list-windows -t <name> -F <fmt>` — enumerate a session's windows.
    ListWindows {
        /// Session whose windows to list.
        name: String,
    },
    /// `list-panes -s -t <name> -F <fmt>` — enumerate ALL of a session's
    /// panes, across every window (`-s`).
    ///
    /// Why (CRITICAL follow-up, discovered by the live-tmux test written for
    /// #2467's fix): without `-s`, tmux resolves a session-name target to
    /// only the session's currently ACTIVE window and lists that window's
    /// panes — exactly the sibling-window-hijack scenario this feature
    /// exists to guard against. A pane in a non-active window (e.g. the
    /// original pane, once a sibling window steals focus) would silently
    /// vanish from this list even though it is still alive, making
    /// `pane_exists` wrongly report `false` and `resume` wrongly refuse with
    /// `PaneGone`.
    ListPanes {
        /// Session whose panes to list.
        name: String,
    },
    /// `send-keys -t <target> [-l] <keys>` — inject keystrokes.
    SendKeys {
        /// Target session/pane.
        target: TmuxTarget,
        /// The keys (or literal text) to send.
        keys: String,
        /// When true, pass `-l` so tmux sends the text literally rather than
        /// interpreting words like `Enter`/`C-c` as key names.
        literal: bool,
    },
    /// `capture-pane -t <target> -p [-S -<lines>]` — capture pane output.
    CapturePane {
        /// Target session/pane.
        target: TmuxTarget,
        /// Optional number of trailing scrollback lines to capture.
        lines: Option<u32>,
    },
    /// `set-environment -t <session> <key> <value>` — durably publish a
    /// variable into the tmux SESSION environment (#2157 item 1).
    ///
    /// Why: the pane-shell `export …;` prefix ([`crate::runtime::claude_code`]'s
    /// `session_id_export_prefix`) only lands in the ONE shell process that ran
    /// it — a sibling pane/window in the same session, or a pane spawned by a
    /// pre-fix build, never sees it. `tmux set-environment` writes into the
    /// session's OWN environment table instead, which any process can query via
    /// `tmux show-environment -t <session>` regardless of which pane/shell asks
    /// or when it was created.
    /// What: targets the SESSION (not a pane) — `set-environment` interprets
    /// `-t` as a session target here, unlike `send-keys`/`capture-pane` which
    /// address a specific pane.
    SetEnvironment {
        /// Session to set the variable in.
        session: String,
        /// Environment variable name.
        key: String,
        /// Environment variable value.
        value: String,
    },
    /// `set-option -g <name> <value>` — set a server-wide (global) tmux
    /// option (#2398).
    ///
    /// Why: managed sessions need a generous scrollback (`history-limit`) and
    /// mouse-wheel scrolling (`mouse`) applied to the SERVER before any pane
    /// is created — `history-limit` is captured into a pane's ring buffer AT
    /// CREATION TIME, so a per-session `set-option` issued after a pane
    /// already exists would not retroactively grow it. trusty-mpm runs every
    /// managed session on the ONE default tmux server (no dedicated `-S`
    /// socket), so a single `-g` (global) `set-option` benefits every session
    /// subsequently created on that server.
    /// What: renders `set-option -g <name> <value>`.
    SetGlobalOption {
        /// tmux option name (e.g. `history-limit`, `mouse`).
        name: String,
        /// Option value (e.g. `"100000"`, `"on"`).
        value: String,
    },
}

/// tmux global option name for scrollback lines retained per pane (#2398).
pub const HISTORY_LIMIT_OPTION: &str = "history-limit";

/// tmux global option name for mouse-wheel scrolling / copy-mode (#2398).
pub const MOUSE_OPTION: &str = "mouse";

/// Build the `set-option -g` command sequence that applies the scrollback +
/// mouse-scroll ergonomics to the tmux server (#2398).
///
/// Why: [`crate::daemon::tmux::TmuxDriver::apply_scrollback_options`] needs a
/// pure, unit-testable builder for the exact commands it issues (and their
/// order) — the resolved values come from
/// `core::trusty_tools_config::resolve_tmux_options`, not from here, so this
/// function stays free of any config/file-system dependency and is testable
/// with plain arguments.
/// What: two [`TmuxCommand::SetGlobalOption`] entries, `history-limit` then
/// `mouse` (rendered `"on"`/`"off"`), in the order the caller must run them —
/// both must land before the caller's subsequent `new-session`.
/// Test: `scrollback_option_commands_uses_configured_values`,
/// `scrollback_option_commands_mouse_off`.
pub fn scrollback_option_commands(history_limit: u32, mouse: bool) -> Vec<TmuxCommand> {
    vec![
        TmuxCommand::SetGlobalOption {
            name: HISTORY_LIMIT_OPTION.to_string(),
            value: history_limit.to_string(),
        },
        TmuxCommand::SetGlobalOption {
            name: MOUSE_OPTION.to_string(),
            value: if mouse { "on" } else { "off" }.to_string(),
        },
    ]
}

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
/// Why: callers that do not need [`daemon::tmux::TmuxDriver::discover`]'s
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
/// What: renders `cmd` via [`tmux_argv`] and runs
/// `Command::new(tmux_bin).args(argv).output()`. Callers own interpreting the
/// exit status / stderr — this function does not classify failures.
/// Test: exercised transitively by every call site; a live `tmux` binary is
/// required to observe real output, so this is covered by the `#[ignore]`
/// integration tests rather than a hermetic unit test.
pub fn run_tmux_with_bin(
    tmux_bin: &str,
    cmd: &TmuxCommand,
) -> std::io::Result<std::process::Output> {
    std::process::Command::new(tmux_bin)
        .args(tmux_argv(cmd))
        .output()
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

/// Create a tmux session with the configured scrollback + mouse ergonomics
/// applied FIRST — THE single choke point for "create a managed session"
/// used by every session-creating call site in the crate (#2398 architecture
/// consolidation).
///
/// Why: `history-limit` is captured into a pane's ring buffer AT CREATION
/// TIME, so the scrollback/mouse `set-option -g` calls MUST run before
/// `new-session` — see [`scrollback_option_commands`]. Baking both steps into
/// one function (rather than asking every caller to remember "apply options,
/// then create") is what makes it structurally impossible for a future call
/// site to bypass the ergonomics again, which is exactly how the #2398 QA
/// finding happened the first time (5 call sites independently shelled out to
/// `tmux new-session`, none aware the daemon-side ergonomics existed).
/// What: resolves the tmux binary (`tmux_bin` if given, else
/// [`resolve_tmux_binary_or_bare`]), loads
/// [`crate::core::trusty_tools_config::TrustyToolsConfig`] and resolves the
/// `tmux:` section, best-effort applies each
/// [`scrollback_option_commands`] entry via [`run_tmux_with_bin`] (a failure
/// here is logged and does NOT block session creation — `set-option -g` is
/// idempotent and virtually never fails on a tmux new enough to run
/// trusty-mpm at all), then issues `new-session` and returns its raw
/// `Output`. Callers classify success/failure from the returned `Output`
/// exactly as they did before this consolidation (`output.status.success()`).
/// Test: argv construction is covered by `scrollback_option_commands_*` and
/// `new_session_argv`; config resolution by `core::trusty_tools_config`'s
/// `tmux_options_*` tests. The live-process call itself needs a real `tmux`
/// binary, so it is exercised transitively by every migrated call site
/// (daemon `#[ignore]` integration tests; CLI/TUI paths are exercised
/// end-to-end by the existing `tm launch`/`tm connect` integration coverage).
pub fn create_managed_session(
    tmux_bin: Option<&str>,
    name: &str,
    workdir: Option<&str>,
) -> std::io::Result<std::process::Output> {
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
    for cmd in scrollback_option_commands(opts.history_limit, opts.mouse) {
        match run_tmux_with_bin(bin, &cmd) {
            Ok(output) if !output.status.success() => {
                warn!(
                    "tmux {cmd:?} exited non-zero (non-fatal, session creation continues): {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Err(e) => {
                warn!("failed to run tmux {cmd:?} (non-fatal, session creation continues): {e}");
            }
            Ok(_) => {}
        }
    }

    run_tmux_with_bin(
        bin,
        &TmuxCommand::NewSession {
            name: name.to_string(),
            workdir: workdir.map(str::to_string),
        },
    )
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
/// Test: argv shapes covered by `core::tmux`'s `send_keys_literal_argv`/
/// `send_keys_keyname_argv`; the live-process call needs a real `tmux`
/// binary, exercised transitively by every migrated call site.
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

/// tmux `-F` format string for `list-sessions`.
///
/// Why: a single canonical format keeps the parser in the daemon aligned with
/// the command emitted here.
/// What: name, creation epoch, attached flag — colon-separated.
pub const SESSION_LIST_FORMAT: &str = "#{session_name}:#{session_created}:#{session_attached}";

/// tmux `-F` format string for `list-windows` (`index:name`).
pub const WINDOW_LIST_FORMAT: &str = "#{window_index}:#{window_name}";

/// tmux `-F` format string for `list-panes` (`pane_id:active`).
pub const PANE_LIST_FORMAT: &str = "#{pane_id}:#{pane_active}";

/// Render a [`TmuxCommand`] into an argv vector suitable for `Command::args`.
///
/// Why: separating argv construction from process spawning makes the command
/// logic pure and unit-testable; the daemon just executes `tmux` with the
/// returned argv.
/// What: returns the argument list (excluding the `tmux` program name itself).
/// Test: `new_session_argv`, `send_keys_literal_argv`, `capture_argv`, etc.
pub fn tmux_argv(cmd: &TmuxCommand) -> Vec<String> {
    match cmd {
        TmuxCommand::NewSession { name, workdir } => {
            // `-A` attaches to an existing session of the same name instead
            // of failing with "duplicate session"; combined with `-d` it
            // stays detached, making session creation idempotent.
            let mut argv = vec![
                "new-session".to_string(),
                "-A".to_string(),
                "-d".to_string(),
                "-s".to_string(),
                name.clone(),
            ];
            if let Some(dir) = workdir {
                argv.push("-c".to_string());
                argv.push(dir.clone());
            }
            argv
        }
        TmuxCommand::KillSession { name } => {
            vec!["kill-session".into(), "-t".into(), name.clone()]
        }
        TmuxCommand::HasSession { name } => {
            vec!["has-session".into(), "-t".into(), name.clone()]
        }
        TmuxCommand::ListSessions => {
            vec![
                "list-sessions".into(),
                "-F".into(),
                SESSION_LIST_FORMAT.into(),
            ]
        }
        TmuxCommand::ListWindows { name } => {
            vec![
                "list-windows".into(),
                "-t".into(),
                name.clone(),
                "-F".into(),
                WINDOW_LIST_FORMAT.into(),
            ]
        }
        TmuxCommand::ListPanes { name } => {
            vec![
                "list-panes".into(),
                // `-s`: list every pane in the SESSION (all windows), not just
                // the currently active window's panes. See the variant doc.
                "-s".into(),
                "-t".into(),
                name.clone(),
                "-F".into(),
                PANE_LIST_FORMAT.into(),
            ]
        }
        TmuxCommand::SendKeys {
            target,
            keys,
            literal,
        } => {
            let mut argv = vec![
                "send-keys".to_string(),
                "-t".to_string(),
                target.as_target(),
            ];
            if *literal {
                argv.push("-l".to_string());
            }
            argv.push(keys.clone());
            argv
        }
        TmuxCommand::CapturePane { target, lines } => {
            let mut argv = vec![
                "capture-pane".to_string(),
                "-t".to_string(),
                target.as_target(),
                "-p".to_string(),
            ];
            if let Some(n) = lines {
                argv.push("-S".to_string());
                argv.push(format!("-{n}"));
            }
            argv
        }
        TmuxCommand::SetEnvironment {
            session,
            key,
            value,
        } => {
            vec![
                "set-environment".to_string(),
                "-t".to_string(),
                session.clone(),
                key.clone(),
                value.clone(),
            ]
        }
        TmuxCommand::SetGlobalOption { name, value } => {
            vec![
                "set-option".to_string(),
                "-g".to_string(),
                name.clone(),
                value.clone(),
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_renders_session_and_bare_pane() {
        // Session-only targets still render the session name.
        assert_eq!(TmuxTarget::session("s").as_target(), "s");
        // Pane targets render the BARE pane id — no `session:` prefix.
        // tmux pane ids are globally unique; `-t "session:%2"` parses `%2` as
        // a WINDOW spec and fails with "can't find window: %2" (live tmux
        // 3.6b), while `-t "%2"` correctly resolves the pane regardless of
        // which window/session currently owns the terminal focus.
        assert_eq!(TmuxTarget::pane("s", "%2").as_target(), "%2");
    }

    #[test]
    fn new_session_argv() {
        let argv = tmux_argv(&TmuxCommand::NewSession {
            name: "trusty-mpm-1".into(),
            workdir: Some("/tmp/proj".into()),
        });
        assert_eq!(
            argv,
            [
                "new-session",
                "-A",
                "-d",
                "-s",
                "trusty-mpm-1",
                "-c",
                "/tmp/proj"
            ]
        );
    }

    #[test]
    fn new_session_argv_without_workdir() {
        let argv = tmux_argv(&TmuxCommand::NewSession {
            name: "s".into(),
            workdir: None,
        });
        assert_eq!(argv, ["new-session", "-A", "-d", "-s", "s"]);
    }

    #[test]
    fn send_keys_literal_argv() {
        // Literal text: -l is present so tmux does not interpret words as keys.
        let argv = tmux_argv(&TmuxCommand::SendKeys {
            target: TmuxTarget::session("s"),
            keys: "claude --help".into(),
            literal: true,
        });
        assert_eq!(argv, ["send-keys", "-t", "s", "-l", "claude --help"]);
    }

    #[test]
    fn send_keys_keyname_argv() {
        // Non-literal: used to send key names like `Enter` or `C-c`.
        let argv = tmux_argv(&TmuxCommand::SendKeys {
            target: TmuxTarget::pane("s", "%1"),
            keys: "Enter".into(),
            literal: false,
        });
        assert_eq!(argv, ["send-keys", "-t", "%1", "Enter"]);
    }

    /// CRITICAL regression guard (follow-up to #2467): asserts the literal
    /// `-t` string built for a pane target is the bare `%NNNN` form with no
    /// `session:` prefix, so a regression back to the session-qualified form
    /// (which tmux parses as an invalid WINDOW spec, not a pane spec) fails
    /// fast in CI without needing a live tmux binary.
    #[test]
    fn send_keys_pane_target_is_bare_id() {
        let argv = tmux_argv(&TmuxCommand::SendKeys {
            target: TmuxTarget::pane("trusty-mpm-xyz", "%6015"),
            keys: "claude --resume".into(),
            literal: true,
        });
        let target_arg = &argv[argv.iter().position(|a| a == "-t").unwrap() + 1];
        assert_eq!(target_arg, "%6015", "pane target must be the bare pane id");
        assert!(
            !target_arg.contains(':'),
            "pane target must not be session-qualified: {target_arg}"
        );
    }

    #[test]
    fn capture_argv() {
        let argv = tmux_argv(&TmuxCommand::CapturePane {
            target: TmuxTarget::session("s"),
            lines: Some(50),
        });
        assert_eq!(argv, ["capture-pane", "-t", "s", "-p", "-S", "-50"]);

        let argv = tmux_argv(&TmuxCommand::CapturePane {
            target: TmuxTarget::session("s"),
            lines: None,
        });
        assert_eq!(argv, ["capture-pane", "-t", "s", "-p"]);
    }

    #[test]
    fn list_sessions_uses_canonical_format() {
        let argv = tmux_argv(&TmuxCommand::ListSessions);
        assert_eq!(argv, ["list-sessions", "-F", SESSION_LIST_FORMAT]);
    }

    #[test]
    fn list_windows_argv() {
        let argv = tmux_argv(&TmuxCommand::ListWindows {
            name: "work".into(),
        });
        assert_eq!(
            argv,
            ["list-windows", "-t", "work", "-F", WINDOW_LIST_FORMAT]
        );
    }

    #[test]
    fn list_panes_argv() {
        // `-s` is required so the listing spans every window in the session,
        // not just the currently active one — without it, a pane in a
        // non-active window (e.g. the original pane once a sibling window
        // steals focus) silently drops out of the list.
        let argv = tmux_argv(&TmuxCommand::ListPanes {
            name: "work".into(),
        });
        assert_eq!(
            argv,
            ["list-panes", "-s", "-t", "work", "-F", PANE_LIST_FORMAT]
        );
    }

    #[test]
    fn set_environment_argv() {
        let argv = tmux_argv(&TmuxCommand::SetEnvironment {
            session: "tmpm-brave-otter".into(),
            key: "TM_MANAGED_SESSION_ID".into(),
            value: "11111111-2222-3333-4444-555555555555".into(),
        });
        assert_eq!(
            argv,
            [
                "set-environment",
                "-t",
                "tmpm-brave-otter",
                "TM_MANAGED_SESSION_ID",
                "11111111-2222-3333-4444-555555555555"
            ]
        );
    }

    #[test]
    fn set_global_option_argv() {
        let argv = tmux_argv(&TmuxCommand::SetGlobalOption {
            name: "history-limit".into(),
            value: "100000".into(),
        });
        assert_eq!(argv, ["set-option", "-g", "history-limit", "100000"]);
    }

    #[test]
    fn scrollback_option_commands_uses_configured_values() {
        // Order matters: history-limit must precede mouse (both must land
        // before the caller's subsequent new-session, but the internal order
        // between the two is asserted here so it stays stable).
        let cmds = scrollback_option_commands(50_000, true);
        assert_eq!(cmds.len(), 2);
        assert_eq!(
            tmux_argv(&cmds[0]),
            ["set-option", "-g", "history-limit", "50000"]
        );
        assert_eq!(tmux_argv(&cmds[1]), ["set-option", "-g", "mouse", "on"]);
    }

    #[test]
    fn scrollback_option_commands_mouse_off() {
        let cmds = scrollback_option_commands(100_000, false);
        assert_eq!(tmux_argv(&cmds[1]), ["set-option", "-g", "mouse", "off"]);
    }

    // ── #2398 architecture consolidation: shared tmux entry point ──────────

    #[test]
    fn resolve_tmux_binary_does_not_panic() {
        // Works whether or not tmux is installed on the test host.
        let _ = resolve_tmux_binary();
    }

    #[test]
    fn resolve_tmux_binary_or_bare_never_empty() {
        // Even when resolution fails entirely, the bare "tmux" fallback keeps
        // this non-empty — callers always get a spawnable binary name.
        assert!(!resolve_tmux_binary_or_bare().is_empty());
    }

    #[test]
    fn run_tmux_with_bin_does_not_panic_on_missing_binary() {
        // A deliberately bogus binary name must surface as an Err, never panic.
        let result = run_tmux_with_bin(
            "definitely-not-a-real-tmux-binary-2398",
            &TmuxCommand::ListSessions,
        );
        assert!(result.is_err());
    }

    #[test]
    fn create_managed_session_does_not_panic_on_missing_binary() {
        // Same smoke guarantee for the session-creation choke point: a bogus
        // binary must degrade to an Err (from the final new-session spawn),
        // never panic — including through the best-effort scrollback loop.
        let result = create_managed_session(
            Some("definitely-not-a-real-tmux-binary-2398"),
            "tmpm-test-2398-no-bin",
            None,
        );
        assert!(result.is_err());
    }
}
