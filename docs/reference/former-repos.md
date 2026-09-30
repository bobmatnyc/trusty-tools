# Former Repos Reference

These repos were merged into this monorepo. Use this table when reading old
PRs, issues, or commit messages that reference the former repo names.

| Former repo | Now lives in |
|---|---|
| `bobmatnyc/trusty-common` | `crates/trusty-common` + 8 library crates |
| `bobmatnyc/trusty-search` | `crates/trusty-search` |
| `bobmatnyc/trusty-memory` | `crates/trusty-common` (`memory-core` feature — storage engine) + `crates/trusty-memory` (MCP frontend) |
| `bobmatnyc/trusty-analyze` | `crates/trusty-analyze` |
| `bobmatnyc/trusty-mpm` | `crates/trusty-mpm/` (unified crate) + `crates/trusty-mpm-gui/` |
| `bobmatnyc/open-mpm` | `crates/trusty-agents` (renamed from `open-mpm` in #831) |

## Moved back out

These crates were in this monorepo and now live in their own repo again. Old
PRs, issues and commits that name `crates/<directory>` refer to the copy that
used to be here.

| Crate (binaries) | Former directory | Now lives in |
|---|---|---|
| `tga` (`tga`) | the former crates/trusty-git-analytics directory | [`bobmatnyc/trusty-git-analytics`](https://github.com/bobmatnyc/trusty-git-analytics) |
| `trusty-audit` (`trusty-audit`, `taudit`) | the former crates/trusty-audit directory | [`bobmatnyc/trusty-git-analytics`](https://github.com/bobmatnyc/trusty-git-analytics) |

## Archived

Components removed from main without a replacement repo. Restore from the recovery tag.

- trusty-voice: archived 2026-09-30 (owner ruling 167); restore with `git checkout archive/trusty-voice-2026-09-30 -- python/trusty-voice`
