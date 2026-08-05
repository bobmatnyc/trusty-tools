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
    against the live estate it reproduces ADR-0028 §C7's table — the top drawer measures
    45.1% of `trusty-tools` turns where the ADR measured 44.8%, and the second measures
    20.7% against its 21.2%. The share is quoted rather than the raw count because the
    count climbs with every logged turn; only the ratio is stable enough to cite
  - two drawers in one palace whose content truncates to the same 220-char excerpt are
    indistinguishable in the logs and receive one combined count. Such rows are marked
    `⚠ SHARED` with a content digest that separates them, and counted in the header, so a
    combined count can never be mistaken for a per-drawer one. The live estate has no
    collisions today
  - a drawer created before the scanned log window carries `predates-log-window`, so a
    reading of 0 injections is distinguishable from "genuinely never retrieved" — the two
    warrant opposite decisions
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
