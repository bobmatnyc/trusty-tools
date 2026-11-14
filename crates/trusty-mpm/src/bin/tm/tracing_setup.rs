//! Tracing subscriber setup for the short-lived CLI inspection commands
//! (issue #4573).
//!
//! Why: `main.rs` registered a subscriber only for the long-running modes
//! (`Command::Daemon`, `Command::Supervisor`), which was fine while every other
//! subcommand printed its own results. It stopped being fine for
//! `tm sessions instructions`: that command's entire diagnostic surface —
//! `claude_md_sections::ProjectOverrides::log` (one event per applied, declined,
//! duplicated or shadowed marker), `claude_md_sections::warn_unapplied`,
//! `instruction_overrides::read_override`, and `resolve_pm_prompt`'s
//! composer-source event — is `tracing`, and with no subscriber registered all
//! of it was dropped. `RUST_LOG=trusty_mpm=info tm sessions instructions --dir
//! <project>` emitted ZERO bytes on stderr, so the ONE command an operator runs
//! to ask "why didn't my override apply?" could not answer.
//!
//! In the #4573 reproduction that mattered concretely: a `CLAUDE.md` CORE block
//! deleted the Prohibitions and Circuit Breakers tables, and the operator's only
//! signal was their absence from 28 KB of output.
//!
//! What: [`init_cli_diagnostics_if_wanted`] — a stderr-only `fmt` subscriber
//! registered for exactly those commands, defaulting to `warn`. Lives in its own
//! module rather than inline in `main.rs` because `main.rs` is already at the
//! 500-SLOC cap `scripts/check_line_cap.sh` enforces.
//!
//! Test: `tests/tm_sessions_instructions_diagnostics.rs`.

use crate::cli::{CatalogAction, Command, SessionAction};

/// Whether this invocation is a CLI command whose value is its diagnostics.
///
/// Why: kept as a predicate over the parsed command rather than a flag threaded
/// from the handler, because the subscriber must exist BEFORE the handler runs —
/// the events fire inside `resolve_pm_prompt`, which the handler calls.
///
/// #4878 widened it past `tm sessions instructions` to every in-process path
/// that runs a DEPLOY. Deployment legitimately declines a file — a
/// checksum-frozen skill, an unreadable ledger, a raced merge — and each
/// decline is a `warn!`/`error!` written to end silent staleness (#4840, and
/// PRs #4848 / #4876 / #4908). With no subscriber those events went nowhere, so
/// the file stayed stale forever with no signal on the four paths an operator
/// runs by hand.
/// #6754 added `tm sessions start` for the same reason. In a project with no
/// git remote that verb cannot reach the daemon, so it falls back to
/// `start_session_in_place`, which runs `prepare_session_with_home` in this
/// process — including the stray-skill sweep, whose removals and declines are
/// `warn!` events (`core/session_launch/skills.rs`). Two live removal runs on
/// tm 1.5.18 left no trace anywhere: not stderr, not `~/.trusty-mpm/logs`, not
/// the daemon's `stderr.log`, even though files were moved on disk.
/// #7823 added `tm install` for the same reason a third time. That verb writes
/// the tm-owned `settings.json` through `standalone::hooks::write_project_hooks`,
/// which since #7789 preserves an unparseable file as a
/// `settings.json.malformed-<stamp>` sibling and names that copy in a
/// `tracing::warn!`. With no subscriber the operator's only notice of a
/// clobbered settings file went nowhere.
/// What: true for `tm sessions instructions` and `tm sessions start`, each with
/// its deprecated singular alias; for bare `tm` (`None`), which deploys
/// in-process on both the launch and the in-place relaunch path; for
/// `tm doctor`, whose `--fix-skills` redeploys; for `tm catalog apply`,
/// which redeploys the manifest-selected roster; and for `tm install`, which
/// deploys the bundle and rewrites the managed `settings.json`.
/// Test: `wants_cli_diagnostics_covers_every_in_process_deploy_path`,
/// `wants_cli_diagnostics_skips_paths_that_own_stdout`,
/// `instructions_emits_override_diagnostics_on_stderr`.
fn wants_cli_diagnostics(command: &Option<Command>) -> bool {
    matches!(
        command,
        // Bare `tm`: `commands::launch` and the in-place relaunch path both
        // deploy in-process before handing off to Claude Code.
        None | Some(Command::Doctor { .. })
            | Some(Command::Catalog {
                action: CatalogAction::Apply { .. }
            })
            // #6754: `start` deploys in-process when the daemon path is
            // unavailable, and its sweep reports only via `tracing`.
            | Some(Command::Sessions {
                action: SessionAction::Instructions { .. } | SessionAction::Start { .. }
            })
            | Some(Command::Session {
                action: SessionAction::Instructions { .. } | SessionAction::Start { .. }
            })
            // #7823: `install` rewrites the managed `settings.json`, and the
            // #7789 malformed-file preservation reports only via `tracing`.
            | Some(Command::Install { .. })
    )
}

/// Register a stderr-only subscriber when `command` is a CLI inspection command.
///
/// Why the default filter is `warn` and not `info`: an operator debugging a
/// silently-ignored override must not have to already know to set `RUST_LOG`
/// before the framework will admit it declined their block — the decline IS the
/// answer they came for. Applied-override `info` lines stay below the default so
/// an ordinary run is not narrated; `RUST_LOG=trusty_mpm=info` opts into those.
///
/// Why stderr, explicitly: `tracing_subscriber::fmt` writes to stdout by
/// default, `tm sessions instructions` prints the resolved system prompt to
/// stdout, and this binary's daemon and MCP modes require stdout stay free for
/// JSON-RPC framing. The `with_writer` call is load-bearing.
///
/// Called at most once per process, before dispatch; the caller still gates
/// this against the daemon/supervisor branch so those keep their file-rotating
/// subscriber.
/// Test: `instructions_emits_override_diagnostics_on_stderr`,
/// `declined_overrides_are_reported_without_rust_log_set`,
/// `a_clean_project_produces_no_override_diagnostics`.
pub(crate) fn init_cli_diagnostics_if_wanted(command: &Option<Command>) {
    if wants_cli_diagnostics(command) {
        init_stderr_only("warn");
    }
}

/// Install a stderr-only `fmt` subscriber, honouring `RUST_LOG` over `default`.
///
/// Why stderr, explicitly: `tracing_subscriber::fmt` writes to STDOUT by
/// default, and every mode of this binary needs stdout for something else —
/// `tm sessions instructions` prints the resolved system prompt there, and the
/// daemon and MCP modes need it free for JSON-RPC framing. The `with_writer`
/// call is the load-bearing line in this function, not decoration.
///
/// Idempotent: `try_init` returns an error instead of panicking when a global
/// subscriber is already registered, and that error is discarded. #4878 widened
/// [`wants_cli_diagnostics`] to cover bare `tm`, so a path that also installs
/// its own subscriber would otherwise abort the process rather than log — and
/// the project convention is `try_init` for exactly this reason.
/// Test: `init_stderr_only_is_idempotent`,
/// `a_clean_project_produces_no_override_diagnostics` (the writer choice is
/// asserted by piping the two streams apart).
pub(crate) fn init_stderr_only(default: &str) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter(default))
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

/// Target prefix held at `warn` unless `RUST_LOG` names exactly it (#8948).
/// A directive target matches by string prefix, so `aws` covers `aws_config`,
/// `aws_sdk_*`, `aws_smithy_*`, `aws_sigv4` and `aws_credential_types`.
const AWS_LOG_GUARD: &str = "aws";

/// The `EnvFilter` every subscriber in this binary builds: `RUST_LOG` over
/// `default`, with the AWS SDK targets held at `warn`.
///
/// Why: #8948 — `aws_config::profile::credentials` logs the access key ID at
/// INFO each time the log-drain S3 adapter (or the `bedrock` SM provider)
/// rebuilds its credential chain, and the daemon's `info` default wrote it to
/// the rotated log about once a minute.
/// What: reads `RUST_LOG` and defers to [`filter_from_directives`].
/// Test: `aws_info_events_are_dropped_under_rust_log_info`.
pub(crate) fn env_filter(default: &str) -> tracing_subscriber::EnvFilter {
    filter_from_directives(std::env::var("RUST_LOG").ok().as_deref(), default)
}

/// Build the filter from an explicit `RUST_LOG` value, so tests never touch
/// the process environment.
///
/// What: `rust_log` when it parses, else `default` — the same choice
/// `EnvFilter::try_from_default_env().unwrap_or_else(|_| default.into())`
/// made. Then `aws=warn` is appended unless a static directive's target is
/// exactly `aws` (a span-scoped `aws[..]` directive does not count). Precedence: `EnvFilter` applies the most specific (longest) matching
/// target, so an operator's `aws_config=debug` overrides the guard for
/// `aws_config::*` only, and `aws=<level>` replaces the guard for all of them.
/// Test: `aws_info_events_are_dropped_under_rust_log_info`,
/// `aws_info_events_are_dropped_at_the_default_level`,
/// `an_explicit_aws_directive_is_honoured`,
/// `a_span_scoped_aws_directive_keeps_the_guard`.
fn filter_from_directives(rust_log: Option<&str>, default: &str) -> tracing_subscriber::EnvFilter {
    use tracing_subscriber::EnvFilter;
    let base = rust_log
        .filter(|s| EnvFilter::try_new(s).is_ok())
        .unwrap_or(default);
    let mut directives = base.to_string();
    // #8948: hold AWS SDK targets at warn (key id in log)
    // Only a static directive (no `[span{fields}]` part) can replace the guard:
    // `aws[foo]=debug` is dynamic and says nothing about events outside `foo`.
    let named = base
        .split(',')
        .map(str::trim)
        .filter(|d| !d.contains('['))
        .filter_map(|d| d.split('=').next())
        .any(|target| target.trim() == AWS_LOG_GUARD);
    if !named {
        directives.push_str(&format!(",{AWS_LOG_GUARD}=warn"));
    }
    // `EnvFilter::new` drops an unparsable directive rather than failing;
    // `base` was validated above and the guard is a literal, so none is dropped.
    EnvFilter::new(directives)
}

/// Whether the long-running modes should also write every event to stderr.
///
/// Why (#6569): under launchd the daemon plist's `StandardErrorPath` captures
/// this process's stderr into a file launchd never rotates, so every line the
/// rotated `~/.trusty-mpm/logs/trusty-mpm.log.*` already holds was written a
/// second time to `~/Library/Logs/trusty-mpm/stderr.log`. That file reached
/// 3.4 GB against 3.0 GB of rotated logs over one 48-hour window. Only one of
/// the two sinks can be dropped without losing anything, and it is the stderr
/// one — nothing reads it that the rotated file does not serve better.
/// What: two states, decided by [`stderr_sink_decision`].
/// Test: `stderr_sink_matrix`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StderrSink {
    /// Register the stderr `fmt` layer — a foreground operator is watching, or
    /// there is no file sink to fall back on.
    Register,
    /// Skip it: launchd is already capturing stderr, and the rotated file layer
    /// has the same events.
    Skip,
}

/// Decide whether to register the stderr layer beside the rotated file layer.
///
/// Why: the duplicate-write condition is a conjunction — a rotated file sink
/// AND launchd capturing stderr — and getting either half wrong loses logs
/// rather than merely duplicating them. Keeping it a pure function of those two
/// inputs is what makes the whole 2x3 matrix assertable without a daemon, a
/// plist, or a real `launchctl`.
/// What: [`StderrSink::Skip`] only when a file layer is configured AND launchd
/// positively reports it supervises this process. Every other cell registers:
/// with no file layer there is nowhere else for events to go; `NotSupervised`
/// is a foreground run whose operator is reading the terminal; and `Unknown`
/// means launchd could not be asked, where duplicating output is strictly
/// better than silently having none. That asymmetry is deliberate — this
/// function may only ever remove a sink on positive evidence, the same rule
/// `trusty_common::supervision` was built around (#4469).
/// Test: `stderr_sink_matrix`, `stderr_sink_never_skips_without_a_file_layer`.
pub(crate) fn stderr_sink_decision(
    file_layer_configured: bool,
    supervision: &trusty_common::supervision::LaunchdSupervision,
) -> StderrSink {
    use trusty_common::supervision::LaunchdSupervision;
    match (file_layer_configured, supervision) {
        (true, LaunchdSupervision::Supervised(_)) => StderrSink::Skip,
        _ => StderrSink::Register,
    }
}

/// Build the daemon/supervisor subscriber: rotated file, optional stderr, capture.
///
/// Why: this composition used to sit inline in `main.rs`, which is at the
/// 500-SLOC cap, and it is where #6569's duplicate sink lived. Moving it here
/// puts the sink choice next to [`stderr_sink_decision`] that governs it.
/// What: creates `~/.trusty-mpm/logs/`, builds the daily-rotating appender, asks
/// launchd once via `trusty_common::supervision::launchd_supervision` (bounded —
/// see that function; an unreachable launchd answers `Unknown` and keeps
/// stderr), and registers the layers a single `init` call. Returns the
/// appender's `WorkerGuard` and the bug-capture `ErrorStore`, both of which the
/// caller must hold for the process lifetime.
///
/// `EnvFilter` is not `Clone`, so each layer gets its own instance re-parsed
/// from `RUST_LOG`; re-parsing is cheap and happens once at startup.
/// Test: the sink choice by `stderr_sink_matrix`; the composition itself is a
/// startup path exercised by every daemon run.
#[cfg(feature = "daemon")]
pub(crate) fn init_daemon_tracing() -> anyhow::Result<(
    tracing_appender::non_blocking::WorkerGuard,
    trusty_common::error_capture::ErrorStore,
)> {
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let log_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot resolve home directory"))?
        .join(".trusty-mpm")
        .join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let file_appender = tracing_appender::rolling::daily(&log_dir, "trusty-mpm.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    // Bug-reporting Phase 1 (#478): ERROR events are captured to
    // <data_dir>/trusty-mpm/errors.jsonl and an in-memory ring. Capture never
    // writes stdout, so it is safe for both the HTTP daemon and MCP stdio.
    let (capture_layer, store) = trusty_common::error_capture::bug_capture_layer(
        "trusty-mpm",
        trusty_common::error_capture::DEFAULT_CAPTURE_CAPACITY,
        env!("CARGO_PKG_VERSION"),
    );

    // #6569: a file sink is always configured on this path, so the only open
    // question is whether launchd is also capturing stderr into its own
    // unrotated file.
    let supervision = trusty_common::supervision::launchd_supervision();
    let stderr_sink = stderr_sink_decision(true, &supervision);

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_filter(env_filter("info"));
    // MCP mode speaks JSON-RPC on stdout — tracing never leaves stdout.
    let stderr_layer = (stderr_sink == StderrSink::Register).then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_filter(env_filter("info"))
    });

    tracing_subscriber::registry()
        .with(stderr_layer)
        .with(file_layer)
        .with(capture_layer)
        .init();

    // First line into the file sink, so an operator reading the rotated log can
    // see which sink layout this process chose and why.
    tracing::info!(
        log_dir = %log_dir.display(),
        stderr_sink = ?stderr_sink,
        supervision = %supervision.describe(),
        "tracing initialised for a long-running mode (#6569)"
    );
    Ok((guard, store))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    use crate::cli::Cli;

    /// Parse a real argv the way `main` does, so the predicate is asserted
    /// against the command clap actually produces.
    fn command_for(argv: &[&str]) -> Option<crate::cli::Command> {
        Cli::try_parse_from(argv)
            .unwrap_or_else(|e| panic!("argv {argv:?} must parse: {e}"))
            .command
    }

    #[test]
    fn wants_cli_diagnostics_covers_every_in_process_deploy_path() {
        // #4878: each of these runs a deploy in-process and emits its declines
        // as `tracing` events. Before the fix only `sessions instructions`
        // registered a subscriber, so every warning here went nowhere.
        for argv in [
            vec!["tm"],
            vec!["tm", "doctor"],
            vec!["tm", "doctor", "--fix-skills"],
            vec!["tm", "catalog", "apply"],
            vec!["tm", "catalog", "apply", "--force", "--prune"],
            vec!["tm", "sessions", "instructions"],
            vec!["tm", "session", "instructions"],
            // #6754: with no daemon reachable, `start` deploys in-process via
            // `start_session_in_place` and its stray-skill sweep warns through
            // `tracing`. RED before the fix — neither arm matched.
            vec!["tm", "sessions", "start"],
            vec!["tm", "sessions", "start", "--dir", "/tmp/project"],
            vec!["tm", "session", "start"],
            // #7823: `install` rewrites the managed `settings.json`, whose
            // #7789 malformed-file preservation warns through `tracing`. RED
            // before the fix — no arm matched `Command::Install`.
            vec!["tm", "install"],
            vec!["tm", "install", "--force"],
            vec!["tm", "install", "--reconcile-skills"],
        ] {
            assert!(
                wants_cli_diagnostics(&command_for(&argv)),
                "{argv:?} runs an in-process deploy and must get a subscriber"
            );
        }
    }

    #[test]
    fn wants_cli_diagnostics_skips_paths_that_own_stdout() {
        // The daemon/supervisor branch installs its own file-rotating
        // subscriber, and `tm serve --stdio` needs stdout for JSON-RPC framing.
        // Neither deploys in-process, so neither is widened here.
        for argv in [
            vec!["tm", "serve", "--stdio"],
            vec!["tm", "status"],
            vec!["tm", "catalog", "ls"],
        ] {
            assert!(
                !wants_cli_diagnostics(&command_for(&argv)),
                "{argv:?} must not register the CLI diagnostics subscriber"
            );
        }
    }

    #[test]
    fn init_stderr_only_is_idempotent() {
        // `try_init`, not `init`: a second registration must return an error
        // that is discarded, never abort the process. Other tests in this
        // binary may already have installed a subscriber, so this asserts the
        // no-panic property in either order.
        init_stderr_only("warn");
        init_stderr_only("warn");
    }

    /// The whole #6569 decision, as a matrix: file layer x what launchd says.
    ///
    /// Why: the duplicate write only happens in ONE of these six cells, and the
    /// cost of getting any other cell wrong is losing log output rather than
    /// duplicating it. Asserting the matrix is what proves the fix removed a
    /// sink in exactly the case that had two.
    /// Test: this is the test. RED before the fix: `stderr_sink_decision` did
    /// not exist and `main.rs` registered the stderr layer unconditionally.
    #[test]
    fn stderr_sink_matrix() {
        use trusty_common::supervision::LaunchdSupervision;

        let supervised = LaunchdSupervision::Supervised("com.trusty.mpm".into());
        let not = LaunchdSupervision::NotSupervised;
        let unknown = LaunchdSupervision::Unknown("launchctl not on PATH".into());

        // The one cell that had two sinks: launchd captures stderr into its own
        // unrotated file while the rotated file layer holds the same events.
        assert_eq!(
            stderr_sink_decision(true, &supervised),
            StderrSink::Skip,
            "a launchd-supervised process with a file sink must not also write stderr"
        );

        // A foreground run: the operator is reading the terminal.
        assert_eq!(stderr_sink_decision(true, &not), StderrSink::Register);

        // Unanswerable is not evidence. Duplicating beats going silent, and the
        // sink may only be removed on a positive answer (#4469's rule).
        assert_eq!(
            stderr_sink_decision(true, &unknown),
            StderrSink::Register,
            "an unreachable launchd must never cost the operator their logs"
        );

        // With no file layer, stderr is the only sink there is.
        for supervision in [&supervised, &not, &unknown] {
            assert_eq!(
                stderr_sink_decision(false, supervision),
                StderrSink::Register,
                "{supervision:?} must still keep stderr when nothing else is configured"
            );
        }
    }

    /// In-memory stand-in for a log sink.
    #[derive(Clone, Default)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("capture lock").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Capture;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// What one daemon-shaped layer (`fmt::layer().with_filter(..)`, the shape
    /// `init_daemon_tracing` registers twice) writes under
    /// `filter_from_directives(rust_log, "info")`. Thread-local subscriber and
    /// an explicit directive string: no global state, no env mutation.
    fn emitted(rust_log: Option<&str>) -> String {
        use tracing_subscriber::Layer as _;
        use tracing_subscriber::layer::SubscriberExt as _;
        let capture = Capture::default();
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(capture.clone())
            .with_ansi(false)
            .with_filter(filter_from_directives(rust_log, "info"));
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "aws_config::profile::credentials", "info-key-8948");
            tracing::warn!(target: "aws_config::profile::credentials", "warn-8948");
            tracing::info!(target: "aws_sdk_s3::client", "sdk-8948");
            tracing::info!(target: "aws_smithy_runtime::client", "smithy-8948");
            tracing::info!(target: "trusty_mpm::daemon::log_drain", "own-8948");
            tracing::debug!(target: "trusty_mpm::daemon::log_drain", "own-debug-8948");
        });
        let bytes = capture.0.lock().expect("capture lock").clone();
        String::from_utf8(bytes).expect("utf8 log output")
    }

    /// REGRESSION (#8948): the installed plist's `RUST_LOG=info` keeps the
    /// credential provider's INFO line out of the log, keeps its WARN, and
    /// leaves trusty's own INFO untouched. RED before the fix: the daemon
    /// layers used `RUST_LOG` verbatim and wrote `info-key-8948`.
    #[test]
    fn aws_info_events_are_dropped_under_rust_log_info() {
        let out = emitted(Some("info"));
        assert!(
            !out.contains("info-key-8948"),
            "credential line leaked: {out}"
        );
        assert!(!out.contains("sdk-8948"), "SDK INFO line leaked: {out}");
        assert!(
            !out.contains("smithy-8948"),
            "smithy INFO line leaked: {out}"
        );
        assert!(out.contains("warn-8948"), "AWS WARN must still show: {out}");
        assert!(out.contains("own-8948"), "own INFO must still show: {out}");
        assert!(
            !out.contains("own-debug-8948"),
            "own DEBUG stays off: {out}"
        );
    }

    /// REGRESSION (#8948): with `RUST_LOG` unset the `info` default holds the
    /// AWS targets down too.
    #[test]
    fn aws_info_events_are_dropped_at_the_default_level() {
        let out = emitted(None);
        assert!(
            !out.contains("info-key-8948"),
            "credential line leaked: {out}"
        );
        assert!(out.contains("warn-8948"), "AWS WARN must still show: {out}");
        assert!(out.contains("own-8948"), "own INFO must still show: {out}");
    }

    /// An operator's per-target directive beats the guard for its own target
    /// only; `aws=<level>` replaces the guard outright.
    #[test]
    fn an_explicit_aws_directive_is_honoured() {
        let out = emitted(Some("info,aws_config=debug"));
        assert!(
            out.contains("info-key-8948"),
            "opt-in must be honoured: {out}"
        );
        assert!(!out.contains("sdk-8948"), "siblings stay guarded: {out}");

        let out = emitted(Some("info,aws=info"));
        assert!(
            out.contains("info-key-8948"),
            "aws=info must open it: {out}"
        );
        assert!(out.contains("sdk-8948"), "aws=info covers the SDK: {out}");
    }

    /// REGRESSION (#8948 review): a span-scoped directive on `aws` is dynamic,
    /// so it must not count as naming `aws` and drop the guard for events
    /// outside that span. `[foo]=debug` (no target) is explicit operator
    /// scope and stays out of the guard's way.
    #[test]
    fn a_span_scoped_aws_directive_keeps_the_guard() {
        for rust_log in [
            "info,aws[foo]=debug",
            "info,aws[{x}]=debug",
            "info,aws[foo]",
        ] {
            let out = emitted(Some(rust_log));
            assert!(
                !out.contains("info-key-8948"),
                "{rust_log}: credential line leaked: {out}"
            );
            assert!(
                !out.contains("sdk-8948"),
                "{rust_log}: SDK INFO line leaked: {out}"
            );
            assert!(
                out.contains("warn-8948"),
                "{rust_log}: AWS WARN must still show: {out}"
            );
        }
    }

    /// The invariant behind the matrix, stated on its own: no combination of
    /// launchd answers may leave a process with zero sinks.
    #[test]
    fn stderr_sink_never_skips_without_a_file_layer() {
        use trusty_common::supervision::LaunchdSupervision;
        for supervision in [
            LaunchdSupervision::Supervised("com.trusty.mpm".into()),
            LaunchdSupervision::NotSupervised,
            LaunchdSupervision::Unknown("timed out".into()),
        ] {
            assert_ne!(stderr_sink_decision(false, &supervision), StderrSink::Skip);
        }
    }
}
