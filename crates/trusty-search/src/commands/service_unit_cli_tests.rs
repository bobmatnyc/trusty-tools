//! End-to-end clap tests for [`super::parse_truthy_bool`] as wired into
//! `--no-auto-discover` (issue #4823).
//!
//! Why: `parse_truthy_bool` being correct is not the same as clap *using*
//! it — the arg needs `num_args`/`require_equals`/`default_missing_value`
//! alongside `value_parser` or the bare flag stops working. These tests
//! drive the real `Cli` so a regression in that wiring is caught.
//! They live here rather than in `main.rs` because `main.rs` sits at its
//! frozen `check_line_cap` budget; `Cli` is still reachable because this
//! module is a descendant of the binary's crate root.
//! What: parses representative `start` argument vectors and asserts the
//! resolved boolean. The env-var path is not driven here — mutating the
//! process env would race a parallel test binary — but clap routes an env
//! value through the same `value_parser` these tests exercise.
//! Test: this module — run with `cargo test -p trusty-search`.

use crate::{Cli, Commands};
use clap::Parser;

/// Parse `args` and yield the resolved flag, or the clap error rendered as
/// a string (`clap::Error` is not `PartialEq`, so it cannot be asserted on
/// directly).
fn parse_flag(args: &[&str]) -> Result<bool, String> {
    match Cli::try_parse_from(args)
        .map_err(|e| e.to_string())?
        .command
    {
        Commands::Start { args } => Ok(args.no_auto_discover),
        _ => panic!("expected Commands::Start"),
    }
}

/// Why: every existing caller — operators, and the `start` detach path in
/// `commands::start::daemon` — passes the flag bare. Requiring a value
/// would break all of them.
/// What: asserts bare presence still resolves to `true`, absence to `false`.
/// Test: this function.
#[test]
fn bare_flag_still_means_true() {
    assert_eq!(
        parse_flag(&["trusty-search", "start", "--no-auto-discover"]),
        Ok(true)
    );
    assert_eq!(parse_flag(&["trusty-search", "start"]), Ok(false));
}

/// Why (issue #4823): this is the trap. `TRUSTY_NO_AUTO_DISCOVER=1` flows
/// through this arg's `value_parser`; with the pre-fix bare `bool` clap
/// used strict `FromStr<bool>` and answered
/// `invalid value '1' … [possible values: true, false]`, so the daemon
/// refused to boot from any unit carrying that value.
/// What: asserts the documented truthy and falsey spellings parse.
/// Test: this function.
#[test]
fn value_form_accepts_documented_spellings() {
    for spelling in ["1", "true", "yes", "on", "TRUE"] {
        let arg = format!("--no-auto-discover={spelling}");
        assert_eq!(
            parse_flag(&["trusty-search", "start", &arg]),
            Ok(true),
            "--no-auto-discover={spelling} must parse as true (issue #4823)"
        );
    }
    for spelling in ["0", "false", "no", "off"] {
        let arg = format!("--no-auto-discover={spelling}");
        assert_eq!(
            parse_flag(&["trusty-search", "start", &arg]),
            Ok(false),
            "--no-auto-discover={spelling} must parse as false"
        );
    }
}

/// Why: loosening the parser must not turn a typo into a silent `false` —
/// that would re-enable a scan the operator disabled, the same
/// silent-capability-change defect class as #4823 itself.
/// What: asserts an unrecognised spelling is a parse error.
/// Test: this function.
#[test]
fn value_form_rejects_garbage() {
    assert!(parse_flag(&["trusty-search", "start", "--no-auto-discover=ture"]).is_err());
}

/// Why (#9536): `--port` and `--no-http` did nothing since #9214 and were
/// accepted for one release only. Now a unit or script that still passes one
/// must fail at parse time, not start a daemon that silently ignores it.
/// What: every spelling of either flag is a clap `UnknownArgument` error that
/// names the flag; a bare `start` still parses.
/// Test: this function.
#[test]
fn removed_start_flags_are_unknown_arguments() {
    assert!(Cli::try_parse_from(["trusty-search", "start"]).is_ok());
    for (args, flag) in [
        (&["--port", "7878"][..], "--port"),
        (&["--port=7878"][..], "--port"),
        (&["--no-http"][..], "--no-http"),
        (&["--no-http=1"][..], "--no-http"),
    ] {
        let argv = ["trusty-search", "start"].iter().chain(args).copied();
        let err = match Cli::try_parse_from(argv) {
            Ok(_) => panic!("#9536: `start {args:?}` must be rejected"),
            Err(err) => err,
        };
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::UnknownArgument,
            "{args:?}: {err}"
        );
        assert!(err.to_string().contains(flag), "{args:?}: {err}");
    }
}

/// Why: #9214 — `start --socket` names the socket the daemon binds; a relative
/// path would resolve against the daemon's cwd, so it is refused at parse time.
/// What: an absolute path parses; a relative one fails with an error naming
/// `--socket` and the absolute-path rule.
/// Test: this function.
#[test]
fn start_socket_flag_takes_an_absolute_path_and_refuses_a_relative_one() {
    let parse = |args: &[&str]| {
        Cli::try_parse_from(args)
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    assert_eq!(
        parse(&[
            "trusty-search",
            "start",
            "--socket",
            "/tmp/ts-9214/custom.sock"
        ]),
        Ok(())
    );
    let err = parse(&["trusty-search", "start", "--socket", "rel/custom.sock"])
        .expect_err("a relative --socket must be refused");
    assert!(err.contains("--socket"), "{err}");
    assert!(err.contains("absolute"), "{err}");
}
