//! Renders `references/doctor.md` from a maintained literal list of `tm
//! doctor` checks (source #5 of the issue #2913 design-research brief).
//!
//! Why: unlike the CLI tree, MCP catalog, and agent/skill rosters, doctor
//! checks have no single data table — each is a `DoctorCheck::new("literal",
//! ...)` call scattered across `run_doctor` and five sibling files
//! (`doctor_fs_checks.rs`, `doctor_deploy_validate.rs`,
//! `doctor_staleness.rs`, `doctor_output_style.rs`,
//! `doctor_agent_skills.rs`). The design-research brief's recommendation for
//! this "semi-extractable" surface is a maintained literal list cross-checked
//! by a count/name assertion test against `run_doctor`'s actual output,
//! rather than fragile literal-string grepping across six files.
//! What: [`DOCTOR_CHECKS`] — name + one-line description, in `run_doctor`'s
//! execution order — plus [`render`]. The drift guard lives in this
//! module's test suite: `doctor_checks_match_run_doctor_names` calls the real
//! `run_doctor` and asserts the name list is identical, so an added/removed/
//! renamed check fails the test suite (not just a docs staleness bug).
//! Test: `doctor_checks_match_run_doctor_names`,
//! `doctor_render_contains_known_check`.

use std::fmt::Write as _;

/// Every `tm doctor` check, in `run_doctor`'s execution order.
///
/// Why: `run_doctor`'s own doc comment (`crates/trusty-mpm/src/daemon/
/// doctor.rs`) is the closest thing to a spec for this list; this constant
/// mirrors it so the generator has a single, reviewable source instead of
/// re-deriving prose per release.
/// What: `(name, what it probes)` pairs. Kept in sync by
/// `doctor_checks_match_run_doctor_names`, which fails the test suite the
/// moment `run_doctor`'s actual check set diverges from this list.
/// Test: `doctor_checks_match_run_doctor_names`.
pub(crate) const DOCTOR_CHECKS: &[(&str, &str)] = &[
    (
        "instructions",
        "Framework instructions deployed and non-empty for the target project.",
    ),
    (
        "agents",
        "Bundled agent roster deployed under the operator/workspace `.claude/agents/` tier.",
    ),
    (
        "agent_reachability",
        "Fails when bundled agents deploy into a settings tier a managed session's `--setting-sources` flag never loads — presence-only checks stay green while every delegation degrades to `general-purpose` (issue #4451).",
    ),
    (
        "asset_tier",
        "Fails when tm-owned agent files sit in a project's `.claude/agents/` — that tier outranks the canonical `$CLAUDE_CONFIG_DIR/agents/` deploy, so a stale or stub copy shadows the real agent while every presence-only check stays green (issue #4442). Warns for leftovers in `~/.claude/agents/`, which a managed session no longer reads. Read-only; never deletes.",
    ),
    (
        "transcript_saving",
        "Fails when a managed spawn would leave Claude Code transcript saving disabled — an inherited `CLAUDE_CODE_CHILD_SESSION` marker costs the session all native `--resume`/`--continue`/`/rewind` recovery, and also fails if the scrub would wrongly take `CLAUDE_CONFIG_DIR` (issue #4467).",
    ),
    (
        "skills",
        "Bundled skill catalog deployed under the operator/workspace `.claude/skills/` tier.",
    ),
    (
        "skill_source",
        "The framework's own skill source directory is present and readable.",
    ),
    (
        "output_style",
        "The `trusty-mpm` Claude Code output style is configured and its file exists (DOC-28 F4).",
    ),
    (
        "output_style_staleness",
        "Deployed output-style file content matches the bundled catalog, and no orphaned files linger under `output-styles/` (issue #2333).",
    ),
    (
        "output_style_legacy_ids",
        "Warns when a legacy/unresolvable `outputStyle` id lingers in a currently-shadowed settings layer (e.g. `settings.local.json`) even though the effective layer resolves fine (issue #3453).",
    ),
    (
        "deployment",
        "Full manifest-completeness diff of the deployed payload against the canonical bundled roster (issue #2158).",
    ),
    (
        "skill_staleness",
        "Deployed skill content matches the RUNNING BINARY's own embedded bundled asset, at every deploy tier (`$CLAUDE_CONFIG_DIR/skills`, `~/.claude/skills`, the project's `.claude/skills`). Reads the deployed FILE, not the deploy manifest, and compares against the compiled-in asset rather than the `~/.trusty-mpm/framework/skills` extraction cache — that cache can itself lag the installed binary, which made every skill it covered report clean regardless of what shipped (issue #4604). Distinguishes drift a redeploy repairs from drift that is FROZEN (hand-edited, so `tm install` deliberately skips it), and reports UNKNOWN — never `Ok` — for anything it cannot verify. Read-only; `tm doctor --fix-skills` is the repair (issues #2876, #4604).",
    ),
    (
        "skill_unmanaged",
        "Reports UNKNOWN when a bundled skill is deployed to a tier whose `.trusty-mpm-skills-manifest.json` does not track it — the tier planner classifies it project-custom and drops it from every deploy, so no `tm` command can refresh it and `skill_staleness` (which compares against that same manifest) cannot see it at all. Never `Ok` for such a skill: content alone cannot distinguish an orphaned tm deployment from a deliberate customization. Scans `$CLAUDE_CONFIG_DIR/skills`, `~/.claude/skills`, and the project's `.claude/skills`. Read-only; `tm install --reconcile-skills` is the repair (issue #4605).",
    ),
    (
        "legacy_sources",
        "No legacy global instruction sources linger from a pre-migration install (issue #2876).",
    ),
    (
        "legacy_overrides",
        "The project carries none of the five RETIRED `.trusty-mpm/` instruction override files. They are no longer read, so a leftover one means the project's instructions are not reaching the PM — migrate the content to `CLAUDE.md` named sections (issue #4286).",
    ),
    (
        "agent_skills",
        "Every agent's declared `skills:` frontmatter resolves to a real skill — dangling references fail (DOC-42, issue #2889).",
    ),
    (
        "agent_skills_prose_hints",
        "Informational: skill names mentioned in agent prose but not declared in `skills:` frontmatter (always `Ok`, issue #2906).",
    ),
    (
        "memory",
        "trusty-memory sidecar reachability probe (bounded by `PROBE_TIMEOUT`).",
    ),
    (
        "search",
        "trusty-search sidecar reachability + expected-index-present probe (bounded by `PROBE_TIMEOUT`).",
    ),
    (
        "search_index_pin",
        "Resolves the index id the project's `.mcp.json` actually PINS (`trusty-search serve --index <id>`) against `GET /indexes/{id}/status`, and FAILS on a 404 — the pin names an index the daemon does not have, so every `search`/`grep` call in the session returns \"unknown index\". Index registration is fail-open at every step, so the pin advances even when creation failed and the `search` check above still reports healthy; 4 of 75 live worktrees had an index when this was measured. Read-only; never creates or reindexes (issue #5045).",
    ),
    (
        "worktrees",
        "No orphaned git worktrees under the managed workspace root (Fix 1b, #1840).",
    ),
    (
        "worktree_disk",
        "Bytes held by every git-registered worktree, and how much sits on already-merged \
         pull requests with no unsaved work (issue #2919).",
    ),
    (
        "base_clone",
        "The base clone each live worktree resolves through still has its git identity — \
         fails naming the base path and how many worktrees hang off it (issue #3605).",
    ),
    (
        "gh_account",
        "Active `gh` CLI identity is unambiguous — warns on multi-account ambiguity.",
    ),
    (
        "oauth_token",
        "Warns when a managed session risks the `CLAUDE_CONFIG_DIR`-keyed Keychain login loop (issue #2246).",
    ),
    (
        "hooks_contamination",
        "Warns when a project's `.claude/settings*.json` still carries tm hook entries from a pre-fix `tm install` — suggests `tm hooks clean` (issue #2940).",
    ),
    (
        "hooks_foreign_conflict",
        "Informational: warns when a project's `.claude/settings*.json` carries foreign (claude-mpm) hook entries that would fire inside a tm session — never auto-removed (issue #2940).",
    ),
    (
        "tcc_taint",
        "macOS: whether managed panes spawn `claude` with TCC responsibility disclaimed so its data-access prompts aren't attributed to the shared tmux server (issue #2997).",
    ),
    (
        "scaffold_tracking",
        "Warns when a harness-scaffolding path (`.claude/agents/`, `.claude/skills/`, `.claude/output-styles/`) is BOTH tracked in git AND regenerated locally by tm — the precondition for a `git merge --ff-only` \"would be overwritten\" collision; reports the exact true-intersection paths, never auto-modifies the index (issue #3427).",
    ),
    (
        "push_guard",
        "Warns when the project's clone carries no trusty-mpm cross-branch `pre-push` guard, or an older revision of it — the guard installs itself only on the clone path, so a base provisioned before it shipped is silently unprotected and a worktree tracking a foreign branch can force-push over that branch's reviewed lineage. Names the `tm repair push-guard` retrofit; doctor never writes into a repository (issue #2867).",
    ),
    (
        "binary_provenance",
        "Where the RUNNING binary came from and whether that source still exists: reads cargo's own `$CARGO_HOME/.crates2.json` install ledger and compares it against the running executable. Fails when the same binary is provided by more than one install, when the running binary is OLDER than the ledger's record for that same file or the two cannot be ordered as semver, or when a `cargo install --path` source directory has been reaped (no provenance, no upgrade path). Warns for a live path/git install, which is invisible to registry update detection. Reports UNKNOWN — never `Ok` — when the ledger is unreadable, does not cover the binary (a prebuilt-installer or package-manager install), or records a version OLDER than what is running, which means the ledger no longer describes the file on disk (issue #4964). Read-only; never installs, moves, or deletes (issue #4033, ADR-0021).",
    ),
    (
        "session_store",
        "Whether `~/.trusty-mpm/session-manager/sessions.json` still LOADS — validated against the same type the daemon deserializes, so this check can never call a store healthy that the daemon rejects. A store the daemon cannot read blocks every write (each write path reloads before it saves) while `tm ls` keeps serving the daemon's in-memory copy, so the condition is otherwise invisible until someone attempts a mutation. Fails with the byte offset where the valid document ends, and names `tm repair session-store` only when truncating there would actually leave a loadable store. An absent store is `Ok` (a machine that has never run a managed session has none); an unreadable one is UNKNOWN, never `Ok`. Read-only — it never truncates or writes (issue #5007).",
    ),
    (
        "stray_mcp_json",
        "Warns when a `.mcp.json` sits ABOVE the workspace or in a temp root. Claude Code discovers `.mcp.json` by walking UP from a session's cwd, so such a file silently supplies the MCP servers of every session started beneath it — agent scratchpads under `/tmp` included — with nothing in the project to point at. The scan is bounded: the workspace's strict ancestors up to the home directory (never the filesystem root, never a recursive descent) plus `$TMPDIR` and `/tmp`. Each finding names the servers it declares and what tm can PROVE about who wrote it, read from the `mcp-json-provenance.json` ledger rather than guessed from content — a file full of `trusty-*` servers may equally be one the operator wrote. Read-only. `tm doctor --fix` quarantines only ledger-proven tm writes (renaming them aside, never deleting); everything else is refused and needs `tm doctor --quarantine-mcp <path>`.",
    ),
    (
        "tmux_options",
        "Whether the live tmux SERVER's globals still match tm's spec — `history-limit`, `mouse`, and the window-scoped `alternate-screen`. `create_managed_session` applies and verifies them before every pane tm creates, but a server tm did not start carries none of them: a tmux-continuum restore recreates `tm-*` sessions through tmux-resurrect's own bare `new-session`, so restored panes bake tmux's factory 2000-line scrollback and can enter the alternate screen (issue #6469). Warns naming each drifted option; UNKNOWN — never `Ok` — when no option could be read (no tmux binary, or no server running). A green row means NEW panes will be correct: `history-limit` is captured into a pane's ring buffer at creation and cannot be grown in place, so an affected session has to be restarted. Read-only — it reads options, never sets one.",
    ),
    (
        "log_drain",
        "Whether the cloud log drain is configured, where it points, and how its last pass ended (issue #6535). The drain uploads the daemon's own log files to an object store on an interval; it is OFF unless `log_drain.enabled` is set in `~/.trusty-tools/trusty-mpm/config.yaml`, and a host that never configured one reports `Ok`. A `log_drain:` section that does not resolve — a malformed destination URI, a zero interval, a source with no root — reports `Fail`, because the daemon refuses to start the scheduler and no bytes move. Enabled but never observed running reports `Warn`. The last pass's verdict is read from `~/.trusty-mpm/log-drain/status.json`, and a pass that errored — including one that finished with per-file failures — reports `Fail`, never a drained-looking `Ok`. Read-only: this probe never drains and never connects to the destination.",
    ),
];

/// Render the full doctor-check reference.
///
/// Why: an operator triaging a `tm doctor` `Warn`/`Fail` needs to know what
/// each check name actually probes without reading five source files.
/// What: one table row per [`DOCTOR_CHECKS`] entry, in `run_doctor`'s
/// execution order (not re-sorted — order communicates the probe sequence).
/// Test: `doctor_render_contains_known_check`.
pub(crate) fn render() -> String {
    let mut out = String::new();
    out.push_str("# Doctor Check Reference\n\n");
    out.push_str(
        "Generated from a maintained literal list cross-checked against \
         `run_doctor`'s actual check names (see this module's \
         `doctor_checks_match_run_doctor_names` test — an added, removed, or \
         renamed check fails the test suite). Source: \
         `crates/trusty-mpm/src/daemon/doctor.rs` and its five sibling \
         `doctor_*.rs` files. Regenerate with `tm generate capabilities`.\n\n",
    );
    let _ = writeln!(out, "{} checks, in execution order.\n", DOCTOR_CHECKS.len());

    out.push_str("| # | Check | What it probes |\n|---|---|---|\n");
    for (i, (name, description)) in DOCTOR_CHECKS.iter().enumerate() {
        let _ = writeln!(out, "| {} | `{name}` | {description} |", i + 1);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::daemon::doctor::run_doctor;

    #[test]
    fn doctor_render_contains_known_check() {
        let rendered = render();
        assert!(rendered.contains("`instructions`"), "{rendered}");
        assert!(rendered.contains("`oauth_token`"), "{rendered}");
    }

    #[test]
    fn doctor_render_is_deterministic() {
        assert_eq!(render(), render());
    }

    /// The drift guard: `DOCTOR_CHECKS`'s name list must exactly match what
    /// `run_doctor` actually produces, in the same order. This is the
    /// "unit test asserting the generator's doctor list length == the
    /// doctor's actual check count" the issue #2913 brief requires — and
    /// goes further by also asserting name equality, not just length.
    #[tokio::test]
    async fn doctor_checks_match_run_doctor_names() {
        let report = run_doctor(None, None, &[], None).await;
        let actual: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
        let expected: Vec<&str> = DOCTOR_CHECKS.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            actual, expected,
            "generate::doctor::DOCTOR_CHECKS has drifted from run_doctor's actual checks"
        );
    }
}
