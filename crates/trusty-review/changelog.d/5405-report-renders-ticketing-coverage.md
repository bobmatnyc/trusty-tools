Added

- The technical due-diligence report renders a new section 8, "Ticketing &
  Delivery Traceability", stating how many commits reference tracked board items
  and which boards those items came from. Gaps & Caveats moves to section 9.
- The manifest's `[report]` section accepts a `ticketing` key naming the JSON
  artifact `tga audit` writes. A manifest that declares none renders the section
  as unassessed and names it under Gaps & Caveats; a run that correlated nothing
  states that in full rather than collapsing, so an empty board is never
  indistinguishable from an absent artifact.
