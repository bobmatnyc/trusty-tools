//! `trusty-secrets serve`: the on-demand `secrets.*` socket (S2, #9065).
//!
//! Why: owner rulings 24, 28 and 31 — a client spawns this binary on first
//! call, it serves until 60 s pass with no answered request, then removes its
//! socket and exits. No launchd job runs it.
//! What: parses `serve [--socket P] [--index-dir P] [--machine-config P]
//! [--idle-timeout-secs N]` (env: `TRUSTY_SECRETS_SOCKET`,
//! `TRUSTY_SECRETS_INDEX_DIR`, `TRUSTY_SECRETS_IDLE_TIMEOUT_SECS`), serves
//! with the real backends, and exits 0 on idle or SIGTERM/SIGINT. A refused
//! bind — a live instance already serving — exits 1 without touching it.
//! Test: `tests/on_demand_server.rs`.

use std::process::ExitCode;

/// trusty-common's UDS stack is Unix-only.
#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("trusty-secrets: the secrets socket needs a Unix platform");
    ExitCode::FAILURE
}

#[cfg(unix)]
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> ExitCode {
    use trusty_secrets::server::{ServerSettings, default_backends, serve};

    let settings = match ServerSettings::from_args(std::env::args_os().skip(1), |name| {
        std::env::var(name).ok()
    }) {
        Ok(settings) => settings,
        Err(e) => {
            eprintln!("trusty-secrets: {e}");
            return ExitCode::from(2);
        }
    };
    let socket = settings.socket.clone();
    let idle = settings.idle_timeout;
    eprintln!(
        "trusty-secrets: serving on {} (exits after {}s idle)",
        socket.display(),
        idle.as_secs()
    );
    match serve(
        settings,
        default_backends(),
        trusty_common::shutdown_signal(),
    )
    .await
    {
        Ok(exit) => {
            eprintln!("trusty-secrets: {exit:?}; exiting");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("trusty-secrets: {e}");
            ExitCode::FAILURE
        }
    }
}
