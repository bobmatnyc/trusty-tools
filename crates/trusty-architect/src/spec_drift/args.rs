//! argparse's behaviour for the script's three options (`-h`, `--root ROOT`,
//! `--no-log`) through the shared [`crate::argparse`] parser, plus the error
//! lines and the help text, as Python 3.14 prints them without a terminal.
//! Test: the `bad-arg-*` and `help` recordings in `tests/replay_spec_drift_check.rs`.

use crate::argparse::{self, HELP, Opt};

/// The parse result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Parsed {
    /// Run with these settings.
    Run { root: Option<String>, no_log: bool },
    /// `-h` / `--help`: print the help, exit 0.
    Help,
    /// An argparse error message (exit 2).
    Error(String),
}

// See #12028: the parser moved to `crate::argparse`, shared with level_lint.
const OPTS: [Opt; 3] = [
    HELP,
    Opt {
        long: "--root",
        short: None,
        value: true,
    },
    Opt {
        long: "--no-log",
        short: None,
        value: false,
    },
];

/// Parses argv after the program name.
pub(super) fn parse(args: &[String]) -> Parsed {
    match argparse::parse(args, &OPTS, false) {
        argparse::Parsed::Run(a) => Parsed::Run {
            root: a.values[1].clone(),
            no_log: a.flags[2],
        },
        argparse::Parsed::Help => Parsed::Help,
        argparse::Parsed::Error(msg) => Parsed::Error(msg),
    }
}

fn usage(prog: &str) -> String {
    argparse::usage(prog, "[-h] [--root ROOT] [--no-log]")
}

/// The `-h` text.
pub(super) fn help(prog: &str) -> String {
    format!(
        "{}\nAnti-drift check for SPEC-ARCHITECT-COMPONENTS (ar-T6w).\n\noptions:\n  \
         -h, --help   show this help message and exit\n  --root ROOT\n  \
         --no-log     do not append to the soak log\n",
        usage(prog)
    )
}

/// The stderr text of an argparse error.
pub(super) fn error(prog: &str, msg: &str) -> String {
    argparse::error(&usage(prog), prog, msg)
}
