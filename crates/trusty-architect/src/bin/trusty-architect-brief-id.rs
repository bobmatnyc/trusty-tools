//! `trusty-architect-brief-id`: the Rust port of `brief-id.py` (#12028 P1e).
//!
//! Why: the drop-in for the script's command line once the replay gate passes.
//! What: forwards argv to `trusty_architect::writers::brief_ids::cli::run`
//! on the system clock, with the working directory as the checkout (`REPO`):
//! the registry is `local/briefs/registry.jsonl` under it. It reads no clock
//! variable.
//! Test: `shipped_brief_id_assigns_in_the_working_directory`.

use std::io::Write;
use std::process::ExitCode;

use trusty_architect::SystemClock;
use trusty_architect::writers::brief_ids::{Paths, cli::run};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let paths = Paths { repo: cwd.clone() };
    let out = run(&args, &paths, &cwd, &SystemClock);
    // A closed pipe on the reader's side is not this program's failure.
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    ExitCode::from(u8::try_from(out.code).unwrap_or(2))
}
