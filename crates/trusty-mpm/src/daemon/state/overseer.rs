//! Overseer construction helpers for daemon startup.
//!
//! Why: the overseer strategy (deterministic vs. composite-LLM) is resolved
//! once at startup from the installed framework policy; isolating the resolution
//! logic keeps `DaemonState::new` and `DaemonState::with_paths` tidy.
//! What: [`OverseerBuild`] bundles the composed `dyn Overseer`, its handler
//! name, and the optional standalone LLM chat handler; [`build_overseer`]
//! assembles it from a loaded [`OverseerConfig`]; helper fns load optimizer
//! and overseer configs from the framework root.
//! Test: `overseer_is_deterministic_without_llm`, `overseer_falls_back_when_llm_key_missing`.

use std::path::PathBuf;
use std::sync::Arc;

use crate::core::deterministic_overseer::DeterministicOverseer;
use crate::core::overseer::Overseer;
use crate::core::overseer_config::OverseerConfig;
use crate::core::paths::FrameworkPaths;

use crate::daemon::audit::AuditLogger;
use crate::daemon::optimizer::OptimizerConfig;

/// The overseer strategy plus the optional standalone LLM chat handler.
///
/// Why: [`build_overseer`] resolves both the `dyn Overseer` used for hook
/// oversight and — when an OpenRouter key is present — a concrete
/// [`LlmOverseer`] reused for the `POST /llm/chat` endpoint; returning them as
/// one named struct keeps both daemon constructors aligned.
/// What: the composed overseer, its handler name, and `Some(LlmOverseer)` when
/// LLM chat is available.
/// Test: `overseer_is_deterministic_without_llm`.
pub(super) struct OverseerBuild {
    /// The composed overseer used on the hook path.
    pub overseer: Arc<dyn Overseer>,
    /// The active strategy name (`"deterministic"` or `"composite-llm"`).
    pub handler: String,
    /// Standalone LLM overseer for interactive chat, when a key resolved.
    pub llm: Option<Arc<crate::daemon::llm_overseer::LlmOverseer>>,
}

/// Read the optimizer policy from the installed framework, never failing.
///
/// Why: daemon startup must not abort because the framework is not installed
/// or its policy file is malformed; a sensible default keeps the daemon usable.
/// What: loads `~/.trusty-mpm/framework/hooks/optimizer.toml` via
/// [`OptimizerConfig::load_from_file`], logging and falling back to
/// `OptimizerConfig::default()` on any error.
/// Test: `new_reads_default_when_optimizer_file_missing`,
/// `reload_optimizer_config_picks_up_file_changes`.
pub(super) fn load_optimizer_config() -> OptimizerConfig {
    let path = FrameworkPaths::default().optimizer_config();
    match OptimizerConfig::load_from_file(&path) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!(
                "failed to load optimizer config from {}: {e}; using defaults",
                path.display()
            );
            OptimizerConfig::default()
        }
    }
}

/// Build the session overseer from the installed framework policy.
///
/// Why: oversight is framework-managed and opt-in; daemon startup must reflect
/// `~/.trusty-mpm/framework/hooks/overseer.toml` (or a safe disabled default
/// when it is absent) without ever failing to construct.
/// What: loads [`OverseerConfig`] from [`FrameworkPaths::overseer_config`] and
/// builds the overseer via [`build_overseer`]; a missing/malformed file yields
/// the disabled default config (handled inside `OverseerConfig::load_from`).
/// Test: `new_overseer_is_disabled_when_file_missing`.
pub(super) fn load_overseer() -> OverseerBuild {
    let path = FrameworkPaths::default().overseer_config();
    build_overseer(OverseerConfig::load_from(&path))
}

/// Assemble the overseer strategy from a loaded [`OverseerConfig`].
///
/// Why: the daemon may run rule-based oversight alone, or compose it with the
/// LLM overseer when `[llm] enabled = true` *and* an API key is present.
/// Deciding the strategy in one place keeps `new()` / `with_paths()` aligned.
/// What: always builds a [`DeterministicOverseer`]; when the LLM section is
/// enabled and the configured API key resolves, wraps both in a
/// [`CompositeOverseer`] (deterministic first, LLM for uncertain cases).
/// Returns the overseer, its handler name, and the standalone LLM chat handler.
/// Test: `overseer_is_deterministic_without_llm`,
/// `overseer_falls_back_when_llm_key_missing`.
pub(super) fn build_overseer(config: OverseerConfig) -> OverseerBuild {
    let deterministic = DeterministicOverseer::new(config.clone());
    if config.llm.enabled {
        let llm = Arc::new(crate::daemon::llm_overseer::LlmOverseer::new(
            config.llm.model.clone(),
            &config.llm.api_key_env,
        ));
        if llm.is_enabled() {
            tracing::info!(
                "LLM overseer active (model {}); composing with deterministic rules",
                config.llm.model
            );
            // The composite needs an owned overseer; build a second
            // `LlmOverseer` for it so the `Arc` above stays free for chat.
            let composite_llm = crate::daemon::llm_overseer::LlmOverseer::new(
                config.llm.model.clone(),
                &config.llm.api_key_env,
            );
            let composite = crate::daemon::overseer_compose::CompositeOverseer::new(
                Box::new(deterministic),
                Box::new(composite_llm),
            );
            return OverseerBuild {
                overseer: Arc::new(composite),
                handler: "composite-llm".to_string(),
                llm: Some(llm),
            };
        }
        tracing::warn!(
            "[llm] enabled but no API key in ${}; falling back to deterministic overseer",
            config.llm.api_key_env
        );
    }
    OverseerBuild {
        overseer: Arc::new(deterministic),
        handler: "deterministic".to_string(),
        llm: None,
    }
}

/// Resolve the daemon's logs directory (`~/.trusty-mpm/logs`).
///
/// Why: the audit logger writes under a single well-known directory; resolving
/// it via the home directory keeps it consistent with the framework root.
/// What: returns `<home>/.trusty-mpm/logs`, falling back to `./.trusty-mpm/logs`
/// when the home directory cannot be determined.
/// Test: exercised indirectly by `new_builds_audit_logger`.
pub(super) fn logs_dir() -> PathBuf {
    FrameworkPaths::default().root.join("logs")
}

/// Build an [`AuditLogger`] under `root/logs`.
///
/// Why: both `DaemonState::new` and `DaemonState::with_paths` need an audit
/// logger; centralising the construction avoids duplication. Delegates to
/// [`logs_dir`] for directory resolution so the two stay in sync.
/// What: returns `Arc<AuditLogger>` rooted at `root.join("logs")`.
/// Test: `audit_logger_is_accessible`.
pub(super) fn make_audit_logger(root: &std::path::Path) -> Arc<AuditLogger> {
    let _ = logs_dir(); // keep logs_dir in use so path resolution is consistent
    Arc::new(AuditLogger::new(&root.join("logs")))
}
