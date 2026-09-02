Fixed

- `version-control` agent — its "Release Workflow" section no longer instructs
  the agent to bump versions, cut release tags, or push tags; that belongs to
  `local-ops` via `Skill(skill="cargo-publish")`, and `version-control` now
  merges a finished release PR like any other. A non-release annotated tag on
  explicit PM instruction stays permitted. Added a "Deterministic Tools" table
  naming the exact commands (`check_changelog_fragment.sh`,
  `check-pr-version-bump.sh`, the live required-contexts read, the
  merge-queue-ownership query, the one-shot pre-merge status read, and
  `tm session prune-worktrees`) the agent runs itself before opening or
  merging a PR, and points the seven-field PR body contract at `tm-workflow`
  by name instead of restating it. Added a pre-push credential-scan reminder.
- `security` agent — the secret-detection protocol now runs
  `detect-secrets scan --baseline .secrets.baseline` before any ad hoc grep.
- `mpm-skills-manager` agent — Tech Stack Detection now defers to
  `framework-manifest.toml` / `tm-capabilities`'s `references/agents.md`
  instead of hand-listing `ls`/`cat` probes; both `mpm-skills-manager` and
  `mpm-agent-manager`'s Improvement Workflow sections now name `tm doctor` and
  `tm doctor --fix-skills --yes` as the on-demand tier-shadow check and repair.
- `rust-engineer` agent — Quality Bar now names
  `scripts/test_trusty_common_lanes.sh` for `trusty-common` edits, since its
  empty default feature set makes a bare `cargo test -p trusty-common` a
  compile error.
