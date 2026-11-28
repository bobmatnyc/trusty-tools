//! `trusty-architect-ids36`: the Rust port of `ids36.py`'s command line (#12029 P2a).
//!
//! Why: the drop-in for `python3 scripts/ids36.py next <digits>` once the
//! replay gate passes.
//! What: forwards argv to `trusty_architect::ids36::run` and writes its
//! streams; an argument that is not UTF-8 is read lossily (it is never two
//! base-34 digits, so it fails the same way).
//! Test: `shipped_ids36_next_and_usage`.

use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let out = trusty_architect::ids36::run(&args);
    // A closed pipe on the reader's side is not this program's failure.
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    ExitCode::from(u8::try_from(out.code).unwrap_or(2))
}
