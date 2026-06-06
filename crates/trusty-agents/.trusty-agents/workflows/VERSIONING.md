# Versioning Policy

trusty-agents follows semantic versioning with a **patch-per-feature** discipline.

## Rules

| Change type | Version bump | Example |
|---|---|---|
| Bug fix | patch (0.1.X) | Fix ASGITransport prohibition |
| Refactor / knowledge migration | patch (0.1.X) | Move rules from agent prompt to skill file |
| New feature (self-contained) | patch (0.1.X) | New skill file, new agent template |
| New capability (user-visible) | minor (0.X.0) | AgentRegistry, SkillRegistry, new subcommand |
| Breaking change | major (X.0.0) | IPC protocol change, TOML schema incompatibility |

**Minimum rule:** Every merged feature or fix gets its own version bump commit. No batch bumps.

## When to bump

After each logical commit group (feature, fix, or refactor):
1. Edit `version` in `Cargo.toml`
2. Run `cargo check` to update `Cargo.lock`
3. Commit: `chore: bump to vX.Y.Z — <one-line description>`
4. The commit message should summarize WHAT changed and reference the issue if applicable

## Workflow integration

The PM should include a version bump as the final step of every implementation task.
Engineer delegations should end with: "After tests pass, bump the patch version in Cargo.toml
and commit: `chore: bump to vX.Y.Z — <description>`"

## Current version

See `Cargo.toml` — the `version` field is always the source of truth.
