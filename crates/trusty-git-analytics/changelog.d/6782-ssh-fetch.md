Fixed

- `tga collect` and `tga audit` can now fetch from an SSH-scheme `origin`. The
  `git2` dependency was built without the `ssh` feature, so libgit2 had no
  libssh2 transport and rejected every `git@host:org/repo.git` or `ssh://`
  remote with `unsupported URL protocol; class=Net (12)` before the fetch's
  credential callback ran — 59 of 59 repositories in a client audit, each
  collected from clone-time refs. Adding the feature links libssh2 from its own
  vendored source and reuses the openssl this crate already vendors, so no new
  system library or runtime dependency (#6782).
- The non-interactive credential chain now offers each source at most once per
  fetch instead of answering from the top every time libgit2 re-enters the
  callback. `ssh-agent` running with no identities loaded reports success, so
  the old behaviour re-offered the empty agent until libgit2 gave up — 120
  seconds per repository — and never reached `~/.ssh/id_ed25519` (#6782).
- A repository collected from stale local refs now leads the report's Gaps &
  Caveats section with **git history is stale: fetch failed (…)**, ahead of the
  failed stages, and is named on stderr during the run. It was one unemphasised
  sentence mid-list, which a reader taking the commit and pull-request figures
  at face value could pass over (#6782).
