Added

- The audit sweep now reads each repository's own git history and reports its
  change hotspots — the files a team keeps rewriting — into the report
  manifest's `[report].findings` under the `churn` category, so the DD report
  states them instead of leaving the reader to guess where the work is (#6079).
  - The history is read by a direct `git log --numstat` shell-out from this
    crate. No tga dependency is added: the owner ruled 2026-08-19 that
    trusty-review must run independently of tga, and that all collector
    intelligence lives in trusty-audit so iteration is a single-crate rebuild.
  - Every threshold is a documented `pub const` in one place: a 180-day window,
    a 4000-commit read cap, 5 commits to be a hotspot at all, 20 to band RED, 20
    rows written and 10 offered to the ranking.
  - Churn is also an OPTIONAL third input to the evidence ranking
    (`evidence::blend_with`), entering each round behind the complexity hotspot
    and every dimension — the smallest non-zero weight that structure can
    express. An absent churn lane reproduces the previous ranking exactly, which
    `blend` now delegates for.
  - Lockfiles and changelogs are excluded by basename: their churn measures a
    generator rather than the code, and they would otherwise take the top of
    both outputs in every repository. The RANKING additionally declines
    configuration and prose by extension — measured against this workspace, the
    unfiltered top ten was seven `Cargo.toml`s, a `.tsv` allowlist and a
    `CLAUDE.md`. The findings still report those, because a manifest rewritten
    200 times in six months is real dependency movement a reader wants.
  - Fail-open, and never silently. A missing `git`, a path holding no
    repository, a shallow clone (whose counts stop at the graft point and would
    understate every hotspot), an empty window, a failed child, and an
    unwritable manifest each get a gap naming which it was — the collector's own
    diagnosis leading, with the child's first stderr line only ever as the
    parenthetical (#6720). A history that read clean states what its count does
    not cover. There is no not-applicable arm: every checkout arrives by `git
    clone`, so a path with no `.git` is an anomaly the report names rather than
    a leg that quietly does not apply.
  - A window deeper than the 4000-commit read cap is reported too. `git log
    --max-count` truncates in silence, so such a window is read to the cap and
    its hotspots ship with a caveat stating that every count is a floor and that
    a file whose work is entirely older than the read is missing outright.
  - Paths are read with `core.quotepath=false`, so a non-ASCII filename such as
    `src/café.rs` reaches the findings and the ranking as itself rather than as
    git's default `"src/caf\303\251.rs"` — a path no reader can open or grep
    for.
  - It needs neither daemon, so it is measured before the trusty-search and
    trusty-analyze gates: a repository whose daemons never answered still gets
    its change hotspots into the report.
