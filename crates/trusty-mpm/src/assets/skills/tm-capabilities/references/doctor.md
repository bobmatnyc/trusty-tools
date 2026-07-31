# Doctor Check Reference

Generated from a maintained literal list cross-checked against `run_doctor`'s actual check names (see this module's `doctor_checks_match_run_doctor_names` test — an added, removed, or renamed check fails the test suite). Source: `crates/trusty-mpm/src/daemon/doctor.rs` and its five sibling `doctor_*.rs` files. Regenerate with `tm generate capabilities`.

23 checks, in execution order.

| # | Check | What it probes |
|---|---|---|
| 1 | `instructions` | Framework instructions deployed and non-empty for the target project. |
| 2 | `agents` | Bundled agent roster deployed under the operator/workspace `.claude/agents/` tier. |
| 3 | `agent_reachability` | Fails when bundled agents deploy into a settings tier a managed session's `--setting-sources` flag never loads — presence-only checks stay green while every delegation degrades to `general-purpose` (issue #4451). |
| 4 | `skills` | Bundled skill catalog deployed under the operator/workspace `.claude/skills/` tier. |
| 5 | `skill_source` | The framework's own skill source directory is present and readable. |
| 6 | `output_style` | The `trusty-mpm` Claude Code output style is configured and its file exists (DOC-28 F4). |
| 7 | `output_style_staleness` | Deployed output-style file content matches the bundled catalog, and no orphaned files linger under `output-styles/` (issue #2333). |
| 8 | `output_style_legacy_ids` | Warns when a legacy/unresolvable `outputStyle` id lingers in a currently-shadowed settings layer (e.g. `settings.local.json`) even though the effective layer resolves fine (issue #3453). |
| 9 | `deployment` | Full manifest-completeness diff of the deployed payload against the canonical bundled roster (issue #2158). |
| 10 | `skill_staleness` | Deployed skill content matches the bundled/embedded source (issue #2876). |
| 11 | `legacy_sources` | No legacy global instruction sources linger from a pre-migration install (issue #2876). |
| 12 | `agent_skills` | Every agent's declared `skills:` frontmatter resolves to a real skill — dangling references fail (DOC-42, issue #2889). |
| 13 | `agent_skills_prose_hints` | Informational: skill names mentioned in agent prose but not declared in `skills:` frontmatter (always `Ok`, issue #2906). |
| 14 | `memory` | trusty-memory sidecar reachability probe (bounded by `PROBE_TIMEOUT`). |
| 15 | `search` | trusty-search sidecar reachability + expected-index-present probe (bounded by `PROBE_TIMEOUT`). |
| 16 | `worktrees` | No orphaned git worktrees under the managed workspace root (Fix 1b, #1840). |
| 17 | `gh_account` | Active `gh` CLI identity is unambiguous — warns on multi-account ambiguity. |
| 18 | `oauth_token` | Warns when a managed session risks the `CLAUDE_CONFIG_DIR`-keyed Keychain login loop (issue #2246). |
| 19 | `hooks_contamination` | Warns when a project's `.claude/settings*.json` still carries tm hook entries from a pre-fix `tm install` — suggests `tm hooks clean` (issue #2940). |
| 20 | `hooks_foreign_conflict` | Informational: warns when a project's `.claude/settings*.json` carries foreign (claude-mpm) hook entries that would fire inside a tm session — never auto-removed (issue #2940). |
| 21 | `tcc_taint` | macOS: whether managed panes spawn `claude` with TCC responsibility disclaimed so its data-access prompts aren't attributed to the shared tmux server (issue #2997). |
| 22 | `scaffold_tracking` | Warns when a harness-scaffolding path (`.claude/agents/`, `.claude/skills/`, `.claude/output-styles/`) is BOTH tracked in git AND regenerated locally by tm — the precondition for a `git merge --ff-only` "would be overwritten" collision; reports the exact true-intersection paths, never auto-modifies the index (issue #3427). |
| 23 | `push_guard` | Warns when the project's clone carries no trusty-mpm cross-branch `pre-push` guard, or an older revision of it — the guard installs itself only on the clone path, so a base provisioned before it shipped is silently unprotected and a worktree tracking a foreign branch can force-push over that branch's reviewed lineage. Names the `tm repair push-guard` retrofit; doctor never writes into a repository (issue #2867). |
