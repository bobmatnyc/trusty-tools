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
//! WHICH LAYER ACTUALLY LOADS THE ROSTER (re-corrected 2026-07-31, issue
//! #4451 — this note supersedes the 2026-07-30 #4409 correction it replaces):
//! the AGENTS deployed here are read by a daemon-managed session ONLY because
//! that session's `--setting-sources` list now names the `user` tier.
//!
//! The 07-30 correction concluded from a probe matrix that
//! `$CLAUDE_CONFIG_DIR/agents/` "is discovered", and on that basis #4437 moved
//! the roster here while the daemon spawn still carried `--setting-sources
//! project,local`. That probe matrix ran a fresh headless `claude -p` with NO
//! `--setting-sources` flag, so it measured Claude Code's DEFAULT tier set (all
//! tiers, `user` included) — not the managed spawn's. Re-run against `claude`
//! 2.1.220 from a cwd with no `.claude/`, with the same `CLAUDE_CONFIG_DIR`:
//!
//!   no flag                          -> all 42 bundled agents resolve
//!   --setting-sources project,local  -> 5 built-ins only, zero bundled
//!   --setting-sources user,project,local -> all 42 bundled agents resolve
//!
//! So the ORIGINAL reading — `project,local` excludes the `user` tier
//! `CLAUDE_CONFIG_DIR` relocates — was right about the mechanism; only its
//! conclusion (that the project layer must therefore deliver the roster) is
//! obsolete. The fix keeps the roster here and teaches the spawn to load the
//! tier: see `core::model_inject::SETTING_SOURCES_FLAG_RELOCATED`.
//!
//! `~/.claude/agents/` still goes unread, which is documented behavior — with
//! `CLAUDE_CONFIG_DIR` set, every `~/.claude` path lives under that directory
//! instead. That is also why re-admitting `user` does not re-admit the
//! operator's global hooks (#1269). So this config dir is the ONE tier bundled
//! agents deploy into, on both the daemon and the flag-less standalone `tm run`
//! path, which is why provisioning the complete set here remains mandatory. It
//! also still carries AUTH + TRUST isolation (keychain / `.credentials.json` /
//! `.claude.json` keyed to this path).
//!
//! SKILLS ride the same tier (corrected #4873). The older note here said they
//! "continue to deploy per-workspace via `prepare_session`" and that this
//! module's skill deploy was belt-and-suspenders. That predates
//! `SETTING_SOURCES_FLAG_RELOCATED`: the managed spawn resolves `config_dir`
//! to `Some`, so it launches with `--setting-sources user,project,local` and
//! the `user` tier this directory relocates is read for skills exactly as it
//! is for agents. The deploy below is load-bearing, not a backup — measured
//! 2026-08-05, 49 of 52 skills under `$CLAUDE_CONFIG_DIR/skills/` were
//! byte-identical to the running binary's bundled source, and all three
//! exceptions were checksum-frozen hand edits the deployer is correct to
//! decline.
//!
//! And this tier WINS for skills (#4958). Claude Code resolves skills
//! `enterprise > personal > project > bundled`, and `CLAUDE_CONFIG_DIR`
//! relocates the personal tier — so `$CLAUDE_CONFIG_DIR/skills` outranks the
//! workspace's `<project_dir>/.claude/skills`. AGENTS invert that (project
//! beats user), which is why the same directory needs `--reset-agents` to
//! clear a project-tier shadow but no equivalent for skills. Do not carry
//! either order across to the other artifact type.
//!
//! What: [`ensure_managed_config_dir`] (1) runs the canonical standalone
//! scaffolding ([`ensure_global_config_dir`] — settings.json + the MPM hook
//! triad + `.mcp.json` + output-styles, plus a best-effort deploy from the
//! `framework/*` dirs), then (2) refreshes the framework source and deploys the
//! FULL agent/skill roster from the resolved source dirs so the complete,
//! spawnable set is guaranteed present. Idempotent — safe to call on every
//! spawn (the deployers checksum-skip unchanged files). Since #4840 the AGENT
//! half of step (2) also REFRESHES the bundled agent source from the
//! compiled-in bundle first, via
//! [`crate::core::agent_source::autodeploy_agents_for`] — before that, only the
//! manual `tm install` ever wrote `~/.trusty-mpm/framework/agents/`, so a
//! merged, compiled-in `BASE-AGENT.md` change silently reached no running
//! agent. Since #4873 the SKILL half warns (once, bounded) about every file it
//! declined to refresh — the deploy itself already ran on all three run paths;
//! only the evidence of it was missing. Since #4880 step (2) also refreshes the
//! session workspace's PROJECT skill tier (`<project_dir>/.claude/skills`),
//! which previously went stale on resume and in-place relaunch; that half is
//! stamp-gated on the project manifest, so
//! it writes nothing when nothing changed. See
//! [`crate::core::project_skill_tier`].
//! Test: `ensure_managed_config_dir_deploys_full_roster`,
//! `ensure_managed_config_dir_is_idempotent`,
//! `ensure_managed_config_dir_refreshes_stale_bundled_agents`,
//! `ensure_managed_config_dir_survives_an_unwritable_agent_target`,
//! `crates/trusty-mpm/src/core/managed_config_tests.rs`.

use std::path::Path;

use crate::core::paths::FrameworkPaths;
use crate::core::skill_tiers::deploy_all_skill_tiers;
use crate::core::standalone::global_config::ensure_global_config_dir;
use crate::core::standalone::trust_seed::preseed_managed_trust;

/// Provision (idempotently) the tm-owned managed `CLAUDE_CONFIG_DIR`.
///
/// Why: called from the daemon managed-spawn path immediately before the
/// launched `claude` is pointed at `config_dir` via `CLAUDE_CONFIG_DIR`, giving
/// the session an isolated, fully-scaffolded config home (auth, trust, MCP
/// stubs, output-styles) plus the full framework roster/skills. The AGENT
/// roster deployed here is load-bearing on BOTH the daemon and the standalone
/// path (issue #4409 — see the module doc's corrected discovery note), and so
/// is the SKILL deploy since the spawn began passing
/// `--setting-sources user,project,local` (#4873 — the module doc's SKILLS
/// paragraph retires the older "belt-and-suspenders" reading). Runs on every
/// spawn, resume, and in-place relaunch because it is cheap and idempotent,
/// matching the standalone `tm run` contract.
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
///    The AGENT half routes through
///    [`crate::core::agent_source::autodeploy_agents_for`] (#4840), which
///    re-materializes the bundled agent source from the compiled-in bundle when
///    its stamp differs, then deploys it — and which never returns an error, so
///    a failed deploy degrades to a warning instead of blocking the spawn.
///
/// Test: `ensure_managed_config_dir_deploys_full_roster`,
/// `ensure_managed_config_dir_is_idempotent`,
/// `ensure_managed_config_dir_refreshes_stale_bundled_agents`.
pub fn ensure_managed_config_dir(config_dir: &Path, project_dir: &Path) -> anyhow::Result<()> {
    ensure_managed_config_dir_with_root(&FrameworkPaths::default(), config_dir, project_dir)
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
///
/// #4873 — WHY THE SKILL HALF ALSO RUNS ON RESUME AND IN-PLACE RELAUNCH.
/// This function is the choke point all THREE run paths share, because each
/// reaches it through `runtime::claude_code::prepare_managed_config`:
/// `ClaudeCodeAdapter::spawn` (fresh), `ClaudeCodeAdapter::spawn_resume`
/// (`daemon::managed_routes::lifecycle::resume_managed`), and
/// `runtime::claude_code::build_inplace_resume_command` (bare `tm` in a
/// managed pane). So the skill deploy below already satisfies the "deploy on
/// every run, not just `tm install`" ruling, exactly as the agent half does —
/// what was missing was any SIGN of it: a skill the deployer declines is
/// silent, which is why three checksum-frozen skills read as "deployment never
/// runs". [`skill_skip_summary`] supplies that sign.
///
/// Test: `ensure_managed_config_dir_deploys_full_roster`,
/// `ensure_managed_config_dir_is_idempotent`,
/// `ensure_managed_config_dir_refreshes_stale_bundled_agents`,
/// `ensure_managed_config_dir_survives_an_unwritable_agent_target`,
/// `ensure_managed_config_dir_refreshes_a_stale_managed_skill`,
/// `ensure_managed_config_dir_skill_deploy_is_a_noop_when_unchanged`,
/// `ensure_managed_config_dir_preserves_a_project_custom_skill`,
/// `ensure_managed_config_dir_skips_a_frozen_skill`,
/// `ensure_managed_config_dir_emits_the_frozen_skill_warning`,
/// `ensure_managed_config_dir_deploys_the_project_skill_tier`,
/// `ensure_managed_config_dir_project_tier_is_a_noop_when_unchanged`.
///
/// #4880 — WHY THE PROJECT TIER IS DEPLOYED FROM HERE TOO. The user tier this
/// function refreshes every run is OUTRANKED by `<project_dir>/.claude/skills`,
/// which until now was written only by `session_launch::prepare_session` and
/// `tm sessions sync-assets` — neither of which runs on resume or in-place
/// relaunch. Hanging the project-tier trigger off this same choke point is what
/// makes it reach all three run paths; see
/// [`crate::core::project_skill_tier`] for the stamp that keeps it a no-op when
/// the project manifest has not moved.
pub fn ensure_managed_config_dir_with_root(
    fw: &FrameworkPaths,
    config_dir: &Path,
    project_dir: &Path,
) -> anyhow::Result<()> {
    // Phase 1: canonical scaffolding shared with the standalone driver.
    ensure_global_config_dir(&fw.root, config_dir)?;

    // #1939 / #4181: heal the claude-mpm palace split-brain — when the derived
    // `owner-repo` palace does not exist but the BARE repo-name one does,
    // register a palace-level alias so the `TRUSTY_MEMORY_PALACE` the spawn
    // exports resolves to the existing store. This ran inside
    // `session_launch::settings::inject_trusty_memory_mcp` until ADR-0042 deleted
    // that injector; this function is the choke point every spawn, resume and
    // in-place relaunch already reaches, so the healing keeps running. Best-effort
    // and side-effect-only — it never returns an error and never blocks a launch.
    crate::core::session_launch::maybe_register_palace_alias(project_dir, None);

    // Phase 2: guarantee the COMPLETE roster from the resolved (submodule-aware)
    // framework source, matching `session_launch::prepare_session`. Refreshing
    // the bundled skill source first removes the dependency on a prior manual
    // `tm install` (#1917); non-fatal so a refresh failure falls back to
    // whatever is already on disk rather than blocking the spawn.
    if let Err(err) = crate::core::skill_source::ensure_skill_source_fresh(fw) {
        tracing::warn!("managed config dir: skill source refresh failed (non-fatal): {err}");
    }

    // #4840: refresh the bundled agent SOURCE from the compiled-in bundle
    // before deploying it. Until this call existed, `~/.trusty-mpm/framework/
    // agents/` was written only by the manual `tm install`, so a compiled-in
    // `BASE-AGENT.md` change reached no running agent until someone remembered
    // to re-run it. `autodeploy_agents_for` never returns an error — a broken
    // deploy must not block a session — so anything it could not do comes back
    // as a warning line instead.
    let agents_dest = config_dir.join("agents");
    let agents = crate::core::agent_source::autodeploy_agents_for(fw, &agents_dest);
    if agents.refreshed {
        tracing::info!(
            "managed config dir: bundled agent source refreshed from the running binary"
        );
    }
    // #4840: a file the deployer declines to overwrite (untracked-and-differing,
    // or a user-owned entry that was edited), and an agent that failed to
    // compose at all, used to be handled SILENTLY — the other half of the
    // defect. `agents.warnings` is a bounded count-plus-preview summary (at
    // most a couple of lines), matching `agents::deployer`'s own #2504 policy:
    // this runs on every spawn AND every resume, and the stale set is never
    // reconciled until someone runs `--reset-agents`, so one line per file
    // would be permanent log spam. PR #4848 review (HIGH).
    for warning in &agents.warnings {
        tracing::warn!("managed config dir: {warning}");
    }

    // PR #2818 review (round 3, MEDIUM decision): route through the multi-tier
    // orchestrator so a user-custom skill (`fw.user_skill_source_dir()`)
    // reaches the tm-global roster too, matching the standalone driver's
    // `global_config::deploy_agents_and_skills` (same decision, same
    // reasoning — see that function's doc comment for why the project-custom
    // tier is naturally N/A at this destination).
    let skills_dest = config_dir.join("skills");
    let skills = deploy_all_skill_tiers(
        &fw.skill_source_dir(),
        &fw.user_skill_source_dir(),
        &skills_dest,
        |_| true,
    )
    .map_err(|e| anyhow::anyhow!("failed to deploy full skill set into managed config dir: {e}"))?;
    // #4873: a declined skill was SILENT, so a stale one looked like a deploy
    // that never ran — see this function's doc for why that is the whole defect.
    if let Some(line) = skill_skip_summary(&skills.stats.skipped) {
        tracing::warn!("managed config dir: {line}");
    }

    // #4880: refresh the PROJECT tier, which no other run path touches on a
    // resume or an in-place relaunch. #4958 corrects why: for SKILLS Claude Code
    // resolves personal > project, so `$CLAUDE_CONFIG_DIR/skills` (deployed just
    // above) OUTRANKS this destination — a stale copy here does not beat it. It
    // is still what loads for every name the managed roster does not carry.
    // Non-fatal, and a no-op whenever the project manifest stamp still matches.
    match crate::core::project_skill_tier::ensure_project_skill_tier(fw, project_dir) {
        Ok(project) => {
            // Deliberately NOT routed through `skill_skip_summary`: a skipped
            // project-tier skill is a local customization the model says must
            // survive, so pointing at a remedy would contradict it.
            if project.deployed && !project.stats.skipped.is_empty() {
                tracing::info!(
                    preserved = project.stats.skipped.len(),
                    "project skill tier: local copies preserved across the redeploy"
                );
            }
        }
        Err(err) => {
            tracing::warn!(
                project_dir = %project_dir.display(),
                "project skill tier deploy failed (non-fatal): {err}"
            );
        }
    }

    Ok(())
}

/// One bounded warning line for the skills a deploy declined to write, or
/// `None` when it declined nothing.
///
/// Why: this closes for SKILLS the half of #4840 that PR #4848 closed for
/// agents. `deploy_one_file` skips a checksum-frozen (hand-edited) or
/// unmanaged skill and reports it only in the returned `DeployStats`, which
/// every caller here discarded — so a skill frozen against the manifest stayed
/// stale on every run, forever, with no warning anywhere. That silence is what
/// #4873 was filed as: three frozen skills read as "skill deployment never
/// runs on resume", when deployment had in fact run and correctly declined
/// them. The skip itself is CORRECT and is deliberately left alone — a hand
/// edit must survive, and `tm doctor --fix-skills --include-frozen` is the
/// remedy the pointer names.
/// What: a count plus a five-entry preview and that remedy pointer. Matches
/// [`crate::core::agent_source::deploy_summary_lines`]'s policy (issue #2504:
/// count + preview, never one line per file) because this runs on EVERY spawn,
/// resume, and in-place relaunch and the frozen set is not reconciled until
/// someone runs the doctor — one line per file would be permanent log spam.
/// Pure — no I/O, no logging.
/// Test: `skill_skip_summary_is_none_on_a_clean_deploy`,
/// `skill_skip_summary_counts_and_previews`,
/// `skill_skip_summary_elides_beyond_five`,
/// `a_declined_skill_reaches_skill_skip_summary_from_a_real_deploy`,
/// `ensure_managed_config_dir_emits_the_frozen_skill_warning` (the call site).
fn skill_skip_summary(skipped: &[String]) -> Option<String> {
    if skipped.is_empty() {
        return None;
    }
    Some(format!(
        "warning: {} skill file(s) were NOT refreshed — each is user-owned \
         (hand-edited away from its recorded checksum, or never managed here), \
         so the bundled version was withheld: {}. Run \
         `tm doctor --fix-skills --include-frozen` to adopt them.",
        skipped.len(),
        crate::core::agent_source::preview(skipped, 5)
    ))
}

/// Resolve, provision, and trust-seed the tm-owned `CLAUDE_CONFIG_DIR` for an
/// INTERACTIVE launch (`tm launch`, `tm connect`).
///
/// Why (issue #4181, ADR-0042 decision 6): both commands spawned `claude` with
/// no `CLAUDE_CONFIG_DIR` at all and `--setting-sources project,local`. Under
/// ADR-0042 the MCP declaration lives once in the user tier of
/// `<CLAUDE_CONFIG_DIR>/.claude.json`, and a session that neither relocates nor
/// loads `user` reads no MCP servers whatsoever. Relocating is what lets the
/// `user` tier be loaded WITHOUT re-admitting the operator's global
/// `~/.claude/settings.json` hooks — #1269's isolation guarantee is then carried
/// by the relocation instead of by the exclusion, exactly as it already is on
/// the daemon path. The rejected alternative was adding `user` to
/// `--setting-sources` while still reading the operator's real `~/.claude`,
/// which drops that guarantee outright.
///
/// This is the interactive counterpart of
/// `runtime::claude_code::prepare_managed_config` and deliberately mirrors it
/// step for step rather than growing a second mechanism: resolve, provision
/// (non-fatal), seed trust into `<config_dir>/.claude.json` (NEVER
/// `~/.claude.json`).
///
/// // #4181 (ADR-0042): the four `_pinned` parameters are gone with the
/// `enabledMcpjsonServers` approval they gated. So is the derivation difference
/// this doc used to warn about — that this path applied no `project_scope_mcp_names`
/// subtraction (#2739) and so pre-approved a repo `[mcp.custom]` name that
/// collided with an operator registry name. Nothing is pre-approved now.
/// What: `Some(dir)` when
/// [`crate::core::trusty_tools_config::managed_claude_config_dir`] resolves —
/// having provisioned it via [`ensure_managed_config_dir`] and seeded trust via
/// [`preseed_managed_trust`], both non-fatal so a failure warns and the session
/// still launches under the relocated dir (strictly safer than silently falling
/// back to the operator's `~/.claude`). `None` when the home is unresolvable (a
/// stripped environment): falls back to
/// [`crate::core::home_trust_seed::preseed_home_trust`] and the caller keeps
/// `SETTING_SOURCES_FLAG`'s `user`-excluding posture.
/// Test: `interactive_config_dir_seeds_trust_in_the_managed_dir`,
/// `interactive_config_dir_never_writes_the_home_claude_json`,
/// `interactive_config_dir_survives_a_malformed_managed_claude_json`,
/// `interactive_config_dir_withholds_builtins_when_a_pin_failed`.
pub fn prepare_interactive_config_dir(workspace: &Path) -> Option<std::path::PathBuf> {
    let Some(config_dir) = crate::core::trusty_tools_config::managed_claude_config_dir() else {
        // #4181: home unresolved — nothing to relocate to. Keep the legacy
        // home-trust seed so the startup dialogs are still dismissed.
        if let Err(e) = crate::core::home_trust_seed::preseed_home_trust(workspace) {
            tracing::warn!(
                workspace = %workspace.display(),
                "home trust pre-seed failed (non-fatal): {e}"
            );
        }
        return None;
    };
    prepare_interactive_config_dir_in(&FrameworkPaths::default(), &config_dir, workspace);
    Some(config_dir)
}

/// Hermetic core of [`prepare_interactive_config_dir`]: provision `config_dir`
/// and seed `workspace`'s trust into it.
///
/// Why: split out so tests drive a tempdir framework root and config dir instead
/// of the operator's real `$HOME` — the same hermetic-core split
/// [`ensure_managed_config_dir_with_root`] already uses. Keeping the resolution
/// in the wrapper is also what makes the "never writes `~/.claude.json`"
/// invariant testable at all.
/// What: [`ensure_managed_config_dir_with_root`] then [`preseed_managed_trust`],
/// each non-fatal — a failure of either warns and returns normally, because a
/// partially-provisioned relocated config home is still safer for the session
/// than not relocating.
/// Test: see [`prepare_interactive_config_dir`].
pub fn prepare_interactive_config_dir_in(fw: &FrameworkPaths, config_dir: &Path, workspace: &Path) {
    // #4181: non-fatal — point the session at the relocated dir even when
    // provisioning was partial, mirroring `prepare_managed_config`.
    if let Err(e) = ensure_managed_config_dir_with_root(fw, config_dir, workspace) {
        tracing::warn!(
            config_dir = %config_dir.display(),
            "managed config dir provisioning failed (non-fatal): {e}"
        );
    }
    // #4181: seed into `<config_dir>/.claude.json` — the file a relocated
    // session actually reads. The pre-relocation `preseed_home_trust` wrote
    // `~/.claude.json`, which such a session never opens.
    if let Err(e) = preseed_managed_trust(config_dir, workspace) {
        tracing::warn!(
            config_dir = %config_dir.display(),
            "managed trust pre-seed failed (non-fatal): {e}"
        );
    }
}

#[cfg(test)]
#[path = "managed_config_tests.rs"]
mod tests;
