//! The compile-time skill catalog — one named skill per registered tool.
//!
//! Why: The owner's model is 1:1 (#3933) — *"each [tool] needs an accompanying
//! skill"* — with human, provider-recognisable names ("MTA Train Time", not
//! `get_train_schedule`). Shipping that as ~170 Markdown files would block the
//! Skills pane on a content project; shipping it as `const` tables gives every
//! tool a named home today and makes the coverage invariant a unit test rather
//! than a review checklist.
//! What: [`all`] concatenates the per-family tables. Every row wraps exactly
//! one tool, except the deliberately tool-less rows in [`prose`] (the owner's
//! *"there can be other skills without tools"* case). Family split is for file
//! size only — it carries no grouping semantics, which is precisely what OQ-2's
//! resolution rejected.
//! Test: `super::tests`.

mod core;
mod gworkspace;
mod knowledge;
mod ops;
mod platform;
mod prose;

use super::{ProviderReq, SkillDef};

/// Google Workspace OAuth, via the first-party `gworkspace` MCP service.
///
/// Why: A Gmail card must never imply it works when no account was ever
/// authorized. There is no environment variable to probe here — the grant lives
/// in the gworkspace account store — so `env_var` is `None`, which this module
/// treats as *"requirement stated, status NOT checked"* and never as configured.
pub(super) static GOOGLE_OAUTH: ProviderReq = ProviderReq {
    provider: "Google Workspace",
    requirement: "An authorized Google account profile in the gworkspace MCP service \
                  (add one with the `add_account` tool). Not verified by this endpoint.",
    env_var: None,
};

/// Brave Search API key — the `web_search` / `fetch_url` backend.
pub(super) static BRAVE_SEARCH: ProviderReq = ProviderReq {
    provider: "Brave Search",
    requirement: "A Brave Search API key (api.search.brave.com).",
    env_var: Some("BRAVE_API_KEY"),
};

/// MTA developer API key — Metro-North schedules and service alerts.
pub(super) static MTA: ProviderReq = ProviderReq {
    provider: "MTA",
    requirement: "An MTA developer API key (api.mta.info).",
    env_var: Some("MTA_API_KEY"),
};

/// The CTO operations SQLite database.
pub(super) static CTO_DB: ProviderReq = ProviderReq {
    provider: "CTO operations database",
    requirement: "A local CTO ops SQLite database, exposed through a per-persona plugin. \
                  Not verified by this endpoint.",
    env_var: None,
};

/// Every built-in skill row, in stable catalog order.
///
/// Why/What: One place the catalog is assembled, so `SkillCatalog::builtin`
/// and the coverage tests can never read different sets.
/// Test: `builtin_ids_are_unique`, `builtin_rows_claim_each_tool_once`.
pub fn all() -> Vec<&'static SkillDef> {
    core::TABLE
        .iter()
        .chain(ops::TABLE)
        .chain(knowledge::TABLE)
        .chain(gworkspace::TABLE)
        .chain(platform::TABLE)
        .chain(prose::TABLE)
        .collect()
}
