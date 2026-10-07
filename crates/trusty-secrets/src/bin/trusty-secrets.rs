//! `trusty-secrets serve`: the on-demand `secrets.*` socket (S2, #9065).
//!
//! Why: owner rulings 24, 28 and 31 — a client spawns this binary on first
//! call, it serves until 60 s pass with no answered request, then removes its
//! socket and exits. No launchd job runs it.
//! What: parses `serve [--socket P] [--index-dir P] [--machine-config P]
//! [--audit-log P] [--idle-timeout-secs N]` (env: `TRUSTY_SECRETS_SOCKET`,
//! `TRUSTY_SECRETS_INDEX_DIR` off the default socket only (#7524),
//! `TRUSTY_SECRETS_IDLE_TIMEOUT_SECS`; the audit log has no environment
//! override), serves
//! with the real backends, and exits 0 on idle or SIGTERM/SIGINT. A refused
//! bind — a live instance already serving — exits 1 without touching it.
//! The git redirect variables are removed from its environment at start.
//! #7519: on a `cli-backends` build, so is every `OP_*` variable but an
//! `op signin` session; the service-account token is kept for the 1Password
//! backend's overlay. `PATH` is read once at start too, and the 1Password
//! backend searches its absolute entries for `op` at each open.
//! Test: `tests/on_demand_server.rs`.

use std::process::ExitCode;

/// trusty-common's UDS stack is Unix-only.
#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("trusty-secrets: the secrets socket needs a Unix platform");
    ExitCode::FAILURE
}

#[cfg(unix)]
fn main() -> ExitCode {
    // #9065: a detached server keeps its first spawner's environment for its
    // whole life. Drop the git redirect variables before any thread exists;
    // every git call also scrubs them itself.
    for var in trusty_secrets::store::git_redirect_vars(std::env::vars_os().map(|(k, _)| k)) {
        // SAFETY: no other thread exists yet — the runtime starts below.
        unsafe { std::env::remove_var(var) };
    }
    let onepassword_token = take_onepassword_env();
    // #7519: handed to the factory, so `op` is found by absolute path only.
    let search_path = std::env::var_os("PATH");
    match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(run(onepassword_token, search_path)),
        Err(e) => {
            eprintln!("trusty-secrets: cannot start the runtime: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Take the 1Password token out of this process's environment, and drop
/// every other inherited `OP_*` variable but an `op signin` session.
///
/// Why: #7519 A4/A10 — the token reaches `op` only through the runner's
/// overlay, and a caller's `OP_ACCOUNT`, `OP_CONFIG_DIR` or
/// `OP_CONNECT_HOST` must not steer `op`; only the machine config may.
/// What: must run before any thread exists, like the git scrub above.
#[cfg(all(unix, feature = "cli-backends"))]
fn take_onepassword_env() -> Option<trusty_secrets::api::SecretValue> {
    use trusty_secrets::store::onepassword::{inherited_op_vars, token_from};
    let token = token_from(|name| std::env::var(name).ok());
    for var in inherited_op_vars(std::env::vars_os().map(|(k, _)| k)) {
        // SAFETY: no other thread exists yet — the runtime starts after this.
        unsafe { std::env::remove_var(var) };
    }
    token
}

/// No 1Password backend in this build, so nothing to take.
#[cfg(all(unix, not(feature = "cli-backends")))]
fn take_onepassword_env() -> Option<trusty_secrets::api::SecretValue> {
    None
}

/// Parse the command line and serve until idle or a signal.
#[cfg(unix)]
async fn run(
    onepassword_token: Option<trusty_secrets::api::SecretValue>,
    search_path: Option<std::ffi::OsString>,
) -> ExitCode {
    use trusty_secrets::server::{ServerSettings, backends_for, serve};

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
    // #7519: CLI backends read the machine config and template directory
    // these settings name, and take the token and `PATH` captured at start.
    let backends = backends_for(&settings, onepassword_token, search_path);
    match serve(settings, backends, trusty_common::shutdown_signal()).await {
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
