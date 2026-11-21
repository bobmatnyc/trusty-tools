//! `trusty-architect-sched`: the Architect's job scheduler (MB1595 spike).
//!
//! Why: one launchd agent runs this binary as `daemon`; the other verbs are
//! the operator's and the Architect's tools around it.
//! What: collects the process environment, installs the stderr logger, calls
//! [`trusty_architect::sched::cli::execute`], prints its stdout and exits with
//! its code; an error prints on stderr and exits with the error's code.
//! Test: `tests/sched_cli.rs` drives `execute`; `tests/sched_daemon.rs` runs
//! this binary for the empty-config exit code.

use std::process::ExitCode;
use std::sync::Arc;

use trusty_architect::SystemClock;
use trusty_architect::sched::cli::{Deps, execute};
use trusty_architect::sched::{SystemRunner, logger};

fn main() -> ExitCode {
    if let Err(e) = logger::init() {
        eprintln!("log setup: {e}");
    }
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let deps = Deps {
        clock: &SystemClock,
        runner: Arc::new(SystemRunner),
        ops: None,
        env: std::env::vars().collect(),
        home,
        cwd: std::env::current_dir().unwrap_or_default(),
        scratch: std::env::temp_dir().join("trusty-architect-sched"),
    };
    match execute(&argv, &deps) {
        Ok(out) => {
            print!("{}", out.stdout);
            ExitCode::from(u8::try_from(out.code).unwrap_or(1))
        }
        Err(e) => {
            eprintln!("trusty-architect-sched: {e}");
            ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(1))
        }
    }
}
