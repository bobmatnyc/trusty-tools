Added

- `trusty-analyze report --manifest <path> [--template cast] [--code-only]` and the matching `tr_report` MCP tool generate a technical due-diligence report over the embedded trusty-review pipeline, under the existing `review` feature. Both call `trusty_review::report::run_report` rather than reimplementing manifest loading, template precedence, or the credential preflight. (#6669)
