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
use trusty_audit::session::Session;
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
    //
    // #5896 review: the PARSED CLI decides, not the `Command`. `to_command`
    // maps both a bare launch and `trusty-audit guided` onto `Command::Guided`,
    // so asking the command made the named verb block on a prompt.
    //
    // #5978: whether this is a BARE launch is kept, separately from whether a
    // terminal opened. The targets files are adopted on both — a scripted run
    // that quietly audits four of a `repos.txt`'s twenty repositories reports
    // success over the sixteen it never saw. What the terminal buys is the
    // review menu.
    let bare = cli::registration::is_interactive(&cli);
    let mut terminal = bare
        .then(cli::credential::DevTty::open)
        .and_then(Result::ok);

    // #5868: the terminal prompt lives HERE, in the front end, and only the
    // resolved key crosses into the library — `Session::execute` is what the
    // Tauri shell calls, and it has no terminal to ask on. The source is
    // reported on stderr because stdout carries the report.
    //
    // #5896 review: an interactive launch resolves NOTHING here. Resolving for
    // the sweep up front gave the read-only launch `CredentialNeed::Required`,
    // so a bare `trusty-audit` against a config with a blank key exited non-zero
    // at `CredentialNotEntered` after three mismatched retypes without ever
    // printing the status card. The key is resolved below, once the operator has
    // agreed to the sweep that needs it.
    let credential = match terminal {
        Some(_) => None,
        None => cli::credential::resolve_for(&command, &config_path)?,
    };
    report_source(credential.as_ref());

    let mut session = Session::new(work)
        .with_config_path(config_path.clone())
        // #6080: `render` with no arguments renders the package the recipient
        // unzipped and is standing in. The cwd is read here, with the rest of
        // the environment, so the library stays a function of what it is handed.
        .with_cwd(&cwd)
        .with_credential(credential.map(cli::credential::Resolved::into_key))
        .with_auto_install(!cli.no_install)
        .with_progress(std::sync::Arc::new(TerminalProgress::to_stderr()));
    if let Some(path) = &cli.manifest {
        session = session.with_manifest_path(path);
    }

    // #5885: the interactive launch drives the same `Session::execute` door,
    // several times — registration, then the guided flow, then the sweep. What
    // it returns is the last outcome, which is what stdout carries.
    // #5990: what the targets files did not register, kept so the process status
    // can be decided from it below.
    let mut adopted = None;
    let outcome = match &mut terminal {
        // #5970: the launch sets the engagement up when there is none — key,
        // config, tools, and only then targets. `from_environment` is the one
        // place the cold start's inputs are read; the seam exists so its other
        // branches are provable without a network and without an exported key.
        Some(tty) => match cli::registration::guided_at_the_terminal(
            &session,
            &cli::bootstrap::ColdStart::from_environment(),
            tty,
        )
        .await?
        {
            cli::registration::Launch::Reported(outcome) => *outcome,
            cli::registration::Launch::SweepConfirmed => {
                let sweep = cli::registration::confirmed_sweep();
                let credential = cli::credential::resolve_for(&sweep, &config_path)?;
                report_source(credential.as_ref());
                session
                    .clone()
                    .with_credential(credential.map(cli::credential::Resolved::into_key))
                    .execute(sweep)
                    .await?
            }
        },
        // #5978: no terminal, so no menu — the counts and the full list are
        // printed and the run proceeds. On stderr, because stdout carries the
        // report. A file with an unparseable line never gets this far: `adopt`
        // refuses the whole read, naming every bad line.
        None => {
            if bare {
                adopted = cli::registration::adopt_without_a_terminal(&session).await?;
            }
            for line in adopted.iter().flat_map(|a| &a.lines) {
                eprintln!("{line}");
            }
            session.execute(command).await?
        }
    };
    print!("{}", cli::render(&outcome));

    // #5215/#5555: a sweep that partly failed, or an acquisition that skipped
    // repositories, returns `Ok` — the per-repo failures are data to render,
    // not an error — so the process status is decided from the outcome.
    // `taudit clone … && taudit run` reads the status, not the text. The
    // judgement itself is `Outcome::exit_code`, in the library, so the Tauri
    // shell reads the same verdict from the same place.
    //
    // #5990: a launch whose `repos.txt` registered none of its repositories is
    // the same fail-open one rung earlier, and the outcome alone cannot show it
    // — so `exit_code_after` folds the shortfall in. Also in the library, for
    // the same reason.
    let code = cli::registration::exit_code_after(&outcome, adopted.as_ref());
    if code == 0 {
        return Ok(());
    }
    // `process::exit` does not flush Rust's stdout buffer.
    std::io::Write::flush(&mut std::io::stdout()).context("cannot write the report")?;
    std::process::exit(code);
}

/// Name which source supplied the credential, on stderr. Never the value.
///
/// Called at both resolution points since #5896's review split them — the
/// non-interactive one before the session is built, and the interactive one
/// after the operator agrees to the sweep.
fn report_source(credential: Option<&cli::credential::Resolved>) {
    if let Some(resolved) = credential {
        eprintln!("{}", resolved.source().describe());
    }
}
