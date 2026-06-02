//! trusty-code — per-project Claude-Code-compatible MPM orchestration harness.
//!
//! # Why
//!
//! open-mpm is the general-purpose MPM orchestration platform, but each project
//! needs a harness that is *already* wired to its own `.claude` configuration:
//! agents, skills, MCP connections, CLAUDE.md, and permissions. `trusty-code`
//! fills that role. It is the Claude-Code-native orchestration entry point —
//! driven by API, CLI, or TUI — that runs the PM main-loop, enforces the
//! mandatory workflow, and delegates authority to typed sub-agents according to
//! MPM protocols. Extraction from open-mpm is tracked in epic #587; this Phase 0
//! establishes the crate and binary skeleton before any logic is moved.
//!
//! # Design constraints
//!
//! * **Claude-Code compatible** — reads `.claude/` config, agents, skills, MCP
//!   descriptors, `CLAUDE.md`, and permission grants exactly as Claude Code does.
//! * **API / CLI / TUI driven** — no hooks support (hooks are a Claude Code
//!   shell-level feature; `tcode` operates above that layer).
//! * **Per-agent model routing** — each agent in the harness may specify its own
//!   model, independently choosing between AWS Bedrock models and OpenRouter
//!   models. The PM is not constrained to a single provider.
//! * **Single-instance per project** — one `tcode serve` process per `.claude/`
//!   root; multiple CLI or TUI clients may attach to it.
//! * **No `unwrap()` in library code** — all fallible paths use `?` with
//!   `thiserror`-derived error types (once errors exist to derive); application
//!   entry points use `anyhow::Result`.
//!
//! # What
//!
//! Currently a Phase 0 scaffold. The public surface is intentionally empty until
//! Phase 1 begins moving PM main-loop logic from open-mpm (#587).
//!
//! # Test
//!
//! `cargo test -p trusty-code` — the test suite is empty at Phase 0. Integration
//! tests will be added in Phase 1 as the PM loop is introduced. The smoke test
//! for this phase is `tcode --version` and the stub subcommand exit codes
//! verified by `cargo run -p trusty-code -- serve --project .` (non-zero exit
//! with "not yet implemented" message).

/// Version string, re-exported so integration tests can assert it without
/// hard-coding the constant.
///
/// Why: single source of truth for the version across CLI and any future API
/// responses that embed it.
/// What: the `CARGO_PKG_VERSION` compile-time env var, captured at build time.
/// Test: `cargo run -p trusty-code -- --version` must print this value.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_non_empty() {
        // Why: guard against accidental blank version strings.
        // What: asserts that VERSION is not the empty string.
        // Test: this test itself.
        assert!(!VERSION.is_empty());
    }
}
