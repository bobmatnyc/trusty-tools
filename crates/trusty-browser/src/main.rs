//! `trusty-browser`: the broker (`serve`) and the Chrome native host (`native-host`).
//!
//! Why: one binary carries both roles, so the enrollment pins can name a
//! single executable on each side (`peer_darwin`). The launchd job runs
//! `<exe> --state <dir> serve`; the Chrome launchers run
//! `<exe> native-host --socket <sock> <origin>`.
//! What: phase 2 ships these two verbs; the operator and install verbs arrive
//! in phase 4 of `docs/plans/trusty-browser-rust-port.md`.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "trusty-browser",
    about = "Trusty Browser broker and native host"
)]
struct Cli {
    /// State directory (default ~/.trusty/browser).
    #[arg(long, global = true)]
    state: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the four broker sockets until interrupted.
    Serve,
    /// Relay Chrome native messaging to a broker socket.
    NativeHost {
        /// The broker socket to relay to.
        #[arg(long)]
        socket: PathBuf,
        /// The calling extension's origin (Chrome passes it).
        origin: String,
    },
}

fn default_state() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".trusty").join("browser"))
}

#[cfg(target_os = "macos")]
fn store() -> Box<dyn trusty_browser::vault::KeyStore> {
    Box::new(trusty_browser::vault::KeyringStore)
}

#[cfg(not(target_os = "macos"))]
fn store() -> Box<dyn trusty_browser::vault::KeyStore> {
    // The vault is macOS-only in 0.7.0; the broker still serves, unsealing fails.
    Box::new(trusty_browser::vault::MemoryKeyStore::new())
}

async fn serve(state: PathBuf) -> anyhow::Result<()> {
    let options = trusty_browser::broker::BrokerOptions::new(&state, store());
    let broker = trusty_browser::broker::Broker::start(options)?;
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        r = tokio::signal::ctrl_c() => r?,
        _ = term.recv() => {}
    }
    broker.close().await;
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::NativeHost { socket, origin } => {
            ExitCode::from(trusty_browser::native_host::main(&socket, &origin) as u8)
        }
        Command::Serve => {
            let run = || -> anyhow::Result<()> {
                let state = match cli.state {
                    Some(s) => s,
                    None => default_state()?,
                };
                tokio::runtime::Runtime::new()?.block_on(serve(state))
            };
            match run() {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}
