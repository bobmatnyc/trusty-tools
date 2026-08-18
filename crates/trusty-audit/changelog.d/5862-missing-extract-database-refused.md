Fixed

- The return package could ship a repository's rendered report with its `extract/<stem>.db` collection database silently missing — `collect_extract` returned `Ok(())` whenever the database was absent, whether `extract/` itself did not exist or simply named nothing for that repository. Assembly now refuses with a named `MissingExtractDatabase` error instead, so the deliverable is always the database and the report together, never one without the other going unnoticed ([#5862](https://github.com/bobmatnyc/trusty-tools/issues/5862))
