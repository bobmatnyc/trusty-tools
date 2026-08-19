Fixed

- `PATCH /api/v1/projects/{name}` no longer stores a `gh_user` that `gh` has
  never heard of
  (closes [#2121](https://github.com/bobmatnyc/trusty-tools/issues/2121)).
  The endpoint accepted and persisted whatever string it was handed, so a
  typo'd or not-yet-authenticated login returned 200 and surfaced much later as
  a delegated-agent failure. Setting the field now runs one bounded
  `gh auth status` and rejects the whole request with 400 when the login is not
  one of the logged-in accounts, naming the field, the rejected login, and the
  accounts that would be accepted; an accepted login is stored in `gh`'s own
  spelling, so the byte-for-byte comparison `ensure_gh_account_in_dir` makes
  later cannot disagree with it. A probe that cannot answer — no `gh` on PATH,
  or a timeout — is 503, never a silent accept and never a 400. Clearing with
  `null` and any request that does not mention `gh_user` run no probe at all.
