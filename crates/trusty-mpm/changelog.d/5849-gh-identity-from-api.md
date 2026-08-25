Fixed

- Per-project `gh` account enforcement now confirms the identity with
  `gh api user`, not by parsing `gh auth status` text
  (refs [#5849](https://github.com/bobmatnyc/trusty-tools/issues/5849)).
  `gh auth status` echoes whatever the scoped `GH_CONFIG_DIR`'s `hosts.yml`
  claims, so a `hosts.yml` naming an account that exists nowhere on the machine
  printed a green checkmark, live token scopes and `Active account: true` — and
  `ensure_gh_account_in_dir` / `ensure_gh_account_for_project` reported success
  for an identity nobody had verified. Both now compare the expected account
  against `gh api user --jq .login`, the login GitHub attributes to the
  credential the scoped `gh` actually resolves, and every probe failure —
  `gh` missing, a non-zero exit, blank output, or the 5s bound — is a
  verification failure rather than a pass. When the transcript and the
  credential name different accounts (the divergence tracked in
  [#5656](https://github.com/bobmatnyc/trusty-tools/issues/5656)) the API
  answer decides and the disagreement is logged at WARN.
