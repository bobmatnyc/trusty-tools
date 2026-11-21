//! `trusty-architect-il`: the Intent Ledger command, a drop-in for `datg.py` and `ilio.py`.
//!
//! Why: hooks and skills call a per-turn CLI where process start is the cost; this
//! binary starts in milliseconds and holds the ledger's lock and atomic-write rules.
//! What: forwards argv, stdin and the environment to `trusty_architect::il::run` and
//! writes its streams. `TRUSTY_IL_NOW` (epoch seconds) freezes the clock for parity tests.
//! Test: `binary_round_trip`; the parity tests run it against `datg.py` and `ilio.py`.

use std::io::{Read, Write};
use std::process::ExitCode;

use trusty_architect::il::{Env, run};
use trusty_architect::{Clock, FakeClock, SystemClock};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env = Env::from_process();
    let frozen = env.get("TRUSTY_IL_NOW").and_then(|v| v.parse::<f64>().ok());
    let clock: Box<dyn Clock> = match frozen {
        Some(t) => {
            // A leftover variable would freeze every timestamp this binary writes: say so each run.
            eprintln!(
                "trusty-architect-il: warning: TRUSTY_IL_NOW is set; the clock is frozen at {t} (test hook)"
            );
            Box::new(FakeClock::new(t))
        }
        None => Box::new(SystemClock),
    };
    let stdin = || {
        let mut bytes = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let out = run(&args, &stdin, &env, clock.as_ref());
    // A closed pipe on the reader's side is not this program's failure.
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    ExitCode::from(u8::try_from(out.code).unwrap_or(2))
}
