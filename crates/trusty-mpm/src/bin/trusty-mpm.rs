//! `trusty-mpm` — a compatibility alias that runs the `tm` installed beside it.
//!
//! Why: trusty-mpm ships one full binary, `tm` (owner ruling 2026-09-27). The
//! old second `[[bin]]` compiled and linked the whole CLI a second time. Paths
//! already installed still name `trusty-mpm`: the `com.trusty.mpm` launchd
//! plist, `trusty-mpm hook` commands persisted in Claude settings, and
//! `.mcp.json` entries. An upgrading `cargo install` deletes any binary the new
//! package no longer declares, so dropping the name would break those paths.
//! What: a std-only program that execs `<dir of this exe>/tm` with the same
//! argv, so the pid, stdio, and exit status are `tm`'s own. It does not link
//! the trusty-mpm library. Exits 127 when `tm` cannot be started.
//! Test: `trusty_mpm_alias::the_alias_prints_what_tm_prints`,
//! `trusty_mpm_alias::the_alias_forwards_stdin_and_the_exit_status`.

use std::path::PathBuf;
use std::process::{Command, ExitCode};

/// Exit status when `tm` cannot be started, the shell's "command not found".
const TM_NOT_STARTED: u8 = 127;

/// The `tm` that the same `cargo install` wrote next to this binary.
fn sibling_tm() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    Ok(exe.with_file_name(format!("tm{}", std::env::consts::EXE_SUFFIX)))
}

#[cfg(unix)]
fn run(tm: PathBuf) -> ExitCode {
    use std::os::unix::process::CommandExt as _;
    let mut args = std::env::args_os();
    let mut cmd = Command::new(&tm);
    // Keep argv[0] so clap's usage lines still read `trusty-mpm`.
    if let Some(arg0) = args.next() {
        cmd.arg0(arg0);
    }
    // `exec` only returns on failure.
    let err = cmd.args(args).exec();
    eprintln!("trusty-mpm: cannot run {}: {err}", tm.display());
    ExitCode::from(TM_NOT_STARTED)
}

#[cfg(not(unix))]
fn run(tm: PathBuf) -> ExitCode {
    match Command::new(&tm).args(std::env::args_os().skip(1)).status() {
        Ok(status) => ExitCode::from(status.code().map_or(1, |c| c as u8)),
        Err(err) => {
            eprintln!("trusty-mpm: cannot run {}: {err}", tm.display());
            ExitCode::from(TM_NOT_STARTED)
        }
    }
}

fn main() -> ExitCode {
    match sibling_tm() {
        Ok(tm) => run(tm),
        Err(err) => {
            eprintln!("trusty-mpm: cannot locate this executable: {err}");
            ExitCode::from(TM_NOT_STARTED)
        }
    }
}
