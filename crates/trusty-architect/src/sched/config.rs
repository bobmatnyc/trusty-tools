//! The job config: serde types over `registry.json`.
//!
//! Why: the contract says the Rust loader must accept every file its schema
//! accepts, and the schema allows unknown keys; the spike also needs fields
//! the contract lacks (job timeout, overlap, cwd, env allowlist, timezone,
//! the poll job).
//! What: [`Registry`] and [`Job`] deserialize with serde and ignore unknown
//! keys. Extension fields are all optional with documented defaults, so the
//! shipped 47-job registry loads unchanged: `timeout_s` (default 600 for a
//! script, `agent.timeout_s` for an agent), `overlap` (only `"skip"`, the
//! default), `cwd` (relative to the Architect root, default the root), `env`
//! (names passed through beyond the fixed allowlist), `schedule.tz` (only
//! `"UTC"`; anything else is refused loudly) and the top-level `poll` object.
//! [`Registry::load`] reads, parses and validates; an empty or unreadable
//! file is an error, never an empty job list.
//! Test: `shipped_shape_registry_loads`, `unknown_keys_are_ignored`,
//! `empty_config_is_a_hard_error`, `unreadable_config_is_a_hard_error`.

use std::path::Path;

use serde::Deserialize;

use super::{SchedError, validate};

/// The default script timeout (contract: 10 minutes).
pub const DEFAULT_SCRIPT_TIMEOUT_S: u64 = 600;

/// `registry.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct Registry {
    /// Always 1.
    pub version: u32,
    /// `limits`, used by `meta_due.py`; read here only to type-check.
    #[serde(default)]
    pub limits: Limits,
    /// The poll job (extension): how the scheduler runs the fleet-poll cycle.
    #[serde(default)]
    pub poll: PollJob,
    /// The jobs.
    pub jobs: Vec<Job>,
}

/// `limits`: owned by `meta_due.py`, kept to validate the shape.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Limits {
    /// `il-task` guidance runs per UTC day.
    pub agent_daily_cap: Option<u64>,
    /// Unanswered triggers before new ones are held.
    pub max_pending: Option<u64>,
    /// Triggers per `meta_due` run.
    pub max_triggers_per_run: Option<u64>,
}

/// The poll job (extension). Defaults make a registry without it shadow the
/// Rust poller every 60 s.
#[derive(Debug, Clone, Deserialize)]
pub struct PollJob {
    /// Seconds between poll cycles; the poller's own cadence.
    #[serde(default = "default_poll_every")]
    pub every_s: u64,
    /// Shadow argv: a dry-run that prints its decision, writes nothing.
    #[serde(default = "default_shadow_argv")]
    pub shadow_argv: Vec<String>,
    /// Live argv after cutover: one real cycle per run.
    #[serde(default = "default_live_argv")]
    pub live_argv: Vec<String>,
    /// Kill a cycle after this many seconds (the hang watchdog).
    #[serde(default = "default_poll_timeout")]
    pub timeout_s: u64,
    /// Off: the scheduler runs no poll job at all.
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn default_poll_every() -> u64 {
    60
}
fn default_poll_timeout() -> u64 {
    120
}
fn yes() -> bool {
    true
}
fn default_shadow_argv() -> Vec<String> {
    ["tm", "architect", "poll", "--dry-run"]
        .map(str::to_owned)
        .to_vec()
}
fn default_live_argv() -> Vec<String> {
    ["tm", "architect", "poll", "--once"]
        .map(str::to_owned)
        .to_vec()
}

impl Default for PollJob {
    fn default() -> Self {
        Self {
            every_s: default_poll_every(),
            shadow_argv: default_shadow_argv(),
            live_argv: default_live_argv(),
            timeout_s: default_poll_timeout(),
            enabled: true,
        }
    }
}

/// One job.
#[derive(Debug, Clone, Deserialize)]
pub struct Job {
    /// `^[A-Za-z][A-Za-z0-9_-]{1,31}$`.
    pub id: String,
    /// At most 100 characters.
    pub title: String,
    /// `architect` or `pm:<session>`.
    pub owner: String,
    /// `script` or `agent`; absent when `managed_by` is set.
    pub kind: Option<String>,
    /// The script, for `kind: script`.
    pub script: Option<ScriptSpec>,
    /// The agent call, for `kind: agent`.
    pub agent: Option<AgentSpec>,
    /// When to run.
    pub schedule: Schedule,
    /// Off: loaded and validated, never run.
    pub enabled: bool,
    /// Required when `enabled` is false.
    pub reason: Option<String>,
    /// Runs per UTC day; 0 is no cap.
    #[serde(default)]
    pub daily_cap: u32,
    /// `meta_due` or `external:<runner>`: something else runs it.
    pub managed_by: Option<String>,
    /// The freshness check.
    pub check: Option<Check>,
    /// Extension: kill after this many seconds.
    pub timeout_s: Option<u64>,
    /// Extension: only `"skip"` (one run at a time) exists.
    pub overlap: Option<String>,
    /// Extension: working directory relative to the root (scripts).
    pub cwd: Option<String>,
    /// Extension: environment variable NAMES to pass through.
    #[serde(default)]
    pub env: Vec<String>,
}

/// `script`.
#[derive(Debug, Clone, Deserialize)]
pub struct ScriptSpec {
    /// Program and arguments; no shell.
    pub argv: Vec<String>,
}

/// `agent`.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentSpec {
    /// `claude --agent <name>`.
    pub name: String,
    /// `--model`.
    pub model: String,
    /// `--tools`, joined by commas.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Kill after this many seconds.
    pub timeout_s: u64,
    /// The prompt template; `{date}` is the UTC date.
    pub prompt: Option<String>,
    /// Where the result text goes; `{date}` expands.
    pub output_file: Option<String>,
}

/// `schedule`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Schedule {
    /// `<N>[smhd]`.
    pub every: Option<String>,
    /// Five fields, UTC.
    pub cron: Option<String>,
    /// Random delay 0..N seconds added to each run.
    #[serde(default)]
    pub jitter_s: u64,
    /// `run_once` (default) or `skip`.
    pub catch_up: Option<String>,
    /// Extension: only `UTC`.
    pub tz: Option<String>,
}

/// `check`.
#[derive(Debug, Clone, Deserialize)]
pub struct Check {
    /// What proves the job ran.
    pub artifact: Artifact,
    /// The skill or agent named in a trigger.
    pub runner: Option<String>,
    /// Misses before one `meta_miss` (default 2).
    pub escalate_after: Option<u32>,
    /// Seconds past the slot before staleness counts (default 3600).
    pub grace_s: Option<u64>,
}

/// `check.artifact`; every field but `type` and `max_age_h` is loose.
#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    /// `glob`, `jsonl_event`, `json_field` or `stamp`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Fresh = newer than this many hours.
    pub max_age_h: f64,
    /// A path or glob; `~` expands.
    pub path: Option<String>,
}

/// How a job is run, from its fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An argv.
    Script,
    /// `claude -p --agent`.
    Agent,
    /// Not the scheduler's to run (disabled, or `managed_by` set).
    NotRun,
}

impl Job {
    /// What the scheduler does with this job.
    pub fn run_kind(&self) -> Kind {
        if !self.enabled || self.managed_by.is_some() {
            return Kind::NotRun;
        }
        match self.kind.as_deref() {
            Some("script") if self.script.is_some() => Kind::Script,
            Some("agent") if self.agent.is_some() => Kind::Agent,
            _ => Kind::NotRun,
        }
    }

    /// The kill deadline in seconds.
    pub fn timeout(&self) -> u64 {
        match (&self.agent, self.timeout_s) {
            (_, Some(t)) => t,
            (Some(a), None) if self.kind.as_deref() == Some("agent") => a.timeout_s,
            _ => DEFAULT_SCRIPT_TIMEOUT_S,
        }
    }
}

impl Registry {
    /// Parses and validates `text`; `origin` names it in errors.
    ///
    /// # Errors
    /// [`SchedError::Empty`] for blank text, [`SchedError::Parse`] for bad
    /// JSON or a wrong shape, [`SchedError::Invalid`] for contract breaks.
    pub fn parse(text: &str, origin: &Path) -> Result<Self, SchedError> {
        if text.trim().is_empty() {
            return Err(SchedError::Empty(origin.to_owned()));
        }
        let reg: Self = serde_json::from_str(text).map_err(|e| SchedError::Parse {
            what: origin.display().to_string(),
            detail: e.to_string(),
        })?;
        let problems = validate::problems(&reg);
        if problems.is_empty() {
            Ok(reg)
        } else {
            Err(SchedError::Invalid(problems))
        }
    }

    /// Reads and [`parse`](Self::parse)s the file at `path`.
    ///
    /// # Errors
    /// [`SchedError::Io`] when it cannot be read, else as `parse`.
    pub fn load(path: &Path) -> Result<Self, SchedError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| SchedError::io(format!("read {}", path.display()), e))?;
        Self::parse(&text, path)
    }
}
