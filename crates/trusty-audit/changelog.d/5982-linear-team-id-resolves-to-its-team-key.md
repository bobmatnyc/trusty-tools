Fixed

- `taudit add board linear:<team-id>` now registers the team's short key, so the
  board collects the issues it was registered for. Validation accepted a team id
  and stored it verbatim; collection matches `linear.team_keys` against the text
  before the hyphen in `ENG-1234`, which a team id never occupies, so the board
  validated green and contributed nothing on every subsequent run. The same
  round trip also stores Linear's own casing, which that exact-match filter is
  equally sensitive to.
- A Linear board already stored as something no issue can carry is now stated as
  a gap in the return package, naming the re-registration to make. It used to
  reach the collector and report as a covered board that happened to be quiet.
