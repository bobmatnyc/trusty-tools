Fixed

- Refactor suggestions from trusty-analyze no longer render in the report's RED/CRITICAL band, and analyze-derived findings now carry the daemon's own rationale and suggested action instead of showing `not stated in source data` in every prose slot. A finding that would still render as a bare title and path is dropped, and a repository whose analysis ran with no external static-analysis tool installed is named under Gaps & Caveats so an empty RED band reads as unassessed rather than clean (#5317).
