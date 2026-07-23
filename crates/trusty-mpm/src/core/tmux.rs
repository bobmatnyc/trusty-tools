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
//! What: re-exports `TmuxTarget`/`TmuxCommand`/`tmux_argv`/
//! `scrollback_option_commands`/the scrollback defaults from
//! `trusty_common::tmux`; [`resolve_tmux_binary`]/[`resolve_tmux_binary_or_bare`]
//! (binary resolution), [`run_tmux_with_bin`]/[`run_tmux`] (the shared spawn
//! primitive), and [`create_managed_session`] (the session-creation choke
//! point with ergonomics baked in, config-resolved locally).
//! Test: `cargo test -p trusty-mpm-core` covers the mpm-specific spawn/config
//! glue; argv construction and the options-before-new-session ordering
//! guarantee are tested once in `trusty_common::tmux` and reused here without
//! re-testing (`managed_session_command_sequence_matches_shared_layer`
//! asserts THIS crate's config-resolved call routes through it correctly).

use tracing::warn;

pub use trusty_common::tmux::{
    DEFAULT_TMUX_HISTORY_LIMIT, DEFAULT_TMUX_MOUSE, HISTORY_LIMIT_OPTION, MOUSE_OPTION,
    PANE_LIST_FORMAT, SESSION_LIST_FORMAT, TmuxCommand, TmuxTarget, WINDOW_LIST_FORMAT,
    managed_session_commands, scrollback_option_commands, tmux_argv,
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
    crate::core::spawn_disclaim::disclaimed_output(tmux_bin, &tmux_argv(cmd))
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
) -> Vec<TmuxCommand> {
    managed_session_commands(name, workdir, history_limit, mouse, true, None)
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
/// `tmux:` section, then builds the ordered command sequence via
/// [`managed_session_command_sequence`] (the shared
/// `trusty_common::tmux::managed_session_commands` recipe). Best-effort
/// applies each scrollback/mouse entry via [`run_tmux_with_bin`] (a failure
/// here is logged and does NOT block session creation — `set-option -g` is
/// idempotent and virtually never fails on a tmux new enough to run
/// trusty-mpm at all), then issues `new-session` and returns its raw
/// `Output`. Callers classify success/failure from the returned `Output`
/// exactly as they did before this consolidation (`output.status.success()`).
/// Test: argv construction and the ordering guarantee are covered once in
/// `trusty_common::tmux`'s own tests; this crate's config-resolved call is
/// covered by `managed_session_command_sequence_matches_shared_layer`. The
/// live-process call itself needs a real `tmux` binary, so it is exercised
/// transitively by every migrated call site (daemon `#[ignore]` integration
/// tests; CLI/TUI paths are exercised end-to-end by the existing `tm
/// launch`/`tm connect` integration coverage).
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

    // #3386: `set-option -g` has no `CMD_STARTSERVER` behavior in tmux — it
    // fails "no server running" if issued before any server-starting
    // command has run. Confirming the server is up FIRST (retrying a bounded
    // number of times) closes the race where a resume/recreate right after
    // the previous server died issued the scrollback options against a
    // socket that did not exist yet, silently logged that as non-fatal, and
    // let the subsequent `new-session` call (the one command that DOES
    // auto-start a server) spawn a fresh server whose pane inherited tmux's
    // factory `history-limit` of 2000 instead of the configured value.
    if let Err(e) = ensure_server_up(bin) {
        warn!(
            "#3386: tmux server could not be confirmed up after \
             {START_SERVER_MAX_ATTEMPTS} attempts; the scrollback/mouse options below may \
             silently fail to apply to the pane about to be created: {e}"
        );
    }

    let config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
    let opts = crate::core::trusty_tools_config::resolve_tmux_options(&config);
    let commands = managed_session_command_sequence(name, workdir, opts.history_limit, opts.mouse);
    let split = commands.len().saturating_sub(1);
    let (options, new_session) = commands.split_at(split);

    for cmd in options {
        match run_tmux_with_bin(bin, cmd) {
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

    // #3386: verify the history-limit actually landed on the server BEFORE
    // the pane that will inherit it is created — a mismatch here means the
    // very next `new-session` call is about to create a pane that silently
    // inherits tmux's factory default rather than the configured value.
    match probe_history_limit(bin) {
        Ok(observed) if observed == opts.history_limit => {}
        Ok(observed) => warn!(
            "#3386: tmux history-limit verification mismatch before pane creation — \
             expected {expected}, server reports {observed}; the pane `new-session` is about \
             to create may inherit tmux's factory default instead",
            expected = opts.history_limit
        ),
        Err(e) => warn!(
            "#3386: tmux history-limit verification probe failed (non-fatal, session creation \
             continues): {e}"
        ),
    }

    run_tmux_with_bin(
        bin,
        new_session
            .first()
            .expect("managed_session_command_sequence always ends with NewSession"),
    )
}

/// Number of attempts [`ensure_server_up`] makes before giving up (#3386).
const START_SERVER_MAX_ATTEMPTS: u8 = 3;

/// Delay between [`ensure_server_up`] retry attempts (#3386).
const START_SERVER_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(25);

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
fn ensure_server_up(bin: &str) -> Result<(), String> {
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
mod tests {
    use super::*;

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

    #[test]
    fn managed_session_command_sequence_matches_shared_layer() {
        // #3004 integration assertion: this crate's config-resolved
        // sequence must be EXACTLY what `trusty_common::tmux` would build
        // for `idempotent: true, command: None` (trusty-mpm's creation
        // semantics) with the same parameters — proving trusty-mpm's
        // session creation genuinely routes through the shared layer
        // rather than a local re-implementation drifting from it.
        let got = managed_session_command_sequence("tmpm-sess", Some("/tmp/proj"), 100_000, true);
        let want = trusty_common::tmux::managed_session_commands(
            "tmpm-sess",
            Some("/tmp/proj"),
            100_000,
            true,
            true,
            None,
        );
        assert_eq!(got, want);
    }

    #[test]
    fn managed_session_command_sequence_applies_options_before_new_session() {
        let cmds = managed_session_command_sequence("tmpm-sess", None, 50_000, false);
        assert_eq!(cmds.len(), 3);
        assert!(matches!(cmds[0], TmuxCommand::SetGlobalOption { .. }));
        assert!(matches!(cmds[1], TmuxCommand::SetGlobalOption { .. }));
        assert!(matches!(cmds[2], TmuxCommand::NewSession { .. }));
    }

    // ── #3386: server-up confirmed before any `set-option -g` ───────────
    //
    // All of these drive a scripted fake `tmux` binary (a `#!/bin/sh` script
    // written to a temp dir) rather than a live tmux server, so they are
    // deterministic and safe to run anywhere `sh` exists.

    /// Write an executable shell script at `dir/name` with body `body`,
    /// returning its path as a `String` suitable for `create_managed_session`
    /// / `run_tmux_with_bin`'s `bin` parameter.
    fn write_fake_tmux(dir: &std::path::Path, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, body).expect("write fake tmux script");
        let mut perms = std::fs::metadata(&path)
            .expect("stat fake tmux script")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod fake tmux script");
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn ensure_server_up_retries_then_succeeds() {
        // Fails the first two `start-server` invocations (simulating the
        // #3386 window right after a dead server's socket dir is torn down),
        // then succeeds on the third — exactly `START_SERVER_MAX_ATTEMPTS`.
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("attempts");
        std::fs::write(&counter, "0").unwrap();
        let script = format!(
            "#!/bin/sh\nn=$(cat '{counter}')\nn=$((n + 1))\necho \"$n\" > '{counter}'\nif [ \"$n\" -lt 3 ]; then\n  echo 'error connecting to socket (No such file or directory)' >&2\n  exit 1\nfi\nexit 0\n",
            counter = counter.display()
        );
        let bin = write_fake_tmux(dir.path(), "fake-tmux-retry", &script);

        let result = ensure_server_up(&bin);

        assert!(
            result.is_ok(),
            "must eventually succeed within {START_SERVER_MAX_ATTEMPTS} attempts: {result:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&counter).unwrap().trim(),
            "3",
            "must have retried exactly up to the succeeding attempt"
        );
    }

    #[test]
    fn ensure_server_up_fails_loudly_after_exhausting_retries() {
        // Always fails — the probe must return a loud Err after exhausting
        // every attempt rather than silently reporting success.
        let dir = tempfile::tempdir().unwrap();
        let script = "#!/bin/sh\necho 'error connecting to socket (No such file or directory)' >&2\nexit 1\n";
        let bin = write_fake_tmux(dir.path(), "fake-tmux-always-fails", script);

        let result = ensure_server_up(&bin);

        assert!(
            result.is_err(),
            "an always-failing server probe must error loudly, never proceed as if it succeeded"
        );
        assert!(
            result.unwrap_err().contains("error connecting to socket"),
            "the Err must carry the underlying failure reason"
        );
    }

    #[test]
    fn probe_history_limit_reads_back_configured_value() {
        let dir = tempfile::tempdir().unwrap();
        let script = "#!/bin/sh\necho 100000\nexit 0\n";
        let bin = write_fake_tmux(dir.path(), "fake-tmux-show-options", script);

        assert_eq!(probe_history_limit(&bin), Ok(100_000));
    }

    #[test]
    fn probe_history_limit_errors_on_unparsable_output() {
        let dir = tempfile::tempdir().unwrap();
        let script = "#!/bin/sh\necho 'not-a-number'\nexit 0\n";
        let bin = write_fake_tmux(dir.path(), "fake-tmux-bad-output", script);

        let result = probe_history_limit(&bin);
        assert!(
            result.is_err(),
            "unparsable show-options output must error loudly, not silently pass verification"
        );
    }

    #[test]
    fn probe_history_limit_errors_on_nonzero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let script = "#!/bin/sh\necho 'no server running' >&2\nexit 1\n";
        let bin = write_fake_tmux(dir.path(), "fake-tmux-show-options-fails", script);

        assert!(probe_history_limit(&bin).is_err());
    }

    #[test]
    fn create_managed_session_confirms_server_before_applying_options() {
        // #3386 end-to-end (still no live tmux): a fake `tmux` that just
        // records each invocation's tmux sub-command (argv[0]) to a log file
        // and always succeeds. This exercises the EXACT choke point the
        // resume-after-server-death path (`SessionManager::resume` →
        // `resume_workdir::create_and_verify_pane` → `RealTmuxDriver::
        // create_session` → `TmuxDriver::create_session`) routes through, so
        // asserting the recorded order here proves the fix for that path too.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("calls.log");
        let script = format!("#!/bin/sh\necho \"$1\" >> '{}'\nexit 0\n", log.display());
        let bin = write_fake_tmux(dir.path(), "fake-tmux-order", &script);

        let _ = create_managed_session(Some(&bin), "tmpm-3386-order-test", None);

        let calls = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = calls.lines().collect();

        assert_eq!(
            lines.first(),
            Some(&"start-server"),
            "the server must be confirmed up BEFORE any set-option -g (#3386): {lines:?}"
        );
        let new_session_pos = lines
            .iter()
            .position(|l| *l == "new-session")
            .expect("new-session must still run");
        let set_option_positions: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| **l == "set-option")
            .map(|(i, _)| i)
            .collect();
        assert!(
            !set_option_positions.is_empty(),
            "the scrollback/mouse options must still be applied: {lines:?}"
        );
        assert!(
            set_option_positions
                .iter()
                .all(|&p| p > 0 && p < new_session_pos),
            "set-option -g must run strictly AFTER start-server and BEFORE new-session \
             (#3386 / pre-existing #2398 ordering guarantee): {lines:?}"
        );
    }
}
