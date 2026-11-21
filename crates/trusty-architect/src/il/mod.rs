//! Intent Ledger reads and writes: the Rust port of `datg.py` and `ilio.py`.
//!
//! Why: every IL read and write goes through this crate (Bob 2026-11-21), faster
//! and safer than the Python: one bounded lock on the file the Python also locks,
//! atomic writes, schema validation before every write, and a log append that is
//! never torn. The on-disk files and formats are unchanged, so going back to the
//! Python is just running it.
//! What: the live `datg` format (`.trusty-mpm/datg/[<session>/]`: `state.json`,
//! `log.jsonl`, `archive.jsonl`, `.lock`), not the DOC-82 `dbatg` format of
//! [`crate::ledger`]. State is held as an order-preserving [`Json`] so a rewrite
//! is byte-identical to Python's. The binary is `trusty-architect-il`.
//! Test: `tests/il_golden.rs`, `tests/il_ops.rs`, `tests/il_reliability.rs`,
//! `tests/il_ilio.rs`, and the ignored `tests/il_parity.rs`.

pub mod act;
pub mod args;
pub mod briefs;
pub mod cli;
pub mod cli_read;
pub mod cli_verbs;
pub mod digest;
pub mod error;
pub mod evidence;
pub mod freshness;
pub mod ids;
pub mod ilio;
pub mod json;
pub mod ledger;
pub mod limits;
pub mod log;
pub mod notify;
pub mod ops;
pub mod ops_remind;
pub mod ops_update;
pub mod prune;
pub mod remind;
pub mod schema;
pub mod session;
pub mod size;
pub mod store;
pub mod time;
pub mod wip;

pub use cli::{Env, Out, run};
pub use digest::build_digest;
pub use error::{IlError, IlResult};
pub use freshness::fresh;
pub use json::Json;
pub use ledger::{Fault, Ledger};
pub use ops::{Ctx, Ev};
pub use store::{Paths, Where, resolve};
