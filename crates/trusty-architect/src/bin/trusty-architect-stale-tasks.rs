//! `trusty-architect-stale-tasks`: the Rust port of `stale-tasks.py` (#12028 P1d).
//!
//! Why: the Architect parks idle dashboard tasks from this list; the binary
//! is the drop-in for the script's command line once the replay gate passes.
//! What: forwards argv to `trusty_architect::tasks::stale::run` on the system
//! clock. With no `--root` and no `$DASHBOARD_ROOT` it reads
//! `local/owner-queue` under the working directory (callers run from the
//! Architect checkout, where the script's store lives); `$DASHBOARD_FORMAT`
//! is passed through. It reads no clock variable.
//! Test: `shipped_stale_tasks_command_reads_the_working_directory_store`.

use std::io::Write;
use std::process::ExitCode;

use trusty_architect::SystemClock;
use trusty_architect::tasks::stale::{Options, run};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let opts = Options {
        prog: "trusty-architect-stale-tasks".into(),
        default_root: cwd.join("local/owner-queue"),
        env_root: std::env::var("DASHBOARD_ROOT").ok(),
        dashboard_format: std::env::var("DASHBOARD_FORMAT").ok(),
    };
    let out = run(&args, &opts, &SystemClock);
    // A closed pipe on the reader's side is not this program's failure.
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    ExitCode::from(u8::try_from(out.code).unwrap_or(2))
}
