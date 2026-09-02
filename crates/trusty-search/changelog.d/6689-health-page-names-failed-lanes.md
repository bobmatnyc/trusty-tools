Added

- The Health page names the indexes with a failed lane, from the `indexes_stage_failed_ids` array [#6694](https://github.com/bobmatnyc/trusty-tools/pull/6694) added to `GET /health`, instead of leaving an operator to poll every registered index to find the one behind a count. The card says outright that a lane can report `ready` and still be empty, because `IndexStages::any_failed()` cannot see the zero-vector case ([#6689](https://github.com/bobmatnyc/trusty-tools/issues/6689))
