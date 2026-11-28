//! `trusty-architect-dashboard-updates`: the Rust port of `dashboard_updates.py` (#12029 P2h).
//!
//! Why: the drop-in for `python3 scripts/dashboard_updates.py [note ...]`
//! once the replay gate passes; it writes the same store files under the
//! same lock.
//! What: forwards argv to `trusty_architect::dashboard::pyupdates::cli::run`
//! on the system clock and writes its streams. The checkout is the working
//! directory (callers run from the Architect checkout, where the script's
//! store, inbox and schemas live). An argument that is not UTF-8 is read
//! lossily.
//! Test: `shipped_dashboard_updates_note_and_usage`.

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;

use trusty_architect::SystemClock;
use trusty_architect::dashboard::pyupdates::cli::{Options, run};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let env = |k: &str| std::env::var(k).ok();
    let opts = Options {
        checkout: cwd.clone(),
        cwd,
        env: &env,
        clock: Arc::new(SystemClock),
    };
    let out = run(&args, &opts);
    // A closed pipe on the reader's side is not this program's failure.
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    ExitCode::from(u8::try_from(out.code).unwrap_or(2))
}
