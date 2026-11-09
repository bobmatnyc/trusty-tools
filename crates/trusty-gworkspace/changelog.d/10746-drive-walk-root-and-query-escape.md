Fixed
- `api::bulk` Drive walk (#10746): it checks its root folder before it lists the first page. An unknown, trashed or non-folder root is refused with an error, and nothing is listed or tombstoned.
- `api::bulk::parents_query` escapes `\` and `'` in the folder ID, so a crafted ID cannot close the Drive query literal and widen the listing.
