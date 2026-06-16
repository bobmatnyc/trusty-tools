# trusty-tools Changelog Index

This workspace is a monorepo consolidating 20 independent Rust crates. Each crate maintains its own **authoritative** changelog in its directory.

## Per-Crate Changelogs

The source of truth for changes to any crate is its own CHANGELOG:

| Crate | Changelog |
|---|---|
| **trusty-search** | [crates/trusty-search/CHANGELOG.md](crates/trusty-search/CHANGELOG.md) |
| **trusty-memory** | [crates/trusty-memory/CHANGELOG.md](crates/trusty-memory/CHANGELOG.md) |
| **trusty-analyze** | [crates/trusty-analyze/CHANGELOG.md](crates/trusty-analyze/CHANGELOG.md) |
| **trusty-mpm** | [crates/trusty-mpm/CHANGELOG.md](crates/trusty-mpm/CHANGELOG.md) |
| **trusty-agents** | [crates/trusty-agents/CHANGELOG.md](crates/trusty-agents/CHANGELOG.md) |
| **trusty-git-analytics** | [crates/trusty-git-analytics/CHANGELOG.md](crates/trusty-git-analytics/CHANGELOG.md) |
| **trusty-review** | [crates/trusty-review/CHANGELOG.md](crates/trusty-review/CHANGELOG.md) |
| **trusty-common** | [crates/trusty-common/CHANGELOG.md](crates/trusty-common/CHANGELOG.md) |
| **trusty-embedderd** | [crates/trusty-embedderd/CHANGELOG.md](crates/trusty-embedderd/CHANGELOG.md) |
| **trusty-mpm-gui** | [crates/trusty-mpm-gui/CHANGELOG.md](crates/trusty-mpm-gui/CHANGELOG.md) |

Other crates (libraries, internal tools) may not maintain per-release changelogs. Consult their git history via `git log crates/<name>/` for changes.

## Versioning

Each crate manages its own independent semantic version. When publishing, only the crates with user-facing changes are released with new versions. See [CONTRIBUTING.md](CONTRIBUTING.md) for the per-crate release convention.

## Release Tags

Releases are tagged as `<crate-name>-v<version>`. Example: `trusty-search-v0.8.0`.

To see all release tags:
```bash
git tag -l
```

To see commits for a specific crate:
```bash
git log crates/<crate-name>/
```

## Finding Changes

To see all changes across the workspace since a date:
```bash
git log --since="2026-01-01" --oneline
```

To see changes affecting a specific crate:
```bash
git log crates/<crate-name>/ --oneline
```

---

**For detailed development and release information**, see [CONTRIBUTING.md](CONTRIBUTING.md) and [docs/reference/release-workflow.md](docs/reference/release-workflow.md).
