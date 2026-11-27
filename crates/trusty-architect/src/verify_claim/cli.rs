//! `main()` of `verify-claim.py` (#12028 P1g).
//!
//! Why: the binary and the replay entry point share one command line, so a
//! clean replay proves the shipped behaviour.
//! What: [`run`] takes exactly one EVENT argument (`-h`/`--help` or any
//! other count prints the usage, exit 2), loads the event (`BADINPUT`, exit
//! 2), reads `repos` from the checkout's `local/architect.json` (or the file
//! `ARCHITECT_CONFIG` names) and prints one `CONFIG` line when that fails,
//! then prints the [`super::Verifier`] lines: exit 1 on any `MISMATCH`, else
//! 0. An exception the script would raise prints its traceback's last line,
//! exit 1. Without a checkout and without `ARCHITECT_CONFIG`, exit 1 before
//! any command runs (#12073).
//! Test: `tests/replay_verify_claim.rs`, `tests/verify_claim_smoke.rs`.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::{LoadFailure, Status, Verifier, load_event, repo_maps};
use crate::architect_config;
use crate::spec_drift::Outcome;
use crate::writers::no_checkout;

/// The script's name in its own messages.
pub const PROG: &str = "verify-claim.py";
const USAGE: &str = "usage: verify-claim.py EVENT   (file path, '-' for stdin, or literal JSON)\n";

/// What one run reads besides its arguments.
pub struct Options<'a> {
    /// The Architect checkout `local/architect.json` sits under.
    pub checkout: Option<PathBuf>,
    /// The caller's environment (`os.environ`).
    pub environ: Vec<(String, String)>,
    /// Standard input, for the `-` event.
    pub stdin: &'a mut dyn Read,
}

fn done(stdout: String, stderr: String, code: i32) -> Outcome {
    Outcome {
        stdout,
        stderr,
        code,
    }
}

/// `main(argv)`.
///
/// Why: the script's command line, for the binary and the replay port.
/// What: see the module doc.
/// Test: `verify_claim_recordings_replay_clean_against_the_port`,
/// `shipped_verify_claim_without_a_checkout_exits_1`.
pub fn run(args: &[String], opts: &mut Options<'_>) -> Outcome {
    if args.len() != 1 || args[0] == "-h" || args[0] == "--help" {
        return done(String::new(), USAGE.into(), 2);
    }
    let event = match load_event(&args[0], opts.stdin) {
        Ok(ev) => ev,
        Err(LoadFailure::Bad(e)) => return done(String::new(), format!("BADINPUT {e}\n"), 2),
        Err(LoadFailure::Raised(line)) => return done(String::new(), format!("{line}\n"), 1),
    };
    let environ = opts.environ.clone();
    let env = |k: &str| environ.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let named = env("ARCHITECT_CONFIG").is_some_and(|v| !v.is_empty());
    let root = match (&opts.checkout, named) {
        (Some(root), _) => root.clone(),
        (None, true) => PathBuf::from("."),
        (None, false) => return no_checkout(PROG, "; set ARCHITECT_CONFIG"),
    };
    let mut stderr = String::new();
    let mut maps = (Vec::new(), Vec::new());
    match architect_config::load(Path::new(&root), &env) {
        Ok(config) => {
            if config.repos.is_empty() {
                stderr += &format!(
                    "CONFIG {}: no `repos` key; gh claims are UNCHECKED\n",
                    config.path
                );
            }
            match repo_maps(&config, env("HOME").as_deref()) {
                Ok(m) => maps = m,
                Err(e) => stderr += &format!("CONFIG {e}; gh claims are UNCHECKED\n"),
            }
        }
        Err(e) => stderr += &format!("CONFIG {e}; gh claims are UNCHECKED\n"),
    }
    match Verifier::new(&event, maps, environ.clone()).verify() {
        Ok(results) => {
            let mut stdout = String::new();
            for (_, line) in &results {
                stdout += line;
                stdout.push('\n');
            }
            let code = i32::from(results.iter().any(|(s, _)| *s == Status::Mismatch));
            done(stdout, stderr, code)
        }
        Err(exc) => {
            stderr += &exc.0;
            stderr.push('\n');
            done(String::new(), stderr, 1)
        }
    }
}
