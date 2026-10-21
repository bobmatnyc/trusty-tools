//! The PM Intent Ledger contract: phase P0 of the IL Rust port (#9756).
//!
//! Why: the per-PM goals/decisions/asks/tasks store runs as a Python
//! prototype (`datg.py`). The port needs one contract that the writer, the
//! daemon methods, the MCP tool, the hooks and the Architect all import
//! (DOC-82 LDG-1.1), defined before any of them is built. Owner ruling
//! 2026-10-21: P0 starts in this leaf crate, with nothing wired into `tm`.
//! What: [`ids`] (the one id grammar), [`model`] (the on-disk records, which
//! round-trip the prototype's bytes), [`limits`] (field limits, size bar,
//! timestamps, schema version), [`store`] (layout, launch-dir anchoring,
//! code registry, `init`) and [`rpc`] (the `mpm.dbatg.*` request and response
//! types).
//! Test: `tests/ledger_contract.rs`, against scrubbed goldens from four live
//! prototype stores in `tests/fixtures/ledger/`.
//!
//! # Spec References
//! - [`SPEC-LEDGER-02~draft`](docs/specs/DOC-82-portfolio-intent-ledger-dashboard.md#SPEC-LEDGER-02~draft)

pub mod ids;
pub mod limits;
pub mod model;
pub mod rpc;
pub mod store;

pub use crate::error::LedgerError;
pub use ids::{Code, LedgerId, Prefix, is_bare_id};
pub use limits::{
    PARAGRAPH_MAX, SCHEMA_VERSION, SESSION_MAX, SIZE_REFUSE, SIZE_WARN, SizeVerdict, TITLE_MAX,
    check_paragraph, check_title, check_writable, normalize_ts, size_verdict,
};
pub use model::{ArchiveLine, LogLine, State};
pub use rpc::Op;
pub use store::{Registration, STORE_ROOT, StorePaths, init_store, launch_dir, load_for_write};
