//! `trusty-architect-dashboard-db`: the Rust port of `dashboard-db.py` (#12029 P2g).
//!
//! Why: the drop-in for `python3 scripts/dashboard-db.py ...` once the replay
//! gate passes; it writes the same store files under the same lock.
//! What: forwards argv and stdin to `trusty_architect::dashboard::pydb::run`
//! on the system clock and writes its streams. The checkout is the working
//! directory (callers run from the Architect checkout, where the script's
//! store, schemas and instance config live). The footer's reminders read
//! runs `datg` from `PATH`; the questions kick is one in-process
//! `questions::rc` pass on the live tmux, waited for at most
//! [`trusty_architect::TMUX_TIMEOUT`]. An argument that is not UTF-8 is
//! read lossily.
//! Test: `shipped_dashboard_db_tally_and_usage`.

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;

use trusty_architect::dashboard::pydb::{Kick, Options, run};
use trusty_architect::questions::rc;
use trusty_architect::{SystemClock, TMUX_TIMEOUT};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let env = |k: &str| std::env::var(k).ok();
    let clock = Arc::new(SystemClock);
    // The kick's own stdout (`questions_rc`'s one line) is not this command's output.
    let kick: Kick = {
        let (cwd, clock) = (cwd.clone(), clock.clone());
        Arc::new(move || -> Result<(), String> {
            let env = |k: &str| std::env::var(k).ok();
            let o = rc::Options::live(&cwd, &env, clock.clone()).map_err(|e| e.to_string())?;
            let done = rc::run(&[], &o);
            match done.code {
                0 => Ok(()),
                _ => Err(done
                    .stderr
                    .trim_end()
                    .trim_start_matches("questions_rc: ")
                    .to_owned()),
            }
        })
    };
    let opts = Options {
        checkout: cwd.clone(),
        cwd: cwd.clone(),
        env: &env,
        clock: clock.clone(),
        datg: "datg".into(),
        kick,
        kick_timeout: TMUX_TIMEOUT,
    };
    let out = run(&args, &mut std::io::stdin(), &opts);
    // A closed pipe on the reader's side is not this program's failure.
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    ExitCode::from(u8::try_from(out.code).unwrap_or(2))
}
