//! `trusty-architect-spec-drift-check`: the Rust port of `spec_drift_check.py` (#12028).
//!
//! Why: registry job M0c runs the anti-drift check daily; this binary is the
//! drop-in it moves to once the replay gate passes.
//! What: forwards argv to `trusty_architect::spec_drift::run` on the real
//! clock, with the current directory as the default root (the job runs from
//! the checkout) and `DASHBOARD_FORMAT` passed through to the store as the
//! script's store reads it. It reads no clock variable.
//! Test: `shipped_spec_drift_command_prints_ten_results_and_logs_one_line`.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use trusty_architect::SystemClock;
use trusty_architect::spec_drift::{Options, run};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = Options {
        prog: "trusty-architect-spec-drift-check".into(),
        default_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        dashboard_format: std::env::var("DASHBOARD_FORMAT").ok(),
    };
    let out = run(&args, &opts, &SystemClock);
    // A closed pipe on the reader's side is not this program's failure.
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    ExitCode::from(u8::try_from(out.code).unwrap_or(2))
}
