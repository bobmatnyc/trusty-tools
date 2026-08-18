Fixed

- `taudit add board linear:<team-id>` now registers the team's short key, so the
  board collects the issues it was registered for. Validation accepted a team id
  and stored it verbatim; collection matches `linear.team_keys` against the text
  before the hyphen in `ENG-1234`, which a team id never occupies, so the board
  validated green and contributed nothing on every subsequent run. The same
  round trip also stores Linear's own casing, which that exact-match filter is
  equally sensitive to.
- Registration now refuses a team whose key the sweep could never match, rather
  than persisting it. `validate` documented that a registered Linear key is one
  `is_linear_team_key` accepts and nothing enforced it: the reply's `key` field
  is optional, so a reply that omitted it registered an empty key — an entry that
  validated green, could never collect, and could not be removed either, because
  `taudit remove linear:` is not a spelling the parser accepts.
- A Linear team key already on disk is uppercased on its way to the sweep rather
  than skipped. tga reads a stored key twice and disagreed with itself: its
  collector compares team keys ignoring case, so `linear:eng` did collect
  `ENG-1234`, while its classifier compares exactly, so the same issue classified
  as nothing. Uppercasing keeps the collection and adds the classification.
- A Linear board that no normalisation can save is now stated as a gap on
  `taudit run` as well as `taudit audit`, and the run exits 2 rather than 0. The
  sweep resolved the boards, discarded the gaps, and returned "every repository
  audited" with no `linear:` section and nothing said — the silent skip the gap
  line exists to replace.
- That gap line now describes the key shape it needs instead of asserting the
  stored key is a team id, and names both halves of the remedy: registering the
  team key leaves the old entry behind, and a registry still holding it keeps
  reporting the board as not audited.
