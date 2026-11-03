//! `tm hook --pm-guard` judges a command dense in `<<` in bounded time
//! (#9720).
//!
//! Why: the guard's here-document scans were quadratic in the `<<` count, and
//! the classifier runs them many times per command. The issue's 64 KiB
//! `time eval` repro took 15.7 s in the Bash classifier alone on EVO, past
//! the 10 s hook timeout, and Claude Code lets a killed hook's command
//! through.
//! What: pipes the repro into the real binary with a `pm` session stamp and
//! kills it past the bound, which fails the test.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_heredoc_density_9720`.

use crate::common;
use crate::tm_hook_pm_guard_trust_anchor_8878::{Fixture, bash_payload};

use std::io::Write;
use std::process::{Output, Stdio};

/// The issue's #9720 repro: `time eval` x12, then `<<x a.b ` up to the
/// 64 KiB cap.
fn time_eval_heredoc_repro_9720() -> String {
    let mut command = "time eval ".repeat(12);
    while command.len() + 8 <= 65_536 {
        command.push_str("<<x a.b ");
    }
    command
}

/// The bound on a hook's verdict on [`time_eval_heredoc_repro_9720`]: ten
/// times the slower hook's 3.5 s EVO debug-build time after #9720.
const REPRO_BOUND_9720: std::time::Duration = std::time::Duration::from_secs(40);

/// Wait for `child` until `bound`, killing it and panicking past the bound:
/// Claude Code kills a hook at its timeout, and a killed hook is an allow.
fn wait_within_9720(mut child: std::process::Child, bound: std::time::Duration) -> Output {
    let started = std::time::Instant::now();
    while child.try_wait().expect("poll the hook").is_none() {
        if started.elapsed() >= bound {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the hook gave no verdict within {bound:?}");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    eprintln!("#9720: the hook answered in {:?}", started.elapsed());
    child.wait_with_output().expect("hook output")
}

/// 🔴 #9720: the repro gets a verdict, and the guard exits 0, within the
/// bound.
#[test]
fn pm_guard_judges_the_dense_heredoc_repro_in_bounded_time_9720() {
    let fx = Fixture::new();
    let mut cmd = common::tm_command_in(&fx.home);
    cmd.args(["--url", "http://127.0.0.1:1", "hook", "--pm-guard"])
        .current_dir(&fx.project)
        .env("CLAUDE_PROJECT_DIR", &fx.project)
        .env("TRUSTY_MPM_SESSION_PROFILE", "pm")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn `tm hook --pm-guard`");
    let stdin = bash_payload(&fx, &time_eval_heredoc_repro_9720());
    let mut pipe = child.stdin.take().expect("child stdin");
    let _ = pipe.write_all(stdin.as_bytes());
    drop(pipe);
    let out = wait_within_9720(child, REPRO_BOUND_9720);
    assert!(out.status.success(), "the guard exits 0: {out:?}");
}
