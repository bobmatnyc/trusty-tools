//! `trusty-architect-level-lint`: the Rust port of `level_lint.py` (#12028).
//!
//! Why: the dashboard and the hooks check owner-facing text against the
//! owner's technical level; this binary is the drop-in for the script's
//! command line once the replay gate passes.
//! What: forwards argv and stdin to `trusty_architect::level_lint::run`. With
//! no `--level` it reads the stored level from `local/owner-queue` under the
//! current directory (callers run from the Architect checkout, where the
//! script's store lives), with `DASHBOARD_FORMAT` passed through. It reads
//! no clock and no clock variable.
//! Test: `shipped_level_lint_command_warns_and_exits_like_the_script`.

use std::io::Write;
use std::process::ExitCode;

use trusty_architect::level_lint::{Options, run};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let opts = Options {
        prog: "trusty-architect-level-lint".into(),
        store_root: cwd.join("local/owner-queue"),
        dashboard_format: std::env::var("DASHBOARD_FORMAT").ok(),
    };
    let out = run(&args, &mut std::io::stdin(), &opts);
    // A closed pipe on the reader's side is not this program's failure.
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    ExitCode::from(u8::try_from(out.code).unwrap_or(2))
}
