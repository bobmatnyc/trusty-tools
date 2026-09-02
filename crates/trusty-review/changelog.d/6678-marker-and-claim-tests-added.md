Added
- `report::mermaid::tests::{parses_full_marker, ignores_non_dataset_comment}` — `parse_marker` had no direct coverage, so nothing proved the `group:` field survives the comma-inside-`y:` grammar or that a marker missing `x:`/`y:` is rejected rather than half-parsed (#6678).
- `report::reporter::tests::a_second_narrative_cannot_overwrite_a_claimed_row` — two narratives sharing one title and one cited file exercise `match_row`'s unclaimed-rows filter on its own, which component disambiguation cannot reach (#6678).
