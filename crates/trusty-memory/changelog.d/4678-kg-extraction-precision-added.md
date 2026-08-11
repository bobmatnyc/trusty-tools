Added
- **`kg-rebuild --purge-stale-subjects`** deletes auto-extracted subjects the
  new filter would now reject. The forward filter only stops new garbage:
  `rebuild_one` re-asserts and never retracts, and the four pattern predicates
  are not in `FUNCTIONAL_PREDICATES`, so an `assert` supersedes only an
  identical object and every triple already in the graph would otherwise stay
  there permanently. The flag is off by default, prints every subject it
  removes, and takes `--dry-run` to report the list while writing nothing at
  all. Selection is deliberately narrow — a subject is skipped if it sits under
  the `drawer:`/`tag:`/`topic:`/`room:` namespaces, or if any of its active
  triples was not stamped `auto:remember`, so one hand-asserted fact protects
  the whole subject.
