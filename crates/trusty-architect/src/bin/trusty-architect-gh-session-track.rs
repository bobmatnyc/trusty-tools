//! `trusty-architect-gh-session-track`: the Rust port of `gh-session-track.py` (#12087).
//!
//! Why: the drop-in for the script's command line once the replay gate passes.
//! What: forwards argv to `trusty_architect::gh_session_track::cli::run` with
//! the process environment and the system clock. The Architect checkout is
//! the nearest directory at or above the working directory holding
//! `ARCHITECT.md`; with none, the run exits 1 before any gh call and writes
//! nothing. gh runs read-only; the store is written through the crate's
//! `Store::batch` unless `--dry-run` is given.
//! Test: `shipped_gh_session_track_dry_run_from_a_foreign_directory`,
//! `shipped_gh_session_track_writes_from_a_foreign_directory`,
//! `shipped_gh_session_track_without_a_checkout_exits_1`,
//! `shipped_gh_session_track_strips_tokens_from_gh`.

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;

use trusty_architect::SystemClock;
use trusty_architect::gh_session_track::cli::{Options, run};
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
    let opts = Options {
        checkout,
        environ,
        clock: Arc::new(SystemClock),
    };
    let out = run(&args, &opts);
    // A closed pipe on the reader's side is not this program's failure.
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    ExitCode::from(u8::try_from(out.code).unwrap_or(2))
}
