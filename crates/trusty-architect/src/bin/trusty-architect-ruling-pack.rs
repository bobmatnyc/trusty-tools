//! `trusty-architect-ruling-pack`: the Rust port of `ruling_pack.py` (#12028 P1i).
//!
//! Why: the drop-in for the script's command line once the replay gate passes.
//! What: forwards argv to `trusty_architect::ruling_pack::cli::run` with the
//! system clock. The Architect checkout (the script's `ROOT`) is the nearest
//! directory at or above the working directory holding `ARCHITECT.md`; it
//! holds the default key map and the pack directory. Without one, a run
//! that passes `--keys` and `--no-write` still works; any other run exits 1
//! before a backend call. `trusty-memory`, `trusty-search` and `rg` are
//! found on `PATH`.
//! Test: `shipped_ruling_pack_runs_all_three_backends`,
//! `shipped_ruling_pack_without_a_checkout_needs_keys_and_no_write`.

use std::io::Write;
use std::process::ExitCode;

use trusty_architect::SystemClock;
use trusty_architect::ruling_pack::cli::{Options, run};
use trusty_architect::writers::find_checkout;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let checkout = std::env::current_dir()
        .ok()
        .as_deref()
        .and_then(find_checkout);
    let out = run(
        &args,
        &Options {
            checkout,
            clock: &SystemClock,
        },
    );
    // A closed pipe on the reader's side is not this program's failure.
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    ExitCode::from(u8::try_from(out.code).unwrap_or(2))
}
