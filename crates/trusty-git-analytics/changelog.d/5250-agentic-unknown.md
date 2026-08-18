Added
- `AgenticMode::Unknown` — a fourth agentic mode for commits whose message git
  or a forge composed, so a marker the author emitted was discarded before the
  commit object existed. It separates "we read the author's message and found
  no AI marker" from "the message we hold was never the author's".
  - Detection reaches it only when no marker matched and the message shows a
    rewrite fingerprint: a git/GitHub merge summary (`Merge branch 'x'`,
    `Merge pull request #N from …`), or a `(#N)` squash subject whose body was
    replaced by synthesized trailers. A marker always wins.
  - "No marker matched" means BOTH the builtin and the operator sets (#5414)
    came up empty. An operator marker still classifies a merge-summary-shaped
    commit; the `Unknown` verdict is decided once, after both scans, so it
    never pre-empts the operator set.
  - No migration: `commits.agentic_mode` is `TEXT NOT NULL DEFAULT 'none'` with
    no CHECK constraint, so `'unknown'` was already schema-legal.
  - `agentic_pct` in `fact_weekly_engineer` keeps `unknown` in the
    `net_commits` denominator and out of both numerators, which makes it an
    explicit lower bound. `persist_weekly_engineer`'s doc states this.
  - Reading an unrecognised `agentic_mode` string back from SQLite now yields
    `Unknown` rather than `None`.
