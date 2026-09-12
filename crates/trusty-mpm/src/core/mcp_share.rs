//! The operator's per-server "projects may use this" decision (#7672).
//!
//! Why: content equivalence ([`crate::core::mcp_content_trust`]) lets an
//! untrusted project load a server whose executable spec the operator already
//! has. Registering a server with `tm mcp add` is NOT that consent. The code
//! review of PR #7692 named the gap: this workspace's own convention ships
//! credential-bearing servers with an EMPTY `env` — `builtin_server_entry` says
//! "the managed session and the probe both inherit the ambient environment",
//! and `tm-secrets` documents exec-wrapper recipes that do the same — so for
//! such a server the normalized spec carries no secret to fail to match, and a
//! hostile repository can reproduce its fully public shape byte for byte. The
//! spec comparison cannot tell those apart, so the operator says which servers
//! may be reached this way. Default false: an unflagged registry entry never
//! contributes to the known set, even on an exact match.
//!
//! THE FLAG LIVES IN A SIDECAR tm OWNS, NOT IN `.claude.json`. Claude Code
//! rewrites `<config_dir>/.claude.json` on its own schedule, so an extra key
//! inside an `mcpServers` record is not durable there — and
//! [`crate::core::mcp_content_trust`] deliberately fails closed on any key it
//! does not model, so storing the flag inside the record would also have to
//! widen that allowlist. The store is therefore
//! `~/.trusty-tools/trusty-mpm/mcp-shared.json`, the same tm-owned root
//! [`crate::core::project_trust`] records its decision under — out of reach of
//! both Claude Code and any repository.
//!
//! RESIDUAL RISK, ACCEPTED. Sharing a server is a standing grant for EVERY
//! untrusted project on this host, not a per-project one. Share only servers
//! whose tools are safe to attach to a session reading code the operator has
//! not reviewed; keep a credentialed actor (a ticketing system, a database, a
//! chat workspace) unshared and reach it through `tm project trust` instead.
//!
//! What: [`McpShareStore`] persists a sorted set of shared server names, with
//! the same load / mutate / save / owner-only-`0600` shape as
//! [`crate::core::project_trust::ProjectTrustStore`]. [`shared_servers`] is the
//! non-fatal production accessor that fails closed to an empty set.
//! Test: `mcp_share_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// On-disk filename, beside `project-trust.json` under the tm state root.
const SHARE_STORE_FILE: &str = "mcp-shared.json";

/// On-disk shape: a sorted array of server names, and nothing else.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ShareStoreData {
    /// Registry server names the operator has shared with untrusted projects.
    #[serde(default)]
    shared_servers: Vec<String>,
}

/// The set of registry servers an untrusted project may match by content.
///
/// Why: see the module doc — registering a server and lending it to unreviewed
/// repository content are two different decisions, so they get two records.
/// What: a `BTreeSet<String>` of server names plus the resolved file path;
/// mutations are committed by an explicit [`McpShareStore::save`].
/// Test: `share_store_new_is_empty`, `save_and_reload_round_trip`.
#[derive(Debug, Default)]
pub struct McpShareStore {
    shared: BTreeSet<String>,
    file_path: PathBuf,
}

impl McpShareStore {
    /// Load (or create) the store at `<root>/mcp-shared.json`.
    ///
    /// Why: a missing file means nothing has been shared — the fail-closed
    /// default — so it is an empty store, never an error.
    /// What: reads and deserializes when present; an empty store otherwise.
    ///
    /// # Errors
    ///
    /// An unreadable or malformed file. A caller that must not fail the launch
    /// uses [`shared_servers`] instead.
    /// Test: `share_store_new_is_empty`, `save_and_reload_round_trip`.
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let file_path = root.join(SHARE_STORE_FILE);
        if !file_path.exists() {
            return Ok(Self {
                shared: BTreeSet::new(),
                file_path,
            });
        }
        let raw = std::fs::read_to_string(&file_path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", file_path.display()))?;
        let data: ShareStoreData = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", file_path.display()))?;
        Ok(Self {
            shared: data.shared_servers.into_iter().collect(),
            file_path,
        })
    }

    /// Persist the store, owner-only.
    ///
    /// Why: this file grants access, so a torn write must never read back as a
    /// share — the write is atomic, and the result is `0600` like every other
    /// security-decision record under the operator's `$HOME`.
    /// What: creates the parent, writes pretty JSON to a temp file, renames,
    /// then (Unix) sets `0o600`.
    ///
    /// # Errors
    ///
    /// Any I/O failure in that sequence.
    /// Test: `save_and_reload_round_trip`, `save_is_owner_only`.
    pub fn save(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.file_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", parent.display()))?;
        }
        let data = ShareStoreData {
            shared_servers: self.shared.iter().cloned().collect(),
        };
        let json = serde_json::to_string_pretty(&data)
            .map_err(|e| anyhow::anyhow!("failed to serialize mcp-shared: {e}"))?;
        let tmp_path = self.file_path.with_extension("json.tmp");
        std::fs::write(&tmp_path, &json)
            .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", tmp_path.display()))?;
        if let Err(e) = std::fs::rename(&tmp_path, &self.file_path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(anyhow::anyhow!(
                "failed to rename {} -> {}: {e}",
                tmp_path.display(),
                self.file_path.display()
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&self.file_path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| {
                    anyhow::anyhow!(
                        "failed to set owner-only permissions on {}: {e}",
                        self.file_path.display()
                    )
                })?;
        }
        Ok(())
    }

    /// Share `name` with untrusted projects; `true` when this changed the store.
    /// Test: `share_inserts_and_is_idempotent`.
    pub fn share(&mut self, name: &str) -> bool {
        self.shared.insert(name.to_owned())
    }

    /// Stop sharing `name`; `true` when it was present.
    /// Test: `unshare_removes_and_is_idempotent`.
    pub fn unshare(&mut self, name: &str) -> bool {
        self.shared.remove(name)
    }

    /// Is `name` shared with untrusted projects?
    /// Test: `share_inserts_and_is_idempotent`.
    pub fn is_shared(&self, name: &str) -> bool {
        self.shared.contains(name)
    }

    /// The shared names, sorted.
    /// Test: `save_and_reload_round_trip`.
    pub fn names(&self) -> BTreeSet<String> {
        self.shared.clone()
    }
}

/// The tm-owned root holding this store — the same one `project-trust.json`
/// uses.
///
/// Why: one derivation, so the CLI writer and the launch reader can never point
/// at different files.
/// What: `~/.trusty-tools/trusty-mpm/`; `None` when the home directory cannot be
/// resolved.
/// Test: `shared_servers_fails_closed_on_a_missing_root`.
pub fn share_store_root() -> Option<PathBuf> {
    trusty_common::crate_config::crate_config_dir(crate::core::trusty_tools_config::CRATE_NAME)
}

/// The shared set, for a launch that must not fail.
///
/// Why: the launch path reads this on every spawn, so an unresolvable home, an
/// unreadable file, or malformed JSON must degrade to "nothing is shared"
/// rather than abort — fail closed, matching the store's whole purpose.
/// What: [`McpShareStore::names`] from the production root, or an empty set.
/// Test: `shared_servers_fails_closed_on_a_missing_root`,
/// `shared_servers_reads_a_real_store`.
pub fn shared_servers() -> BTreeSet<String> {
    share_store_root()
        .and_then(|root| McpShareStore::load(&root).ok())
        .map(|store| store.names())
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "mcp_share_tests.rs"]
mod tests;
