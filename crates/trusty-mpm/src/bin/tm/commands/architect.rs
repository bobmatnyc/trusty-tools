//! `tm architect <verb>`: the Architect's deterministic commands (#9822).
//!
//! Why: the Architect's poller and skills ran five Python scripts; #9171 A4
//! swaps them for this binary with the same arguments, stdout and exit codes.
//! What: [`run`] resolves the Architect directory (`ARCHITECT_PROJECT_DIR`,
//! else the current directory), the home directory and the tmux server
//! (`TMUX_SOCKET`), calls `trusty_architect::verbs`, prints the verb's stdout
//! as-is, and on an error prints `tm architect: <reason>` to stderr and
//! returns the exit code: 2 for a bad argument, 1 for input it cannot read,
//! a tmux it cannot reach, or state it cannot parse. #9167: `poll` records
//! each wake it submits on the trusty-events bus
//! (`super::architect_messages`).
//! Test: `cli_parses_architect_verbs`; the binary in
//! `tests/tm_architect_verbs.rs`.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use trusty_architect::SystemTmux;
use trusty_architect::verbs::{self, InputTarget, Output, PollArgs, VerbError};

use crate::cli::ArchitectAction;

/// Runs `action` against this process's environment; returns the exit code.
pub(crate) fn run(action: ArchitectAction) -> i32 {
    let get = |name: &str| std::env::var(name).ok();
    let out = execute(action, &get, &mut io::stdin().lock());
    let mut stdout = io::stdout().lock();
    if let Err(e) = stdout
        .write_all(out.stdout.as_bytes())
        .and_then(|()| stdout.flush())
    {
        eprintln!("tm architect: cannot write stdout: {e}");
        return 1;
    }
    match out.error {
        None => 0,
        Some(e) => {
            eprintln!("tm architect: {e}");
            e.exit_code()
        }
    }
}

/// The verb's stdout and error; see the module doc.
fn execute(
    action: ArchitectAction,
    get: &dyn Fn(&str) -> Option<String>,
    stdin: &mut dyn Read,
) -> Output {
    let result = match action {
        ArchitectAction::Poll {
            once,
            interval,
            dry_run,
        } => config(get).and_then(|config| {
            let args = PollArgs {
                once,
                interval,
                dry_run,
            };
            // #9167: each submitted wake becomes a `poll_wake` message event.
            let messages = super::architect_messages::poll_sink(&config.poll_session);
            verbs::poll(&args, &config, messages.as_ref()).map(|()| String::new())
        }),
        ArchitectAction::Classify { file } => {
            read_capture(file.as_deref(), stdin).and_then(|bytes| verbs::classify(&bytes))
        }
        ArchitectAction::InputState { targets } => return input_state(&targets, get),
        ArchitectAction::Quiet { file } => {
            let path = match file {
                Some(file) => Ok(file),
                None => config(get).map(|c| c.quiet_sessions),
            };
            path.and_then(|p| verbs::quiet(&p))
        }
        ArchitectAction::SelfCtx { dir } => {
            let dir = match dir {
                Some(dir) => Ok(dir),
                None => config(get).map(|c| c.self_ctx_dir),
            };
            dir.and_then(|d| verbs::self_ctx(&d))
        }
    };
    match result {
        Ok(stdout) => Output {
            stdout,
            error: None,
        },
        Err(e) => Output {
            stdout: String::new(),
            error: Some(e),
        },
    }
}

/// The poller configuration for the Architect this process serves.
fn config(
    get: &dyn Fn(&str) -> Option<String>,
) -> Result<trusty_architect::poller::Config, VerbError> {
    let cwd = std::env::current_dir()
        .map_err(|e| VerbError::Failed(format!("cannot read the current directory: {e}")))?;
    let home = dirs::home_dir()
        .ok_or_else(|| VerbError::Failed("cannot find the home directory".to_owned()))?;
    let root = verbs::project_root(get, &cwd);
    verbs::poll_config(&root, &home, get)
}

/// `input-state.py <targets>`: every target is checked before any capture.
fn input_state(targets: &[String], get: &dyn Fn(&str) -> Option<String>) -> Output {
    let parsed: Result<Vec<InputTarget>, VerbError> =
        targets.iter().map(|t| InputTarget::parse(t)).collect();
    let parsed = match parsed {
        Ok(parsed) => parsed,
        Err(e) => {
            return Output {
                stdout: String::new(),
                error: Some(e),
            };
        }
    };
    // `input-state.py` reads TMUX_SOCKET the same way: empty means the default.
    let tmux = get("TMUX_SOCKET")
        .filter(|s| !s.is_empty())
        .map_or_else(SystemTmux::new, SystemTmux::with_socket);
    verbs::input_state(&tmux, &parsed)
}

/// The capture bytes from `file`, or from `stdin` when it is absent or `-`.
fn read_capture(file: Option<&Path>, stdin: &mut dyn Read) -> Result<Vec<u8>, VerbError> {
    let mut bytes = Vec::new();
    match file {
        None => stdin.read_to_end(&mut bytes).map(drop),
        Some(path) if path == Path::new("-") => stdin.read_to_end(&mut bytes).map(drop),
        Some(path) => std::fs::read(path).map(|b| bytes = b),
    }
    .map_err(|e| {
        let from = file.map_or_else(|| PathBuf::from("stdin"), Path::to_path_buf);
        VerbError::Failed(format!("cannot read {}: {e}", from.display()))
    })?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command};

    fn parse(args: &[&str]) -> ArchitectAction {
        let mut argv = vec!["tm", "architect"];
        argv.extend_from_slice(args);
        match Cli::try_parse_from(argv).map(|c| c.command) {
            Ok(Some(Command::Architect { action })) => action,
            other => panic!("expected tm architect, got {other:?}"),
        }
    }

    /// Each verb takes its script's arguments; argparse's refusals are
    /// clap's exit-2 refusals.
    #[test]
    fn cli_parses_architect_verbs() {
        assert!(matches!(
            parse(&["poll"]),
            ArchitectAction::Poll { once: false, ref interval, dry_run: false } if interval == "60"
        ));
        assert!(matches!(
            parse(&["poll", "--once", "--interval", "30", "--dry-run"]),
            ArchitectAction::Poll { once: true, ref interval, dry_run: true } if interval == "30"
        ));
        assert!(matches!(
            parse(&["poll", "--interval=45"]),
            ArchitectAction::Poll { ref interval, .. } if interval == "45"
        ));
        assert!(matches!(
            parse(&["input-state", "%1", "=tm-a:"]),
            ArchitectAction::InputState { ref targets } if targets == &["%1", "=tm-a:"]
        ));
        assert!(matches!(
            parse(&["classify", "-"]),
            ArchitectAction::Classify { file: Some(_) }
        ));
        assert!(matches!(
            parse(&["quiet"]),
            ArchitectAction::Quiet { file: None }
        ));
        assert!(matches!(
            parse(&["self-ctx", "/d"]),
            ArchitectAction::SelfCtx { dir: Some(_) }
        ));
        for bad in [
            &["architect", "poll", "--bogus"][..],
            &["architect", "poll", "--interval"],
            &["architect", "poll", "--once", "extra"],
            &["architect", "input-state"],
        ] {
            let mut argv = vec!["tm"];
            argv.extend_from_slice(bad);
            let err = Cli::try_parse_from(argv).expect_err("refused");
            assert_eq!(err.exit_code(), 2, "{bad:?}");
        }
    }

    /// #10063: every `dashboard` verb parses with the prototype's
    /// arguments; a missing argument is clap's exit-2 refusal.
    #[test]
    fn cli_parses_architect_dashboard_verbs() {
        let ok: &[&[&str]] = &[
            &["dashboard", "get", "items", "D01"],
            &["dashboard", "list", "items", "--status", "open"],
            &[
                "dashboard",
                "set",
                "items",
                "D01",
                "--json",
                "{}",
                "--if-version",
                "0",
            ],
            &["dashboard", "update", "items", "D01", "--json", "-"],
            &["dashboard", "delete", "items", "D01", "--if-version", "2"],
            &["dashboard", "batch", "ops.json"],
            &[
                "dashboard",
                "pruned",
                "--id",
                "D01",
                "--collection",
                "items",
            ],
            &["dashboard", "validate", "--schemas", "/s"],
            &["dashboard", "fsck"],
            &["dashboard", "tally", "--json"],
            &["dashboard", "footer", "--tailnet", "https://t/"],
            &["dashboard", "ids", "D"],
            &[
                "dashboard",
                "--root",
                "/r",
                "--format",
                "jsonl",
                "--actor",
                "a",
                "fsck",
            ],
        ];
        for args in ok {
            let mut argv = vec!["tm", "architect"];
            argv.extend_from_slice(args);
            assert!(Cli::try_parse_from(&argv).is_ok(), "{args:?}");
        }
        for bad in [
            &["dashboard", "get", "items"][..],
            &["dashboard", "set", "items", "D01"],
        ] {
            let mut argv = vec!["tm", "architect"];
            argv.extend_from_slice(bad);
            let err = Cli::try_parse_from(argv).expect_err("refused");
            assert_eq!(err.exit_code(), 2, "{bad:?}");
        }
    }

    #[test]
    fn classify_reads_stdin_and_fails_closed_on_an_empty_capture() {
        let get = |_: &str| None;
        let out = execute(
            ArchitectAction::Classify { file: None },
            &get,
            &mut &b""[..],
        );
        assert_eq!(out.stdout, "");
        assert_eq!(out.error.map(|e| e.exit_code()), Some(1));
        let out = execute(
            ArchitectAction::Classify {
                file: Some(PathBuf::from("/nonexistent/capture.ansi")),
            },
            &get,
            &mut &b""[..],
        );
        assert_eq!(out.error.map(|e| e.exit_code()), Some(1));
    }
}
