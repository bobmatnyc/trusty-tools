//! `trusty-browser-mcp`: flags and start-up of the stdio MCP server.
//!
//! Why: the Python `agent_mcp.main` chooses the mode from its flags and, in
//! workflow mode, must open the ticket's `agent.sock` session before it
//! serves; the port keeps the flags, checks and failures.
//! What: `--state` (default `~/.trusty/browser`), `--workflow NAME` or
//! `--credential FILE` (mutually exclusive; either selects workflow mode),
//! and `--upload-root DIR`. With neither workflow flag it serves connect
//! mode and opens no socket until `browser_connect`. Usage errors exit 2,
//! start-up failures exit 1, each with one line on stderr.
//! Test: `workflow_exchange_replays_byte_identically` and
//! `bad_flags_and_credentials_are_refused` in `tests/parity_mcp.rs`.

use std::fs;
use std::io::{self, BufReader};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use crate::mcp_session::{CodeSession, WorkflowWire};
use crate::mcp_tools::{Mode, Server};
use crate::mcp_upload::upload_root;
use crate::pyjson::Json;
use crate::vault::{PREFIX, Vault};

/// Expose Trusty Browser to one agent over stdio MCP, by workflow ticket or a connected profile.
#[derive(Parser, Debug)]
#[command(name = "trusty-browser-mcp")]
struct Args {
    /// State directory.
    #[arg(long)]
    state: Option<PathBuf>,
    /// Workflow ID shown under Activation details.
    #[arg(long, conflicts_with = "credential")]
    workflow: Option<String>,
    /// Explicit private workflow credential file.
    #[arg(long)]
    credential: Option<PathBuf>,
    /// Directory page_upload may read files from; without it uploads are refused.
    #[arg(long = "upload-root")]
    upload_root: Option<String>,
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("trusty-browser-mcp: error: {msg}");
    ExitCode::from(2)
}

/// `[a-z0-9][a-z0-9_-]{0,63}`.
fn workflow_name_ok(name: &str) -> bool {
    let b = name.as_bytes();
    let first = b
        .first()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    first
        && b.len() <= 64
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

#[cfg(target_os = "macos")]
fn store() -> Box<dyn crate::vault::KeyStore> {
    Box::new(crate::vault::KeyringStore)
}

#[cfg(not(target_os = "macos"))]
fn store() -> Box<dyn crate::vault::KeyStore> {
    Box::new(crate::vault::MemoryKeyStore::new())
}

/// Workflow start-up (Python `run` before `server.run`).
fn open_workflow(state: &Path, credential: &Path) -> anyhow::Result<WorkflowWire> {
    let file = fs::File::open(credential)?;
    let meta = file.metadata()?;
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    if !meta.file_type().is_file() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        anyhow::bail!("Use an owner-only workflow credential file (0600)");
    }
    let grant = Json::parse(&fs::read(credential)?)?;
    let field = |k: &str| {
        grant
            .get(k)
            .and_then(Json::as_str)
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("credential has no {k:?}"))
    };
    let workflow = field("workflow")?;
    let mut ticket = field("ticket")?;
    let parent = credential.parent().map(fs::canonicalize);
    let in_workflows = matches!((parent, fs::canonicalize(state.join("workflows"))), (Some(Ok(a)), Ok(b)) if a == b);
    if in_workflows || ticket.starts_with(PREFIX) {
        ticket = Vault::new(state, store())?.unseal(&format!("ticket:{workflow}"), &ticket)?;
    }
    let mut wire = WorkflowWire::connect(&state.join("agent.sock"))?;
    wire.call(
        "open",
        vec![
            ("workflow", Json::Str(workflow)),
            ("ticket", Json::Str(ticket)),
        ],
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    Ok(wire)
}

/// Parse the flags, start the chosen mode, and serve stdio until EOF.
pub fn main() -> ExitCode {
    let args = match Args::try_parse() {
        Ok(a) => a,
        Err(e) if e.use_stderr() => {
            let _ = e.print();
            return ExitCode::from(2);
        }
        Err(e) => {
            let _ = e.print();
            return ExitCode::SUCCESS;
        }
    };
    let state = match args.state {
        Some(s) => s,
        None => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(".trusty").join("browser"),
            None => return usage_error("HOME is not set"),
        },
    };
    let root = match args.upload_root.as_deref().map(upload_root).transpose() {
        Ok(r) => r,
        Err(msg) => return usage_error(&msg),
    };
    let mut credential = args.credential;
    if let Some(name) = &args.workflow {
        if !workflow_name_ok(name) {
            return usage_error("invalid workflow name");
        }
        credential = Some(state.join("workflows").join(format!("{name}.json")));
    }
    let mode = match credential {
        None => Mode::Connect(CodeSession::new(state.join("agent.sock")), root),
        Some(cred) => match open_workflow(&state, &cred) {
            Ok(wire) => Mode::Workflow(wire),
            Err(e) => {
                eprintln!("trusty-browser-mcp: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    let mut server = Server { mode };
    let served = server.run(BufReader::new(io::stdin().lock()), io::stdout().lock());
    if let Mode::Connect(session, _) = &mut server.mode {
        session.disconnect();
    }
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("trusty-browser-mcp: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_names_follow_the_python_pattern() {
        assert!(workflow_name_ok("a"));
        assert!(workflow_name_ok("fixture_1-x"));
        assert!(!workflow_name_ok("_a"));
        assert!(!workflow_name_ok("A"));
        assert!(!workflow_name_ok(""));
        assert!(!workflow_name_ok(&"a".repeat(65)));
    }
}
