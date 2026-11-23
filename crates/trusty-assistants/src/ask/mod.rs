//! The ask engine: answer one question for one assistant instance (#11650).
//!
//! Why: slice 3a of the Trusty Assistants plan. An instance answers from its
//! own `instructions.md`, its own OKG and the read-only part of the MCP
//! catalog, and remembers earlier questions. It runs on trusty-code's
//! `AgentLoop` as a library; this module adds no loop of its own and no
//! socket or CLI verb (those are slice 3b).
//! What:
//! - [`ask`] — the production entry point: the real inference client and the
//!   operator's `servers.toml`.
//! - [`AskEngine`] — the same ask with the adapter and the MCP config path
//!   injectable; [`AskEngine::ask_with_cancel`] takes a per-ask cancel flag.
//! - `config` — the `[ask]` table ([`AskSettings`]).
//! - `tools` — the read-only registry ([`build_registry`]).
//! - `okg_search` — [`OkgSearchTool`].
//! - `turn_log` — [`TurnLog`], the per-instance jsonl log replayed into each
//!   new transcript and appended only after an answer exists.
//! - `error` — [`AskError`].
//!
//! Test: `tests` — the submodules under `ask/tests/`.

pub mod config;
pub mod error;
pub mod okg_search;
pub mod tools;
pub mod turn_log;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use trusty_code::agent_loop::{AgentLoop, AgentLoopConfig, CompactionConfig, Transcript};
use trusty_code::llm::InferenceAdapter;
use trusty_code::mcp::McpToolSet;
use trusty_code::provider::resolve_context_window;

use crate::assistants::AssistantHome;
use crate::assistants::own_index_id;
use crate::assistants::project::read_home_config;

pub use config::{
    AskLimits, AskSettings, DEFAULT_MAX_REPLAY_CHARS, DEFAULT_MAX_REPLAY_TURNS, DEFAULT_MAX_TURNS,
    DEFAULT_TIMEOUT_SECS,
};
pub use error::AskError;
pub use okg_search::{OKG_SEARCH_TOOL_NAME, OkgSearchTool};
pub use tools::{
    AskTools, Exclusion, READ_ONLY_MCP_TOOLS, build_registry, classify, serves_read_only_tools,
};
pub use turn_log::{LoadedLog, Recovery, TURN_LOG_FILE, TurnLog, TurnRecord, replay_window};

/// Answer `question` as the instance whose home is `home`.
///
/// Why: the one-call form slice 3b's daemon wraps.
/// What: builds the production client with
/// `trusty_code::task::mock_llm::build_llm_client` (no credential access at
/// construction) and runs [`AskEngine::ask`] against the live MCP config.
/// Test: `tests::engine_tests` drives the same path through [`AskEngine`]
/// with a scripted adapter.
pub async fn ask(home: &AssistantHome, question: &str) -> Result<String, AskError> {
    let llm = trusty_code::task::mock_llm::build_llm_client()
        .map_err(|e| AskError::Client(e.to_string()))?;
    AskEngine::new(home.clone(), llm).ask(question).await
}

/// Where the MCP server list comes from.
#[derive(Debug, Clone)]
enum McpSource {
    /// The operator's `servers.toml`; loads nothing under a test harness.
    Live,
    /// An explicit `servers.toml`, for tests.
    At(PathBuf),
}

/// One instance's ask engine.
///
/// Why: the injectable form of [`ask`]; tests pass a scripted adapter and a
/// temp `servers.toml`. A long-lived engine serves many asks, so it holds no
/// cancel state of its own (#11650): each ask gets its own flag.
/// What: holds the home, the adapter and the MCP source. Construction touches
/// no filesystem and spawns nothing.
/// Test: `tests::engine_tests` — the whole module.
pub struct AskEngine {
    home: AssistantHome,
    llm: Arc<dyn InferenceAdapter>,
    mcp: McpSource,
}

impl AskEngine {
    /// An engine for `home` using `llm` and the operator's MCP config.
    pub fn new(home: AssistantHome, llm: Arc<dyn InferenceAdapter>) -> Self {
        Self {
            home,
            llm,
            mcp: McpSource::Live,
        }
    }

    /// Load MCP servers from `servers_toml` instead of the operator's file.
    /// Test: `tests::tools_tests::the_registry_holds_no_coding_write_or_send_tool`.
    pub fn with_mcp_config(mut self, servers_toml: impl Into<PathBuf>) -> Self {
        self.mcp = McpSource::At(servers_toml.into());
        self
    }

    /// Answer `question` with a fresh cancel flag nobody else holds.
    /// Test: `tests::engine_tests::a_cancel_does_not_reach_the_next_ask`.
    pub async fn ask(&self, question: &str) -> Result<String, AskError> {
        self.ask_with_cancel(question, Arc::new(AtomicBool::new(false)))
            .await
    }

    /// Answer `question`, then record the turn. Setting `cancel` stops the
    /// loop at the next turn boundary.
    ///
    /// Why: #11650 — the flag belongs to one ask. An engine-wide flag was never
    /// reset, so one cancel failed every later ask on a long-lived engine.
    ///
    /// What, in order: refuse an empty question; read `instructions.md`
    /// ([`AskError::Instructions`]); read `config.toml` and resolve `[ask]`;
    /// load the turn log ([`TurnLog::load`]); load MCP and build the read-only registry; replay
    /// the log into a transcript under the system prompt; run the loop with
    /// `cancel`. Only a non-empty final answer is returned and
    /// appended to the log. Every other outcome is an [`AskError`] and leaves
    /// the log unchanged.
    /// Test: `tests::engine_tests::a_second_ask_sees_the_first_turn`,
    /// `tests::engine_tests::cancel_stops_at_the_next_turn_boundary`,
    /// `tests::engine_tests::a_cancel_does_not_reach_the_next_ask`,
    /// `tests::engine_tests::a_timeout_is_typed_and_leaves_the_log_alone`,
    /// `tests::engine_tests::an_instructions_error_stops_the_ask`,
    /// `tests::engine_tests::a_failed_ask_never_advances_the_log`,
    /// `tests::engine_tests::an_empty_model_is_sent_as_the_default`.
    pub async fn ask_with_cancel(
        &self,
        question: &str,
        cancel: Arc<AtomicBool>,
    ) -> Result<String, AskError> {
        let question = question.trim();
        if question.is_empty() {
            return Err(AskError::EmptyQuestion);
        }
        let instructions = self
            .home
            .read_instructions()
            .map_err(AskError::Instructions)?;
        let config = read_home_config(&self.home).map_err(AskError::Config)?;
        let project_root = config
            .as_ref()
            .and_then(|c| c.project_dir.clone())
            .unwrap_or_else(|| self.home.path().to_path_buf());
        let limits = config
            .map(|c| c.ask)
            .unwrap_or_default()
            .resolve(&self.home.config_path())?;
        let log = TurnLog::for_home(&self.home);
        // #11650: `load` moves a corrupt last line aside (and warns) instead of
        // refusing every later ask; mid-file corruption is still an error.
        let turns = log.load()?.turns;

        let tools = build_registry(&self.home.okg_dir(), &self.load_mcp(&project_root).await);
        // #11650: a dead or broken MCP server must not be silent.
        tools.diagnostics.log();
        if !tools.excluded.is_empty() {
            tracing::debug!(
                instance = %self.home.id(),
                excluded = tools.excluded.len(),
                "ask: MCP tools off the read-only allowlist were not registered",
            );
        }
        let system = system_prompt(&self.home, &instructions);
        // #11650: replay only the newest turns that fit the limits.
        let window = replay_window(&turns, limits.max_replay_turns, limits.max_replay_chars);
        if window.len() < turns.len() {
            tracing::debug!(
                instance = %self.home.id(),
                logged = turns.len(),
                replayed = window.len(),
                "ask: older turns left out of the replay window",
            );
        }
        let mut transcript = replay(&system, window, question);
        let loop_config = AgentLoopConfig {
            max_turns: limits.max_turns,
            timeout_secs: limits.timeout_secs,
            // #11650: size compaction to the model's window, as trusty-code's
            // own loop sites do, not the test-only default.
            compaction: CompactionConfig::for_context_window(resolve_context_window(&limits.model)),
            model: limits.model,
            ..AgentLoopConfig::default()
        };
        let agent = AgentLoop::new(loop_config, Arc::clone(&self.llm), Arc::new(tools.registry))
            .with_cancel_flag(cancel)
            .with_session_id(format!("ta-ask:{}", self.home.id()));
        let output = agent.run_with_transcript(&mut transcript, question).await?;

        let answer = output.content.trim();
        if answer.is_empty() {
            return Err(AskError::EmptyAnswer);
        }
        log.append(&TurnRecord::now(question, answer))?;
        Ok(answer.to_string())
    }

    /// Load the MCP tool set from this engine's source.
    ///
    /// What: #11650 — spawns only the servers with at least one allowlisted
    /// tool ([`serves_read_only_tools`]); the others are never started.
    /// Test: `tests::engine_tests::only_servers_with_allowlisted_tools_are_spawned`.
    async fn load_mcp(&self, project_root: &Path) -> McpToolSet {
        let wanted = |server: &str| serves_read_only_tools(server);
        match &self.mcp {
            McpSource::Live => McpToolSet::load_only(project_root, &wanted).await,
            McpSource::At(path) => McpToolSet::load_at_only(path, project_root, &wanted).await,
        }
    }
}

/// The system prompt: the instance's instructions, then a note naming its
/// primary OKG and the read-only tool scope.
/// Test: `tests::engine_tests::a_second_ask_sees_the_first_turn`.
pub fn system_prompt(home: &AssistantHome, instructions: &str) -> String {
    let id = home.id();
    format!(
        "{instructions}\n\n---\n\
         Primary OKG: your knowledge graph is the store `okg://{id}` at `{okg}` \
         (search index `{index}`). Search it with the `{OKG_SEARCH_TOOL_NAME}` tool.\n\
         Your tools are read-only: you cannot run commands, change files or send messages.\n",
        instructions = instructions.trim_end(),
        okg = home.okg_dir().display(),
        index = own_index_id(id),
    )
}

/// A transcript holding `turns` (the replay window), then `question` as the
/// new user turn.
/// Test: `tests::engine_tests::a_second_ask_sees_the_first_turn`.
fn replay(system: &str, turns: &[TurnRecord], question: &str) -> Transcript {
    let Some((first, rest)) = turns.split_first() else {
        return Transcript::seed(system, question);
    };
    let mut transcript = Transcript::seed(system, &first.question);
    transcript.push_assistant(Some(first.answer.clone()), &[]);
    for turn in rest {
        transcript.push_user(&turn.question);
        transcript.push_assistant(Some(turn.answer.clone()), &[]);
    }
    transcript.push_user(question);
    transcript
}
