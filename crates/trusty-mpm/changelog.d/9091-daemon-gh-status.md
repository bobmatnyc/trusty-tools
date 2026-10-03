Fixed

- `tm projects config <p> set gh-user <login>` no longer refuses a logged-in
  account when the daemon runs under launchd. launchd passes no
  `GH_CONFIG_DIR`, so the daemon's own `gh auth status` found no account when
  the operator keeps gh's config in a non-default dir. The check now also asks
  gh in the project's pinned `github.config_dir` and in tm's per-account dir
  (`~/.trusty-mpm/gh-accounts/<login>`, the dir the `--account` clone uses).
- A refused `gh_user` now says which of two things happened: "could not run
  gh: <reason>" (HTTP 503) or "gh reports no logged-in account" (HTTP 400). A
  `gh auth status` that fails without naming an account, such as a broken gh
  config, is no longer read as "no account". Neither case stores the login.
