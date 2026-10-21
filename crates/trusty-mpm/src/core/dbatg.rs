//! The `mpm.dbatg.*` method bodies: PM ledger `init`, `batch` and `show`
//! (#9758).
//!
//! Why: DOC-82 LDG-3.3 wants one set of methods with identical schema,
//! limits and exit codes behind the daemon socket, the MCP tool and the CLI.
//! The daemon is the preferred writer; when it is down, `tm dbatg` runs the
//! same body in-process under the same store lock (owner ruling, #9758
//! amendment).
//! What: [`init`], [`batch`] and [`show`] call `trusty_architect::ledger`
//! with the system clock, this crate's credential scanner
//! ([`scan_for_credentials`]) and the host code registry at
//! [`registry_path`]. [`METHODS`] names what the daemon serves; the rest of
//! the family (`digest`, `fresh`, `stamp`, …) is #9995.
//! Test: `dbatg_methods_round_trip_over_the_daemon_socket`,
//! `dbatg_cli_falls_back_in_process_when_the_daemon_is_down`.
//!
//! # Spec References
//! - [`SPEC-LEDGER-02~draft`](docs/specs/DOC-82-portfolio-intent-ledger-dashboard.md#SPEC-LEDGER-02~draft)

use std::path::PathBuf;

use trusty_architect::SystemClock;
use trusty_architect::ledger::rpc::{
    BatchRequest, BatchResponse, InitRequest, InitResponse, ShowRequest, StoreRef, TextResponse,
};
use trusty_architect::ledger::store::resolve_project;
use trusty_architect::ledger::writer::{self, Writer};
use trusty_architect::ledger::{Code, LedgerError, Registration, State, StorePaths, init_store};

use crate::core::session_ref_publish::scan_for_credentials;

/// The `mpm.dbatg.*` methods the daemon serves (#9758).
pub const METHODS: &[&str] = &["mpm.dbatg.init", "mpm.dbatg.batch", "mpm.dbatg.show"];

/// The per-host code registry: `<data dir>/dbatg/codes.json` (LDG-2.2.2).
///
/// # Errors
/// [`LedgerError::Unavailable`] when the data directory cannot be resolved.
pub fn registry_path() -> Result<PathBuf, LedgerError> {
    let dir = trusty_common::resolve_data_dir("trusty-mpm")
        .map_err(|e| LedgerError::Unavailable(format!("data dir: {e}")))?;
    Ok(dir.join("dbatg").join("codes.json"))
}

/// A registered session is live while tmux has a session of that name.
fn session_is_live(reg: &Registration) -> bool {
    std::process::Command::new("tmux")
        .args(["has-session", "-t", &format!("={}", reg.session)])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `mpm.dbatg.init`: creates the session's store with `code`; the same code
/// and session again is a no-op.
///
/// # Errors
/// As [`init_store`]; a code a live session holds is refused.
pub fn init(req: &InitRequest) -> Result<InitResponse, LedgerError> {
    let launch = resolve_project(&req.project)?;
    // Refuse bad input before the host registry is touched.
    Code::parse(&req.code)?;
    StorePaths::new(&launch, &req.session)?;
    let now = writer::minute(&SystemClock);
    let paths = init_store(
        &registry_path()?,
        &launch,
        &req.session,
        &req.code,
        &now,
        &session_is_live,
    )?;
    let text = std::fs::read_to_string(&paths.state).map_err(|source| LedgerError::Io {
        path: paths.state.clone(),
        source,
    })?;
    let store = StoreRef {
        project: launch,
        session: req.session.clone(),
    };
    Ok(InitResponse {
        store,
        v: State::from_json(&text)?.v,
    })
}

/// `mpm.dbatg.batch`: every op or none, under the store's lock.
///
/// # Errors
/// As [`Writer::batch`].
pub fn batch(req: &BatchRequest) -> Result<BatchResponse, LedgerError> {
    let scan = scan_for_credentials;
    Writer::new(&SystemClock, &scan).batch(req)
}

/// `mpm.dbatg.show`: the summary line or the whole store. Reads only.
///
/// # Errors
/// As [`writer::show`].
pub fn show(req: &ShowRequest) -> Result<TextResponse, LedgerError> {
    writer::show(&req.store, req.brief).map(|text| TextResponse { text })
}
