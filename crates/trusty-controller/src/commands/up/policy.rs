//! Effective boot policy = flags > config > defaults (DOC-12 §4 / §8).
//!
//! Why: `tctl up` resolves its run-time behaviour from three layers with a
//! strict precedence (DOC-12 §8 "config-overridable vs fixed" table): explicit
//! CLI flags win over the on-disk config, which wins over built-in defaults.
//! Isolating the fold here keeps the orchestrator free of precedence branching.
//!
//! What: `UpFlags` captures the parsed CLI flags; `ResolvedBootPolicy` is the
//! folded result the orchestrator runs against; `resolve()` performs the fold,
//! including the §4 opt-in-orchestrator precedence (flag > config > interactive
//! > non-TTY-off).
//!
//! Test: `super::tests` covers each precedence rule (flag wins, config wins,
//! non-TTY default off, analyze-mode override).

use super::config::{AnalyzeMode, BootConfig};

/// Tri-state for an opt-in flag pair like `--with-mpm` / `--no-mpm`.
///
/// Why: A bare `bool` cannot distinguish "user said no" from "user said
/// nothing"; the §4 precedence needs that distinction (an explicit `--no-mpm`
/// must beat a config `with_mpm = true`).
///
/// What: `Yes` (forced on), `No` (forced off), `Unset` (defer to config/prompt).
///
/// Test: `super::tests::with_mpm_flag_precedence`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TriFlag {
    /// Explicit on.
    Yes,
    /// Explicit off.
    No,
    /// No flag supplied; defer to lower-precedence layers.
    Unset,
}

impl TriFlag {
    /// Build a `TriFlag` from the `--with-mpm` / `--no-mpm` flag pair.
    ///
    /// Why: clap exposes the two flags as separate bools; this collapses them
    /// into the tri-state, treating the (invalid) both-set case as `Yes` (an
    /// explicit opt-in dominates — the safer of the two for a user who asked).
    ///
    /// What: `with → Yes`, else `no → No`, else `Unset`.
    ///
    /// Test: `super::tests::with_mpm_flag_precedence`.
    pub fn from_pair(with: bool, no: bool) -> Self {
        if with {
            Self::Yes
        } else if no {
            Self::No
        } else {
            Self::Unset
        }
    }
}

/// Parsed `tctl up` CLI flags (pre-resolution).
///
/// Why: A plain data record decouples clap parsing (command layer) from policy
/// resolution (this module), keeping each unit-testable in isolation.
///
/// What: Captures the §4/§3.5/§6 boot flags plus environment facts (`is_tty`)
/// the resolver needs for the §4 interactive/non-TTY rule.
///
/// Test: Constructed directly in `super::tests`.
#[derive(Clone, Debug)]
pub struct UpFlags {
    /// `--with-mpm` / `--no-mpm` (§4).
    pub with_mpm: TriFlag,
    /// `--analyze-core <blocking|background|skip>` (§3.5); `None` = unset.
    pub analyze_core: Option<AnalyzeMode>,
    /// `--skip-claude-upgrade` (§6).
    pub skip_claude_upgrade: bool,
    /// `--wait` — block STAGE 5 ensure-project to `fresh` (§3.5).
    pub wait: bool,
    /// Whether stdout/stdin is an interactive TTY (drives the §4 prompt rule).
    pub is_tty: bool,
    /// `--yes` — suppress interactive prompts (treat as the safe default).
    pub yes: bool,
}

/// How the orchestrator decides the opt-in orchestrator stage (§4).
///
/// Why: The §4 resolution has three outcomes the orchestrator must distinguish:
/// definitely start, definitely skip, or *prompt the user* (TTY, nothing set).
///
/// What: `On` / `Off` / `Prompt`.
///
/// Test: `super::tests::with_mpm_flag_precedence`,
/// `super::tests::non_tty_defaults_off`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OrchestratorDecision {
    /// Start the orchestrator stage.
    On,
    /// Skip the orchestrator stage.
    Off,
    /// Ask the user interactively (TTY, neither flag nor config set).
    Prompt,
}

/// The effective, folded boot policy (`flags > config > defaults`).
///
/// Why: One value the orchestrator runs against, so no stage re-derives
/// precedence.
///
/// What: Carries the §4 orchestrator decision, the §3.5 analyze mode, the §6
/// Claude Code policy/skip, the §3.3 analyze target set, and the §3.5 `wait`.
///
/// Test: `super::tests` precedence cases.
#[derive(Clone, Debug)]
pub struct ResolvedBootPolicy {
    /// §4 orchestrator stage decision.
    pub orchestrator: OrchestratorDecision,
    /// §3.5 analyze blocking/background/skip.
    pub analyze_core: AnalyzeMode,
    /// §6 Claude Code policy: `latest` | `present`.
    pub claude_policy: String,
    /// §6 skip the Claude Code upgrade step.
    pub skip_claude_upgrade: bool,
    /// §3.3 core-analysis target set (resolved: config override or default).
    pub analyze_targets: Vec<String>,
    /// §3.5 block STAGE 5 ensure-project to `fresh`.
    pub wait: bool,
}

impl ResolvedBootPolicy {
    /// Fold flags, config, and defaults into the effective boot policy.
    ///
    /// Why: This is the single place the DOC-12 §8 precedence ("flags win over
    /// config win over defaults") and the §4 opt-in precedence are applied.
    ///
    /// What:
    /// - orchestrator: `--with-mpm`→On / `--no-mpm`→Off; else config
    ///   `boot.with_mpm`→On if true; else TTY→Prompt, non-TTY→Off (§4).
    /// - analyze_core: flag wins, else config, else `Background`.
    /// - claude policy: config `boot.claude_code.policy`; skip = flag OR config.
    /// - analyze targets: config `analyze_core_targets` if non-empty, else the
    ///   §3.3 default set (supplied by the caller as `default_targets`).
    ///
    /// Test: `super::tests::with_mpm_flag_precedence`,
    /// `super::tests::config_with_mpm_true`, `super::tests::non_tty_defaults_off`,
    /// `super::tests::analyze_mode_flag_over_config`.
    pub fn resolve(flags: &UpFlags, cfg: &BootConfig, default_targets: Vec<String>) -> Self {
        let orchestrator = match flags.with_mpm {
            TriFlag::Yes => OrchestratorDecision::On,
            TriFlag::No => OrchestratorDecision::Off,
            TriFlag::Unset => {
                if cfg.boot.with_mpm {
                    OrchestratorDecision::On
                } else if flags.is_tty && !flags.yes {
                    OrchestratorDecision::Prompt
                } else {
                    // Non-TTY (or --yes non-interactive) default off (§4 rule 4).
                    OrchestratorDecision::Off
                }
            }
        };

        let analyze_core = flags.analyze_core.unwrap_or(cfg.boot.analyze_core);

        let skip_claude_upgrade = flags.skip_claude_upgrade || cfg.boot.claude_code.skip_upgrade;

        let analyze_targets = if cfg.analyze_core_targets.is_empty() {
            default_targets
        } else {
            cfg.analyze_core_targets.clone()
        };

        Self {
            orchestrator,
            analyze_core,
            claude_policy: cfg.boot.claude_code.policy.clone(),
            skip_claude_upgrade,
            analyze_targets,
            wait: flags.wait,
        }
    }
}
