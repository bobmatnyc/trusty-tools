//! The assistant tier of MCP configuration: `[mcp]` in an assistant home's
//! `config.toml` (#7454, ADR-0060 decision 4).
//!
//! Why: the owner's 2026-09-11 ruling for epic #7425 item (g) is that an MCP
//! connection is set at the global level OR the assistant level. The global
//! level is the file `trusty-code` shares
//! (`trusty_mcp::config::McpConfigFile`); this module is the other half — the
//! per-assistant deltas that let one assistant add a connector nobody else
//! sees, replace a global one, or switch a global one off without touching
//! anyone else's.
//!
//! What: [`McpOverrides`] is the `[mcp]` table — `servers` (each a
//! `trusty_mcp::config::McpServerOverride::Set`) and `disabled` (each a
//! `Disable`). [`read_overrides`] is the fail-soft reader the runtime uses and
//! [`write_overrides`] the `toml_edit` writer, mirroring [`super::memory`]'s
//! pair exactly — the home is the USER's file, so a write replaces one table
//! and leaves every other byte, including their comments, alone.
//! [`parse_overrides`] and [`render_overrides`] are those two with the file
//! I/O removed, for the HTTP route, which has to read and write inside ONE
//! held lock (#7454).
//!
//! Reading is deliberately fail-soft AND reporting: a malformed `[mcp]` table
//! resolves to the global set (ADR-0060 decision 6) and carries the parse
//! error back with it, so the surface that renders connectors can say WHICH
//! file is wrong instead of showing an assistant that quietly lost its
//! overrides.
//!
//! Test: `super::tests::mcp_tests` — the whole module.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use trusty_mcp::config::{McpServerConfig, McpServerOverride};

use super::error::AssistantError;
use super::home::AssistantHome;

/// The `[mcp]` table of an assistant home's `config.toml` (#7454).
///
/// Why: two lists rather than one enum list, because TOML has no tagged-union
/// spelling a person would enjoy hand-writing. `disabled = ["github"]` is one
/// line; the equivalent as a tagged array-of-tables is four.
/// What: `servers` are complete replacements or additions, matched by name
/// against the global list — wholesale, never a field merge (ADR-0060). An
/// entry here may carry `enabled = true` for a server the global file disabled,
/// which is how an assistant re-enables one. `disabled` names global servers
/// this assistant does not connect to. Both fields are `#[serde(default)]`, so
/// every `config.toml` written before this table existed still parses.
///
/// A name appearing in BOTH lists resolves as a `Set`: `trusty_mcp`'s resolver
/// folds overrides left to right, [`as_overrides`](Self::as_overrides) emits
/// the disables first,
/// and a later `Set` wins — which is the reading that matches the more
/// specific instruction.
/// Test: `super::tests::mcp_tests::overrides_default_when_the_table_is_absent`,
/// `super::tests::mcp_tests::a_set_after_a_disable_wins`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct McpOverrides {
    /// Servers this assistant adds, or replaces wholesale by name.
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
    /// Global servers this assistant does not connect to.
    #[serde(default)]
    pub disabled: Vec<String>,
}

impl McpOverrides {
    /// Whether this assistant overrides nothing.
    ///
    /// Test: `super::tests::mcp_tests::overrides_default_when_the_table_is_absent`.
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty() && self.disabled.is_empty()
    }

    /// The table as the resolver's own override list.
    ///
    /// Why: `trusty_mcp::config::resolve` is the ONE resolver (ADR-0060
    /// decision 5); this is the only conversion into its vocabulary, so the
    /// disable-then-set ordering documented above is decided in one place.
    /// What: every `disabled` name as a `Disable`, then every `servers` entry
    /// as a `Set`.
    /// Test: `super::tests::mcp_tests::a_set_after_a_disable_wins`.
    pub fn as_overrides(&self) -> Vec<McpServerOverride> {
        let mut out: Vec<McpServerOverride> =
            Vec::with_capacity(self.servers.len() + self.disabled.len());
        for name in &self.disabled {
            out.push(McpServerOverride::Disable { name: name.clone() });
        }
        for server in &self.servers {
            out.push(McpServerOverride::Set(server.clone()));
        }
        out
    }
}

/// What [`read_overrides`] found, including why it found nothing.
///
/// Why: the read path must not fail (a chat turn is not the place to discover
/// a typo), but the surface that RENDERS connectors has to distinguish "this
/// assistant overrides nothing" from "this assistant's overrides did not
/// parse". One value carrying both is what keeps those two apart without a
/// second read of the same file.
/// Test: `super::tests::mcp_tests::a_malformed_table_reads_as_no_overrides_with_an_error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverridesRead {
    /// The parsed table, or the default when `error` is set.
    pub overrides: McpOverrides,
    /// The parse failure, phrased for a person. `None` on success.
    pub error: Option<String>,
    /// The file the answer came from, named whether or not it exists.
    pub path: PathBuf,
}

/// Read `[mcp]` out of an assistant home's `config.toml`, fail-soft.
///
/// Why: the home is user-editable by design (#4325) and ADR-0060 decision 6
/// requires a malformed override table to fall back to the global set with a
/// visible status — never to a failed startup and never to silence.
/// What: the parsed `[mcp]` table for a readable, well-formed file. An absent
/// or unreadable file is the default with NO error, because "you have not
/// configured this" is not a fault. A file that is not valid TOML, or an
/// `[mcp]` table that does not match the schema, is the default WITH the
/// error. The `[mcp]` value is extracted before deserialising so an unrelated
/// broken table elsewhere in the file is reported as what it is.
/// Test: `super::tests::mcp_tests::overrides_default_when_the_table_is_absent`,
/// `super::tests::mcp_tests::a_malformed_table_reads_as_no_overrides_with_an_error`,
/// `super::tests::mcp_tests::reads_servers_and_disabled`.
pub fn read_overrides(home: &AssistantHome) -> OverridesRead {
    let path = home.config_path();
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return OverridesRead {
            overrides: McpOverrides::default(),
            error: None,
            path,
        };
    };
    parse_overrides(&raw, path)
}

/// The parse half of [`read_overrides`], for a caller holding the bytes.
///
/// Why: `PUT /api/assistants/:id/mcp` fills an absent `servers` field from
/// what is stored, and that read has to happen INSIDE the lock it writes
/// under — a read through [`read_overrides`] would be a second, unlocked read
/// of the same path and a lost update (#7454). Exposing the parse step is what
/// lets that route keep one read, while still answering exactly the way
/// [`read_overrides`] does.
/// What: `path` names the file the text came from and is only carried into the
/// answer; nothing here touches the filesystem. An empty `raw` — what an
/// absent file hands a locked reader — parses as no overrides with no error,
/// the same as an unreadable file.
/// Test: `super::tests::mcp_tests::parsing_an_empty_document_is_no_overrides`,
/// and every [`read_overrides`] test through the delegation.
pub(crate) fn parse_overrides(raw: &str, path: PathBuf) -> OverridesRead {
    let document: toml::Value = match toml::from_str(raw) {
        Ok(document) => document,
        Err(e) => {
            return OverridesRead {
                overrides: McpOverrides::default(),
                error: Some(format!("`config.toml` is not valid TOML: {e}")),
                path,
            };
        }
    };
    let Some(table) = document.get("mcp") else {
        return OverridesRead {
            overrides: McpOverrides::default(),
            error: None,
            path,
        };
    };
    match McpOverrides::deserialize(table.clone()) {
        Ok(overrides) => OverridesRead {
            overrides,
            error: None,
            path,
        },
        Err(e) => OverridesRead {
            overrides: McpOverrides::default(),
            error: Some(format!("the `[mcp]` table does not match the schema: {e}")),
            path,
        },
    }
}

/// Persist `[mcp]` into an assistant home's `config.toml`.
///
/// Why: identical rationale to [`super::memory::write_memory_config`] — the
/// file carries the user's comments and any key they hand-added, so this
/// rewrites ONE table through `toml_edit` and leaves every other byte alone.
/// Serialising the whole [`super::home::AssistantHomeConfig`] back out would
/// delete whatever the struct does not model.
/// What: reads the current document (an absent file starts empty), replaces
/// `[mcp]` through [`render_overrides`], and writes the result atomically.
/// `disabled` is always written, even when empty, so clearing the list is a
/// durable edit rather than a no-op; `servers` is written as an array of tables
/// only when non-empty, because an empty `[[mcp.servers]]` renders as nothing
/// either way.
///
/// The read here is NOT inside the write's lock, so this is the entry point
/// for a caller whose new table does not depend on the stored one. A caller
/// that fills part of its table from what is stored — the HTTP route, whose
/// `servers` field is optional — must do that read inside its own
/// `state_writer::atomic_update` and publish [`render_overrides`]'s output
/// instead (#7454).
/// Test: `super::tests::mcp_tests::writing_overrides_preserves_other_keys`,
/// `super::tests::mcp_tests::writing_an_empty_disable_list_clears_it`.
pub fn write_overrides(
    home: &AssistantHome,
    overrides: &McpOverrides,
) -> Result<(), AssistantError> {
    let path = home.config_path();
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let rendered = render_overrides(&raw, overrides, &path)?;
    crate::state_writer::atomic_write(&path, rendered.as_bytes()).map_err(|e| AssistantError::Io {
        path,
        source: std::io::Error::other(e.to_string()),
    })
}

/// The document [`write_overrides`] would write, without writing it.
///
/// Why: the mirror of [`parse_overrides`] — the HTTP route publishes these
/// bytes through its own locked writer (`state_writer::atomic_update`), and
/// calling [`write_overrides`] from inside that lock would take the same
/// advisory lock twice and deadlock (#7454). One renderer means the two
/// writers cannot disagree about what replacing `[mcp]` does to the rest of
/// the user's file.
/// What: parses `raw` (an empty string starts an empty document), replaces the
/// `[mcp]` table, and returns the whole document as text. `path` labels errors
/// only; nothing here touches the filesystem.
/// Test: `super::tests::mcp_tests::writing_overrides_preserves_other_keys`,
/// `super::tests::mcp_tests::writing_an_empty_disable_list_clears_it`.
pub(crate) fn render_overrides(
    raw: &str,
    overrides: &McpOverrides,
    path: &std::path::Path,
) -> Result<String, AssistantError> {
    let path = path.to_path_buf();
    let mut document = raw
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| AssistantError::Io {
            path: path.clone(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
        })?;

    // Render through `toml`'s own serialiser and re-parse: `McpServerConfig`
    // is `#[non_exhaustive]` with a tagged transport enum, so hand-building
    // the `toml_edit` items here would be a second, drifting encoder of a
    // shape this crate does not own.
    let rendered = toml::to_string(overrides).map_err(|e| AssistantError::Io {
        path: path.clone(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
    })?;
    let table = rendered
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| AssistantError::Io {
            path: path.clone(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
        })?;
    let mut table = table.as_table().clone();
    table.set_implicit(false);
    if table.get("disabled").is_none() {
        table["disabled"] = toml_edit::value(toml_edit::Array::new());
    }
    document["mcp"] = toml_edit::Item::Table(table);
    Ok(document.to_string())
}
