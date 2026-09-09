Fixed

- `session_context_pause` publishes its snapshot as a pushed `chore/sessions-*` branch and an auto-merging docs-only PR instead of leaving it to be committed onto the main checkout's current branch, where a PR-only `main` stranded it. The branch it publishes from is the project's own default — `config.yaml`'s `projects:` entry, else `origin/HEAD`, else `main` — so a `master` or `develop` project no longer errors on every pause, and a skipped publish reports why (`not_tracked`, `not_a_git_repo`, `unchanged`) ([#7282](https://github.com/bobmatnyc/trusty-tools/issues/7282))
