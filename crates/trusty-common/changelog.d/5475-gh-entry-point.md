Added

- `gh::GhCommand` — the workspace's single entry point for invoking the GitHub
  CLI ([#5475](https://github.com/bobmatnyc/trusty-tools/issues/5475)), behind
  the new `gh-cli` feature (implied by `tickets`). It renders `gh <args>` with
  an optional `--repo`, working directory, and environment overlay/removals,
  and runs it blocking, on tokio, or hands back the unspawned
  `std::process::Command` for call sites with their own timeout machinery.
  Every runner returns the full exit/stdout/stderr triple in `GhOutput` and
  never decides that a non-zero exit is fatal — `gh pr checks` reports check
  state through its exit code, so that policy stays at the call site.
  `GhOutput::ok`, `nonempty_stdout`, `json`, and `gh_available` are the shared
  policies on top; a missing binary is classified as `GhError::NotInstalled`
  with the `gh auth login` hint rather than an opaque IO error.
- `tickets`' GitHub backend resolves its `gh auth token` fallback through that
  entry point. Behaviour is unchanged, including the post-trim blank-output
  rejection: `gh auth token` exits non-zero when no account is logged in, but
  with `GH_TOKEN="   "` it exits ZERO printing whitespace, which a status-only
  check would pass on as a credential.
