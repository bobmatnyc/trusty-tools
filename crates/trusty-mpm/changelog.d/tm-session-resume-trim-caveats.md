Documentation

- `tm-session-resume` condenses its `session_context_catchup` caveat
  section — ownership (`owned`, `resolved_via`, never inventing a
  `session_id`), `sessions` paging/truncation, `sessions` vs
  `resolved_snapshot`, `session_refs` cache-refresh, and the watermark note —
  from nine repeated blockquote paragraphs to one paragraph per topic, with
  the happy path (omit `session_id`, resolve via your own id) stated first.
  Every cited rule and issue reference (#7830, #5557, #5272, #5386, #6888,
  ADR-0062) is preserved.
