//! `trusty-architect-aliases`: the Rust port of `aliases.py` (#12028 P1e).
//!
//! Why: the drop-in for the script's command line once the replay gate passes.
//! What: forwards argv to `trusty_architect::writers::aliases::run`. The
//! table is `$ALIASES_FILE`, or `local/aliases.json` under the working
//! directory (callers run from the Architect checkout).
//! Test: `shipped_aliases_add_writes_the_table_atomically`.

use std::io::Write;
use std::process::ExitCode;

use trusty_architect::writers::aliases::{Options, run};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let mut stdin = std::io::stdin();
    let mut opts = Options {
        default_path: cwd.join("local/aliases.json"),
        aliases_file: std::env::var("ALIASES_FILE").ok(),
        stdin: &mut stdin,
    };
    let out = run(&args, &mut opts);
    // A closed pipe on the reader's side is not this program's failure.
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    ExitCode::from(u8::try_from(out.code).unwrap_or(2))
}
