Added

- `trusty-memory backfill-report` — the read-only triage list for ADR-0028's human-gated
  backfill (Migration step 3, closes [#4891](https://github.com/bobmatnyc/trusty-tools/issues/4891))
  - ranks existing drawers by how many turns they actually reached, so the worst
    offenders are triaged first — the ADR's motivating case is a 19-day-old session
    checkpoint reaching 44.8% of turns, and a stale drawer nobody retrieves costs nothing
  - each row carries what a single triage decision needs: drawer id, content excerpt,
    age, injection count and share of that palace's turns, stored importance and its
    decayed value, whether `expires_at` is already set, and room/palace
  - injection frequency is recovered from the enriched-prompt hook logs, which record the
    rendered injection but no drawer id. The join re-renders each drawer's preview with
    the same `drawer_preview` the injection pipeline uses and counts matching bullets;
    against the live estate it reproduces ADR-0028 §C7's table (3,705 injections / 45.1%
    for the top drawer, where the ADR measured 3,612 / 44.8% a day earlier)
  - no tier is suggested. §C4 measured why: `resume-target` splits 71/26 across tiers, so
    a tag-derived verdict would be wrong for a quarter of rows while looking exactly as
    confident as the rest. Rows carry checkable observations instead
  - with no hook log present the report says the counts are missing data rather than
    presenting estate-wide zeros as an absence of stale drawers
  - `--json` for scripted triage; `--palace`, `--limit`, `--min-injections`, `--logs-dir`
  - the command writes nothing, by construction: it never opens a `PalaceHandle` (that
    path deletes expired drawers at open, and expired drawers are exactly what a human
    triages), and it reads a private copy of each palace's redb store rather than the live
    file, because `OpenIntent::ReadOnlyClient` only snapshots when the file is already
    locked and otherwise reaches `Database::create` — which runs an init write
    transaction and can rename an incompatible-format store aside
