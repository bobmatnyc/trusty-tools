Fixed
- The Architect dashboard's `/api/fleet`, `/api/projects` and `/api/cli-profiles` no longer run their route after refusing a session-mode or non-GET request; the refusal was sent and the route body then ran too (#11065).
