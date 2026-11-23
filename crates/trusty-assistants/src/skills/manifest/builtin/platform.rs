//! Built-in skills for the platform-hosted personal tools and the web.
//!
//! Why: These are the rows the owner named directly — *"MTA Train Time, is that
//! skill"*. They are also the rows where the honesty rule bites hardest: all
//! four providers here need a credential, three of them an environment variable
//! this process can actually check, so a card can state CONFIGURED / NOT
//! CONFIGURED from evidence rather than assertion.
//! What: A `const` table of one-tool [`SkillDef`] rows carrying [`ProviderReq`]
//! back-references.
//! Test: `super::super::tests::provider_env_presence_is_reported_only_for_env_backed_credentials`.

use super::super::{SkillDef, SkillKind::Action, tool_skill};
use super::{BRAVE_SEARCH, CTO_DB, MTA};

pub(super) static TABLE: &[SkillDef] = &[
    // --- transit + weather (the owner's naming exemplars) ----------------
    tool_skill(
        "mta-train-time",
        "MTA Train Time",
        "Look up the next Metro-North departures between two stations.",
        "get_train_schedule",
        Action,
        Some(&MTA),
    ),
    tool_skill(
        "mta-service-alerts",
        "MTA Service Alerts",
        "Report current Metro-North delays, cancellations and service advisories.",
        "get_train_alerts",
        Action,
        Some(&MTA),
    ),
    tool_skill(
        "weather-forecast",
        "Weather Forecast",
        "Report current conditions and the forecast for a location.",
        "get_weather",
        Action,
        None,
    ),
    // --- web -------------------------------------------------------------
    tool_skill(
        "web-search",
        "Web Search",
        "Search the live web and return ranked results with citations.",
        "web_search",
        Action,
        Some(&BRAVE_SEARCH),
    ),
    tool_skill(
        "web-fetch",
        "Fetch a Web Page",
        "Fetch one URL and return its readable text.",
        "fetch_url",
        Action,
        None,
    ),
    // --- CTO operations database (per-persona plugin) --------------------
    tool_skill(
        "cto-headcount",
        "CTO Headcount Query",
        "Query engineering headcount from the CTO operations database.",
        "query_headcount",
        Action,
        Some(&CTO_DB),
    ),
    tool_skill(
        "cto-budget",
        "CTO Budget Query",
        "Query engineering budget from the CTO operations database.",
        "query_budget",
        Action,
        Some(&CTO_DB),
    ),
    tool_skill(
        "cto-risks",
        "CTO Risk Register",
        "Query the recorded engineering risks from the CTO operations database.",
        "query_risks",
        Action,
        Some(&CTO_DB),
    ),
    tool_skill(
        "cto-work-classification",
        "CTO Work Classification",
        "Query how engineering effort is classified in the CTO operations database.",
        "query_work_classification",
        Action,
        Some(&CTO_DB),
    ),
];
