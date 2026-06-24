//! Clone or refresh a managed project workspace for a registered alias.
//!
//! Why: `tm load <alias>` is the idempotent entry point that turns a registered
//! name into a ready-to-drive, isolated project directory (DOC-24
//! SPEC-STANDALONE-MPM-02). Making it idempotent (clone-once, refresh-many)
//! means `run` can call it unconditionally and it also serves as the
//! "bring this project up to date" verb.
//! What: [`load_alias`] resolves the alias from the registry, clones (or
//! fast-forward-pulls) the repo into
//! `<managed_root>/projects/<alias>/repo/`, runs `prepare_session` from the
//! session-launch core, writes `.trusty-mpm/managed.toml`, and returns the
//! absolute path to `repo/`.
//! Test: unit tests for the marker-file write logic; git operations are
//! integration-only (require network/git binary).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use super::registry::ManagedRegistry;

/// The marker file written into every managed project.
///
/// Why: the marker lets `path`, `ls`, `rm`, and `update` know a directory is
/// tm-managed and carries the metadata needed to replay a launch deterministically
/// (DOC-24 SPEC-STANDALONE-MPM-03d).
/// What: a TOML struct at `.trusty-mpm/managed.toml`.
/// Test: `test_marker_write_round_trip`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ManagedMarker {
    /// The alias this project was cloned from.
    pub alias: String,
    /// The clone URL.
    pub url: String,
    /// Sentinel for the branch/ref intent (currently always `"default"`, meaning
    /// the remote's default branch was cloned without `-b`; branch selection is
    /// a future enhancement — do not interpret this as a literal Git ref name).
    pub git_ref: String,
    /// Absolute path to the tm-global CLAUDE_CONFIG_DIR.
    pub claude_config_dir: String,
}

/// Load (clone or refresh) the managed workspace for `alias`.
///
/// Why: the idempotent load verb is the load-bearing primitive behind `run`;
/// separating it lets callers (`tm load`, `tm run`) both call it without
/// duplicating the clone/prepare logic.
/// What:
/// 1. Looks up the alias in the registry under `managed_root`.
/// 2. Derives `<managed_root>/projects/<alias>/repo/` as the checkout dir.
/// 3. If `repo/` doesn't exist: clones via `git clone --depth 1 <url> repo/`.
/// 4. If `repo/` exists: `git -C repo/ pull --ff-only` (best-effort, non-fatal).
/// 5. Runs `prepare_session` from `crate::core::session_launch` on `repo/`.
/// 6. Writes `.trusty-mpm/managed.toml`.
/// 7. (WI-3) Pre-seeds project trust + MCP-server approval into
///    `<claude_config_dir>/.claude.json` via [`super::trust_seed::preseed_managed_trust`].
/// 8. Returns the absolute `PathBuf` to `repo/`.
///
/// The trust seed (step 7) writes ONLY into `<claude_config_dir>` — never to
/// `~/.claude.json` or `~/.claude/` (isolation invariant, WI-7).
///
/// Test: `test_marker_write_round_trip` (marker); git operations require network.
pub fn load_alias(
    alias: &str,
    managed_root: &Path,
    claude_config_dir: &Path,
) -> anyhow::Result<PathBuf> {
    let registry = ManagedRegistry::load(managed_root)
        .with_context(|| format!("failed to load registry from {}", managed_root.display()))?;
    let entry = registry
        .get(alias)
        .with_context(|| format!("alias '{alias}' is not registered"))?;
    let url = entry.url.clone();
    let git_ref = entry.git_ref.clone();

    let project_dir = managed_root.join("projects").join(alias);
    let repo_dir = project_dir.join("repo");

    if !repo_dir.exists() {
        clone_repo(&url, &project_dir)?;
    } else {
        pull_ff_only(&repo_dir);
    }

    // Issue #1651: thread the registry clone `url` as the authoritative git
    // remote so the per-project `repo/.mcp.json` pins `TRUSTY_MEMORY_PALACE` to
    // the repo's canonical `owner-repo` slug (the same `derive_palace_id`
    // mechanism #1605 uses for the per-project injection). The clone URL is more
    // authoritative than probing the checkout's own origin remote (which a
    // shallow / renamed / origin-less clone might lack); the operator
    // `TRUSTY_MEMORY_PALACE` override still wins over it per `derive_palace_id`
    // precedence.
    run_prepare_session(&repo_dir, Some(&url))?;
    write_marker(&repo_dir, alias, &url, &git_ref, claude_config_dir)?;

    // WI-3 sub-parts 2+3: pre-seed project trust + MCP-server approval into
    // <claude_config_dir>/.claude.json so managed sessions start without dialogs.
    // Writes ONLY to <claude_config_dir>/.claude.json — never to ~/.claude.json.
    // Non-fatal: a seed failure only means the operator may see the trust dialog.
    if let Err(err) = super::trust_seed::preseed_managed_trust(claude_config_dir, &repo_dir) {
        tracing::warn!("failed to pre-seed managed trust for '{alias}': {err}");
    }

    Ok(repo_dir)
}

/// Clone the repository into `<project_dir>/repo/`.
///
/// Why: `git clone` is the authoritative way to get a fresh checkout;
/// shelling out avoids a heavy libgit2 dependency.
/// What: runs `git clone --depth 1 <url> repo/` in `project_dir`, creating
/// the directory first.
/// Test: integration-only (requires git binary + network).
fn clone_repo(url: &str, project_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(project_dir).with_context(|| {
        format!(
            "failed to create project directory {}",
            project_dir.display()
        )
    })?;
    let status = Command::new("git")
        .args(["clone", "--depth", "1", url, "repo"])
        .current_dir(project_dir)
        .status()
        .context("failed to spawn git clone")?;
    if !status.success() {
        anyhow::bail!("git clone failed for '{url}'");
    }
    Ok(())
}

/// Attempt a fast-forward pull; non-fatal on any failure.
///
/// Why: idempotent `load` should refresh an existing checkout but must never
/// destroy user edits — fast-forward only is the safest pull strategy.
/// What: runs `git -C <repo_dir> pull --ff-only`; silently ignores failures
/// (dirty tree, network unavailable) so a load with local modifications still
/// succeeds.
/// Test: integration-only.
fn pull_ff_only(repo_dir: &Path) {
    let result = Command::new("git")
        .args(["pull", "--ff-only"])
        .current_dir(repo_dir)
        .status();
    if let Ok(status) = result
        && !status.success()
    {
        eprintln!(
            "warning: git pull --ff-only failed in {}; skipping refresh",
            repo_dir.display()
        );
    }
}

/// Run `prepare_session` on the given repo directory, pinning the palace to the
/// cloned-from `repo_url` (issue #1651).
///
/// Why: `prepare_session` deploys composed agents, skills, and CLAUDE.md so the
/// project-local half of the managed configuration is complete — and (issue
/// #1605/#1651) injects the per-project `repo/.mcp.json` trusty-memory stub.
/// Threading the registry clone URL as the authoritative git remote makes the
/// injector pin `env.TRUSTY_MEMORY_PALACE` to the repo's canonical `owner-repo`
/// slug (the highest-precedence `derive_palace_id` source after the operator
/// override), so the managed session lands in the SAME palace a non-tm Claude
/// session in that repo would use. `None` falls back to probing the checkout's
/// own `git remote get-url origin`, then to the directory-basename slug.
/// What: resolves `FrameworkPaths` from the home directory and calls
/// `crate::core::session_launch::prepare_session_with_repo_url`, forwarding
/// `repo_url` to the trusty-memory MCP palace-slug derivation.
/// Test: `run_prepare_session_pins_palace_from_clone_url`,
/// `run_prepare_session_bare_stub_when_no_identity`; `prepare_session` itself is
/// covered in session_launch/tests.rs.
fn run_prepare_session(repo_dir: &Path, repo_url: Option<&str>) -> anyhow::Result<()> {
    let fw = crate::core::paths::FrameworkPaths::default();
    crate::core::session_launch::prepare_session_with_repo_url(&fw, repo_dir, repo_url)
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("prepare_session failed: {e}"))
}

/// Write `.trusty-mpm/managed.toml` into the repo directory.
///
/// Why: the marker lets every other lifecycle verb (`path`, `ls`, `rm`) detect
/// a managed directory and read the metadata needed to replay a launch.
/// What: creates `.trusty-mpm/` if absent, serializes [`ManagedMarker`] to
/// TOML, and writes `managed.toml`.
/// Test: `test_marker_write_round_trip`.
fn write_marker(
    repo_dir: &Path,
    alias: &str,
    url: &str,
    git_ref: &str,
    claude_config_dir: &Path,
) -> anyhow::Result<()> {
    let dot_dir = repo_dir.join(".trusty-mpm");
    std::fs::create_dir_all(&dot_dir)
        .with_context(|| format!("failed to create {}", dot_dir.display()))?;
    let marker = ManagedMarker {
        alias: alias.to_string(),
        url: url.to_string(),
        git_ref: git_ref.to_string(),
        claude_config_dir: claude_config_dir.to_string_lossy().to_string(),
    };
    let toml = toml::to_string_pretty(&marker).context("failed to serialize managed.toml")?;
    std::fs::write(dot_dir.join("managed.toml"), toml)
        .context("failed to write .trusty-mpm/managed.toml")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// RAII guard that clears an env var for the duration of a test and restores
    /// the prior value on drop.
    ///
    /// Why: the palace-pin test must be hermetic against an ambient
    /// `TRUSTY_MEMORY_PALACE` override (which `derive_palace_id` honours at
    /// highest precedence and would otherwise mask the URL-derived slug).
    /// What: snapshots the prior value, removes the var, and restores it on drop.
    /// Pairs with `#[serial_test::serial]` so concurrent tests never race on the
    /// shared process environment.
    /// Test: used by `run_prepare_session_pins_palace_from_clone_url`.
    struct EnvClearGuard {
        key: &'static str,
        prior: Option<String>,
    }

    impl EnvClearGuard {
        fn clear(key: &'static str) -> Self {
            let prior = std::env::var(key).ok();
            // SAFETY: tests run serially under `#[serial_test::serial]`, so no
            // other thread reads/writes the environment concurrently.
            unsafe { std::env::remove_var(key) };
            Self { key, prior }
        }
    }

    impl Drop for EnvClearGuard {
        fn drop(&mut self) {
            // SAFETY: see `clear`.
            unsafe {
                match self.prior.take() {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    /// Why (issue #1651): the standalone `load` path must pin the per-project
    /// `repo/.mcp.json` trusty-memory palace to the registry CLONE URL's
    /// canonical `owner-repo` slug — the same `derive_palace_id` mechanism #1605
    /// uses — rather than leaving a bare stub or relying on probing the
    /// checkout's own origin remote. Threading the clone URL explicitly is the
    /// authoritative identity (a shallow/origin-less clone might lack a remote).
    /// What: runs `run_prepare_session` on a repo dir with an explicit clone URL
    /// and asserts the injected `.mcp.json` carries
    /// `env.TRUSTY_MEMORY_PALACE == "bobmatnyc-trusty-tools"`.
    /// Test: itself.
    #[test]
    #[serial_test::serial]
    fn run_prepare_session_pins_palace_from_clone_url() {
        // Hermetic: an ambient override would win over the URL-derived slug.
        let _guard = EnvClearGuard::clear("TRUSTY_MEMORY_PALACE");
        let tmp = TempDir::new().unwrap();
        // Deliberately a session-id-like basename: the pin must come from the
        // clone URL, NOT this directory name.
        let repo = tmp.path().join("0c8f1a2b3c4d");
        std::fs::create_dir_all(&repo).unwrap();

        run_prepare_session(&repo, Some("git@github.com:bobmatnyc/trusty-tools.git"))
            .expect("run_prepare_session succeeds");

        let mcp_path = repo.join(".mcp.json");
        assert!(
            mcp_path.exists(),
            "run_prepare_session must write repo/.mcp.json"
        );
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mcp_path).unwrap()).unwrap();
        assert_eq!(
            value["mcpServers"]["trusty-memory"]["env"]["TRUSTY_MEMORY_PALACE"],
            serde_json::json!("bobmatnyc-trusty-tools"),
            "standalone load must pin the palace to the clone URL's owner-repo slug, \
             not the workspace basename"
        );
    }

    /// Why (issue #1651): when no clone URL is threaded AND the checkout has no
    /// git origin remote AND no env override, the injection must fail open with
    /// a BARE trusty-memory stub (no `env` key) so the session still gets the
    /// memory tools — backward-compatible with the pre-#1651 behaviour.
    /// What: runs `run_prepare_session(repo, None)` on a non-repo dir whose
    /// basename does not slugify and asserts the trusty-memory block has no
    /// `TRUSTY_MEMORY_PALACE` pin. (A non-empty basename would fall back to the
    /// parent-dir slug, so we assert only the no-`env` shape is preserved when
    /// pinning is impossible.)
    /// Test: itself.
    #[test]
    #[serial_test::serial]
    fn run_prepare_session_bare_stub_when_no_identity() {
        let _guard = EnvClearGuard::clear("TRUSTY_MEMORY_PALACE");
        let tmp = TempDir::new().unwrap();
        // A basename that slugifies to empty (only separators) cannot yield a
        // parent-dir slug, so with no URL/remote/override the stub stays bare.
        let repo = tmp.path().join("---");
        std::fs::create_dir_all(&repo).unwrap();

        run_prepare_session(&repo, None).expect("run_prepare_session succeeds");

        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(repo.join(".mcp.json")).unwrap())
                .unwrap();
        let memory = &value["mcpServers"]["trusty-memory"];
        assert_eq!(
            memory["command"],
            serde_json::json!("trusty-memory"),
            "the trusty-memory stub must still be present"
        );
        assert!(
            memory.get("env").is_none(),
            "with no derivable identity the stub must be bare (no TRUSTY_MEMORY_PALACE env); got {memory}"
        );
    }

    #[test]
    fn test_marker_write_round_trip() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let cfg = tmp.path().join("claude-config");

        write_marker(
            &repo,
            "my-alias",
            "https://github.com/org/r",
            "default",
            &cfg,
        )
        .unwrap();

        let toml_path = repo.join(".trusty-mpm").join("managed.toml");
        assert!(toml_path.exists());
        let text = std::fs::read_to_string(&toml_path).unwrap();
        let marker: ManagedMarker = toml::from_str(&text).unwrap();
        assert_eq!(marker.alias, "my-alias");
        assert_eq!(marker.url, "https://github.com/org/r");
        assert_eq!(
            marker.git_ref, "default",
            "git_ref must be the 'default' sentinel, not a literal branch name"
        );
    }
}
