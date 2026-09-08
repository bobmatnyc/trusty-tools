//! Why the durable event log could not do something.
//!
//! Why: every fallible operation in `log/` — resolving the directory,
//! hardening it, opening or reading a day file — needs one error type callers
//! can match on, rather than each submodule inventing its own. Library code
//! (this crate is also linked as `trusty_console`, per `Cargo.toml`'s `[lib]`)
//! uses `thiserror`, not `anyhow`, per this repo's convention.
//! What: [`LogError`] covers directory resolution/hardening, file open/read/
//! write, and a malformed on-disk filename encountered during a directory
//! listing. Test: exercised indirectly by every `log::tests` case that opens
//! a [`super::DurableLog`] against an unwritable or malformed directory.

use std::path::PathBuf;

/// Errors the durable event log surface can return.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LogError {
    /// The console data directory could not be resolved.
    #[error("resolve the console data directory: {0}")]
    ResolveDir(#[source] anyhow::Error),

    /// The log directory exists but could not be hardened to `0700` (or
    /// failed [`trusty_common::uds::prepare_socket_dir`]'s symlink/foreign-
    /// owner checks — reused here for its directory-hardening guarantee, not
    /// because this directory holds a socket).
    #[error("harden the event-log directory at {path}: {source}")]
    PrepareDir {
        path: PathBuf,
        #[source]
        source: trusty_common::uds::UdsSecurityError,
    },

    /// A day file could not be listed, opened, read, or written.
    #[error("{op} the event-log file at {path}: {source}")]
    Io {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
