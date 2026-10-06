//! The secrets MCP tool descriptor (#7522, DOC-74 §10.2).
//!
//! Why: ruling 34 keeps `secrets_get_ref` in the tm daemon's catalog as a
//! client of the trusty-secrets socket. It returns references, never values.
//! `secrets_list` (also named in §10.2) is outside #7522's scope.
//! What: [`secrets_tools`] returns the one `secrets_get_ref` descriptor,
//! with a closed schema and the full response shape in its description.
//! Test: `super::tests::secrets_get_ref_schema_is_closed_and_documents_no_value`,
//! `super::tests::catalog_names_match_constant`.

use serde_json::{Value, json};

use super::tool;

/// Build the one secrets tool descriptor.
///
/// Why: the description is the only schema documentation a model reads, so
/// it names every response field and states that none carries a value.
/// What: `secrets_get_ref`, two required string arguments, closed schema.
/// Test: `super::tests::secrets_get_ref_schema_is_closed_and_documents_no_value`.
pub(super) fn secrets_tools() -> Vec<Value> {
    vec![tool(
        "secrets_get_ref",
        "Resolve a secret key to its `secret://` reference for one project, on \
         demand, through the trusty-secrets socket. NEVER returns a value, a \
         masked head or a length: the answer holds names, a flag and a \
         timestamp only, so a reference can go in a `.env` file or an \
         `exec --env` flag without the value entering the transcript. The key \
         is looked up in the names-only index: the project scope first, then \
         the owner scope, unless `key` is an explicit \
         `secret://<owner>/KEY` or `secret://<owner>/<repo>/KEY` reference. \
         Returns `{ reference, key, present, scope, vault, backend, \
         imported_at }`: `reference` (string) is the canonical reference; `key` \
         (string) the key name; `present` (bool) whether a value is stored; \
         `scope` (`project` | `owner` | null) and `vault` (string | null) say \
         where; `backend` (string) is the backend the project's config \
         selects; `imported_at` (integer Unix seconds | null) is the key's \
         last write. An unknown key is `present: false`, not an error. Errors \
         are fixed text: a relative `project`, an invalid key, an unreachable \
         socket, or a project whose scopes cannot be derived from its git \
         remote.",
        json!({
            "type": "object",
            "properties": {
                "project": {
                    "type": "string",
                    "description": "Absolute path to the project directory (a git checkout); its remote names the project and owner scopes."
                },
                "key": {
                    "type": "string",
                    "description": "A key name (`API_TOKEN`) or a `secret://` reference (`secret://KEY`, `secret://<owner>/KEY`, `secret://<owner>/<repo>/KEY`)."
                }
            },
            "required": ["project", "key"],
            "additionalProperties": false
        }),
    )]
}
