//! `trusty-architect-state-archive`: the Rust port of `state-archive.py` (#12028 P1e).
//!
//! Why: the drop-in for the script's command line once the replay gate passes.
//! What: forwards argv to `trusty_architect::writers::state_archive::run`.
//! Without `--state` / `--archive` it uses `local/state.md` and
//! `local/state-archive.md` under the Architect checkout at or above the
//! working directory (the nearest directory holding `ARCHITECT.md`); with
//! none found such a run exits 1 (#12073). It prints the script's name in
//! usage text.
//! Test: `shipped_state_archive_moves_sections_in_the_working_directory`,
//! `shipped_state_archive_finds_the_checkout_from_a_foreign_directory`.

use std::io::Write;
use std::process::ExitCode;

use trusty_architect::writers::find_checkout;
use trusty_architect::writers::state_archive::{Options, run};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // #12073: defaults hang off the checkout; explicit paths need none.
    let root = std::env::current_dir()
        .ok()
        .and_then(|cwd| find_checkout(&cwd));
    let opts = Options {
        prog: "state-archive.py".into(),
        default_state: root.as_ref().map(|r| r.join("local/state.md")),
        default_archive: root.as_ref().map(|r| r.join("local/state-archive.md")),
    };
    let out = run(&args, &opts);
    // A closed pipe on the reader's side is not this program's failure.
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    ExitCode::from(u8::try_from(out.code).unwrap_or(2))
}
