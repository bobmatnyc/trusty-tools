Fixed
- pm-guard's secret-file rule no longer refuses a `curl` or `wget` of one of Google's OAuth2 `tokeninfo` endpoints, a `cd`/`pushd` into a source directory named `secrets` or `credentials`, or a `git checkout`/`git switch` of an existing branch whose name carries one of those words. Any other URL naming a secret-shaped word, such as a metadata server's `…/token`, is still refused (#8110).
