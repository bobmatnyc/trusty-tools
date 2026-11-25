//! The `[ask]` table of an instance's `config.toml` (#11650).
//!
//! Why: the turn cap, the wall-clock budget and the model are per instance;
//! a missing table must still give a working ask.
//! What: [`AskSettings`] (the raw table, every key optional) and
//! [`AskLimits`] (resolved values). A blank `model` resolves to
//! [`crate::agents::FALLBACK_MODEL`]; it is never sent as an empty string.
//! Test: `super::tests::config_tests` — the whole module.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::error::AskError;
use crate::agents::FALLBACK_MODEL;

/// Turn cap when `[ask] max_turns` is absent.
pub const DEFAULT_MAX_TURNS: u32 = 8;

/// Wall-clock budget in seconds when `[ask] timeout_secs` is absent.
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Logged turns replayed into one ask when `[ask] max_replay_turns` is absent.
// #11650: replay is bounded so a long-lived instance's requests stay inside
// the model's context window.
pub const DEFAULT_MAX_REPLAY_TURNS: usize = 20;

/// Characters of replayed questions and answers allowed into one ask when
/// `[ask] max_replay_chars` is absent (about 12k tokens at 4 chars a token).
pub const DEFAULT_MAX_REPLAY_CHARS: usize = 48_000;

/// The `[ask]` table as written; every key is optional.
/// Test: `super::tests::config_tests::an_absent_table_resolves_to_the_defaults`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct AskSettings {
    /// Model slug; blank means the crate default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Most LLM round-trips one ask may take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    /// Wall-clock budget for one ask, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Most logged turns replayed into one ask; 0 replays none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_replay_turns: Option<usize>,
    /// Most characters of replayed turns; 0 replays none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_replay_chars: Option<usize>,
}

/// The resolved settings one ask runs with.
/// Test: `super::tests::config_tests::set_values_are_used`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskLimits {
    /// Model slug sent on every request; never empty.
    pub model: String,
    /// Turn cap, at least 1.
    pub max_turns: u32,
    /// Wall-clock budget in seconds, at least 1.
    pub timeout_secs: u64,
    /// Most logged turns replayed into one ask.
    pub max_replay_turns: usize,
    /// Most characters of question plus answer text replayed into one ask.
    pub max_replay_chars: usize,
}

impl AskSettings {
    /// Resolve against the defaults; `config_path` names the file in errors.
    ///
    /// What: a blank or absent `model` becomes [`FALLBACK_MODEL`]; absent
    /// limits take [`DEFAULT_MAX_TURNS`] / [`DEFAULT_TIMEOUT_SECS`] /
    /// [`DEFAULT_MAX_REPLAY_TURNS`] / [`DEFAULT_MAX_REPLAY_CHARS`]; a zero
    /// `max_turns` or `timeout_secs` is [`AskError::InvalidSetting`], since it
    /// would end every ask before the first turn. A zero replay limit is valid
    /// and replays nothing.
    /// Test: `super::tests::config_tests::an_empty_model_resolves_to_the_default`,
    /// `super::tests::config_tests::zero_limits_are_typed_errors`.
    pub fn resolve(&self, config_path: &Path) -> Result<AskLimits, AskError> {
        let model = self
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or(FALLBACK_MODEL)
            .to_string();
        let max_turns = self.max_turns.unwrap_or(DEFAULT_MAX_TURNS);
        let timeout_secs = self.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS);
        let zero = |key| AskError::InvalidSetting {
            path: config_path.to_path_buf(),
            key,
            reason: "it must be at least 1".into(),
        };
        if max_turns == 0 {
            return Err(zero("max_turns"));
        }
        if timeout_secs == 0 {
            return Err(zero("timeout_secs"));
        }
        Ok(AskLimits {
            model,
            max_turns,
            timeout_secs,
            max_replay_turns: self.max_replay_turns.unwrap_or(DEFAULT_MAX_REPLAY_TURNS),
            max_replay_chars: self.max_replay_chars.unwrap_or(DEFAULT_MAX_REPLAY_CHARS),
        })
    }
}
