//! The replay entry point's `dashboard-db` arm (#12029 P2g).
//!
//! Why: `examples/replay_port.rs` runs every port on the recorded instant;
//! this arm needs a kick and a `datg`, and only a replay may wire them to
//! the recording's fakes.
//! What: [`replay`] runs [`super::run`] with the working directory as the
//! checkout (the driver points the script's checkout at the sandbox root),
//! `datg` from `PATH` (the recording's fake), and a kick that reaches tmux
//! only on the private replay socket (every recording sets
//! `QUESTIONS_RC_KICK=off`). Never used by a shipped binary.
//! Test: `dashboard_db_recordings_replay_clean_against_the_port`.

use std::sync::Arc;

use super::cli::{Kick, Options, run};
use crate::clock::{Clock, FakeClock};
use crate::questions::cases::REPLAY_SOCKET;
use crate::questions::{Socket, TmuxCli, rc};
use crate::spec_drift::Outcome;

/// Runs the command line `args` at Unix time `at`.
pub fn replay(args: &[String], at: f64) -> Outcome {
    let clock: Arc<dyn Clock + Send + Sync> = Arc::new(FakeClock::new(at));
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let env = |k: &str| std::env::var(k).ok();
    let kick: Kick = {
        let (cwd, clock) = (cwd.clone(), clock.clone());
        Arc::new(move || -> Result<(), String> {
            let env = |k: &str| std::env::var(k).ok();
            let o = rc::Options {
                root: cwd.clone(),
                scripts: std::path::PathBuf::new(),
                store_root: cwd.join("local/owner-queue"),
                schema_dir: cwd.join("schemas/dashboard"),
                cwd: cwd.clone(),
                env: &env,
                tmux: TmuxCli::on_path_with_socket(Socket::Name(REPLAY_SOCKET.into())),
                clock: clock.clone(),
            };
            let done = rc::run(&[], &o);
            match done.code {
                0 => Ok(()),
                _ => Err(done.stderr.trim_end().to_owned()),
            }
        })
    };
    let o = Options {
        checkout: cwd.clone(),
        cwd: cwd.clone(),
        env: &env,
        clock: clock.clone(),
        datg: "datg".into(),
        kick,
        kick_timeout: crate::TMUX_TIMEOUT,
    };
    run(args, &mut std::io::stdin(), &o)
}
