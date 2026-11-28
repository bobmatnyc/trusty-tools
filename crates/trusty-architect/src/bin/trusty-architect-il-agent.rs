//! `trusty-architect-il-agent`: the Rust port of `il_agent.py` (#12028 P1f).
//!
//! Why: the drop-in for the script's command line once the replay gate passes.
//! What: forwards argv to `trusty_architect::il_agent::cli::run`. The ledger
//! is `--project`, `$ILIO_PROJECT` or `local/il`, and the R library
//! `$ILIO_DIR` or `local/ilio`, both under the Architect checkout at or above
//! the working directory (the nearest directory holding `ARCHITECT.md`). A
//! default needed without a checkout exits 1 and reads nothing (#12073). The
//! `IL:` line of `dispatch` names this binary. `$TMUX_PANE` is resolved by
//! tmux, bounded at 5 s as `datg.py` bounds it.
//! Test: `shipped_il_agent_brief_reads_the_checkout_ledger`,
//! `shipped_il_agent_dispatch_names_the_binary`,
//! `shipped_il_agent_handback_validates_stdin`,
//! `shipped_il_agent_finds_the_checkout_from_a_foreign_directory`.

use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

use trusty_architect::SystemTmux;
use trusty_architect::il_agent::cli::{Options, run};
use trusty_architect::writers::find_checkout;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let checkout = std::env::current_dir()
        .ok()
        .as_deref()
        .and_then(find_checkout);
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "trusty-architect-il-agent".to_owned());
    let env = |k: &str| std::env::var(k).ok();
    let tmux = SystemTmux::new().with_timeout(Duration::from_secs(5));
    let mut stdin = std::io::stdin();
    let mut opts = Options {
        checkout,
        ilio_project: env("ILIO_PROJECT"),
        ilio_dir: env("ILIO_DIR"),
        brief_cmd: vec![exe],
        env: &env,
        tmux: &tmux,
        stdin: &mut stdin,
    };
    let out = run(&args, &mut opts);
    // A closed pipe on the reader's side is not this program's failure.
    let _ = std::io::stdout().write_all(out.stdout.as_bytes());
    let _ = std::io::stderr().write_all(out.stderr.as_bytes());
    ExitCode::from(u8::try_from(out.code).unwrap_or(2))
}
