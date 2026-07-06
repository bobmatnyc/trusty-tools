//! Project-registry MCP tool descriptors (WI-2 + WI-5, #1519 / #1517).
//!
//! Why: the driver skill needs a typed, JSON-native surface to list, register,
//! look up, and NL-resolve projects in the registry — without scraping CLI text.
//! Keeping these descriptors in their own file mirrors the pattern of `session.rs`
//! and keeps each file well under the 500-SLOC production cap.
//! What: [`project_tools`] returns the four `{ name, description, inputSchema }`
//! descriptors — `project_list`, `project_register`, `project_get`, and
//! `project_resolve`.
//! Test: `super::tests::project_tools_present`,
//! `super::tests::catalog_names_match_constant`.

use serde_json::{Value, json};

use super::tool;

/// Build the four project-registry tool descriptors (WI-2 + WI-5).
///
/// Why: project listing, registration, lookup, and NL-resolution are the MCP
/// surface the driver skill needs to interact with the project registry without
/// CLI text scraping. Keeping them in their own builder keeps this file under
/// the SLOC cap and gives the project tools an auditable home.
/// What: returns the four descriptors in catalog order. `project_register`
/// requires `name` and `repo_url` and accepts optional fields; `project_get`
/// and `project_resolve` each require one string argument; `project_list` takes
/// no arguments. Every schema sets `additionalProperties: false`.
/// Test: `super::tests::project_tools_present`.
pub(super) fn project_tools() -> Vec<Value> {
    vec![
        tool(
            "project_list",
            "List all projects registered in the project registry. Returns a JSON \
             array of project objects, each with `name`, `repo_url`, \
             `default_branch`, and optional `stack_hint`, `tags`, `description`, \
             and `gh_user` (the project's preferred GitHub account login, #2081).",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        tool(
            "project_register",
            "Register or update a project in the project registry. Registration \
             is idempotent — calling with the same `name` updates the existing \
             entry rather than creating a duplicate. Returns the registered \
             project record.",
            json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Short name used as the registry key (e.g. `trusty-tools`)."
                    },
                    "repo_url": {
                        "type": "string",
                        "description": "Full repository URL (e.g. `https://github.com/owner/trusty-tools`)."
                    },
                    "default_branch": {
                        "type": "string",
                        "description": "Default branch for session spawns. Defaults to `main` when omitted."
                    },
                    "stack_hint": {
                        "type": "string",
                        "description": "Optional technology-stack hint (e.g. `rust`, `python`)."
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional classification tags (e.g. [\"backend\", \"production\"])."
                    },
                    "description": {
                        "type": "string",
                        "description": "Optional free-form human-readable description."
                    },
                    "gh_user": {
                        "type": "string",
                        "description": "Optional preferred GitHub account login for `gh` \
                                         operations on this project (#2081). When set, `gh` \
                                         calls for this project should be scoped to this \
                                         account rather than whatever identity is ambient."
                    }
                },
                "required": ["name", "repo_url"],
                "additionalProperties": false
            }),
        ),
        tool(
            "project_get",
            "Look up a single project by name. Returns the project record \
             (`name`, `repo_url`, `default_branch`, and optional fields including \
             `gh_user`, the project's preferred GitHub account login) or an \
             error when the name is not found in the registry.",
            json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Project name to look up."
                    }
                },
                "required": ["name"],
                "additionalProperties": false
            }),
        ),
        tool(
            "project_resolve",
            "Resolve a natural-language query to the best-matching registered \
             project. Accepts free-text task descriptions, GitHub URLs, ticket \
             IDs (e.g. PROJ-123), project names, keywords, or tags. Returns a \
             `primary` match with confidence score and reason, a \
             `needs_disambiguation` flag (true when multiple candidates score \
             above the disambiguation floor), and a ranked `matches` list. \
             Confidence is always in [0.0, 1.0]. On no match, `primary` is null \
             and an `error` field explains the failure.",
            json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Free-text query: project name, GitHub URL, \
                                        ticket ID, keyword, or any natural-language \
                                        task description."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        ),
    ]
}
