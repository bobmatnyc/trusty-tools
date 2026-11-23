//! Thin `ta` binary wrapper (trusty-assistants; `tagent` before #11632).
//!
//! Why: The argv dispatch lives in the library at `trusty_assistants::cli::run`. Hosting it in the library lets a
//!      downstream launcher install additional agent plugins via
//!      `trusty_assistants::install_plugins(...)` BEFORE invoking `run()`, without
//!      polluting the crate with references to `publish = false` agent crates
//!      such as `cto-assistant`.
//!      #7359: this is now the only launcher in this workspace — the
//!      former `trusty-agents-local` binary was byte-for-byte this same
//!      pass-through and was removed.
//! What: Synchronous entry point that delegates straight to
//!       `trusty_assistants::cli::run_to_completion()`, which owns the tokio runtime.
//!       No additional setup, no plugin wiring — the binary ships with an
//!       empty plugin registry by default.
//! Test: `cargo run -p trusty-assistants -- --version` prints the build banner.
//!
//! #3655: this used to be `#[tokio::main]`. That attribute drops the runtime
//! at the end of `main` and the drop waits on the blocking pool forever, so a
//! single background task stuck in a syscall left the binary unable to exit.
//! `run_to_completion` bounds that wait.

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

use anyhow::Result;

fn main() -> Result<()> {
    trusty_assistants::cli::run_to_completion()
}
