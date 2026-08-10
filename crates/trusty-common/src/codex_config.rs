//! Codex CLI MCP-server registration (`~/.codex/config.toml`).
//!
//! Why: Codex Desktop lists a registered stdio server as "enabled" whether or
//! not the process it launches ever speaks MCP. `trusty-search` was registered
//! with `args = []`, so Codex exec'd the bare binary, which printed its
//! top-level help and exited before initialization — the connection showed
//! green and the model got no tools (#5264). The same argument-boundary defect
//! was filed for `trusty-memory` (#5265), so the repair lives here rather than
//! in either binary.
//!
//! What: [`codex_config_path`] resolves the file and [`patch_mcp_server`]
//! upserts one `[mcp_servers.<key>]` entry with a real argument vector,
//! repairing a registration whose `args` is empty, a single joined string, or a
//! JSON-looking string such as `["[\"serve\"]"]`.
//!
//! # This file belongs to the operator
//!
//! `~/.codex/config.toml` is hand-maintained and holds provider credentials.
//! Three rules follow, and each one is a defect this module already had:
//!
//! 1. **Never replace a container you did not author.** TOML has two spellings
//!    for a table, and `mcp_servers = { … }` (inline) is not an `Item::Table`.
//!    Treating that as "broken" and overwriting it deleted every other
//!    registered server. Every lookup goes through `as_table_like()`, which
//!    matches both spellings; anything that is genuinely neither is an `Err`,
//!    never an overwrite.
//! 2. **Merge, never rewrite the entry.** Codex keeps provider secrets in a
//!    per-server `env` table. Replacing the entry wholesale deleted them.
//!    Only the keys this module owns are written.
//! 3. **Publish under the shared critical section.** Writes go through
//!    [`crate::json_rmw::update_with`] — advisory lock, `fsync`, atomic rename,
//!    preserved file mode, symlink-aware — rather than a fourth hand-rolled
//!    atomic writer.
//!
//! Test: `codex_config_path_is_under_dot_codex`, and the `patch_mcp_server_*`
//! family below — one per config shape and one per broken-`args` spelling.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, TableLike, Value};

use crate::json_rmw::{DocumentCodec, JsonRmwError, update_with_decision};

/// Table the Codex CLI reads stdio MCP-server registrations from.
const MCP_SERVERS_TABLE: &str = "mcp_servers";

/// Path to the Codex CLI config file under `home`.
///
/// Why: the location is a Codex convention, not something the caller should
/// re-derive; taking `home` as an argument keeps the function testable without
/// touching the real `$HOME`.
/// What: `<home>/.codex/config.toml`.
/// Test: `codex_config_path_is_under_dot_codex`.
pub fn codex_config_path(home: &Path) -> PathBuf {
    home.join(".codex").join("config.toml")
}

/// What a caller wants one `[mcp_servers.<key>]` entry to say.
///
/// Why (#5264): the first cut of this API took `(&str command, &[&str] args)`
/// positionally, which cannot express `env`, `cwd`, or `startup_timeout_sec` —
/// all of which Codex supports and operators use. Once this crate is on
/// crates.io a positional signature is permanent for external consumers, so the
/// shape is a `#[non_exhaustive]` struct that can gain fields without a breaking
/// release.
/// What: `command` and `args` are authoritative — they are what a repair
/// rewrites. `env` is ADDITIVE: supplied keys are set, and keys already in the
/// file are never removed, because that table holds secrets this module has no
/// copy of. `cwd` and `startup_timeout_sec` are written only when `Some`.
/// Test: `patch_mcp_server_merges_env_without_dropping_existing_keys`,
/// `patch_mcp_server_writes_optional_fields`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct McpServerSpec {
    /// Executable Codex should launch.
    pub command: String,
    /// Argument vector, one real element per argument.
    pub args: Vec<String>,
    /// Environment entries to set on the launched process. Additive.
    pub env: BTreeMap<String, String>,
    /// Working directory for the launched process, when the caller pins one.
    pub cwd: Option<String>,
    /// Startup timeout Codex should allow before giving up on initialization.
    ///
    /// `u32` deliberately: TOML integers are `i64`, and a `u64` seconds field
    /// would let `u64::MAX` write `-1` and then round-trip as `Unchanged`.
    pub startup_timeout_sec: Option<u32>,
}

impl McpServerSpec {
    /// A stdio server launched as bare `command`, with no arguments.
    ///
    /// Why: `new(cmd, &[])` cannot infer the element type and fails to compile
    /// (E0283), so the natural spelling of "no arguments" needs its own
    /// constructor rather than forcing a turbofish at every call site.
    pub fn stdio(command: impl Into<String>) -> Self {
        Self::new(command, &[] as &[&str])
    }

    /// A stdio server launched as `command args…` with no other settings.
    ///
    /// `args` takes `&[impl AsRef<str>]` so a caller holding a `Vec<String>`
    /// need not collect into `Vec<&str>` first. For an empty vector use
    /// [`McpServerSpec::stdio`].
    pub fn new<S: AsRef<str>>(command: impl Into<String>, args: &[S]) -> Self {
        Self {
            command: command.into(),
            args: args.iter().map(|a| a.as_ref().to_owned()).collect(),
            env: BTreeMap::new(),
            cwd: None,
            startup_timeout_sec: None,
        }
    }

    /// Set one environment entry on the launched process.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Pin the working directory of the launched process.
    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Allow `secs` for the server to complete MCP initialization.
    #[must_use]
    pub fn with_startup_timeout_sec(mut self, secs: u32) -> Self {
        self.startup_timeout_sec = Some(secs);
        self
    }
}

/// What [`patch_mcp_server`] did.
///
/// Why (#5264): the first cut returned `bool`, which collapses "registered for
/// the first time" and "repaired a registration that could never have worked"
/// into one value — so a setup command could not tell an operator which had
/// happened, and neither could a provisioning script. `#[non_exhaustive]` keeps
/// room for a future outcome without a breaking release.
/// Test: `patch_mcp_server_creates_missing_file` (Created),
/// `patch_mcp_server_repairs_empty_args` (Repaired),
/// `patch_mcp_server_is_idempotent` (Unchanged).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PatchOutcome {
    /// No entry existed for this key; one was added.
    Created,
    /// An entry existed and disagreed with the spec; the owned keys were rewritten.
    Repaired,
    /// The entry already matched. Nothing was written.
    Unchanged,
}

impl PatchOutcome {
    /// Whether the file was written. `false` for [`PatchOutcome::Unchanged`].
    #[must_use]
    pub fn wrote(self) -> bool {
        !matches!(self, Self::Unchanged)
    }
}

/// Failure modes of [`patch_mcp_server`].
///
/// Why: "the config holds something where a table should be" is a case the
/// caller must be able to report verbatim, because the only safe response is to
/// tell the operator which key to look at — never to overwrite it.
#[derive(Debug)]
#[non_exhaustive]
pub enum CodexConfigError {
    /// Locking, reading, writing or parsing the config failed.
    Rmw(JsonRmwError),
    /// A key that must hold a table holds something else.
    #[non_exhaustive]
    NotATable {
        /// Dotted path of the offending key, e.g. `mcp_servers.trusty-search`.
        key: String,
    },
}

impl std::fmt::Display for CodexConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rmw(e) => write!(f, "{e}"),
            Self::NotATable { key } => write!(
                f,
                "`{key}` in the Codex config is not a table; refusing to overwrite it — \
                 edit it by hand or remove it and re-run setup"
            ),
        }
    }
}

impl std::error::Error for CodexConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Rmw(e) => Some(e),
            Self::NotATable { .. } => None,
        }
    }
}

impl From<JsonRmwError> for CodexConfigError {
    fn from(e: JsonRmwError) -> Self {
        Self::Rmw(e)
    }
}

/// [`DocumentCodec`] that parses and renders TOML with `toml_edit`.
///
/// Why: `toml_edit` round-trips comments, key order and inline/standard table
/// spelling, so an edit to one entry leaves the rest of an operator's config
/// byte-identical. A `toml::Value` round-trip would reflow the whole file.
/// What: `None` bytes (absent file) decode to an empty document.
/// Test: `patch_mcp_server_preserves_other_servers_and_comments`.
struct TomlEditCodec;

impl crate::json_rmw::sealed::Sealed for TomlEditCodec {}

impl DocumentCodec for TomlEditCodec {
    type Document = DocumentMut;

    fn decode(path: &Path, bytes: Option<&[u8]>) -> Result<DocumentMut, JsonRmwError> {
        let Some(raw) = bytes else {
            return Ok(DocumentMut::new());
        };
        let text = std::str::from_utf8(raw).map_err(|e| JsonRmwError::serialize(path, e))?;
        text.parse::<DocumentMut>()
            .map_err(|e| JsonRmwError::serialize(path, e))
    }

    fn encode(_path: &Path, doc: &DocumentMut) -> Result<Vec<u8>, JsonRmwError> {
        Ok(doc.to_string().into_bytes())
    }

    /// #5264: this file carries per-server `env` provider credentials, and the
    /// call that creates it is `patch_mcp_server`. Mode PRESERVATION cannot help
    /// a file that does not exist yet, so a fresh config would have been born
    /// 0644 — world-readable, holding a live API key.
    fn new_file_mode() -> Option<u32> {
        Some(0o600)
    }
}

/// Idempotently register one stdio MCP server in a Codex config file.
///
/// Why: re-running a setup command must be safe, and must REPAIR a broken
/// registration rather than leave it — #5264's reporter had already hand-edited
/// their config into `args = ["[\"serve\"]"]`, one literal argument that
/// launches `trusty-search '["serve"]'` and still never initializes MCP. A
/// writer that only fills in an absent key would have left that in place.
///
/// What: under [`crate::json_rmw`]'s cross-process lock, re-reads the config
/// (absent file ⇒ empty document), merges `spec` into
/// `[mcp_servers.<server_key>]`, and publishes atomically. Only the keys the
/// spec owns are written — `command`, `args`, any `env` entries the spec
/// carries, and `cwd` / `startup_timeout_sec` when `Some`. Every other key in
/// the entry, every other registered server, and the file's comments and
/// formatting survive. Both TOML table spellings are accepted wherever a table
/// is expected; a key holding a non-table yields
/// [`CodexConfigError::NotATable`] with nothing written.
///
/// Returns [`PatchOutcome::Unchanged`] without writing when the entry already
/// agrees, so a second run touches nothing.
///
/// Test: `patch_mcp_server_creates_missing_file`,
/// `patch_mcp_server_is_idempotent`, `patch_mcp_server_repairs_empty_args`,
/// `patch_mcp_server_repairs_joined_args`,
/// `patch_mcp_server_repairs_nested_json_string_args`,
/// `patch_mcp_server_preserves_an_inline_mcp_servers_table`,
/// `patch_mcp_server_preserves_an_inline_server_entry`,
/// `patch_mcp_server_merges_env_without_dropping_existing_keys`,
/// `patch_mcp_server_rejects_a_non_table_mcp_servers`,
/// `patch_mcp_server_rejects_a_non_table_entry`,
/// `patch_mcp_server_preserves_other_servers_and_comments`,
/// `patch_mcp_server_preserves_file_mode`.
pub fn patch_mcp_server(
    path: &Path,
    server_key: &str,
    spec: &McpServerSpec,
) -> Result<PatchOutcome, CodexConfigError> {
    update_with_decision::<TomlEditCodec, _, CodexConfigError, _>(path, |doc| {
        let outcome = apply_spec(doc, server_key, spec)?;
        Ok((outcome, outcome.wrote()))
    })
}

/// Merge `spec` into `doc`'s `[mcp_servers.<server_key>]`. See [`patch_mcp_server`].
///
/// Kept separate from the locked write so the whole decision tree is testable
/// against an in-memory document.
fn apply_spec(
    doc: &mut DocumentMut,
    server_key: &str,
    spec: &McpServerSpec,
) -> Result<PatchOutcome, CodexConfigError> {
    let existed = doc
        .get(MCP_SERVERS_TABLE)
        .and_then(Item::as_table_like)
        .and_then(|t| t.get(server_key))
        .is_some();

    if existed && entry_matches(doc, server_key, spec) {
        // The caller suppresses the publish on this outcome, so a re-run leaves
        // the operator's file byte-for-byte (and mtime-) untouched.
        return Ok(PatchOutcome::Unchanged);
    }

    // `mcp_servers` may legitimately be a standard table OR an inline table.
    // Only a value that is neither is an error — never an overwrite (#5264).
    let root = doc.as_table_mut();
    if !root.contains_key(MCP_SERVERS_TABLE) {
        let mut t = Table::new();
        t.set_implicit(true);
        root.insert(MCP_SERVERS_TABLE, Item::Table(t));
    }
    let servers_is_inline = root
        .get(MCP_SERVERS_TABLE)
        .and_then(Item::as_value)
        .is_some();
    let servers = root
        .get_mut(MCP_SERVERS_TABLE)
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| CodexConfigError::NotATable {
            key: MCP_SERVERS_TABLE.to_string(),
        })?;

    if servers.get(server_key).is_none() {
        // An inline parent can only hold values, so a standard sub-table would
        // render invalid TOML. Match the parent's spelling.
        let fresh = if servers_is_inline {
            Item::Value(Value::InlineTable(InlineTable::new()))
        } else {
            Item::Table(Table::new())
        };
        servers.insert(server_key, fresh);
    }
    let entry = servers
        .get_mut(server_key)
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| CodexConfigError::NotATable {
            key: format!("{MCP_SERVERS_TABLE}.{server_key}"),
        })?;

    entry.insert("command", Item::Value(Value::from(spec.command.as_str())));
    entry.insert("args", Item::Value(Value::Array(args_array(&spec.args))));
    if let Some(cwd) = &spec.cwd {
        entry.insert("cwd", Item::Value(Value::from(cwd.as_str())));
    }
    if let Some(secs) = spec.startup_timeout_sec {
        entry.insert("startup_timeout_sec", Item::Value(Value::from(secs as i64)));
    }
    if !spec.env.is_empty() {
        merge_env(entry, &spec.env, server_key)?;
    }

    Ok(if existed {
        PatchOutcome::Repaired
    } else {
        PatchOutcome::Created
    })
}

/// Set the spec's `env` keys on `entry`, leaving every other key in place.
///
/// Why: Codex stores provider credentials here. Replacing the table would
/// delete a secret the operator may have no other copy of, so this only ever
/// inserts.
fn merge_env(
    entry: &mut dyn TableLike,
    env: &BTreeMap<String, String>,
    server_key: &str,
) -> Result<(), CodexConfigError> {
    if entry.get("env").is_none() {
        // An inline table is valid inside BOTH spellings of the parent
        // (`env = { … }` renders correctly under `[mcp_servers.x]` and inside an
        // inline entry), so it needs no per-parent branch here.
        entry.insert("env", Item::Value(Value::InlineTable(InlineTable::new())));
    }
    let env_table = entry
        .get_mut("env")
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| CodexConfigError::NotATable {
            key: format!("{MCP_SERVERS_TABLE}.{server_key}.env"),
        })?;
    for (k, v) in env {
        env_table.insert(k, Item::Value(Value::from(v.as_str())));
    }
    Ok(())
}

/// Render an argument vector as a TOML array.
fn args_array(args: &[String]) -> Array {
    let mut arr = Array::new();
    for a in args {
        arr.push(a.as_str());
    }
    arr
}

/// Whether the entry already says everything `spec` asks for.
///
/// Why: the idempotency check compares the parsed ARGUMENT VECTOR, not rendered
/// text — `args = ["serve"]` and `args = ["[\"serve\"]"]` differ by one
/// element's contents, and only the second is broken. Keys the spec does not
/// mention are ignored, because they are not this module's to judge.
fn entry_matches(doc: &DocumentMut, server_key: &str, spec: &McpServerSpec) -> bool {
    let Some(entry) = doc
        .get(MCP_SERVERS_TABLE)
        .and_then(Item::as_table_like)
        .and_then(|t| t.get(server_key))
        .and_then(Item::as_table_like)
    else {
        return false;
    };
    if entry.get("command").and_then(Item::as_str) != Some(spec.command.as_str()) {
        return false;
    }
    let Some(existing) = entry.get("args").and_then(Item::as_array) else {
        return false;
    };
    if existing.len() != spec.args.len()
        || !existing
            .iter()
            .zip(&spec.args)
            .all(|(got, want)| got.as_str() == Some(want.as_str()))
    {
        return false;
    }
    if spec.cwd.is_some() && entry.get("cwd").and_then(Item::as_str) != spec.cwd.as_deref() {
        return false;
    }
    if spec.startup_timeout_sec.is_some()
        && entry.get("startup_timeout_sec").and_then(Item::as_integer)
            != spec.startup_timeout_sec.map(i64::from)
    {
        return false;
    }
    if !spec.env.is_empty() {
        let Some(env) = entry.get("env").and_then(Item::as_table_like) else {
            return false;
        };
        if !spec
            .env
            .iter()
            .all(|(k, v)| env.get(k).and_then(Item::as_str) == Some(v.as_str()))
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
#[path = "codex_config_tests.rs"]
mod tests;
