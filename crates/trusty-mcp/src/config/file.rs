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
//! `save_creates_parent_directory`, `load_rejects_malformed_toml`.
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
/// constant is mirrored here and must stay equal to it.
/// What: `".trusty-tools"`.
/// Test: `default_path_at_layout`.
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
    /// refuses.
    /// Test: `toml_round_trip_preserves_extensions`, `save_creates_parent_directory`,
    /// `save_rejects_duplicate_names`, `save_replaces_existing_file`.
    pub fn save(&self, path: &Path) -> Result<(), McpConfigError> {
        self.reject_duplicate_names(path)?;
        let rendered = toml::to_string_pretty(self).map_err(|e| McpConfigError::Serialize {
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
        std::fs::write(&tmp, rendered.as_bytes()).map_err(|source| McpConfigError::Io {
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
