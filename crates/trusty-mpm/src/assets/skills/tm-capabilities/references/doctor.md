# Doctor Check Reference

Generated from a maintained literal list cross-checked against `run_doctor`'s actual check names (see this module's `doctor_checks_match_run_doctor_names` test — an added, removed, or renamed check fails the test suite). Source: `crates/trusty-mpm/src/daemon/doctor.rs` and its five sibling `doctor_*.rs` files. Regenerate with `tm generate capabilities`.

18 checks, in execution order.

| # | Check | What it probes |
|---|---|---|
| 1 | `instructions` | Framework instructions deployed and non-empty for the target project. |
| 2 | `agents` | Bundled agent roster deployed under the operator/workspace `.claude/agents/` tier. |
| 3 | `skills` | Bundled skill catalog deployed under the operator/workspace `.claude/skills/` tier. |
| 4 | `skill_source` | The framework's own skill source directory is present and readable. |
| 5 | `output_style` | The `trusty-mpm` Claude Code output style is configured and its file exists (DOC-28 F4). |
| 6 | `output_style_staleness` | Deployed output-style file content matches the bundled catalog, and no orphaned files linger under `output-styles/` (issue #2333). |
| 7 | `deployment` | Full manifest-completeness diff of the deployed payload against the canonical bundled roster (issue #2158). |
| 8 | `skill_staleness` | Deployed skill content matches the bundled/embedded source (issue #2876). |
| 9 | `legacy_sources` | No legacy global instruction sources linger from a pre-migration install (issue #2876). |
| 10 | `agent_skills` | Every agent's declared `skills:` frontmatter resolves to a real skill — dangling references fail (DOC-42, issue #2889). |
| 11 | `agent_skills_prose_hints` | Informational: skill names mentioned in agent prose but not declared in `skills:` frontmatter (always `Ok`, issue #2906). |
| 12 | `memory` | trusty-memory sidecar reachability probe (bounded by `PROBE_TIMEOUT`). |
| 13 | `search` | trusty-search sidecar reachability + expected-index-present probe (bounded by `PROBE_TIMEOUT`). |
| 14 | `worktrees` | No orphaned git worktrees under the managed workspace root (Fix 1b, #1840). |
| 15 | `gh_account` | Active `gh` CLI identity is unambiguous — warns on multi-account ambiguity. |
| 16 | `oauth_token` | Warns when a managed session risks the `CLAUDE_CONFIG_DIR`-keyed Keychain login loop (issue #2246). |
| 17 | `hooks_contamination` | Warns when a project's `.claude/settings*.json` still carries tm hook entries from a pre-fix `tm install` — suggests `tm hooks clean` (issue #2940). |
| 18 | `hooks_foreign_conflict` | Informational: warns when a project's `.claude/settings*.json` carries foreign (claude-mpm) hook entries that would fire inside a tm session — never auto-removed (issue #2940). |
