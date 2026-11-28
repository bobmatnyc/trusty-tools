//! `trusty-architect-aliases`: the Rust port of `aliases.py` (#12028 P1e).
//!
//! Why: the drop-in for the script's command line once the replay gate passes.
//! What: forwards argv to `trusty_architect::writers::aliases::run`. The
//! table is `$ALIASES_FILE`, or `local/aliases.json` under the Architect
//! checkout at or above the working directory (the nearest directory holding
//! `ARCHITECT.md`); with neither it exits 1 and touches nothing (#12073).
//! Test: `shipped_aliases_add_writes_the_table_atomically`,
//! `shipped_aliases_finds_the_checkout_from_a_foreign_directory`.

use std::io::Write;
use std::process::ExitCode;

use trusty_architect::writers::aliases::{Options, run};
use trusty_architect::writers::checkout_from_cwd;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let aliases_file = std::env::var("ALIASES_FILE").ok();
    // #12073: the script's `ALIASES_FILE or DEFAULT_PATH`; the default is the
    // checkout's table, never one under the working directory.
    let default_path = if aliases_file.as_deref().is_some_and(|p| !p.is_empty()) {
        Default::default()
    } else {
        match checkout_from_cwd("aliases.py", "; set ALIASES_FILE") {
            Ok((_, root)) => root.join("local/aliases.json"),
            Err(refused) => return finish(&refused),
        }
    };
    let mut stdin = std::io::stdin();
    let mut opts = Options {
        default_path,
        aliases_file,
        stdin: &mut stdin,
    };
    finish(&run(&args, &mut opts))
}

fn finish(out: &trusty_architect::writers::Outcome) -> ExitCode {
    // A closed pipe on the reader's side is not this program's failure.
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    ExitCode::from(u8::try_from(out.code).unwrap_or(2))
}
