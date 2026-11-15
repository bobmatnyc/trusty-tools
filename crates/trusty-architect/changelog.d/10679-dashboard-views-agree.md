Fixed
- Dashboard store reads (`get`, `list`, `snapshot`) take `<root>/.lock` shared with the store's bounded wait, so a read never lands between the files of one batch and the task, session and header documents of one write are seen together; `snapshot` reads every collection under one lock. A read behind a writer that never lets go is `Unavailable` within the wait (#10679).
