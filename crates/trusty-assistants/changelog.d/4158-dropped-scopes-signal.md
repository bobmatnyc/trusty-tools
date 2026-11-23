Added

- `GET /api/agents/{name}/permissions` now reports the legacy `[tools].scopes`
  entries that DOC-57 CC-9 superseded, in a new top-level `dropped_scopes[]`
  array carrying each entry's `pattern`, `source` (`declared` or
  `inherited:<base>`) and `reason`
  ([#4158](https://github.com/bobmatnyc/trusty-tools/issues/4158)). When an
  agent declares both `[permissions].scopes` and legacy `[tools].scopes`, the
  new declaration wins and the union is deliberately not taken — but until now
  the only signal that anything was dropped was a server-side `tracing::warn!`
  no API or GUI consumer reads, and the app-launched daemon writes its logs to
  `/dev/null` ([#4111](https://github.com/bobmatnyc/trusty-tools/issues/4111)),
  so in the shipped product that warning did not exist. The warning is
  unchanged; this is its readable half, and the field is present only when
  something was actually dropped, so an existing client sees the exact shape it
  saw before. New `agents::permissions::dropped_scopes` computes the list; the
  scope set the route reports as effective is unchanged, so nothing is widened.
