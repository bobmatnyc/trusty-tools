# Documentation Layout Reference

Documentation is organized by **published crate**, not by topic. Each crate gets
a directory under `docs/` containing three standard subdirectories:

```
docs/
├── trusty-search/              # See here for the worked example
│   ├── regression-testing/     # Versioned snapshots: v{VERSION}-{DATE}.md
│   ├── research/               # Investigation & decision docs: *-{DATE}.md or *-decision-{DATE}.md
│   └── sessions/               # Engineering session summaries: SESSION-{DATE}-{topic}.md
├── trusty-memory/              # Follows the same three-subdir convention
├── trusty-common/              # (and all other published crates)
├── trusty-mpm/                 # covers all 8 trusty-mpm-* binaries
├── trusty-agents/
├── trusty-analyze/
└── trusty-git-analytics/
```

## Purpose of each subdir

- **`regression-testing/`** — Performance snapshots tied to releases. One `.md`
  file per measured release named `v{VERSION}-{YYYY-MM-DD}.md`; alternate-corpus
  baselines (e.g., synthetic, trusty-agents) live alongside; `current.md` is a
  symlink to the latest snapshot.
- **`research/`** — Investigation outcomes, audits, decision documents. Named
  `{topic}-{YYYY-MM-DD}.md` or `{topic}-decision-{YYYY-MM-DD}.md`.
- **`sessions/`** — Engineering-session narratives. Named
  `SESSION-{YYYY-MM-DD}-{topic}.md`.

Each subdir has a `README.md` explaining its purpose, file naming, and indexing
conventions. **See `docs/trusty-search/` as the authoritative worked example.**

For **cross-release performance tracking**, see GitHub issue
[#129](https://github.com/bobmatnyc/trusty-tools/issues/129): it accumulates
benchmark deltas across all measured versions.
