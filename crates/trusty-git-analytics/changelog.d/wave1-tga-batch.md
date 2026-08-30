Fixed

- **A short security keyword no longer matches inside a longer word (#4331)** —
  the Tier-1 exact matcher is an Aho-Corasick substring search, so
  `kw-security`'s three-letter `rce` keyword fired inside `source`, `resource`,
  `commerce`, and `enforce`. Any commit carrying one of those words classified
  as `security` at priority 80 and confidence 0.9, outranking the rule that
  should have won — measured on `feat(pegasus): S3 SignalStore schema for source
  events`, which returned `("security", ExactRule, 0.9)`. A match now has to sit
  on a word boundary, and which of a keyword's two edges are checked comes from
  the keyword's own spelling, so `cve-` still matches `CVE-2024-1234`.
- **Linear collection reads its API key through the shared credential resolver
  (#5983)** — `LinearClient::new` consulted `linear.api_key` and nothing else,
  so an operator holding the key where the rest of the workspace looks for it
  (`LINEAR_API_KEY` in the environment, `.env.local`, the OS keychain) could not
  collect until they hand-edited YAML. Config stays the first tier; when it is
  silent or expands to empty, resolution falls through to
  `trusty_common::credentials::resolve_key("linear")`. No CLI is involved on
  this path, and the error now names every place the key may live.
- **A test invocation that ran nothing is refused (#4307)** — `cargo test` with
  a filter matching zero tests exits 0 and prints `ok`, so a filter derived from
  a `#[path]` module's FILE name reports green having proved nothing.
  `scripts/check_test_count.sh` wraps a test invocation, sums its
  `running N tests` lines, and fails when the aggregate across the invocation is
  zero, passing the command's own exit status through otherwise.
  `.github/workflows/test-count.yml` runs it against two real cargo invocations
  each build, alongside `scripts/check_test_count_selftest.sh`'s fixtures.
- **#4251 needed no change** — the Tier-3/4 fuzzy-fallback gate it asks for
  landed in PR #4291 (`IdentityResolver::fuzzy_fallback`, the
  `fuzzy_identity_fallback` config key, and the `ISSUE_4251_MISATTRIBUTIONS`
  reproduction in `resolver_tests`). Recorded here because the issue was in this
  batch; the code is unchanged.
