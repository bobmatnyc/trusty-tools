Added

- `trusty-memory palace legacy-kg <palace>` reports the drawers a pre-redb SQLite `kg.db` still holds that the live store lacks, plus unreadable rows, legacy triples and `.v2-incompatible` files. It is a dry run by default; `--apply` imports the missing drawers with their original id, room and timestamps, then embeds them so recall finds them. It never deletes or rewrites `kg.db`, and a re-run imports nothing ([#8434](https://github.com/bobmatnyc/trusty-tools/issues/8434))
