Fixed

- The authorship artifact's monthly trajectory counts commits per month, not
  file touches ([#6082](https://github.com/bobmatnyc/trusty-tools/issues/6082)).
  The source query joins `files`, so it returns one row per file a commit
  touched, and the month counter incremented once per row. On a repository
  whose commits average ten files each, a month with 824 commits reported 9416.
  `active_authors` was always a distinct-set count and is unchanged.
