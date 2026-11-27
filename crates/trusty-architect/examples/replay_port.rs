//! Replay entry point for the Architect script ports (#12028); never ships.
//!
//! Why: a recording made with `record --clock` hands its instant to the
//! target in `TRUSTY_REPLAY_CLOCK`, and D6a keeps that variable out of every
//! shipped binary. One dispatching example serves every port instead of one
//! stand-in per script.
//! What: `replay_port <script> [ARGS...]` runs the named port's library
//! function on a `FakeClock` at the recorded instant (the real clock when the
//! variable is absent) and writes its streams. The recordings always pass
//! `--root`, so the default root is the current directory.
//! Test: `spec_drift_recordings_replay_clean_against_the_port`.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use trusty_architect::replay::CLOCK_ENV;
use trusty_architect::spec_drift;
use trusty_architect::{Clock, FakeClock, SystemClock};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let script = args.next().unwrap_or_default();
    let rest: Vec<String> = args.collect();
    let frozen = std::env::var(CLOCK_ENV)
        .ok()
        .and_then(|v| v.parse::<i64>().ok());
    let clock: Box<dyn Clock> = match frozen {
        Some(t) => Box::new(FakeClock::new(t as f64)),
        None => Box::new(SystemClock),
    };
    let out = match script.as_str() {
        "spec_drift_check" => {
            let opts = spec_drift::Options {
                prog: "spec_drift_check.py".into(),
                default_root: PathBuf::from("."),
                dashboard_format: std::env::var("DASHBOARD_FORMAT").ok(),
            };
            spec_drift::run(&rest, &opts, clock.as_ref())
        }
        other => {
            eprintln!("replay_port: no port named {other:?}; known: spec_drift_check");
            return ExitCode::from(2);
        }
    };
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    ExitCode::from(u8::try_from(out.code).unwrap_or(2))
}
