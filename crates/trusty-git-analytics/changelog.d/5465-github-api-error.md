Changed

- `collect::errors::CollectError` gains a `GithubApi { status, endpoint, message }` variant carrying GitHub's response body. `error_for_status()` discards that body, which is where GitHub puts the only actionable part of a write failure ("Resource not accessible by personal access token") — without it a token-scope problem is indistinguishable from any other 403, and the new write path is exactly where that distinction decides what the operator has to fix. The enum is `#[non_exhaustive]`, so this is additive.
