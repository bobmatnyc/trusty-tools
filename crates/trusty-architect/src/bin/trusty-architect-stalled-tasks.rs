//! `trusty-architect-stalled-tasks`: the Rust port of `stalled_tasks.py`'s command line (#12028 P1d).
//!
//! Why: the Architect's pass prints the STALLED and OWNER WAIT lines; the
//! binary is the drop-in for `python3 scripts/stalled_tasks.py`.
//! What: reads the ledger under `local/owner-queue/../il` and the asks under
//! `local/owner-queue/items`, both below the working directory (the paths
//! the script builds beside its checkout), and prints both lines in full on
//! the system clock. Like the script it ignores its arguments, and reads no
//! clock variable.
//! Test: `shipped_task_commands_read_the_working_directory_ledger`.

use std::io::Write;
use std::process::ExitCode;

use trusty_architect::SystemClock;
use trusty_architect::tasks::stalled::run;

fn main() -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let items = cwd.join("local/owner-queue/items");
    let il = cwd.join("local/owner-queue/../il");
    let env = |k: &str| std::env::var(k).ok();
    let out = run(&il, &items, None, &env, &SystemClock);
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    ExitCode::from(u8::try_from(out.code).unwrap_or(2))
}
