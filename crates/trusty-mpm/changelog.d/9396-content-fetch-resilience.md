Fixed
- `tm content update` and the first-use content fetch retry a GitHub 5xx up to 3 times, after 1 s, 2 s and 4 s (#9396).
- A 403 or 429 from GitHub names the rate limit, the `x-ratelimit-remaining`, `retry-after` and `x-ratelimit-reset` headers it sent, and how to authenticate: set `GITHUB_TOKEN` or `GH_TOKEN`, or run `gh auth login` (#9396).
- With neither `GITHUB_TOKEN` nor `GH_TOKEN` set, the content fetch takes its token from `gh auth token`. A missing or failing `gh` leaves the fetch unauthenticated; it is never an error, and the token is never logged (#9396).
- Every network, missing-sidecar and no-content error names the manual install, with the release tag when it is known: `gh release download <tag> --repo bobmatnyc/trusty-tools`, then `tm content install --from <dir>/<tag>.tar.gz` (#9396).
- `tm doctor`, the doctor repairs, skill retirement, the savings row and the SM prompt never fetch the content release; only session composition, `tm install` and `tm reinstall` do. With no lock, the doctor `content` row warns with the same remedy as every not-installed error (#9396).
- A session launch reads its agent roster from the content it resolved for the project, not from tm's working directory, so one launch never mixes two content sources (#9396).
- After a failed first-use fetch, tm does not retry for 60 s, so repeated compositions do not each wait on a dead network; content installed in the meantime is served at once. The fetch no longer holds a tokio worker thread while it blocks (#9396).
