//! Enriched-prompt logger for the UserPromptSubmit / SessionStart hooks
//! (issue #105).
//!
//! Why: `trusty-memory prompt-context` and `trusty-memory inbox-check` both
//! inject context into Claude Code sessions. Without a record of what was
//! injected we can't evaluate the effectiveness of either pipeline (relevance,
//! length, signal-vs-noise) or iterate on the recall / message-surfacing
//! logic. This module captures every invocation as a single JSONL entry under
//! the daemon data root so the logs are grep- and `jq`-friendly.
//!
//! What: a small, self-contained rolling writer. `PromptLogger::from_env`
//! reads the [`PromptLogConfig`] env vars, computes the active log path, and
//! returns a logger that swallows every I/O failure (best-effort by contract
//! — the hook caller must never fail because of a log write). The on-disk
//! layout is `<data_root>/logs/enriched-prompts.<YYYY-MM-DD>.jsonl` with a
//! `.<n>.jsonl` numeric suffix appended on size-cap rotation
//! (`enriched-prompts.2026-05-25.1.jsonl`, `.2.jsonl`, …).
//!
//! Sub-modules:
//!   - `config`: `PromptLogConfig`, `PromptLogEntry`, env-var constants.
//!   - `writer`: `PromptLogger` + rotation/retention/hash logic.
//!
//! Test: see [`writer::tests`] for round-trip, rotation, retention, disabled,
//! hash and integration-style assertions.

mod config;
mod writer;

pub use config::{
    PromptLogConfig, PromptLogEntry, DEFAULT_MAX_BYTES, DEFAULT_RETENTION_DAYS, ENV_DIR,
    ENV_ENABLED, ENV_HASH_PROMPTS, ENV_MAX_BYTES, ENV_RETENTION_DAYS,
};
pub use writer::PromptLogger;
