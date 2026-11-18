Added

- `tm issue next [--repo] [--goal <id>] [--limit N] [--json]` lists the ready issues a factory session should take next. Ready is a ready board Status or a ready `status:*` label. Issues without acceptance criteria, or assigned to someone else, are skipped. Results rank by the session's goal in `goals.md`, then priority label, then age, and each names its signal and reason. A label/board disagreement marks the issue as a `tm issue repair` candidate. Board Status is read in one batched query per board and cached for 10 minutes; a rate limit exits non-zero with no list.
