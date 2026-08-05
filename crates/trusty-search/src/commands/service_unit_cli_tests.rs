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
        Commands::Start {
            no_auto_discover, ..
        } => Ok(no_auto_discover),
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
