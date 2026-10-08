//! `gchat-mcp` binary — stdio MCP server and `doctor` for one project's
//! Google Chat channel (#9448 S2b).
//!
//! Why: a session asks a person a question and reads the answer through MCP
//! tools; the process must be the single consumer of the project's Pub/Sub
//! subscription, and the operator needs per-route health rows.
//! What: initialises tracing to stderr (stdout carries JSON-RPC only),
//! resolves the project dir (`--project-dir`, then
//! `TRUSTY_CHANNELS_PROJECT_DIR`, then cwd) and either serves — opening the
//! project's one `GchatChannel`, which fails with the state-lock message if
//! another `gchat-mcp` already serves the project — or prints the doctor
//! rows and exits non-zero when any check fails.
//! Test: `tests/gchat_mcp_bin.rs` — `second_server_on_a_locked_project_fails_with_the_lock_message`,
//! `stdout_carries_only_json_rpc_and_logs_go_to_stderr`.

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

use std::path::Path;
use std::process::ExitCode;

use anyhow::Context as _;
use trusty_channels::gchat::api::client::Endpoints;
use trusty_channels::gchat::cli::{
    parse_args, resolve_project_dir, Command, PROJECT_DIR_ENV, USAGE,
};
use trusty_channels::gchat::doctor::run_doctor;
use trusty_channels::gchat::server::serve;
use trusty_channels::gchat::{GchatChannel, StateError};

fn main() -> ExitCode {
    trusty_common::init_tracing(0);
    let command = match parse_args(std::env::args().skip(1)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("gchat-mcp: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(command) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("gchat-mcp: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> anyhow::Result<ExitCode> {
    let (flag, mode) = match command {
        Command::Help => {
            eprintln!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        Command::Serve {
            project_dir,
            poll_interval,
        } => (project_dir, Mode::Serve(poll_interval)),
        Command::Doctor {
            project_dir,
            offline,
        } => (project_dir, Mode::Doctor { offline }),
    };
    let dir = resolve_project_dir(
        flag,
        std::env::var_os(PROJECT_DIR_ENV),
        std::env::current_dir,
    )
    .context("cannot resolve the project directory")?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("cannot start the async runtime")?;
    match mode {
        Mode::Doctor { offline } => {
            let report = runtime.block_on(run_doctor(&dir, Endpoints::default(), offline));
            print!("{}", report.render());
            Ok(ExitCode::from(report.exit_code()))
        }
        Mode::Serve(poll_interval) => {
            let channel = open_channel(&dir)?;
            tracing::info!(project = %dir.display(), "gchat-mcp serving");
            runtime.block_on(serve(channel, poll_interval))?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

enum Mode {
    Serve(std::time::Duration),
    Doctor { offline: bool },
}

/// Open the project's channel, naming the lock when another server holds it.
fn open_channel(dir: &Path) -> anyhow::Result<GchatChannel> {
    GchatChannel::open(dir).map_err(|e| match e {
        // One consumer per subscription (#9448, Architect ruling).
        StateError::Locked { path } => anyhow::anyhow!(
            "another gchat-mcp already serves {}: the state lock {} is held. Only one \
             server may poll a project's subscription; stop the other one first.",
            dir.display(),
            path.display()
        ),
        other => anyhow::Error::new(other).context("cannot open the gchat channel"),
    })
}
