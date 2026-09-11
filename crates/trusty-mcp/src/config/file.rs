//! The one shared MCP server file: `~/.trusty-tools/mcp/servers.toml` (#7452).
//!
//! Why: trusty-code and trusty-agents are both supposed to read ONE list of
//! configured MCP servers, and neither has any such file today (the gap
//! analysis' §3 and §4). Putting the path, the format and the atomic write in
//! one place means the two crates cannot disagree about where the file is or
//! how a half-written save behaves.
//!
//! What: [`McpConfigFile`] is `servers = [ … ]` in TOML, with [`load`],
//! [`load_or_default`] and [`save`]. Writes go to a temporary file in the same
//! directory and are renamed into place, so a reader never observes a
//! truncated file and a crashed writer never destroys the previous config.
//!
//! Test: `crates/trusty-mcp/tests/mcp_config.rs` — `toml_round_trip_preserves_extensions`,
//! `save_creates_parent_directory`, `load_rejects_malformed_toml`,
//! `save_round_trips_config_with_null_extension_value`.
//!
//! [`load`]: McpConfigFile::load
//! [`load_or_default`]: McpConfigFile::load_or_default
//! [`save`]: McpConfigFile::save

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{McpConfigError, McpServerConfig};

/// Top-level directory, under `$HOME`, holding every trusty-* crate's state.
///
/// Why: `trusty_common::crate_config::TRUSTY_TOOLS_DIR` fixes this name for the
/// whole workspace. This module cannot import it — `trusty-common` is an
/// optional dependency behind `daemon-bridge-json-rpc`, and making `config`
/// pull it in would undo the lean-rlib property ADR-0040 protects — so the
/// constant is mirrored here and must stay equal to it. A `[dev-dependencies]`
/// edge on `trusty-common` lets the test assert that equality without the
/// `config` feature carrying the dependency at runtime.
/// What: `".trusty-tools"`.
/// Test: `default_path_at_layout`, `mirrored_trusty_tools_dir_matches_trusty_common`.
pub const TRUSTY_TOOLS_DIR: &str = ".trusty-tools";

/// The MCP subdirectory within that tree.
///
/// Why: the file is shared between crates, so it sits under a capability name
/// (`mcp`) rather than under any one crate's own directory.
/// What: `"mcp"`.
/// Test: `default_path_at_layout`.
pub const MCP_DIR: &str = "mcp";

/// The shared file's name.
///
/// Why: one exact path so docs and tooling can name it.
/// What: `"servers.toml"`.
/// Test: `default_path_at_layout`.
pub const SERVERS_FILE: &str = "servers.toml";

/// The shared config path under an explicit base directory.
///
/// Why: the hermetic core of [`default_path`]. Tests and any consumer that
/// redirects `$HOME` point `base` at a temp directory and never touch the real
/// tree.
/// What: `<base>/.trusty-tools/mcp/servers.toml`.
/// Test: `default_path_at_layout`.
pub fn default_path_at(base: &Path) -> PathBuf {
    base.join(TRUSTY_TOOLS_DIR).join(MCP_DIR).join(SERVERS_FILE)
}

/// The shared config path for this machine.
///
/// Why: production callers want the canonical location without resolving the
/// home directory themselves.
/// What: `~/.trusty-tools/mcp/servers.toml`, with the home directory from
/// `dirs::home_dir()` — the same resolver `trusty_common::crate_config` uses,
/// mirrored rather than imported (see [`TRUSTY_TOOLS_DIR`]). There is no
/// `TRUSTY_TOOLS_HOME` override, because no such convention exists anywhere in
/// the workspace today; introducing one here would make this file the only
/// trusty-* config that honours it.
/// Errors with [`McpConfigError::NoHome`] when the home directory cannot be
/// determined, which happens only in a stripped environment.
/// Test: `default_path_at_layout` covers the layout; the wrapper is the one
/// `dirs::home_dir()` call.
pub fn default_path() -> Result<PathBuf, McpConfigError> {
    dirs::home_dir()
        .map(|home| default_path_at(&home))
        .ok_or(McpConfigError::NoHome)
}

/// The contents of one MCP server config file.
///
/// Why: a named wrapper rather than a bare `Vec<McpServerConfig>` so the file
/// can gain a `version` or a `[defaults]` table later without breaking every
/// reader — `#[non_exhaustive]` plus a tolerant deserialiser makes that
/// additive.
/// What: `servers = [ … ]`. An absent or empty `servers` key is a valid empty
/// file, so a freshly created config is not an error.
/// Test: `toml_round_trip_preserves_extensions`, `empty_file_loads_as_no_servers`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct McpConfigFile {
    /// The configured servers, in file order.
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

impl McpConfigFile {
    /// A file holding exactly these servers.
    ///
    /// Why: `#[non_exhaustive]` blocks a struct literal from another crate.
    /// Test: `toml_round_trip_preserves_extensions`.
    pub fn new(servers: Vec<McpServerConfig>) -> Self {
        Self { servers }
    }

    /// Read and parse the file at `path`.
    ///
    /// Why: the one reader every consumer shares, so "what counts as a broken
    /// config" is decided once.
    /// What: fails closed — a missing file, unreadable file, unparseable TOML,
    /// or two servers sharing a name all return an error naming `path`. None
    /// of them yields an empty config, because a consumer that silently starts
    /// with no connectors is indistinguishable from one whose connectors are
    /// all broken. Use [`load_or_default`](Self::load_or_default) when a
    /// missing file genuinely means "nothing configured yet".
    /// Test: `toml_round_trip_preserves_extensions`, `load_rejects_malformed_toml`,
    /// `load_rejects_duplicate_names`, `load_missing_file_is_io_error`.
    pub fn load(path: &Path) -> Result<Self, McpConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| McpConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let parsed: Self = toml::from_str(&text).map_err(|e| McpConfigError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        parsed.reject_duplicate_names(path)?;
        Ok(parsed)
    }

    /// Like [`load`](Self::load), but an absent file is an empty config.
    ///
    /// Why: on a fresh install the file does not exist yet, and every consumer
    /// would otherwise write the same not-found branch. A file that exists and
    /// is broken still fails — absence and corruption are different answers.
    /// What: `Ok(McpConfigFile::default())` for `NotFound`; every other error
    /// propagates.
    /// Test: `load_or_default_on_missing_is_empty`, `load_or_default_still_rejects_malformed`.
    pub fn load_or_default(path: &Path) -> Result<Self, McpConfigError> {
        match Self::load(path) {
            Err(McpConfigError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(Self::default())
            }
            other => other,
        }
    }

    /// Write this config to `path`, atomically.
    ///
    /// Why: a save that is interrupted halfway must not leave a truncated file
    /// where a working config used to be — a reader that fails closed (above)
    /// would then refuse to start.
    /// What: creates the parent directory, renders TOML, writes it to a
    /// sibling temporary file, and renames that over `path`. Rename within one
    /// directory is atomic on every platform this workspace targets, so a
    /// concurrent reader sees either the old file or the new one. Rejects a
    /// duplicate name before writing, so `save` cannot produce a file `load`
    /// refuses. On unix the file is owner-read/write only — a stdio server's
    /// `env` and a remote server's `headers` routinely carry API keys and
    /// bearer tokens, and the default umask would leave them world-readable.
    /// The mode is set on the temporary file, which the rename carries over,
    /// so the config is never briefly readable at its real path.
    ///
    /// A JSON null anywhere in an [`extensions`] map is handled before
    /// rendering, because `toml` refuses one with the bare string
    /// `unsupported unit type` and names neither the server nor the key — one
    /// absent optional field would make the whole file unsavable with nothing
    /// saying which field. A null MEMBER of an object (including a top-level
    /// `extensions` key) is dropped, which is what a consumer serialising an
    /// `Option::None` means. A null ELEMENT of an array returns
    /// [`McpConfigError::NullExtensionValue`] naming the server and the dotted
    /// key, because dropping it would renumber the surviving elements.
    /// Test: `toml_round_trip_preserves_extensions`, `save_creates_parent_directory`,
    /// `save_rejects_duplicate_names`, `save_replaces_existing_file`,
    /// `saved_config_file_is_owner_only`,
    /// `save_round_trips_config_with_null_extension_value`,
    /// `save_rejects_null_inside_an_extension_array`.
    ///
    /// [`extensions`]: McpServerConfig::extensions
    pub fn save(&self, path: &Path) -> Result<(), McpConfigError> {
        self.reject_duplicate_names(path)?;
        let renderable = self.without_null_extensions(path)?;
        let rendered =
            toml::to_string_pretty(&renderable).map_err(|e| McpConfigError::Serialize {
                path: path.to_path_buf(),
                message: e.to_string(),
            })?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| McpConfigError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let tmp = temp_sibling(path);
        write_private(&tmp, rendered.as_bytes()).map_err(|source| McpConfigError::Io {
            path: tmp.clone(),
            source,
        })?;
        std::fs::rename(&tmp, path).map_err(|source| {
            // Leaving the temporary file behind after a failed rename would
            // accumulate junk next to the real config on every retry.
            let _ = std::fs::remove_file(&tmp);
            McpConfigError::Io {
                path: path.to_path_buf(),
                source,
            }
        })
    }

    /// A copy of this config whose `extensions` hold no JSON null.
    ///
    /// Why: kept separate from [`save`](Self::save) so the sanitising is one
    /// named step rather than a branch inside the write path, and so the
    /// caller's own config is never mutated by saving it.
    /// What: clones, then drops every null object member — top-level
    /// `extensions` keys included — and errors on the first null array
    /// element. An object left empty by the drop stays, rendering as an empty
    /// TOML table, because absence and "present but empty" are different
    /// answers to a consumer reading the key back.
    /// Test: `save_round_trips_config_with_null_extension_value`,
    /// `save_rejects_null_inside_an_extension_array`.
    fn without_null_extensions(&self, path: &Path) -> Result<Self, McpConfigError> {
        let mut out = self.clone();
        for server in &mut out.servers {
            let name = server.name.clone();
            for (key, value) in server.extensions.iter_mut() {
                strip_null_members(value, &name, key, path)?;
            }
            server.extensions.retain(|_, value| !value.is_null());
        }
        Ok(out)
    }

    /// Fail when two servers claim one name.
    ///
    /// Why: names are the key overrides match on, so a duplicate makes
    /// [`resolve`](mod@super::resolve) ambiguous and one of the two entries
    /// silently unreachable. Catching it at the file boundary means neither
    /// `load` nor `save` can produce that state.
    /// What: `Err(DuplicateName)` naming the first repeated name.
    /// Test: `load_rejects_duplicate_names`, `save_rejects_duplicate_names`.
    fn reject_duplicate_names(&self, path: &Path) -> Result<(), McpConfigError> {
        let mut seen = BTreeSet::new();
        for server in &self.servers {
            if !seen.insert(server.name.as_str()) {
                return Err(McpConfigError::DuplicateName {
                    path: path.to_path_buf(),
                    name: server.name.clone(),
                });
            }
        }
        Ok(())
    }
}

/// Drop null members of `value` in place, refusing a null array element.
///
/// Why: the recursive half of
/// [`without_null_extensions`](McpConfigFile::without_null_extensions). A
/// consumer's `Option::None` can sit at any depth once it serialises a nested
/// struct into `extensions`, so checking only the top level would leave the
/// same unnamed `unsupported unit type` failure one field deeper.
/// What: for an object, removes null members and recurses into what remains;
/// for an array, errors on a null element and recurses into the rest; every
/// scalar is left alone. `key_path` accumulates the dotted and indexed path so
/// the error names the exact location (`auth.endpoints[1]`).
/// Test: `save_round_trips_config_with_null_extension_value`,
/// `save_rejects_null_inside_an_extension_array`.
fn strip_null_members(
    value: &mut serde_json::Value,
    server: &str,
    key_path: &str,
    path: &Path,
) -> Result<(), McpConfigError> {
    match value {
        serde_json::Value::Object(members) => {
            members.retain(|_, member| !member.is_null());
            for (key, member) in members.iter_mut() {
                strip_null_members(member, server, &format!("{key_path}.{key}"), path)?;
            }
            Ok(())
        }
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                let child = format!("{key_path}[{index}]");
                if item.is_null() {
                    return Err(McpConfigError::NullExtensionValue {
                        path: path.to_path_buf(),
                        server: server.to_string(),
                        key: child,
                    });
                }
                strip_null_members(item, server, &child, path)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Write `bytes` to `path`, readable only by its owner.
///
/// Why: this file holds whatever an operator put in a server's `env` or
/// `headers`, which in practice is API keys and bearer tokens (#4568 is the
/// work to stop storing them inline at all). `std::fs::write` creates at
/// `0666 & !umask`, which on a default umask is `0644` — every local account
/// can read it.
/// What: on unix, creates the file with mode `0600`, then sets the mode again
/// so a leftover temporary file from a crashed earlier save is tightened
/// rather than inherited. Elsewhere it is a plain write: Windows has no mode
/// bits and no equivalent single call, and no trusty-* daemon runs there.
/// Test: `saved_config_file_is_owner_only`.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        // `.mode()` applies only when the open CREATES the file.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.flush()
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

/// A temporary path beside `path` for the write-then-rename.
///
/// Why: the temporary file must share a directory with its target, or the
/// rename crosses a filesystem boundary and stops being atomic. The process id
/// keeps two concurrent savers from clobbering each other's temporary file.
/// What: `<dir>/.<filename>.tmp.<pid>`, falling back to the target's own
/// directory-less form when `path` has no file name.
/// Test: `save_replaces_existing_file` exercises the rename path.
fn temp_sibling(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| SERVERS_FILE.to_string());
    let tmp_name = format!(".{}.tmp.{}", name, std::process::id());
    match path.parent() {
        Some(parent) => parent.join(tmp_name),
        None => PathBuf::from(tmp_name),
    }
}
