//! Factory and error-mapping helpers for PM adapters.
//!
//! Why: separated from `pm_adapter.rs` to keep that file under the 500-SLOC
//! production cap while still co-locating the factory logic that wires
//! adapters to the top-level [`crate::core::config::Config`].
//! What: provides [`build_adapters`] (the public entry-point), plus the
//! internal error-mapping helpers [`collect_err_to_pm`] and
//! [`azdo_err_to_pm`] used by every adapter `impl`.
//! Test: adapter wiring is exercised by `detect_ticket_refs_handles_empty_corpus`
//! and `build_adapters_*` tests in `pm_adapter_tests.rs`.

use tracing::warn;

use super::{AzureDevOpsAdapter, GitHubAdapter, JiraAdapter, LinearAdapter, PmAdapter, PmError};
use crate::core::config::Config;

/// Best-effort mapping from [`crate::collect::errors::CollectError`] to
/// [`PmError`]. Tagged with `system` so the resulting error message names the
/// PM backend that produced it.
///
/// Why: adapter implementations must return `PmError`; the underlying clients
/// return `CollectError`. This helper centralises the mapping so each adapter
/// `impl` doesn't repeat the same match arm pattern.
/// What: maps Http → Http, Json → Serialization, Config → Config, others → Other.
/// Test: covered indirectly by every adapter `fetch_ticket` test that triggers
/// a transport failure.
pub(super) fn collect_err_to_pm(
    system: &'static str,
    e: crate::collect::errors::CollectError,
) -> PmError {
    use crate::collect::errors::CollectError;
    match e {
        CollectError::Http(err) => PmError::Http(err),
        CollectError::Json(err) => PmError::Serialization(err),
        CollectError::Config(msg) => PmError::Config {
            system: system.to_string(),
            message: msg,
        },
        other => PmError::Other {
            system: system.to_string(),
            message: other.to_string(),
        },
    }
}

/// Best-effort mapping from [`crate::collect::azdo::AzdoError`] to
/// [`PmError`]. HTTP-status variants surface as `Auth`, `NotFound`, etc.
///
/// Why: the ADO adapter must return `PmError`; the ADO client returns `AzdoError`.
/// What: maps each `AzdoError` variant to the closest `PmError` semantic.
/// Test: covered indirectly by `AzureDevOpsAdapter::fetch_ticket` error paths.
pub(super) fn azdo_err_to_pm(e: crate::collect::azdo::AzdoError) -> PmError {
    use crate::collect::azdo::AzdoError;
    match e {
        AzdoError::Unauthorized => PmError::Auth {
            system: "azure_devops".into(),
            message: "401 unauthorized".into(),
        },
        AzdoError::Forbidden => PmError::Auth {
            system: "azure_devops".into(),
            message: "403 forbidden".into(),
        },
        AzdoError::InvalidCredentials(msg) => PmError::Auth {
            system: "azure_devops".into(),
            message: msg,
        },
        AzdoError::NotFound => PmError::NotFound {
            id: "(connection)".into(),
        },
        AzdoError::Request(err) => PmError::Http(err),
        AzdoError::Config(msg) => PmError::Config {
            system: "azure_devops".into(),
            message: msg,
        },
        AzdoError::Parse(msg) | AzdoError::InvalidUrl(msg) => PmError::Other {
            system: "azure_devops".into(),
            message: msg,
        },
        AzdoError::Http { status, message } => PmError::Other {
            system: "azure_devops".into(),
            message: format!("HTTP {status}: {message}"),
        },
        AzdoError::NotImplemented { method, phase } => PmError::Other {
            system: "azure_devops".into(),
            message: format!("not implemented: {method} (phase {phase})"),
        },
    }
}

/// Build every PM adapter that is configured in `config`.
///
/// Why: the pipeline iterates `Vec<Box<dyn PmAdapter>>` without knowing the
/// concrete types; this factory encapsulates adapter construction and
/// configuration validation, letting callers stay generic.
/// What: instantiates one adapter per configured integration; skips adapters
/// with missing or invalid config (with a `warn!`) so a single bad
/// integration never fails the whole pipeline.
/// Test: `build_adapters_returns_empty_for_default_config` and
/// `build_adapters_includes_ado_when_configured` in `pm_adapter_tests.rs`.
pub fn build_adapters(config: &Config) -> Vec<Box<dyn PmAdapter>> {
    let mut out: Vec<Box<dyn PmAdapter>> = Vec::new();

    if let Some(cfg) = &config.jira {
        match crate::collect::jira::JiraClient::new(cfg) {
            Ok(client) => out.push(Box::new(JiraAdapter::with_ticket_regex(
                client,
                cfg.ticket_regex.as_deref(),
            ))),
            Err(e) => warn!(error = %e, "skipping JIRA adapter: invalid config"),
        }
    }

    if let Some(cfg) = &config.github {
        match crate::collect::github::GitHubClient::new(cfg) {
            Ok(client) => out.push(Box::new(GitHubAdapter::with_ticket_regex(
                client,
                cfg.ticket_regex.as_deref(),
            ))),
            Err(e) => warn!(error = %e, "skipping GitHub adapter: invalid config"),
        }
    }

    if let Some(cfg) = &config.linear {
        match crate::collect::linear::LinearClient::new(cfg) {
            Ok(client) => out.push(Box::new(LinearAdapter::with_ticket_regex(
                client,
                cfg.ticket_regex.as_deref(),
            ))),
            Err(e) => warn!(error = %e, "skipping Linear adapter: invalid config"),
        }
    }

    if let Some(cfg) = config.azure_devops_config() {
        let client = crate::collect::azdo::AzureDevOpsClient::new(cfg.clone());
        out.push(Box::new(AzureDevOpsAdapter::new(client)));
    }

    out
}
