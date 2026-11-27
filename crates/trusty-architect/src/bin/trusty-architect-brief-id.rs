//! `trusty-architect-brief-id`: the Rust port of `brief-id.py` (#12028 P1e).
//!
//! Why: the drop-in for the script's command line once the replay gate passes.
//! What: forwards argv to `trusty_architect::writers::brief_ids::cli::run`
//! on the system clock. `REPO` is the Architect checkout at or above the
//! working directory (the nearest directory holding `ARCHITECT.md`), so the
//! registry is `local/briefs/registry.jsonl` under it; with none found it
//! exits 1 and touches nothing (#12073). It reads no clock variable.
//! Test: `shipped_brief_id_assigns_in_the_working_directory`,
//! `shipped_brief_id_finds_the_checkout_from_a_foreign_directory`.

use std::io::Write;
use std::process::ExitCode;

use trusty_architect::SystemClock;
use trusty_architect::writers::brief_ids::{Paths, cli::run};
use trusty_architect::writers::checkout_from_cwd;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // #12073: the checkout, never the working directory itself, is `REPO`.
    let out = match checkout_from_cwd("brief-id.py", "") {
        Ok((cwd, repo)) => run(&args, &Paths { repo }, &cwd, &SystemClock),
        Err(refused) => refused,
    };
    // A closed pipe on the reader's side is not this program's failure.
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    ExitCode::from(u8::try_from(out.code).unwrap_or(2))
}
