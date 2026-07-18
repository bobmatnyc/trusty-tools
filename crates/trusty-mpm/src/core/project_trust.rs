//! Local per-project trust store gating project-scope custom MCP bridging.
//!
//! Why (issue #3033 security review, `session_launch::custom_mcp`): a
//! project-scope `[mcp.custom]` manifest entry ships WITH the cloned repo
//! itself, unlike a user-scope `tm mcp add` registry entry, which the
//! operator personally typed into their OWN machine. Without an explicit
//! consent step, a malicious or compromised repo could smuggle a live MCP
//! server declaration (arbitrary stdio command execution, or a remote
//! endpoint impersonating `trusty-memory`/`trusty-search`) into every fleet
//! session launched against it, the very first time anyone runs `tm session
//! start` against that clone. This store is the durable, per-project,
//! USER-scope (never repo-scope) trust decision that gates that: it lives
//! under `~/.trusty-tools/trusty-mpm/`, the same tm-owned root
//! [`crate::core::trusty_tools_config::managed_claude_config_dir`] nests
//! `claude-config/` under — never inside any project working directory — so a
//! cloned repo cannot flip its own trust bit no matter what it contains.
//! `session_launch::custom_mcp::inject_custom_trusty_mcps` calls
//! [`is_project_trusted`] before honoring ANY project-scope `[mcp.custom]`
//! entry for a given project path; the `tm project trust` /
//! `tm project trust --revoke` CLI verbs (`bin/tm/commands/project.rs`) are
//! the only way to flip it. User-scope registry entries (`tm mcp add`) are
//! NOT gated by this store — the operator already consented to those by
//! registering them locally.
//!
//! What: [`ProjectTrustStore`] persists a set of trusted, canonicalized
//! project paths to `<root>/project-trust.json`. [`ProjectTrustStore::load`]/
//! [`ProjectTrustStore::save`] are the read/write primitives (missing file →
//! empty store, mirroring [`super::project_aliases::ProjectAliasStore`]);
//! [`ProjectTrustStore::trust`]/[`ProjectTrustStore::revoke`] mutate the set
//! and report whether anything changed (so the CLI only writes when needed);
//! [`ProjectTrustStore::is_trusted`] answers a membership query. Paths are
//! canonicalized on both insert and lookup so `tm project trust .` and a
//! later absolute-path lookup from `session_launch` agree even across a
//! trailing slash or a symlinked ancestor; a path that cannot be canonicalized
//! (e.g. it no longer exists) falls back to the path as given rather than
//! failing the operation. [`is_project_trusted`] is the convenience, non-fatal
//! entry point `session_launch::custom_mcp` calls: it resolves the production
//! store root and treats ANY error (unreadable home dir, malformed JSON, I/O
//! failure) as "not trusted" — fail-closed, matching this store's whole
//! purpose.
//! Test: `trust_store_new_is_empty`, `trust_inserts_new_entry`,
//! `trust_is_idempotent`, `revoke_removes_trust`,
//! `revoke_unknown_path_is_noop`, `is_trusted_after_trust_and_revoke`,
//! `save_and_reload_round_trip`, `is_project_trusted_at_reflects_store`,
//! `is_project_trusted_at_fails_closed_when_root_missing`.
//!
//! **Trust is per-directory, NOT per-repo-content (issue #3033, MEDIUM
//! finding 2 — read this before relying on a trust decision).** [`normalize`]
//! canonicalizes and stores a filesystem PATH — it records nothing about what
//! is checked out there (no commit hash, no remote URL, no content digest).
//! Once a directory is trusted, EVERY future `[mcp.custom]` read from that
//! same canonical path is honored, no matter what later replaces the
//! directory's contents: `rm -rf` + `git clone <different-repo>` into the same
//! path, `git checkout` to an attacker-controlled branch, or any other content
//! swap all inherit the existing trust grant silently. To get a fresh consent
//! decision for genuinely different content, either **clone to a new path**
//! (a new canonical path is untrusted by default) or **explicitly
//! `tm project trust --revoke <path>` before, and re-trust after, replacing a
//! trusted directory's contents.** A future hardening — binding the trust
//! record to the git remote URL and/or toplevel commit rather than the bare
//! path — is tracked as a follow-up (issue #3051, filed fresh since #3045 is
//! the unrelated stale-`.mcp.json`-entry-pruning tracker) and is deliberately
//! NOT implemented here.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::trusty_tools_config::CRATE_NAME;

/// On-disk filename for the project-trust store, nested under
/// `~/.trusty-tools/trusty-mpm/` (the same root
/// [`crate::core::trusty_tools_config::managed_claude_config_dir`] nests
/// `claude-config/` under).
const TRUST_STORE_FILE: &str = "project-trust.json";

/// Canonicalize `path` best-effort, falling back to `path` unchanged when
/// canonicalization fails (e.g. the directory does not exist in this
/// environment).
///
/// Why: a single normalization rule must be shared by `trust`/`revoke`/
/// `is_trusted` so a path recorded one way is still found looked up another
/// (trailing slash, relative vs. absolute, symlinked ancestor).
/// What: `path.canonicalize()`, falling back to `path.to_path_buf()`.
/// Test: covered transitively by `is_trusted_after_trust_and_revoke`.
fn normalize(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// In-memory + on-disk registry of operator-trusted project paths.
///
/// Why: mirrors [`super::project_aliases::ProjectAliasStore`]'s
/// load-mutate-save shape so this store follows the SAME established,
/// already-reviewed persistence pattern rather than inventing a new one.
/// What: a `BTreeSet<PathBuf>` of canonicalized trusted paths plus the
/// resolved `file_path`. Mutations are committed by explicit `save()` calls.
/// Test: `trust_store_new_is_empty`, `save_and_reload_round_trip`.
#[derive(Debug, Default)]
pub struct ProjectTrustStore {
    trusted: BTreeSet<PathBuf>,
    file_path: PathBuf,
}

/// On-disk shape: a bare sorted array of path strings, kept intentionally
/// minimal (no timestamps or metadata) since the only question this file ever
/// answers is membership.
#[derive(Debug, Default, Serialize, Deserialize)]
struct TrustStoreData {
    #[serde(default)]
    trusted_paths: Vec<PathBuf>,
}

impl ProjectTrustStore {
    /// Load (or create) the trust store at `<root>/project-trust.json`.
    ///
    /// Why: a missing file means "no project has been trusted yet" — the
    /// fail-closed default this whole module exists to enforce — so it is
    /// treated as an empty store rather than an error.
    /// What: reads and deserializes the JSON file when present; returns an
    /// empty store with the correct path when the file is absent.
    /// Test: `trust_store_new_is_empty`.
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let file_path = root.join(TRUST_STORE_FILE);
        if !file_path.exists() {
            return Ok(Self {
                trusted: BTreeSet::new(),
                file_path,
            });
        }
        let raw = std::fs::read_to_string(&file_path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", file_path.display()))?;
        let data: TrustStoreData = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", file_path.display()))?;
        Ok(Self {
            trusted: data.trusted_paths.into_iter().collect(),
            file_path,
        })
    }

    /// Persist the store to disk.
    ///
    /// Why: an atomic write-then-rename avoids ever leaving a half-written
    /// trust file (this file gates a security decision, so a torn write must
    /// never be silently read back as "trusted"). On Unix, the file is also
    /// restricted to owner-only `0o600` (issue #3033, LOW finding) — it is a
    /// security-decision record under the operator's own `$HOME`, so it should
    /// never be group/world-readable on a shared machine.
    /// What: creates parent directories if absent, serialises the sorted path
    /// list to pretty JSON, writes atomically via `rename`, then (Unix only)
    /// sets the final file's permissions to `0o600`.
    /// Test: `save_and_reload_round_trip`, `save_sets_owner_only_perms_on_unix`.
    pub fn save(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.file_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", parent.display()))?;
        }
        let data = TrustStoreData {
            trusted_paths: self.trusted.iter().cloned().collect(),
        };
        let json = serde_json::to_string_pretty(&data)
            .map_err(|e| anyhow::anyhow!("failed to serialize project-trust: {e}"))?;
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
            use std::os::unix::fs::PermissionsExt;
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

    /// Mark `path` as trusted.
    ///
    /// What: inserts the canonicalized path; returns `true` when this changed
    /// the store (caller should `save()`), `false` when it was already trusted.
    /// Test: `trust_inserts_new_entry`, `trust_is_idempotent`.
    pub fn trust(&mut self, path: &Path) -> bool {
        self.trusted.insert(normalize(path))
    }

    /// Revoke trust for `path`.
    ///
    /// What: removes the canonicalized path; returns `true` when it was
    /// present (caller should `save()`), `false` when it was already absent.
    /// Test: `revoke_removes_trust`, `revoke_unknown_path_is_noop`.
    pub fn revoke(&mut self, path: &Path) -> bool {
        self.trusted.remove(&normalize(path))
    }

    /// Whether `path` is currently trusted.
    ///
    /// What: membership test against the canonicalized path.
    /// Test: `is_trusted_after_trust_and_revoke`.
    pub fn is_trusted(&self, path: &Path) -> bool {
        self.trusted.contains(&normalize(path))
    }

    /// All trusted paths, sorted.
    ///
    /// Why: lets the `project list`/`project info` CLI surface trust status
    /// without exposing the internal `BTreeSet` type.
    /// What: a cloned, already-sorted `Vec<PathBuf>` (a `BTreeSet` iterates in
    /// order).
    /// Test: covered transitively by `save_and_reload_round_trip`.
    pub fn list(&self) -> Vec<PathBuf> {
        self.trusted.iter().cloned().collect()
    }
}

/// Resolve the production root the trust store is nested under:
/// `~/.trusty-tools/trusty-mpm/`.
///
/// Why: the SAME base [`crate::core::trusty_tools_config::managed_claude_config_dir`]
/// resolves `claude-config/` under, so the trust file lives alongside the
/// rest of tm's user-scope state rather than inventing a new base directory.
/// What: delegates to [`trusty_common::crate_config::crate_config_dir`].
/// Returns `None` only when the home directory cannot be resolved (a
/// stripped CI environment) — callers treat that as "no trust store, so
/// fail closed".
/// Test: exercised indirectly; hermetic tests use [`ProjectTrustStore::load`]
/// against a tempdir root directly instead of this accessor.
pub fn trust_store_root() -> Option<PathBuf> {
    trusty_common::crate_config::crate_config_dir(CRATE_NAME)
}

/// Non-fatal, fail-closed convenience check: is `project_path` trusted?
///
/// Why: `session_launch::custom_mcp` must never let a store read failure
/// (missing home dir, malformed JSON, permissions) silently fall through to
/// "trusted" — the entire point of this module is a fail-closed gate, so
/// every error path here returns `false`.
/// What: resolves [`trust_store_root`]; on `None` (or a load error, logged at
/// `warn!`) returns `false`. Otherwise delegates to
/// [`ProjectTrustStore::is_trusted`].
/// Test: `is_project_trusted_at_reflects_store`,
/// `is_project_trusted_at_fails_closed_when_root_missing` (via the hermetic
/// [`is_project_trusted_at`] used by tests; production wiring is exercised in
/// `custom_mcp_tests.rs`).
pub fn is_project_trusted(project_path: &Path) -> bool {
    match trust_store_root() {
        Some(root) => is_project_trusted_at(project_path, &root),
        None => {
            tracing::warn!(
                "skipping project-trust check: cannot resolve the home directory — \
                 treating project as untrusted (fail-closed)"
            );
            false
        }
    }
}

/// Hermetic core of [`is_project_trusted`] against an explicit store root.
///
/// Why: splitting the root resolution from the actual check lets tests target
/// a tempdir root without touching the real `$HOME`, while production code
/// still goes through the single [`is_project_trusted`] entry point.
/// What: loads the store at `root`; a load error is logged at `warn!` and
/// treated as untrusted (fail-closed, mirroring the native/custom MCP
/// injectors' own "never fatal to launch" posture — except here the fail-safe
/// direction is "skip the bridge", not "allow it").
/// Test: `is_project_trusted_at_reflects_store`,
/// `is_project_trusted_at_fails_closed_when_root_missing`.
pub fn is_project_trusted_at(project_path: &Path, root: &Path) -> bool {
    match ProjectTrustStore::load(root) {
        Ok(store) => store.is_trusted(project_path),
        Err(err) => {
            tracing::warn!(
                "failed to read project-trust store: {err} — treating project as untrusted \
                 (fail-closed)"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store(dir: &TempDir) -> ProjectTrustStore {
        ProjectTrustStore::load(dir.path()).expect("load")
    }

    #[test]
    fn trust_store_new_is_empty() {
        let root = TempDir::new().expect("tmpdir");
        let store = make_store(&root);
        assert!(store.list().is_empty());
    }

    #[test]
    fn trust_inserts_new_entry() {
        let root = TempDir::new().expect("tmpdir");
        let project = TempDir::new().expect("project dir");
        let mut store = make_store(&root);

        assert!(store.trust(project.path()), "new trust should return true");
        assert!(store.is_trusted(project.path()));
    }

    #[test]
    fn trust_is_idempotent() {
        let root = TempDir::new().expect("tmpdir");
        let project = TempDir::new().expect("project dir");
        let mut store = make_store(&root);

        assert!(store.trust(project.path()));
        let changed_again = store.trust(project.path());
        assert!(!changed_again, "re-trusting the same path must be a no-op");
    }

    #[test]
    fn revoke_removes_trust() {
        let root = TempDir::new().expect("tmpdir");
        let project = TempDir::new().expect("project dir");
        let mut store = make_store(&root);

        store.trust(project.path());
        assert!(store.is_trusted(project.path()));

        let revoked = store.revoke(project.path());
        assert!(revoked, "revoke must report the change");
        assert!(!store.is_trusted(project.path()));
    }

    #[test]
    fn revoke_unknown_path_is_noop() {
        let root = TempDir::new().expect("tmpdir");
        let project = TempDir::new().expect("project dir");
        let mut store = make_store(&root);

        let revoked = store.revoke(project.path());
        assert!(!revoked, "revoking an untrusted path must be a no-op");
    }

    #[test]
    fn is_trusted_after_trust_and_revoke() {
        let root = TempDir::new().expect("tmpdir");
        let project_a = TempDir::new().expect("project a");
        let project_b = TempDir::new().expect("project b");
        let mut store = make_store(&root);

        store.trust(project_a.path());
        assert!(store.is_trusted(project_a.path()));
        assert!(
            !store.is_trusted(project_b.path()),
            "an unrelated path must never read as trusted"
        );
    }

    #[test]
    fn save_and_reload_round_trip() {
        let root = TempDir::new().expect("tmpdir");
        let project = TempDir::new().expect("project dir");
        {
            let mut store = make_store(&root);
            store.trust(project.path());
            store.save().expect("save");
        }
        {
            let store2 = make_store(&root);
            assert!(store2.is_trusted(project.path()));
            assert_eq!(store2.list().len(), 1);
        }
    }

    #[test]
    fn is_project_trusted_at_reflects_store() {
        let root = TempDir::new().expect("tmpdir");
        let project = TempDir::new().expect("project dir");

        assert!(
            !is_project_trusted_at(project.path(), root.path()),
            "must be untrusted before any `trust` call"
        );

        let mut store = make_store(&root);
        store.trust(project.path());
        store.save().expect("save");

        assert!(is_project_trusted_at(project.path(), root.path()));
    }

    #[test]
    #[cfg(unix)]
    fn save_sets_owner_only_perms_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let root = TempDir::new().expect("tmpdir");
        let project = TempDir::new().expect("project dir");
        let mut store = make_store(&root);
        store.trust(project.path());
        store.save().expect("save");

        let meta = std::fs::metadata(root.path().join("project-trust.json")).expect("metadata");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "project-trust.json must be owner-read/write only, got {mode:o}"
        );
    }

    #[test]
    fn is_project_trusted_at_fails_closed_when_root_missing() {
        // A root whose parent does not exist at all still yields a clean
        // empty-store load (load() only checks file existence, not root
        // existence), so this asserts the more realistic failure: a
        // project-trust.json that is present but malformed must fail closed,
        // not panic or default to trusted.
        let root = TempDir::new().expect("tmpdir");
        std::fs::write(root.path().join("project-trust.json"), "not json").unwrap();
        let project = TempDir::new().expect("project dir");

        assert!(
            !is_project_trusted_at(project.path(), root.path()),
            "a malformed trust store must fail closed (untrusted), never panic or default open"
        );
    }
}
