//! The auditor client's binary.
//!
//! Why: deliberately the thinnest possible shim. #5502 requires every capability
//! to be reachable from the CLI forever, and the way that stays true is that
//! there is no logic here to diverge from what the library does — parse, read
//! the process environment once, dispatch, print. Anything added here would be
//! invisible to the Tauri shell that arrives in a later milestone.
//! What: builds a [`trusty_audit::Session`] from the parsed arguments and prints
//! the rendered outcome.
//! Test: the parse and the rendering are covered in `trusty_audit::cli`; this
//! file has no branch of its own to test.

use anyhow::{Context, Result};
use clap::Parser;
use trusty_audit::cli::{self, Cli};
use trusty_audit::config::EngagementConfig;
use trusty_audit::session::Session;
use trusty_audit::workdir::{WORKDIR_ENV, WorkDir};

// #5495: installing the pinned tools downloads, so `execute` is async and this
// shim owns the runtime. Nothing else moved here.
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let cwd = std::env::current_dir().context("cannot determine the current directory")?;
    let env_value = std::env::var(WORKDIR_ENV).ok();
    let work = WorkDir::resolve(cli.work_dir.clone(), env_value.as_deref(), &cwd);

    let mut session = Session::new(work)
        .with_config_path(EngagementConfig::resolve_path(cli.config.clone(), &cwd));
    if let Some(path) = &cli.manifest {
        session = session.with_manifest_path(path);
    }

    let outcome = session.execute(cli.to_command()).await?;
    print!("{}", cli::render(&outcome));

    // #5215/#5555: a sweep that partly failed, or an acquisition that skipped
    // repositories, returns `Ok` — the per-repo failures are data to render,
    // not an error — so the process status is decided from the outcome.
    // `taudit clone … && taudit run` reads the status, not the text. The
    // judgement itself is `Outcome::exit_code`, in the library, so the Tauri
    // shell reads the same verdict from the same place.
    let code = outcome.exit_code();
    if code == 0 {
        return Ok(());
    }
    // `process::exit` does not flush Rust's stdout buffer.
    std::io::Write::flush(&mut std::io::stdout()).context("cannot write the report")?;
    std::process::exit(code);
}
