Fixed

- A Bitbucket workspace that cannot be read no longer reads as a workspace with
  no repositories. A rejected credential surfaces as `CollectError::BitbucketApi`
  carrying the HTTP status and Bitbucket's own explanation, a rate limit as
  `CollectError::Throttled` with any `Retry-After` hint, and the 5 000-repository
  page cap logs that the set is partial. (#5220)
