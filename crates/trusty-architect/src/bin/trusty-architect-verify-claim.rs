//! `trusty-architect-verify-claim`: the Rust port of `verify-claim.py` (#12028 P1g).
//!
//! Why: the drop-in for the script's command line once the replay gate passes.
//! What: forwards argv to `trusty_architect::verify_claim::cli::run` with the
//! process environment and stdin. `local/architect.json` is read under the
//! Architect checkout at or above the working directory (the nearest
//! directory holding `ARCHITECT.md`), or from the file `ARCHITECT_CONFIG`
//! names; with neither, the run exits 1 before any command starts (#12073).
//! Read-only: it runs only the guarded gh, git and curl reads.
//! Test: `shipped_verify_claim_checks_a_pr_from_a_foreign_directory`,
//! `shipped_verify_claim_without_a_checkout_exits_1`,
//! `shipped_verify_claim_strips_tokens_from_gh_children`.

use std::io::Write;
use std::process::ExitCode;

use trusty_architect::verify_claim::cli::{Options, run};
use trusty_architect::writers::find_checkout;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let checkout = std::env::current_dir()
        .ok()
        .as_deref()
        .and_then(find_checkout);
    let environ: Vec<(String, String)> = std::env::vars_os()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .collect();
    let mut stdin = std::io::stdin();
    let mut opts = Options {
        checkout,
        environ,
        stdin: &mut stdin,
    };
    let out = run(&args, &mut opts);
    // A closed pipe on the reader's side is not this program's failure.
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    ExitCode::from(u8::try_from(out.code).unwrap_or(2))
}
