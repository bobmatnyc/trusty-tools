Changed

- `grounding::quality::MIN_EVIDENCE_SCORE` is replaced by `MIN_EVIDENCE_SHARE`,
  and `is_evidence` takes the floor its query earned as a second argument.
  `evidence_floor` derives that floor from the query's best hit.
