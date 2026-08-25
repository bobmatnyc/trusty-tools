Fixed

- The pinned-tool installer retries the release-list lookup when the crate is
  published and only the requested version is missing, and says so when the
  retries run out. `trusty-audit audit` failed hard half an hour after
  trusty-review 0.23.0 went live, naming `0.22.1` as the newest published
  version; a manual retry with no other change succeeded, and the message gave
  no hint that waiting was the answer. A crate with no published releases at all
  is still refused on the first answer — that is a typo, not lag.
