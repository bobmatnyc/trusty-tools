//! Compile-time embedded bundled agent set + user-tier first-run deploy.
//!
//! Why (#3405/#3406 follow-up): `tagent` launched from OUTSIDE this repo
//! checkout (e.g. `cd ~ && tagent --plain`, the owner's reported clean-shell
//! failure) has no project-tier `.trusty-agents/agents/` at all, and — until
//! this module — nothing ever populated the resolver's `$HOME` fallback tier
//! ([`super::loader`]'s `agents_dir_candidates`, and
//! [`super::registry::agent_search_paths`]) either. The bundled roster
//! (`assistant`, `ctrl`, `pm`, the specialist set, …) only ever existed as
//! plain files under THIS crate's own `.trusty-agents/agents/` — reachable
//! only when the process happens to run inside (or was built to auto-detect)
//! this checkout. A `cargo install`ed binary run from any other directory had
//! no bundled roster to fall back to, so `/switch assistant` (and the plain
//! CLI's default-persona switch) failed with a bare "file not found" and
//! silently degraded to `ctrl` (local Ollama) with no explanation.
//!
//! trusty-mpm solves the identical problem for ITS bundled agents/skills via
//! `include_str!` + an explicit `install` deploy step
//! (`crates/trusty-mpm/src/core/bundle.rs`); this module is the same pattern
//! adapted to trusty-agents' directory-package + flat-`.toml` agent formats
//! (too varied in shape for one `include_str!` constant per file, so this
//! uses `rust-embed` — already a dependency for the web UI bundle, see
//! `api/server/ui.rs` — to embed the whole `.trusty-agents/agents/` tree at
//! once).
//!
//! What: [`deploy_bundled_agents`] is the hermetic, path-injectable core —
//! writes every embedded file under `target_dir`, but ONLY when that path
//! doesn't already exist there, so a user's own edits/overrides (or a
//! previous deploy) are never clobbered. [`ensure_bundled_agents_deployed`]
//! is the production entry point: resolves `target_dir` to
//! `$HOME/.trusty-agents/agents` (the exact fallback tier
//! `agents_dir_candidates()` and `agent_search_paths()` already search) and
//! deploys there. Once deployed, NO other resolver code needed to change —
//! `AgentConfig::by_name`/`by_name_in`/`by_name_async` and the REPL's own
//! `[agents_dir, $HOME fallback]` list all already check that tier.
//!
//! Test: `deploy_writes_missing_files_only`, `deploy_is_idempotent_on_rerun`,
//! `deploy_never_overwrites_existing_file`,
//! `deploy_writes_directory_package_agents` (this file);
//! `ensure_bundled_agents_deployed` itself is exercised end-to-end by the
//! live-verify transcript in the PR description (it reaches into the real
//! `$HOME`, so it is not independently unit tested — mirrors every other
//! best-effort `dirs::home_dir()` operation in this crate, e.g. the REPL log
//! dir and user-profile save).

use std::path::Path;

use anyhow::{Context, Result};

/// Embeds this crate's own `.trusty-agents/agents/` tree — the shared
/// persona set (`assistant`, `ctrl`, `pm`, `cto-assistant`, `izzie`) and the
/// specialist roster (`python-engineer`, `qa-agent`, …) — into the compiled
/// binary at build time.
#[derive(rust_embed::RustEmbed)]
#[folder = ".trusty-agents/agents/"]
struct BundledAgents;

/// Idempotently materialise every embedded bundled-agent file under
/// `target_dir`, without ever overwriting a file that is already there.
///
/// Why: the hermetic core — takes `target_dir` as a parameter (rather than
/// resolving `$HOME` itself) so tests can point it at a tempdir instead of
/// touching the real filesystem outside the sandbox.
/// What: for each path `rust-embed` walked out of `.trusty-agents/agents/`,
/// skips it when `target_dir.join(path)` already exists (preserving any
/// prior deploy or user customization), otherwise creates parent
/// directories and writes the embedded bytes verbatim. Returns the count of
/// newly-written files.
/// Test: `deploy_writes_missing_files_only`, `deploy_is_idempotent_on_rerun`,
/// `deploy_never_overwrites_existing_file`,
/// `deploy_writes_directory_package_agents`.
pub fn deploy_bundled_agents(target_dir: &Path) -> Result<usize> {
    let mut written = 0usize;
    for rel in BundledAgents::iter() {
        let dest = target_dir.join(rel.as_ref());
        if dest.exists() {
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }
        let file = BundledAgents::get(&rel)
            .with_context(|| format!("embedded bundled-agent asset vanished: {rel}"))?;
        std::fs::write(&dest, file.data.as_ref())
            .with_context(|| format!("failed to write {}", dest.display()))?;
        written += 1;
    }
    Ok(written)
}

/// Production entry point: deploy the bundled agent set to
/// `$HOME/.trusty-agents/agents/` — the resolver's `$HOME` fallback tier —
/// so `tagent` launched from ANY directory resolves `assistant` (and the
/// rest of the bundled roster) instead of failing with "agent not found".
///
/// Why: called once per process from `runtime::startup::run_startup_init`,
/// mirroring where the credential-onboarding banner already runs — this is
/// the single choke point every interactive/one-shot invocation passes
/// through, but NOT `TrustyAgentsRepl::new` (many unit tests construct a REPL
/// directly without sandboxing `$HOME`; hooking there would make routine
/// `cargo test` runs write into a developer's real home directory).
/// What: no-op (`Ok(0)`) when `$HOME` cannot be resolved at all — matches
/// every other best-effort `dirs::home_dir()` operation in this crate (the
/// REPL log dir, the user-profile save). Never fatal: a deploy failure (e.g.
/// a read-only `$HOME`) is reported to the caller as an `Err` so it can log a
/// `warn` and continue — a missing bundled roster degrades to the pre-fix
/// "not found" error, it does not crash the harness.
pub fn ensure_bundled_agents_deployed() -> Result<usize> {
    let Some(home) = dirs::home_dir() else {
        return Ok(0);
    };
    let target = home.join(".trusty-agents").join("agents");
    deploy_bundled_agents(&target)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the core contract — a fresh empty target directory gets every
    /// embedded file, and the count matches what was actually written.
    /// Test: itself.
    #[test]
    fn deploy_writes_missing_files_only() {
        let tmp = tempfile::tempdir().unwrap();
        let written = deploy_bundled_agents(tmp.path()).unwrap();
        assert!(written > 0, "expected at least one bundled file deployed");
        assert!(
            tmp.path().join("analysis-agent.toml").is_file(),
            "flat .toml bundled agent should be deployed"
        );
    }

    /// Why: directory-package agents (`assistant/agent.toml` +
    /// `assistant/persona.md`) are the primary format the REPL's
    /// `/switch assistant` resolves — the exact case the owner's clean-shell
    /// repro hit. Both files inside the package directory must land.
    /// Test: itself.
    #[test]
    fn deploy_writes_directory_package_agents() {
        let tmp = tempfile::tempdir().unwrap();
        deploy_bundled_agents(tmp.path()).unwrap();
        assert!(
            tmp.path().join("assistant").join("agent.toml").is_file(),
            "assistant/agent.toml must be deployed"
        );
        assert!(
            tmp.path().join("assistant").join("persona.md").is_file(),
            "assistant/persona.md must be deployed"
        );
    }

    /// Why: re-running the deploy (every process startup) must not re-write
    /// files that already exist — the idempotency contract.
    /// Test: itself.
    #[test]
    fn deploy_is_idempotent_on_rerun() {
        let tmp = tempfile::tempdir().unwrap();
        let first = deploy_bundled_agents(tmp.path()).unwrap();
        assert!(first > 0);
        let second = deploy_bundled_agents(tmp.path()).unwrap();
        assert_eq!(second, 0, "re-running the deploy must write nothing new");
    }

    /// Why: a user who has customized a bundled agent (or a prior deploy's
    /// output) must never be silently overwritten — the "never clobber"
    /// contract that lets a user safely edit their deployed copy.
    /// Test: itself.
    #[test]
    fn deploy_never_overwrites_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("analysis-agent.toml"),
            "# user customized\n",
        )
        .unwrap();
        deploy_bundled_agents(tmp.path()).unwrap();
        let contents = std::fs::read_to_string(tmp.path().join("analysis-agent.toml")).unwrap();
        assert_eq!(contents, "# user customized\n");
    }

    /// Why: `PathBuf` returned by `deploy_bundled_agents` must be usable
    /// directly as an `agents_dir_candidates()`/REPL `$HOME` tier — assert
    /// the deployed layout round-trips through `AgentConfig::by_name_in`
    /// exactly like a real `$HOME/.trusty-agents/agents` would.
    /// Test: itself.
    #[test]
    fn deployed_assistant_resolves_via_by_name_in() {
        let tmp = tempfile::tempdir().unwrap();
        deploy_bundled_agents(tmp.path()).unwrap();
        let cfg = crate::agents::AgentConfig::by_name_in(&[tmp.path().to_path_buf()], "assistant")
            .expect("deployed assistant package must resolve");
        assert_eq!(cfg.agent.name, "assistant");
    }
}
