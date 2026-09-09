Fixed

- `session_context_pause` publishes its snapshot as a pushed `chore/sessions-*` branch and an auto-merging docs-only PR instead of leaving it to be committed onto the main checkout's current branch, where a PR-only `main` stranded it ([#7282](https://github.com/bobmatnyc/trusty-tools/issues/7282))
