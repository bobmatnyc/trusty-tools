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
//! MPM protocols. Extraction from open-mpm is tracked in epic #587; Phase 1
//! moves the leaf/protocol modules here.
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
//! Phase 1 public surface (leaf/protocol modules extracted from open-mpm per
//! #640):
//!
//! * [`events`] — process-global broadcast event bus (`Event` enum, `publish`,
//!   `subscribe`, `emit`).
//! * [`ipc`] — NDJSON IPC protocol for PM ↔ sub-agent communication
//!   (`IpcMessage`, `HistoryMessage`, `serialize_message`, `parse_message`).
//! * [`perf`] — per-phase latency + token/cost instrumentation (`TokenUsage`,
//!   `PerfCollector`, `PerfRecord`, `cost_usd`).
//! * [`intent`] — pure-Rust heuristic intent classifier (`IntentClass`,
//!   `classify_intent`) for PM fast-pathing.
//! * [`progress`] — real-time phase progress reporter (`ProgressReporter`,
//!   `format_duration`).
//! * [`build_info`] — monotonic build counter + version banner (`BuildInfo`,
//!   `VERSION`, `GIT_HASH`, `version_string`).
//!
//! # Test
//!
//! `cargo test -p trusty-code` — all leaf modules carry their own unit tests.
//! The event bus has a round-trip async test (`publish_round_trips_through_subscribe`).
//! Phase 2+ will add integration tests as the PM loop is introduced.

// ── Phase 1 leaf/protocol modules (extracted from open-mpm per #640) ──

/// Process-wide broadcast event bus for real-time UI streaming.
///
/// Why: Centralises telemetry emission so any code path can publish events
/// without threading a sender through dozens of call sites.
/// What: `Event` enum, `publish`/`subscribe`/`emit` helpers, the
/// `EVENT_LINE_PREFIX` constant for subprocess relay.
/// Test: `events::tests::publish_round_trips_through_subscribe`.
pub mod events;

/// NDJSON IPC protocol for PM ↔ sub-agent communication.
///
/// Why: Provides a framing-safe wire protocol over stdin/stdout pipes so the
/// PM and sub-agent subprocesses exchange structured messages without ambiguity.
/// What: `IpcMessage` enum (Task/Result/Error), `HistoryMessage` wire type,
/// `serialize_message`/`parse_message` helpers, `extract_summary`/
/// `extract_files_from_content` utilities.
/// Test: `ipc::tests::*` round-trips every variant.
pub mod ipc;

/// Per-phase latency + token/cost instrumentation.
///
/// Why: Tracks how long each workflow phase takes, how many tokens it consumed,
/// and the resulting USD cost so runs can be compared build-over-build.
/// What: `TokenUsage`, `PhaseRecord`, `PerfRecord`, `PerfTotals`,
/// `PerfCollector`, `cost_usd`.
/// Test: `perf::tests::*`.
pub mod perf;

/// Pure-Rust heuristic intent classifier for PM fast-pathing.
///
/// Why: Avoids routing trivial conversational inputs through the full
/// subprocess pipeline — a 60-90s round trip for what should be sub-second.
/// What: `IntentClass` enum, `classify_intent` function.
/// Test: `intent::classifier_tests::*`, `intent::classifier_tests_2::*`,
/// `intent::classifier_property_tests::*`, `intent::tests::*`.
pub mod intent;

/// Real-time phase progress reporter to stderr.
///
/// Why: Workflow runs take 20–70 minutes; users need live feedback without
/// polluting stdout (reserved for structured JSON output).
/// What: `ProgressReporter` struct with phase/wave lifecycle hooks and
/// `format_duration` helper.
/// Test: `progress::tests::*`.
pub mod progress;

/// Build and version tracking.
///
/// Why: A monotonic build counter that increments on every process start gives
/// a deterministic identifier that pairs with semver for log correlation.
/// What: `BuildInfo` struct, `VERSION`/`GIT_HASH`/`PKG_NAME` constants,
/// `version_string` helper.
/// Test: `build_info::tests::*`.
pub mod build_info;

/// Version string, re-exported so integration tests can assert it without
/// hard-coding the constant.
///
/// Why: Single source of truth for the version across CLI and any future API
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
