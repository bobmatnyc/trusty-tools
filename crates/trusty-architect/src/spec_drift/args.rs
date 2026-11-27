//! argparse's behaviour for the script's three options (`-h`, `--root ROOT`,
//! `--no-log`) through the shared [`crate::argparse`] parser, plus the error
//! lines and the help text, as Python 3.14 prints them without a terminal.
//! Test: the `bad-arg-*` recordings in `tests/replay_spec_drift_check.rs`;
//! `help_drops_the_instance_id_from_the_description` for the help text.

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
// #12028: the script's description names an instance id; the port drops it.
pub(super) fn help(prog: &str) -> String {
    format!(
        "{}\nAnti-drift check for SPEC-ARCHITECT-COMPONENTS.\n\noptions:\n  \
         -h, --help   show this help message and exit\n  --root ROOT\n  \
         --no-log     do not append to the soak log\n",
        usage(prog)
    )
}

/// The stderr text of an argparse error.
pub(super) fn error(prog: &str, msg: &str) -> String {
    argparse::error(&usage(prog), prog, msg)
}

#[cfg(test)]
mod tests {
    use super::help;

    // #12028: the one divergence from the script; its help description ends in an instance id the public port omits.
    #[test]
    fn help_drops_the_instance_id_from_the_description() {
        let text = help("spec_drift_check.py");
        assert_eq!(
            text,
            "usage: spec_drift_check.py [-h] [--root ROOT] [--no-log]\n\n\
             Anti-drift check for SPEC-ARCHITECT-COMPONENTS.\n\noptions:\n  \
             -h, --help   show this help message and exit\n  --root ROOT\n  \
             --no-log     do not append to the soak log\n"
        );
        let description = text.lines().nth(2).unwrap();
        assert!(!description.contains('('), "{description}");
    }
}
