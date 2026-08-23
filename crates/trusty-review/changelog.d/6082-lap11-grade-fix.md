Fixed

- A withholding disclosure now names the finding whose field it is ([#6082](https://github.com/bobmatnyc/trusty-tools/issues/6082))
  - the reachability tier is read from the FILE, so the grounding check resolved it through that file's first loopback-scoped record and quoted that record's title; the last report's RED findings 2 and 3 both sit in `control_routes.rs`, so their two Synthesis Status lines shipped byte-identical, both naming finding 2
  - the check now takes the field's own finding title and prefers it over the shared record, for both the withheld and the corrected-wording line; report-level prose, which names no finding, still cites the one the subject-token match found for it
