Added

- **`kg_list_subjects` MCP tool — discover a palace's KG subjects instead of
  guessing at them** (closes [#4776](https://github.com/bobmatnyc/trusty-tools/issues/4776)).
  `kg_query` needs a subject the caller already knows, and a subject that does
  not exist returns the same empty result as an empty graph, so a guess was
  indistinguishable from a miss. The enumeration already existed at the HTTP
  layer (`GET /api/v1/palaces/{id}/kg/subjects`); this exposes it over MCP.
  Returns `{palace, subjects, with_counts, truncated}` — bare subject strings,
  or `{subject, count}` pairs under `with_counts: true` — alphabetically, with
  `limit` defaulting to 50 and clamped to `1..=200`. `truncated` is set when
  the page filled to the effective limit, so a partial view is never mistaken
  for the whole graph. The two page-size bounds moved to
  `service::core_kg` so the tool and the HTTP routes read one definition; they
  previously lived in `web::kg_routes`, which is not compiled without the
  `axum-server` feature.
