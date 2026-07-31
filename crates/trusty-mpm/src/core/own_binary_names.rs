//! Canonical binary names this crate ships — the single source of truth.
//!
//! Why (#4058): `Cargo.toml`'s `[[bin]]` targets `tm` and `trusty-mpm` build
//! from the identical `src/bin/tm/main.rs` entry point, so any code that
//! recognises "a trusty-mpm process" or "the trusty-mpm binary" must
//! recognise BOTH names — they are the same artifact under two names, not two
//! artifacts. Before this module existed, four call sites each hand-maintained
//! their own copy of this two-name list (`daemon::discovery::CLAUDE_COMMANDS`,
//! `core::standalone::hooks::MPM_BIN_NAMES`,
//! `core::session_launch::settings::STATUSLINE_BIN_NAMES`, and an inline
//! `name == "trusty-mpm" || name == "tm"` check in the `tm stop` daemon-PID
//! scan) and had already drifted once: `CLAUDE_COMMANDS` was missing
//! `"trusty-mpm"`, so a session launched via the `trusty-mpm` binary was
//! silently invisible to auto-discovery (`GET /sessions` and friends) even
//! though an identical session launched via `tm` was discovered correctly.
//! What: [`OWN_BINARY_NAMES`] is the two names, in no particular priority
//! order — a caller that needs a search priority (e.g. the statusline
//! resolver's PATH-lookup fallback) is free to reorder its own iteration
//! without needing a different source list. Adding a third `[[bin]]` target
//! to `Cargo.toml` now only requires updating this one array.
//! Test: covered indirectly by every call site's own test suite
//! (`daemon::discovery`, `core::standalone::hooks`,
//! `core::session_launch::settings`, `bin::tm::commands::daemon`); this
//! module has no branching logic of its own beyond the constant.

/// The `[[bin]]` names this crate's `Cargo.toml` produces.
pub const OWN_BINARY_NAMES: &[&str] = &["tm", "trusty-mpm"];
