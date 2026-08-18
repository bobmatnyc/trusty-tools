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

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

use anyhow::{Context, Result};
use clap::Parser;
use trusty_audit::cli::{self, Cli};
use trusty_audit::config::EngagementConfig;
use trusty_audit::progress::terminal::TerminalProgress;
use trusty_audit::run::RunOptions;
use trusty_audit::session::{Command, Session};
use trusty_audit::workdir::{WORKDIR_ENV, WorkDir};

// #5495: installing the pinned tools downloads, so `execute` is async and this
// shim owns the runtime. Nothing else moved here.
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let cwd = std::env::current_dir().context("cannot determine the current directory")?;
    let env_value = std::env::var(WORKDIR_ENV).ok();
    // #5915: the home directory is read here, with the rest of the environment,
    // so `resolve` stays a pure function of what it is handed.
    let home = dirs::home_dir();
    let work = WorkDir::resolve(
        cli.work_dir.clone(),
        env_value.as_deref(),
        home.as_deref(),
        &cwd,
    );

    // #5797: the policy lives on `Session`, so the Tauri shell sets it the same
    // way rather than reimplementing when to install.
    // #5823: the CLI's display is injected here rather than reached for inside
    // the library, which is what keeps `Session::execute` callable by a front
    // end that has no terminal. `TerminalProgress` autodetects stderr: a
    // spinner on a TTY, plain `info:` lines everywhere else.
    let command = cli.to_command();
    let config_path = EngagementConfig::resolve_path(cli.config.clone(), &cwd);

    // #5885: a bare launch ON A TERMINAL walks the operator through registration
    // and on into the sweep, instead of printing a card telling them to run a
    // different command. `/dev/tty` is the probe, exactly as the credential
    // prompt uses it (#5868) — under `curl … | sh` stdin is the pipe carrying
    // the script, so it is never the thing to ask on. No terminal means no
    // interactive path and the launch prints the card it always did.
    let mut terminal = cli::registration::is_interactive(&command)
        .then(cli::credential::DevTty::open)
        .and_then(Result::ok);

    // #5868: the terminal prompt lives HERE, in the front end, and only the
    // resolved key crosses into the library — `Session::execute` is what the
    // Tauri shell calls, and it has no terminal to ask on. The source is
    // reported on stderr because stdout carries the report.
    //
    // #5885: the interactive launch resolves the key the SWEEP needs, before
    // registration, so the operator answers key → targets → run in that order
    // rather than being stopped for a credential after naming their targets.
    let resolving_for = match terminal {
        Some(_) => Command::Run(RunOptions::default()),
        None => command.clone(),
    };
    let credential = cli::credential::resolve_for(&resolving_for, &config_path)?;
    if let Some(resolved) = &credential {
        eprintln!("{}", resolved.source().describe());
    }

    let mut session = Session::new(work)
        .with_config_path(config_path)
        .with_credential(credential.map(cli::credential::Resolved::into_key))
        .with_auto_install(!cli.no_install)
        .with_progress(std::sync::Arc::new(TerminalProgress::to_stderr()));
    if let Some(path) = &cli.manifest {
        session = session.with_manifest_path(path);
    }

    // #5885: the interactive launch drives the same `Session::execute` door,
    // several times — registration, then the guided flow, then the sweep. What
    // it returns is the last outcome, which is what stdout carries.
    let outcome = match &mut terminal {
        Some(tty) => cli::registration::guided_at_the_terminal(&session, tty).await?,
        None => session.execute(command).await?,
    };
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
