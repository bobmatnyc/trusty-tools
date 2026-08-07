# Former repositories (fixture)

Minimal stand-in for the real `docs/reference/former-repos.md`. Its only job is
to give `derive_former_repo_urls()` something to derive from, so the STALE
fixtures exercise the `@FORMER_REPO_CLONE_URLS@` expansion instead of skipping
it. Keep the backticked `bobmatnyc/<repo>` shape — that is what the gate greps.

| Former repo | Folded into |
|---|---|
| `bobmatnyc/open-mpm` | `bobmatnyc/trusty-tools` |
| `bobmatnyc/trusty-search` | `bobmatnyc/trusty-tools` |
