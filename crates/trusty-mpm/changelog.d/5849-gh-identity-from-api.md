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
  verification failure rather than a pass — including a lost network, which now
  blocks enforcement instead of passing it (every operation it gates needs the
  network anyway). A failed probe also stops before `gh auth switch`, so a blip
  cannot rewrite the project's config dir. Enforcement additionally refuses
  outright when an ambient `GH_TOKEN`/`GITHUB_TOKEN` is set: that token outranks
  `GH_CONFIG_DIR` for every later `gh` call, so no scoped probe can speak for
  what those calls will do — including a whitespace-only token, which `gh`
  selects rather than ignores. Enforcement also now runs on the project's
  configured `github.host` instead of assuming github.com, so a project pointed
  at an enterprise instance is verified on the host its work actually uses;
  `configured_account_pair` returns an `AccountTarget` carrying that host
  alongside the config dir and account. When the transcript and the
  credential name different accounts (the divergence tracked in
  [#5656](https://github.com/bobmatnyc/trusty-tools/issues/5656)) the API
  answer decides and the disagreement is logged at WARN.
