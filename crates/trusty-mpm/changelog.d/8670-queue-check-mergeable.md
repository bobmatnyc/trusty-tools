Fixed

- `tm pr queue-check` now requests GitHub's `mergeable` and `mergeStateStatus`
  fields and refuses a PR marked `CONFLICTING` or `DIRTY`, reports `UNKNOWN`
  as pending rather than mergeable, and fails closed on a missing field —
  previously it never read either field and could report MERGEABLE for a PR
  GitHub itself already flagged as conflicting.
