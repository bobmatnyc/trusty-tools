//! `trusty-architect-task-wait-check`: the Rust port of `task_wait_check.py` (#12028 P1d).
//!
//! Why: the advisory pass over what open ledger tasks wait on; the binary is
//! the drop-in for `python3 scripts/task_wait_check.py`.
//! What: reads the ledger under `local/il` and the dashboard items under
//! `local/owner-queue/items`, below the working directory (the Architect
//! checkout), and prints the advisory lines or `none`; exit 0 always. It
//! ignores its arguments and reads no clock.
//! Test: `shipped_task_commands_read_the_working_directory_ledger`.

use std::io::Write;
use std::process::ExitCode;

use trusty_architect::tasks::wait_check::run;

fn main() -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let env = |k: &str| std::env::var(k).ok();
    let out = run(
        &cwd.join("local/il"),
        &cwd.join("local/owner-queue/items"),
        &env,
    );
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    ExitCode::from(u8::try_from(out.code).unwrap_or(2))
}
