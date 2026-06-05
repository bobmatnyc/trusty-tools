//! Output formatters for `tm` CLI commands.
//!
//! Why: keeping formatting helpers separate from handler logic makes each
//! file focused and keeps the handler files below the 500-line cap.
//! What: re-exports from `banner`, `services`, and `session` sub-modules.
//! Test: formatters are exercised by the unit tests in `tests.rs`.

pub(crate) mod banner;
pub(crate) mod services;
pub(crate) mod session;
