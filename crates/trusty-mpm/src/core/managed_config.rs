//! Provision the shared tm-owned `CLAUDE_CONFIG_DIR` for daemon-managed sessions
//! (DOC-34 / #1996).
//!
//! Why: the daemon managed-spawn path (`ClaudeCodeAdapter`) previously launched
//! `claude` with NO `CLAUDE_CONFIG_DIR`, so Claude Code read the *target
//! project's* committed `.claude/` — for trusty-tools that is a partial, stale
//! agent roster (missing most specialists, carrying a malformed `rust-engineer`)
//! that shadowed the framework set and forced the general-purpose fallback
//! (#1996). The **standalone** driver already does the right thing via
//! [`super::standalone::global_config::ensure_global_config_dir`]; this module
//! gives the DAEMON path the same isolation primitive, pointed at
//! `~/.trusty-tools/trusty-mpm/claude-config/` (the shared #1220 base) instead of
//! the standalone `~/.trusty-mpm/claude-config/`.
//!
//! CRITICAL: the config dir must carry the **COMPLETE** agent roster (engineer,
//! rust-engineer, research, qa, local-ops, version-control, ticketing,
//! documentation, security + every specialist) and all `tm-*` skills — never a
//! manifest-filtered subset. We therefore deploy from the resolved framework
//! *source* dirs ([`FrameworkPaths::agent_source_dir`] / `skill_source_dir`,
//! submodule-aware — the same source `session_launch::prepare_session` uses)
//! with an unfiltered `deploy_agents` / `deploy_skills`, so the full set lands
//! whether the binary is a dev checkout (git submodule source) or an installed
//! binary (`~/.trusty-mpm/framework/*`).
//!
//! WHICH LAYER ACTUALLY LOADS THE ROSTER (DOC-34 review, empirically verified
//! against `claude` 2.1.201): the DAEMON managed-spawn path launches `claude`
//! with `--setting-sources project,local` (see `runtime::claude_code`), which
//! restricts subagent/skill/settings discovery to the `project` + `local` tiers
//! and EXCLUDES the `user` tier that `CLAUDE_CONFIG_DIR` relocates. So under the
//! daemon path the agents/skills deployed HERE are NOT read — the roster is
//! delivered by the PROJECT layer (`<workspace>/.claude/{agents,skills}`, which
//! `prepare_session` deploys) and the config-dir roster is belt-and-suspenders
//! (it would load only if that flag were dropped). The config dir's load-bearing
//! role on the daemon path is instead AUTH + TRUST isolation (keychain /
//! `.credentials.json` / `.claude.json` keyed to this path). The **standalone**
//! `tm run` driver, by contrast, passes NO `--setting-sources` flag, so THERE
//! the full roster/skills/hooks this module deploys DO load from the config dir —
//! which is why provisioning the complete set here remains mandatory.
//!
//! What: [`ensure_managed_config_dir`] (1) runs the canonical standalone
//! scaffolding ([`ensure_global_config_dir`] — settings.json + the MPM hook
//! triad + `.mcp.json` + output-styles, plus a best-effort deploy from the
//! `framework/*` dirs), then (2) refreshes the framework source and deploys the
//! FULL agent/skill roster from the resolved source dirs so the complete,
//! spawnable set is guaranteed present. Idempotent — safe to call on every
//! spawn (the deployers checksum-skip unchanged files).
//! Test: `ensure_managed_config_dir_deploys_full_roster`,
//! `ensure_managed_config_dir_is_idempotent`.

use std::path::Path;

use crate::core::agent_deployer::deploy_agents;
use crate::core::paths::FrameworkPaths;
use crate::core::skill_tiers::deploy_all_skill_tiers;
use crate::core::standalone::global_config::ensure_global_config_dir;

/// Provision (idempotently) the tm-owned managed `CLAUDE_CONFIG_DIR`.
///
/// Why: called from the daemon managed-spawn path immediately before the
/// launched `claude` is pointed at `config_dir` via `CLAUDE_CONFIG_DIR`, giving
/// the session an isolated, fully-scaffolded config home (auth, trust, MCP
/// stubs, output-styles) plus the full framework roster/skills. NOTE (DOC-34
/// review): under the daemon path's `--setting-sources project,local` flag the
/// roster/skills deployed here are NOT read (that flag excludes the `user` tier
/// this dir relocates) — the roster loads from the PROJECT layer via
/// `prepare_session`; the config-dir provisioning is belt-and-suspenders for the
/// daemon and load-bearing for the flag-less standalone `tm run` path (see the
/// module doc). Runs on every spawn because it is cheap and idempotent, matching
/// the standalone `tm run` contract.
/// What: resolves the framework layout from the daemon's fixed home-relative
/// root ([`FrameworkPaths::default`]), then:
/// 1. calls [`ensure_global_config_dir`]`(&fw.root, config_dir)` for the shared
///    scaffolding (settings.json, the MPM hook triad, `.mcp.json`, output-styles,
///    credential seed) — this also deploys agents/skills from the `framework/*`
///    dirs, which may be empty on a dev checkout;
/// 2. refreshes the bundled skill source ([`crate::core::skill_source::ensure_skill_source_fresh`],
///    non-fatal) and deploys the COMPLETE, unfiltered agent + skill roster from
///    the resolved source dirs ([`FrameworkPaths::agent_source_dir`] /
///    `skill_source_dir`) into `<config_dir>/agents` and `<config_dir>/skills`,
///    guaranteeing the full spawnable set regardless of install/submodule state.
///
/// Test: `ensure_managed_config_dir_deploys_full_roster`,
/// `ensure_managed_config_dir_is_idempotent`.
pub fn ensure_managed_config_dir(config_dir: &Path) -> anyhow::Result<()> {
    ensure_managed_config_dir_with_root(&FrameworkPaths::default(), config_dir)
}

/// Hermetic core of [`ensure_managed_config_dir`], taking an explicit framework
/// layout so tests can point the source dirs at a temp tree.
///
/// Why: [`FrameworkPaths::default`] reads `dirs::home_dir()`, which unit tests
/// must not depend on; injecting the layout keeps the roster-deploy assertions
/// hermetic and parallel-safe.
/// What: performs the two-phase provisioning described on
/// [`ensure_managed_config_dir`], using `fw` for both the `ensure_global_config_dir`
/// managed-root argument (`fw.root`) and the full-roster source dirs.
/// Test: `ensure_managed_config_dir_deploys_full_roster`,
/// `ensure_managed_config_dir_is_idempotent`.
pub fn ensure_managed_config_dir_with_root(
    fw: &FrameworkPaths,
    config_dir: &Path,
) -> anyhow::Result<()> {
    // Phase 1: canonical scaffolding shared with the standalone driver.
    ensure_global_config_dir(&fw.root, config_dir)?;

    // Phase 2: guarantee the COMPLETE roster from the resolved (submodule-aware)
    // framework source, matching `session_launch::prepare_session`. Refreshing
    // the bundled skill source first removes the dependency on a prior manual
    // `tm install` (#1917); non-fatal so a refresh failure falls back to
    // whatever is already on disk rather than blocking the spawn.
    if let Err(err) = crate::core::skill_source::ensure_skill_source_fresh(fw) {
        tracing::warn!("managed config dir: skill source refresh failed (non-fatal): {err}");
    }

    let agents_dest = config_dir.join("agents");
    deploy_agents(&fw.agent_source_dir(), &agents_dest).map_err(|e| {
        anyhow::anyhow!("failed to deploy full agent roster into managed config dir: {e}")
    })?;

    // PR #2818 review (round 3, MEDIUM decision): route through the multi-tier
    // orchestrator so a user-custom skill (`fw.user_skill_source_dir()`)
    // reaches the tm-global roster too, matching the standalone driver's
    // `global_config::deploy_agents_and_skills` (same decision, same
    // reasoning — see that function's doc comment for why the project-custom
    // tier is naturally N/A at this destination).
    let skills_dest = config_dir.join("skills");
    deploy_all_skill_tiers(
        &fw.skill_source_dir(),
        &fw.user_skill_source_dir(),
        &skills_dest,
        |_| true,
    )
    .map_err(|e| anyhow::anyhow!("failed to deploy full skill set into managed config dir: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Build a `FrameworkPaths` whose framework SOURCE dirs are populated from a
    /// small but representative slice of the real bundled roster, so the deploy
    /// assertions do not depend on a prior `tm install` or the git submodule.
    fn seed_framework(base: &Path) -> FrameworkPaths {
        let fw = FrameworkPaths::under(base);
        std::fs::create_dir_all(&fw.agents).unwrap();
        std::fs::create_dir_all(&fw.skills).unwrap();
        // A minimal set spanning the roster's shape: a base to inherit from, a
        // core specialist, and a couple of the agents #1996 called out.
        let agents: &[(&str, &str)] = &[
            (
                "BASE-AGENT.md",
                "---\nname: BASE-AGENT\ndescription: base\n---\n\nBase.\n",
            ),
            (
                "engineer.md",
                "---\nname: engineer\ndescription: implementation specialist\n---\n\nEngineer.\n",
            ),
            (
                "rust-engineer.md",
                "---\nname: rust-engineer\ndescription: rust specialist\n---\n\nRust.\n",
            ),
            (
                "research.md",
                "---\nname: research\ndescription: research specialist\n---\n\nResearch.\n",
            ),
            (
                "qa.md",
                "---\nname: qa\ndescription: quality specialist\n---\n\nQA.\n",
            ),
            (
                "version-control.md",
                "---\nname: version-control\ndescription: git specialist\n---\n\nVCS.\n",
            ),
            (
                "ticketing.md",
                "---\nname: ticketing\ndescription: ticketing specialist\n---\n\nTickets.\n",
            ),
        ];
        for (name, body) in agents {
            std::fs::write(fw.agents.join(name), body).unwrap();
        }
        std::fs::write(
            fw.skills.join("tm-doctor.md"),
            "---\nname: tm-doctor\ndescription: doctor\n---\n\nDoctor.\n",
        )
        .unwrap();
        fw
    }

    #[test]
    fn ensure_managed_config_dir_deploys_full_roster() {
        let tmp = TempDir::new().unwrap();
        let fw = seed_framework(tmp.path());
        let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");

        ensure_managed_config_dir_with_root(&fw, &config_dir).unwrap();

        // Scaffolding landed.
        assert!(config_dir.join("settings.json").exists());
        assert!(config_dir.join(".mcp.json").exists());

        // Every seeded specialist must be present AND spawnable (has a
        // `description` in its deployed frontmatter).
        let agents_dir = config_dir.join("agents");
        for name in [
            "engineer",
            "rust-engineer",
            "research",
            "qa",
            "version-control",
            "ticketing",
        ] {
            let path = agents_dir.join(format!("{name}.md"));
            assert!(path.exists(), "agent {name} must be deployed to config dir");
            let body = std::fs::read_to_string(&path).unwrap();
            assert!(
                body.contains("description:"),
                "deployed agent {name} must carry a description (spawnable)"
            );
        }

        // Skills landed too.
        assert!(
            config_dir.join("skills/tm-doctor/SKILL.md").exists(),
            "tm-* skills must be deployed to config dir"
        );
    }

    #[test]
    fn ensure_managed_config_dir_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let fw = seed_framework(tmp.path());
        let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");

        ensure_managed_config_dir_with_root(&fw, &config_dir).unwrap();
        let first = std::fs::read_to_string(config_dir.join("agents/engineer.md")).unwrap();

        // A second call must not fail and must leave the deployed roster intact.
        ensure_managed_config_dir_with_root(&fw, &config_dir).unwrap();
        let second = std::fs::read_to_string(config_dir.join("agents/engineer.md")).unwrap();

        assert_eq!(first, second, "re-provisioning must be idempotent");
    }

    #[test]
    fn ensure_managed_config_dir_deploys_user_tier_skill() {
        // PR #2818 review (round 3, MEDIUM decision): a user-custom skill
        // (`fw.user_skill_source_dir()`, i.e. `<root>/skills`) must reach the
        // tm-global roster, not just per-project deploys.
        let tmp = TempDir::new().unwrap();
        let fw = seed_framework(tmp.path());
        std::fs::create_dir_all(&fw.user_skills).unwrap();
        std::fs::write(
            fw.user_skills.join("my-custom-skill.md"),
            "---\nname: my-custom-skill\n---\n\nUSER CUSTOM.\n",
        )
        .unwrap();

        let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");
        ensure_managed_config_dir_with_root(&fw, &config_dir).unwrap();

        assert!(
            config_dir.join("skills/my-custom-skill/SKILL.md").exists(),
            "user-custom skill must be deployed to the tm-global config dir"
        );
    }

    /// Static verification against the REAL bundled roster (`src/assets/agents`,
    /// `src/assets/skills`). Ignored by default because it composes and writes the
    /// full ~40-agent set; run explicitly to prove the complete specialist roster
    /// deploys and that every deployed agent is spawnable (carries a `description`):
    ///
    /// `cargo test -p trusty-mpm --lib manual_static_verify_full_roster -- --ignored --nocapture`
    #[test]
    #[ignore = "static verification — composes the full real roster; run explicitly"]
    fn manual_static_verify_full_roster() {
        let tmp = TempDir::new().unwrap();
        let fw = FrameworkPaths::under(tmp.path());
        std::fs::create_dir_all(&fw.agents).unwrap();
        std::fs::create_dir_all(&fw.skills).unwrap();

        // Copy the REAL bundled raw sources into the framework source dirs.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for (src, dst) in [
            (manifest.join("src/assets/agents"), &fw.agents),
            (manifest.join("src/assets/skills"), &fw.skills),
        ] {
            for entry in std::fs::read_dir(&src).unwrap() {
                let entry = entry.unwrap();
                if entry.path().extension().and_then(|e| e.to_str()) == Some("md") {
                    std::fs::copy(entry.path(), dst.join(entry.file_name())).unwrap();
                }
            }
        }

        let config_dir = tmp.path().join(".trusty-tools/trusty-mpm/claude-config");
        ensure_managed_config_dir_with_root(&fw, &config_dir).unwrap();

        // List every deployed agent and confirm each is spawnable.
        let agents_dir = config_dir.join("agents");
        let mut deployed: Vec<String> = std::fs::read_dir(&agents_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        deployed.sort();

        println!("\n=== deployed agents ({}) ===", deployed.len());
        let mut missing_desc = Vec::new();
        for name in &deployed {
            let body = std::fs::read_to_string(agents_dir.join(name)).unwrap();
            let has_desc = body.contains("description:");
            println!("  {name}  spawnable={has_desc}");
            // BASE-*.md are inheritance templates (parents of `extends:` agents),
            // intentionally without a `description` and never independently
            // spawnable — exclude them from the spawnable requirement.
            if !has_desc && !name.starts_with("BASE-") {
                missing_desc.push(name.clone());
            }
        }

        // The specialists #1996 and the task call out must all be present.
        for required in [
            "engineer.md",
            "rust-engineer.md",
            "research.md",
            "qa.md",
            "local-ops.md",
            "version-control.md",
            "ticketing.md",
            "documentation.md",
            "security.md",
        ] {
            assert!(
                deployed.iter().any(|d| d == required),
                "required agent {required} missing from deployed roster: {deployed:?}"
            );
        }
        assert!(
            missing_desc.is_empty(),
            "these deployed agents are not spawnable (no description): {missing_desc:?}"
        );
    }
}
