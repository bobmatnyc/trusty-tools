Fixed
- The `gh` login now reaches the child's git transport. The sweep already read
  `gh auth token` once and handed it over as `TRUSTY_AUDIT_GITHUB_TOKEN`, which
  only tga's `github:` config section references; the fetch that runs before
  every collection reads `GITHUB_TOKEN`. A recipient logged in with `gh` and
  nothing else had every fetch fail and got a header-only `pr-metrics.csv` per
  repository, over a usable credential this process was holding. An
  operator-exported token is never replaced — the forward happens only when the
  `gh` login is the sole source.
- A sweep with no non-interactive git credential at all refuses before any child
  runs, naming how many repositories would have been fetched and every way to
  supply one (`gh auth login`, `GITHUB_TOKEN`/`GH_TOKEN`, an SSH key,
  `ssh-agent`). It fires only when the selected checkouts provably name a
  `github.com` remote, so an engagement over paths on disk still runs. Before
  this the run reported success having collected no pull-request data at all,
  and said so only in one repeated line inside each repository's own manifest.
