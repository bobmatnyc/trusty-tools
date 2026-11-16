Fixed
- The Architect dashboard store now stamps a document's `created` and `updated` from its own clock: a client-supplied value for either is ignored, `created` keeps the stored time on later writes, and one batch shares one write time with its change-log line. Event fields such as `at` and `done_at` still carry the writer's value (#10678).
